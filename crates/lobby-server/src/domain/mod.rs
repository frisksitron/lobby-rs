//! Every durable write. One writer task owns the SQLite writer and runs the accepted writes one at
//! a time, in the order they were accepted. Each checks and commits in one `BEGIN IMMEDIATE`
//! transaction, then tells the live Lobby before the next write starts, so commit order is
//! broadcast order and a READY sees a write whole or not at all.
//!
//! A write is accepted when it joins the queue. From then on it finishes even if the request that
//! asked for it goes away; before that it has changed nothing. Shutdown stops admitting writes
//! ([`Domain::stop_admitting`]); the writer task drains the queue, closes the connection, lets go
//! of the database's lock and ends.
//!
//! No write is timed out in the queue: one that waited long has still been accepted, and a caller
//! told it failed would see it land later. SQLite's busy timeout bounds each write, and HTTP keeps
//! its request timeout. What bounds the queue is admission: a write asked for without a session is
//! refused while [`FRONT_DOOR_WRITES`] are in flight.

mod account;
mod admission;
mod chat;
mod cleanup;
mod error;
mod files;
mod members;
mod sessions;

use std::{panic::AssertUnwindSafe, sync::Arc};

use futures_util::{FutureExt as _, future::BoxFuture};
use sqlx::{Connection as _, SqliteConnection};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::task::TaskTracker;
use tracing::{debug, error};

pub use self::admission::MAX_PENDING_PER_SITE;
pub(crate) use self::{
    admission::{CodeRequest, Join, Verify},
    cleanup::Batch,
    error::WriteError,
    files::Staged,
    sessions::{Authenticated, Refreshed},
};
use crate::{
    blobs::BlobStore, config::Config, db::DbLock, limits::CodeSends, lobby::Lobby, mail::Mailer,
};

/// What a write works with: the one writer connection, and what it tells after the commit.
pub(crate) struct Writer {
    pub conn: SqliteConnection,
    /// Let go only after `conn` closed, so no second server writes while this one still can.
    pub lock: DbLock,
    pub lobby: Lobby,
    pub blobs: BlobStore,
    pub cfg: Arc<Config>,
    pub code_sends: Arc<CodeSends>,
    pub mailer: Mailer,
}

/// Accepted writes in flight, anyone's, past which a write asked for without a session (a Magic
/// Code, a join, a Vouch check, a refresh) is refused with [`WriteError::Busy`]. A flood at
/// the front door fills only this much of the queue, so a Member's write waits behind at most this
/// many front door writes and is never refused for them.
pub const FRONT_DOOR_WRITES: usize = 64;

type Job = Box<dyn for<'w> FnOnce(&'w mut Writer) -> BoxFuture<'w, ()> + Send>;

struct Queued {
    job: Job,
    counted: bool,
}

/// Where a write comes from: a session or the server's own jobs, or the front door.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Door {
    Inside,
    Front,
}

#[derive(Clone)]
pub(crate) struct Domain {
    queue: watch::Sender<Option<mpsc::UnboundedSender<Queued>>>,
    tally: watch::Sender<usize>,
}

impl Domain {
    pub fn start(writer: Writer, tier: &TaskTracker) -> Domain {
        let (tx, rx) = mpsc::unbounded_channel();
        let tally = watch::Sender::new(0);
        tier.spawn(run(writer, rx, tally.clone()));
        Domain {
            queue: watch::Sender::new(Some(tx)),
            tally,
        }
    }

    /// Every later write is refused with [`WriteError::Unavailable`]. The accepted ones still run.
    pub fn stop_admitting(&self) {
        self.queue.send_replace(None);
    }

    /// Queues `job`. A write that `door` names is counted in flight; one at the front door is
    /// refused while [`FRONT_DOOR_WRITES`] are.
    fn admit(&self, job: Job, door: Option<Door>) -> Result<(), WriteError> {
        let queue = self.queue.borrow();
        let tx = queue.as_ref().ok_or(WriteError::Unavailable)?;
        // Counted before the send: the writer task may finish the write before this returns.
        if let Some(door) = door {
            let room = self.tally.send_if_modified(|t| {
                let room = door == Door::Inside || *t < FRONT_DOOR_WRITES;
                *t += usize::from(room);
                room
            });
            if !room {
                debug!("a front door write refused: the writer is busy");
                return Err(WriteError::Busy);
            }
        }
        let counted = door.is_some();
        tx.send(Queued { job, counted }).map_err(|_| {
            if counted {
                self.tally.send_modify(|t| *t -= 1);
            }
            WriteError::Unavailable
        })
    }

    /// Accepts `write` and waits for its outcome. Dropping the returned future does not cancel it.
    async fn run<T, F>(&self, write: F) -> Result<T, WriteError>
    where
        T: Send + 'static,
        F: for<'w> FnOnce(&'w mut Writer) -> BoxFuture<'w, Result<T, WriteError>> + Send + 'static,
    {
        self.run_from(Door::Inside, write).await
    }

    /// [`Domain::run`] for a write anyone can ask for without a session.
    async fn run_at_front_door<T, F>(&self, write: F) -> Result<T, WriteError>
    where
        T: Send + 'static,
        F: for<'w> FnOnce(&'w mut Writer) -> BoxFuture<'w, Result<T, WriteError>> + Send + 'static,
    {
        self.run_from(Door::Front, write).await
    }

    async fn run_from<T, F>(&self, door: Door, write: F) -> Result<T, WriteError>
    where
        T: Send + 'static,
        F: for<'w> FnOnce(&'w mut Writer) -> BoxFuture<'w, Result<T, WriteError>> + Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        let job: Job = Box::new(move |w| {
            Box::pin(async move {
                let _ = tx.send(write(w).await);
            })
        });
        self.admit(job, Some(door))?;
        // Dropped unanswered only if the write panicked.
        rx.await.unwrap_or(Err(WriteError::Panicked))
    }
}

async fn run(
    mut writer: Writer,
    mut queue: mpsc::UnboundedReceiver<Queued>,
    tally: watch::Sender<usize>,
) {
    while let Some(Queued { job, counted }) = queue.recv().await {
        // A panic is a bug in that write: it answers nothing, and the next write still runs.
        if AssertUnwindSafe(job(&mut writer))
            .catch_unwind()
            .await
            .is_err()
        {
            error!("a write panicked");
        }
        if counted {
            tally.send_modify(|t| *t -= 1);
        }
    }
    let Writer { conn, lock, .. } = writer;
    if let Err(e) = conn.close().await {
        error!(error = %e, "the writer did not close cleanly");
    }
    drop(lock);
    debug!("writes drained");
}

/// Accepted writes wait behind this until it drops: a test orders a race instead of timing it.
#[cfg(feature = "test-support")]
pub struct HeldWrites {
    _release: oneshot::Sender<()>,
}

#[cfg(feature = "test-support")]
impl Domain {
    /// Returns once the writer task is held.
    pub async fn hold(&self) -> HeldWrites {
        let (holding, held) = oneshot::channel();
        let (release, released) = oneshot::channel::<()>();
        let job: Job = Box::new(move |_| {
            Box::pin(async move {
                let _ = holding.send(());
                let _ = released.await;
            })
        });
        self.admit(job, None).expect("the server admits writes");
        held.await.expect("the writer task runs");
        HeldWrites { _release: release }
    }

    pub async fn in_flight_at_least(&self, n: usize) {
        let mut rx = self.tally.subscribe();
        let _ = rx.wait_for(|t| *t >= n).await;
    }

    pub async fn settled(&self) {
        let mut rx = self.tally.subscribe();
        let _ = rx.wait_for(|t| *t == 0).await;
    }
}
