//! The one owner of a running [`Entry`]: a task on the client runtime that takes the person's
//! Intents in order, the answers of the requests Entry asked for and its deadlines, and after each
//! change publishes the newest [`EntryView`].

use std::{
    future::{Future, pending},
    sync::Arc,
};

use futures_util::{FutureExt as _, future::BoxFuture};
use lobby_protocol::{JoinStatusRequest, MagicCodeRequest};
use tokio::{
    runtime::Handle,
    sync::{mpsc, watch},
    task::JoinSet,
};
use tokio_util::task::AbortOnDropHandle;
use tracing::{Instrument as _, info_span, warn};

use super::{Done, Entry, EntryIntent, EntryView, Op};
use crate::{Clock, LobbyOrigin, api, tokens::Keychain};

#[derive(Clone)]
pub(crate) struct EntryIo {
    pub rt: Handle,
    pub http: reqwest::Client,
    pub keychain: Keychain,
    pub clock: Arc<dyn Clock>,
}

/// Puts the person's Intents into a running Entry. Dropping it aborts Entry's requests; tokens
/// being stored for a sign in nobody saw are cleared once their save is done.
pub struct EntryHandle {
    inputs: mpsc::UnboundedSender<EntryIntent>,
    dispatched: u64,
    _driver: AbortOnDropHandle<()>,
}

impl EntryHandle {
    /// Returns the Intent's number: a view whose `taken` is at least that has taken it.
    pub fn dispatch(&mut self, intent: EntryIntent) -> u64 {
        self.dispatched += 1;
        let _ = self.inputs.send(intent);
        self.dispatched
    }
}

/// What a running Entry publishes: the newest view.
pub struct EntryUpdates {
    rx: watch::Receiver<Arc<EntryView>>,
}

impl EntryUpdates {
    /// Waits for a newer view; `false` once Entry has ended. Needs no reactor.
    pub fn changed(&mut self) -> impl Future<Output = bool> + Send + '_ {
        self.rx.changed().map(|changed| changed.is_ok())
    }

    pub fn view(&mut self) -> Arc<EntryView> {
        self.rx.borrow_and_update().clone()
    }
}

pub(crate) fn drive(io: EntryIo, entry: Entry) -> (EntryHandle, EntryUpdates) {
    let (inputs_tx, inputs) = mpsc::unbounded_channel();
    let (out, rx) = watch::channel(Arc::new(entry.view(io.clock.now())));
    let rt = io.rt.clone();
    let driver = Driver {
        io,
        entry,
        inputs,
        out,
        ops: JoinSet::new(),
    };
    let task = rt.spawn(driver.run());
    let handle = EntryHandle {
        inputs: inputs_tx,
        dispatched: 0,
        _driver: AbortOnDropHandle::new(task),
    };
    (handle, EntryUpdates { rx })
}

struct Driver {
    io: EntryIo,
    entry: Entry,
    inputs: mpsc::UnboundedReceiver<EntryIntent>,
    out: watch::Sender<Arc<EntryView>>,
    /// The requests in flight; dropped with the driver, which aborts them.
    ops: JoinSet<(u64, Done)>,
}

enum Work {
    Intent(EntryIntent),
    Done(u64, Done),
    Due,
}

impl Driver {
    async fn run(mut self) {
        loop {
            // The time is read once a round, so what came due since the last is shown too.
            let now = self.io.clock.now();
            self.entry.tick(now);
            self.start_ops();
            let view = Arc::new(self.entry.view(now));
            self.out.send_if_modified(|shown| {
                let changed = **shown != *view;
                if changed {
                    *shown = view;
                }
                changed
            });
            let due = self
                .entry
                .next_deadline(now)
                .map(|at| self.io.clock.sleep_until(at));
            let work = tokio::select! {
                biased;
                intent = self.inputs.recv() => match intent {
                    Some(intent) => Work::Intent(intent),
                    None => return,
                },
                Some(joined) = self.ops.join_next() => match joined {
                    Ok((epoch, done)) => Work::Done(epoch, done),
                    Err(e) => {
                        warn!(error = %e, "an entry request ended without an answer");
                        continue;
                    }
                },
                () = async { match due { Some(due) => due.await, None => pending().await } } => {
                    Work::Due
                }
            };
            let now = self.io.clock.now();
            match work {
                Work::Intent(intent) => self.entry.dispatch(intent, now),
                Work::Done(epoch, done) => self.entry.complete(epoch, done, now),
                Work::Due => {}
            }
        }
    }

    fn start_ops(&mut self) {
        for (epoch, op) in self.entry.take_ops() {
            let span = info_span!("entry", lobby = %op_origin(&op));
            let answer = run(&self.io, op);
            self.ops.spawn_on(
                async move { (epoch, answer.await) }.instrument(span),
                &self.io.rt,
            );
        }
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        if let Some(origin) = self.entry.storing() {
            // In line behind the save once called: it runs with nobody waiting for its answer.
            drop(self.io.keychain.clear(origin));
        }
    }
}

fn op_origin(op: &Op) -> &LobbyOrigin {
    match op {
        Op::Reach(o)
        | Op::Describe(o)
        | Op::RequestCode(o, _)
        | Op::Verify(o, _)
        | Op::Join(o, _)
        | Op::Status(o, _)
        | Op::Store(o, _)
        | Op::Clear(o) => o,
    }
}

/// The request for `op`. A store or a clear takes its place in the keychain's line now, in the
/// order Entry asked, and runs even when its request is aborted: tokens nobody signed in with go
/// after their save.
fn run(io: &EntryIo, op: Op) -> BoxFuture<'static, Done> {
    let http = io.http.clone();
    match op {
        Op::Store(origin, tokens) => io.keychain.save(&origin, &tokens).map(Done::Stored).boxed(),
        Op::Clear(origin) => io
            .keychain
            .clear(&origin)
            .map(|cleared| {
                if let Err(e) = cleared {
                    warn!(error = %e, "could not clear the tokens of a sign in that was left");
                }
                Done::Cleared
            })
            .boxed(),
        Op::Reach(origin) => {
            let io = io.clone();
            async move {
                let info = api::checked_lobby_info(&io.http, &origin).await;
                let stored = match &info {
                    Ok(_) => io.keychain.load(&origin).await.map(|t| t.is_some()),
                    Err(_) => Ok(false),
                };
                Done::Reached(info, stored)
            }
            .boxed()
        }
        Op::Describe(origin) => {
            let io = io.clone();
            async move {
                let info = api::checked_lobby_info(&io.http, &origin).await;
                Done::Described(origin, info)
            }
            .boxed()
        }
        Op::RequestCode(origin, email) => async move {
            let body = MagicCodeRequest {
                email: email.clone(),
            };
            Done::CodeSent(email, api::request_magic_code(&http, &origin, body).await)
        }
        .boxed(),
        Op::Verify(origin, body) => {
            async move { Done::Verified(api::verify_magic_code(&http, &origin, body).await) }
                .boxed()
        }
        Op::Join(origin, body) => async move {
            let name = body.username.clone();
            Done::Joined(name, api::join(&http, &origin, body).await)
        }
        .boxed(),
        Op::Status(origin, wait) => async move {
            let body = JoinStatusRequest { wait_token: wait };
            Done::Status(api::join_status(&http, &origin, body).await)
        }
        .boxed(),
    }
}
