//! The one owner of a running [`Session`]: a task on the client runtime that takes the Member's
//! Intents in the order they were dispatched, the connection's and the operations' events, and
//! the Session's deadlines, and after each round publishes the newest [`Snapshot`] with the news
//! in order. [`SessionHandle`] puts Intents in; [`Updates`] takes what comes out.

use std::{
    any::Any,
    future::{Future, pending},
    panic::AssertUnwindSafe,
    pin::Pin,
    sync::{Arc, Mutex, MutexGuard},
    time::Instant,
};

use futures_util::FutureExt as _;
use tokio::{
    runtime::Handle,
    sync::mpsc::{self, error::TryRecvError},
};
use tokio_util::task::AbortOnDropHandle;

use super::{Intent, Session, SessionEvent, UiEffects};
use crate::{ClientTask, LobbyOrigin, LobbyState, VideoFeed, media::OwnShareFeed};

/// After the first piece of work, a round takes at most this many more inputs and this many more
/// events, then ticks and publishes: a flood of either still lets deadlines and the window through.
const ROUND: usize = 256;

/// The time a driven Session lives by: when it is, and a wait until then.
pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
    fn sleep_until(&self, at: Instant) -> Pin<Box<dyn Future<Output = ()> + Send>>;
}

/// The client runtime's clock: Tokio's, so a paused Tokio clock moves it.
pub(crate) struct RuntimeClock;

impl Clock for RuntimeClock {
    fn now(&self) -> Instant {
        tokio::time::Instant::now().into_std()
    }

    fn sleep_until(&self, at: Instant) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(tokio::time::sleep_until(at.into()))
    }
}

/// What the views read of a running Session: its state, and what the Session keeps beside it.
#[derive(Clone, Default)]
pub struct Snapshot {
    /// The same `Arc` as the snapshot before when the round left the state as it was.
    pub state: Arc<LobbyState>,
    pub pen_down: bool,
    pub dnd: bool,
    pub idle: bool,
    /// How many Intents the Session has taken: every one [`SessionHandle::dispatch`] numbered up
    /// to this.
    pub intents: u64,
    own_share: Option<Arc<OwnShareFeed>>,
}

impl Snapshot {
    /// A new feed of what this Member's running share sends, drawn without the network. It
    /// closes the feed opened before, and it ends with the share.
    pub fn own_share_feed(&self) -> Option<VideoFeed> {
        Some(self.own_share.as_ref()?.open())
    }

    pub fn typing_line(&self, now: Instant) -> Option<String> {
        self.state.typing_line(now)
    }

    /// Whether a view would draw `other` as it draws this one: only the Intent count moved.
    pub fn shows_the_same(&self, other: &Snapshot) -> bool {
        Arc::ptr_eq(&self.state, &other.state)
            && (self.pen_down, self.dnd, self.idle) == (other.pen_down, other.dnd, other.idle)
    }
}

impl std::fmt::Debug for Snapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Snapshot")
            .field("state", &self.state)
            .field("pen_down", &self.pen_down)
            .field("dnd", &self.dnd)
            .field("idle", &self.idle)
            .field("intents", &self.intents)
            .finish_non_exhaustive()
    }
}

impl Session {
    fn snapshot(&self, state: Arc<LobbyState>, intents: u64) -> Snapshot {
        Snapshot {
            state,
            pen_down: self.pen_is_down(),
            dnd: self.dnd(),
            idle: self.idle(),
            intents,
            own_share: self.own_share(),
        }
    }
}

/// What came out of the Session since the last take.
#[derive(Debug)]
pub struct SessionUpdate {
    pub snapshot: Arc<Snapshot>,
    pub news: UiEffects,
    /// The pictures of the Watch that started, once. Every picture names its Sharer, and the
    /// snapshot's `share.watch` says whose the viewer shows.
    pub watch_feed: Option<VideoFeed>,
}

#[derive(Default)]
struct Outbox {
    snapshot: Arc<Snapshot>,
    news: UiEffects,
    watch_feed: Option<VideoFeed>,
    /// Why the driver ended without a Stop: a bug in the Session.
    panic: Option<Box<dyn Any + Send>>,
}

fn lock(out: &Mutex<Outbox>) -> MutexGuard<'_, Outbox> {
    out.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Debug, Clone, Copy)]
enum Stop {
    Shutdown,
    SignOut,
    LeaveLobby,
}

enum Input {
    Intent(Intent),
    /// An Intent whose own effects a test reads back, as well as in the updates.
    #[cfg(feature = "test-support")]
    Act(Intent, tokio::sync::oneshot::Sender<UiEffects>),
    Stop(Stop),
    #[cfg(feature = "test-support")]
    Inspect(Box<dyn FnOnce(&Session) + Send>),
}

/// Puts the Member's Intents into the running Session. Dropping it ends the Session with its
/// call, its connection and its operations.
pub struct SessionHandle {
    origin: LobbyOrigin,
    inputs: mpsc::UnboundedSender<Input>,
    dispatched: u64,
    driver: AbortOnDropHandle<()>,
}

impl SessionHandle {
    pub fn origin(&self) -> &LobbyOrigin {
        &self.origin
    }

    /// The one way a Member's action comes in. Returns its number: a snapshot whose `intents` is
    /// at least that has taken it, and a refusal is an Issue among that update's news.
    pub fn dispatch(&mut self, intent: Intent) -> u64 {
        self.dispatched += 1;
        let _ = self.inputs.send(Input::Intent(intent));
        self.dispatched
    }

    /// Stops Voice media, closes the WebSocket (1000) and aborts HTTP, after the Intents
    /// dispatched before it; resolves once the media threads and the connection have exited.
    pub fn shutdown(self) -> ClientTask<()> {
        self.stop(Stop::Shutdown)
    }

    /// Signs this device out of the Lobby: see [`Session::sign_out`].
    pub fn sign_out(self) -> ClientTask<()> {
        self.stop(Stop::SignOut)
    }

    /// After the Lobby let this Member leave: see [`Session::leave_lobby`].
    pub fn leave_lobby(self) -> ClientTask<()> {
        self.stop(Stop::LeaveLobby)
    }

    fn stop(self, stop: Stop) -> ClientTask<()> {
        let _ = self.inputs.send(Input::Stop(stop));
        ClientTask::from_handle(self.driver)
    }

    #[cfg(feature = "test-support")]
    pub(crate) fn act(
        &mut self,
        intent: Intent,
    ) -> (u64, tokio::sync::oneshot::Receiver<UiEffects>) {
        self.dispatched += 1;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = self.inputs.send(Input::Act(intent, tx));
        (self.dispatched, rx)
    }

    #[cfg(feature = "test-support")]
    pub(crate) fn inspect(&self, read: Box<dyn FnOnce(&Session) + Send>) {
        let _ = self.inputs.send(Input::Inspect(read));
    }
}

/// What the running Session publishes: the newest snapshot, the news in order, and a wake per
/// round that left something to take.
pub struct Updates {
    out: Arc<Mutex<Outbox>>,
    wake: mpsc::Receiver<()>,
}

impl Updates {
    /// Waits until a round published something to [`take`](Updates::take); `false` once the
    /// Session has ended and the last round was taken. Needs no reactor. A panic in the Session
    /// is raised again here, on the caller's thread.
    pub fn changed(&mut self) -> impl Future<Output = bool> + Send + '_ {
        let Updates { out, wake } = self;
        wake.recv().map(|woke| {
            if woke.is_none()
                && let Some(panic) = lock(out).panic.take()
            {
                std::panic::resume_unwind(panic);
            }
            woke.is_some()
        })
    }

    /// The newest snapshot, which also stays for the next take with the news.
    pub fn newest(&self) -> Arc<Snapshot> {
        lock(&self.out).snapshot.clone()
    }

    /// What was published since the last take, without waiting.
    pub fn take(&mut self) -> SessionUpdate {
        let mut out = lock(&self.out);
        SessionUpdate {
            snapshot: out.snapshot.clone(),
            news: std::mem::take(&mut out.news),
            watch_feed: out.watch_feed.take(),
        }
    }
}

/// Starts the driver of `session`, whose events arrive on `events`.
pub(crate) fn drive(
    rt: &Handle,
    clock: Arc<dyn Clock>,
    session: Session,
    events: mpsc::Receiver<SessionEvent>,
) -> (SessionHandle, Updates) {
    let (inputs_tx, inputs) = mpsc::unbounded_channel();
    let (wake_tx, wake) = mpsc::channel(1);
    // The state it starts from is there to take before the first round, so a view of a Lobby
    // switched back to draws its feed on its first frame.
    let published = Arc::new(session.state.clone());
    let out = Arc::new(Mutex::new(Outbox {
        snapshot: Arc::new(session.snapshot(published.clone(), 0)),
        ..Outbox::default()
    }));
    let origin = session.origin().clone();
    // This clone outlives the driver, so the wake channel closes only once its panic is in the
    // outbox.
    let last_wake = wake_tx.clone();
    let driver = Driver {
        session,
        events,
        inputs,
        clock,
        out: out.clone(),
        wake: wake_tx,
        taken: 0,
        published,
    };
    let outbox = out.clone();
    let task = rt.spawn(async move {
        if let Err(panic) = AssertUnwindSafe(driver.run()).catch_unwind().await {
            lock(&outbox).panic = Some(panic);
        }
        drop(last_wake);
    });
    let handle = SessionHandle {
        origin,
        inputs: inputs_tx,
        dispatched: 0,
        driver: AbortOnDropHandle::new(task),
    };
    (handle, Updates { out, wake })
}

struct Driver {
    session: Session,
    events: mpsc::Receiver<SessionEvent>,
    inputs: mpsc::UnboundedReceiver<Input>,
    clock: Arc<dyn Clock>,
    out: Arc<Mutex<Outbox>>,
    wake: mpsc::Sender<()>,
    taken: u64,
    /// The state the last snapshot carried.
    published: Arc<LobbyState>,
}

enum Work {
    Input(Option<Input>),
    Event(SessionEvent),
    Due,
}

impl Driver {
    async fn run(mut self) {
        loop {
            let mut fx = UiEffects::default();
            let due = self
                .session
                .next_deadline()
                .map(|at| self.clock.sleep_until(at));
            let first = tokio::select! {
                biased;
                input = self.inputs.recv() => Work::Input(input),
                () = async { match due { Some(due) => due.await, None => pending().await } } => {
                    Work::Due
                }
                Some(ev) = self.events.recv() => Work::Event(ev),
            };
            let stop = match first {
                Work::Input(None) => return,
                Work::Input(Some(input)) => self.input(input, &mut fx),
                Work::Event(ev) => {
                    fx.extend(self.session.handle(ev, self.clock.now()));
                    None
                }
                Work::Due => None,
            };
            if let Some(stop) = stop.or_else(|| self.queued(&mut fx)) {
                self.publish(&mut fx);
                return self.finish(stop).await;
            }
            fx.extend(self.session.tick(self.clock.now()));
            self.publish(&mut fx);
        }
    }

    /// What is already waiting, a bounded share of each. A Stop ends the round at its turn.
    fn queued(&mut self, fx: &mut UiEffects) -> Option<Stop> {
        for _ in 0..ROUND {
            match self.inputs.try_recv() {
                Ok(input) => {
                    if let Some(stop) = self.input(input, fx) {
                        return Some(stop);
                    }
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        for _ in 0..ROUND {
            let Ok(ev) = self.events.try_recv() else {
                break;
            };
            fx.extend(self.session.handle(ev, self.clock.now()));
        }
        None
    }

    fn input(&mut self, input: Input, fx: &mut UiEffects) -> Option<Stop> {
        match input {
            Input::Intent(intent) => {
                fx.extend(self.take(intent));
                None
            }
            #[cfg(feature = "test-support")]
            Input::Act(intent, reply) => {
                let own = self.take(intent);
                let _ = reply.send(own.clone());
                fx.extend(own);
                None
            }
            Input::Stop(stop) => Some(stop),
            #[cfg(feature = "test-support")]
            Input::Inspect(read) => {
                self.publish(fx);
                read(&self.session);
                None
            }
        }
    }

    /// The Session takes `intent`; what it raised at once.
    fn take(&mut self, intent: Intent) -> UiEffects {
        self.session.dispatch(intent, self.clock.now());
        self.taken += 1;
        self.session.take_effects()
    }

    /// Never waits: the window takes the newest snapshot when it gets to it. The state is
    /// compared and cloned here, off the window's thread, and kept when it did not change.
    fn publish(&mut self, fx: &mut UiEffects) {
        if *self.published != self.session.state {
            self.published = Arc::new(self.session.state.clone());
            self.session.hold_history();
        }
        let snapshot = Arc::new(self.session.snapshot(self.published.clone(), self.taken));
        // A Watch that ended in this round took its feed with it, so a feed goes out only
        // with a snapshot that still has its Watch.
        let feed = self.session.take_watch_feed();
        {
            let mut out = lock(&self.out);
            out.snapshot = snapshot;
            out.news.extend(std::mem::take(fx));
            if feed.is_some() {
                out.watch_feed = feed;
            }
        }
        let _ = self.wake.try_send(());
    }

    /// The events that still come are taken and dropped until the teardown is done: a media
    /// thread waiting for room in the queue must get to its stop. The queue stays open, so the
    /// connection still closes with 1000.
    async fn finish(self, stop: Stop) {
        let Driver {
            session,
            mut events,
            ..
        } = self;
        let mut task = match stop {
            Stop::Shutdown => session.shutdown(),
            Stop::SignOut => session.sign_out(),
            Stop::LeaveLobby => session.leave_lobby(),
        };
        loop {
            tokio::select! {
                biased;
                _ = &mut task => return,
                Some(_) = events.recv() => {}
            }
        }
    }
}
