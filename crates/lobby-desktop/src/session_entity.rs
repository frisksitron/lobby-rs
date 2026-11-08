//! `SessionEntity`: the window's view of the running Session. It holds the Session's handle and
//! the newest snapshot its driver published, and turns each update into one `cx.notify()` and its
//! news into a `SessionUiEvent`. It owns no rule, timer or IO: views act through
//! [`SessionEntity::dispatch`] and read the snapshot. Dropping it drops the handle, which ends the
//! Session with its call and its connection.

use std::{sync::Arc, time::Instant};

use futures_lite::future::yield_now;
use gpui_kit::{Context, EventEmitter, Task};
use lobby_client::{
    ClientTask, Intent, LobbyState, SessionHandle, SessionUpdate, Snapshot, UiEffects, Updates,
    VideoFeed,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionUiEvent(pub UiEffects);

pub struct SessionEntity {
    session: Option<SessionHandle>,
    snapshot: Arc<Snapshot>,
    dispatched: u64,
    watch_feed: Option<VideoFeed>,
    _updates: Task<()>,
    /// The Session left to sign out (the view says "Signing out…" while it closes).
    pub signing_out: bool,
}

impl EventEmitter<SessionUiEvent> for SessionEntity {}

impl SessionEntity {
    pub fn new(session: SessionHandle, mut updates: Updates, cx: &mut Context<Self>) -> Self {
        // What the Session starts from, for the first frame. Its news wait for the subscribers.
        let snapshot = updates.newest();
        // Awaiting the wake needs no reactor, so this runs on the GPUI executor. One update a
        // turn: under a flood the next wake is ready at once, and the window's input and frames
        // must get theirs in between.
        let task = cx.spawn(async move |this, cx| {
            while updates.changed().await {
                let update = updates.take();
                if this
                    .update(cx, |entity, cx| entity.install(update, cx))
                    .is_err()
                {
                    break;
                }
                yield_now().await;
            }
        });
        Self {
            session: Some(session),
            snapshot,
            dispatched: 0,
            watch_feed: None,
            _updates: task,
            signing_out: false,
        }
    }

    fn install(&mut self, update: SessionUpdate, cx: &mut Context<Self>) {
        if self.session.is_none() {
            return;
        }
        // A round that changed nothing a view reads (a Typing, a pen move the stroke already had)
        // costs no frame.
        let same = self.snapshot.shows_the_same(&update.snapshot)
            && update.news.is_empty()
            && update.watch_feed.is_none();
        self.snapshot = update.snapshot;
        if update.watch_feed.is_some() {
            self.watch_feed = update.watch_feed;
        }
        if !update.news.is_empty() {
            cx.emit(SessionUiEvent(update.news));
        }
        if !same {
            cx.notify();
        }
    }

    pub fn origin(&self) -> Option<&lobby_client::LobbyOrigin> {
        self.session.as_ref().map(SessionHandle::origin)
    }

    pub fn state(&self) -> Option<&LobbyState> {
        self.session.as_ref().map(|_| &*self.snapshot.state)
    }

    /// The state with its `Arc`, which stays the same while the state does.
    pub fn shared_state(&self) -> Option<&Arc<LobbyState>> {
        self.session.as_ref().map(|_| &self.snapshot.state)
    }

    /// What the next Session of this Lobby may start from: the state shown, unless this one ended
    /// signed out or before it reached the Lobby.
    pub fn kept_state(&self) -> Option<Arc<LobbyState>> {
        let state = &self.snapshot.state;
        let signed_out = self.signing_out || state.phase == lobby_client::Phase::NeedsAuth;
        (self.session.is_some() && !signed_out && state.me.is_some()).then(|| state.clone())
    }

    /// The one way a view acts on the Lobby. A refusal comes back as an Issue among the news of
    /// an update; what changed is in its snapshot. Returns the Intent's number for
    /// [`SessionEntity::taken`].
    pub fn dispatch(&mut self, intent: Intent, _: &mut Context<Self>) -> u64 {
        let Some(session) = self.session.as_mut() else {
            return 0;
        };
        self.dispatched = session.dispatch(intent);
        self.dispatched
    }

    /// Whether the snapshot shown has taken the Intent `dispatch` numbered `n`.
    pub fn taken(&self, n: u64) -> bool {
        self.snapshot.intents >= n
    }

    /// Whether the snapshot shown has taken every Intent dispatched so far.
    pub fn caught_up(&self) -> bool {
        self.session.is_none() || self.taken(self.dispatched)
    }

    /// The pictures of the newest Watch the Session took, once.
    pub fn take_watch_feed(&mut self) -> Option<VideoFeed> {
        self.watch_feed.take()
    }

    /// What this Member's running share sends, drawn without the network.
    pub fn own_share_feed(&self) -> Option<VideoFeed> {
        self.snapshot.own_share_feed()
    }

    pub fn pen_is_down(&self) -> bool {
        self.snapshot.pen_down
    }

    pub fn typing_line(&self, now: Instant) -> Option<String> {
        self.snapshot.typing_line(now)
    }

    pub fn dnd(&self) -> bool {
        self.snapshot.dnd
    }

    pub fn idle(&self) -> bool {
        self.snapshot.idle
    }

    pub fn sign_out(&mut self, cx: &mut Context<Self>) -> ClientTask<()> {
        self.signing_out = true;
        cx.notify();
        match self.session.take() {
            Some(session) => session.sign_out(),
            None => ClientTask::ready(()),
        }
    }

    /// The Lobby let this Member leave: the Session is shut down and the Lobby's tokens forgotten.
    pub fn leave_lobby(&mut self, cx: &mut Context<Self>) -> ClientTask<()> {
        self.signing_out = true;
        cx.notify();
        match self.session.take() {
            Some(session) => session.leave_lobby(),
            None => ClientTask::ready(()),
        }
    }

    pub fn shutdown(&mut self, cx: &mut Context<Self>) -> ClientTask<()> {
        cx.notify();
        match self.session.take() {
            Some(session) => session.shutdown(),
            None => ClientTask::ready(()),
        }
    }
}
