use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use lobby_client::{
    Client, EntryHandle, EntryIntent, EntryStep, EntryUpdates, EntryView, HttpPolicy, Intent,
    Issue, IssueKind, LobbyOrigin, LobbyState, MediaConfig, OwnerAction, Phase, ScreenSource,
    SessionHandle, SessionUpdate, Snapshot, UiEffect, Updates, VideoFeed,
    testing::{Session, act, inspect},
};
use lobby_protocol::{Message, Presence, Quality, UserId, VoiceEnded};
use lobby_server::testing::{SmtpSink, TestServer, email_of};

pub const WAIT: Duration = Duration::from_secs(15);

pub fn origin(server: &TestServer) -> LobbyOrigin {
    LobbyOrigin::parse(&server.origin(), HttpPolicy::LoopbackOnly).unwrap()
}

pub fn active(state: &LobbyState, name: &str) -> bool {
    state
        .members
        .values()
        .find(|m| m.username.as_str() == name)
        .and_then(|m| m.voice)
        .is_some_and(|v| !v.joining)
}

pub fn presence(state: &LobbyState, name: &str) -> Option<Presence> {
    state
        .members
        .values()
        .find(|m| m.username.as_str() == name)
        .map(|m| m.presence)
}

pub fn streaming(state: &LobbyState, name: &str) -> bool {
    state
        .members
        .values()
        .find(|m| m.username.as_str() == name)
        .is_some_and(|m| state.share_of(&m.id).is_some())
}

const MEMBER_WAIT: Duration = Duration::from_secs(5);

/// A person's way into a Lobby, run by Entry's driver as the desktop runs it: Intents in, the
/// newest view out.
pub struct Entering {
    handle: EntryHandle,
    updates: EntryUpdates,
}

impl Entering {
    pub fn new((handle, updates): (EntryHandle, EntryUpdates)) -> Entering {
        Entering { handle, updates }
    }

    /// At `origin`'s first step, reached from its address.
    pub async fn at(client: &Client, origin: &LobbyOrigin) -> Entering {
        let mut entry = Entering::new(client.entry(None));
        entry.act(EntryIntent::Connect(origin.to_string()));
        entry
            .wait("the Lobby answered", |v| v.step != EntryStep::Address)
            .await;
        entry
    }

    /// Returns the Intent's number, for [`Self::answered`].
    pub fn act(&mut self, intent: EntryIntent) -> u64 {
        self.handle.dispatch(intent)
    }

    /// Waits until Entry took Intent `n` and nothing it asked for is in flight.
    pub async fn answered(&mut self, n: u64) -> Arc<EntryView> {
        self.wait("the answer", |v| v.taken >= n && !v.busy).await
    }

    pub fn view(&mut self) -> Arc<EntryView> {
        self.updates.view()
    }

    /// Waits for a view that `cond` holds for and returns it.
    pub async fn wait(&mut self, what: &str, cond: impl Fn(&EntryView) -> bool) -> Arc<EntryView> {
        let waited = tokio::time::timeout(MEMBER_WAIT, async {
            loop {
                let view = self.updates.view();
                if cond(&view) {
                    return view;
                }
                assert!(
                    self.updates.changed().await,
                    "Entry ended waiting for {what}"
                );
            }
        })
        .await;
        waited.unwrap_or_else(|_| panic!("{what}: not within {MEMBER_WAIT:?}: {:?}", self.view()))
    }

    /// Asks for a code to `email` from the email step and waits for the code step.
    pub async fn send_code(&mut self, email: &str) {
        let n = self.act(EntryIntent::SendCode(email.to_owned()));
        let view = self.answered(n).await;
        assert!(
            matches!(view.step, EntryStep::Code { .. }),
            "a code was asked for: {view:?}"
        );
    }

    /// Types the `n`th code the Lobby mailed to `email` and waits for its answer.
    pub async fn enter_code(
        &mut self,
        server: &TestServer,
        email: &str,
        n: usize,
    ) -> Arc<EntryView> {
        let code = server.mail.code_number(email, n).await;
        let n = self.act(EntryIntent::SubmitCode(code));
        self.answered(n).await
    }
}

pub struct Member {
    handle: SessionHandle,
    updates: Updates,
    snapshot: Arc<Snapshot>,
    effects: Vec<UiEffect>,
    watch_feed: Option<VideoFeed>,
}

impl Member {
    pub fn start(client: &Client, origin: &LobbyOrigin) -> Member {
        let (handle, updates) = client.connect(origin);
        Member {
            handle,
            updates,
            snapshot: Arc::default(),
            effects: Vec::new(),
            watch_feed: None,
        }
    }

    /// Signs `name` in through Entry, as a person does: a code to `<name>@lobby.test`, read from
    /// `mail`, then typed in. Starts the Session; `Err` is where Entry stopped instead.
    pub async fn sign_in(
        client: &Client,
        origin: &LobbyOrigin,
        name: &str,
        mail: &SmtpSink,
    ) -> Result<Member, Arc<EntryView>> {
        let mut entry = Entering::at(client, origin).await;
        if entry.view().step != EntryStep::SignedIn {
            let email = email_of(name);
            let n = mail.count_to(&email) + 1;
            entry.send_code(&email).await;
            let code = mail.code_number(&email, n).await;
            let k = entry.act(EntryIntent::SubmitCode(code));
            let view = entry.answered(k).await;
            if view.step != EntryStep::SignedIn {
                return Err(view);
            }
        }
        let mut me = Self::start(client, origin);
        me.wait_connected().await;
        Ok(me)
    }

    fn handle(&mut self) -> &mut SessionHandle {
        &mut self.handle
    }

    pub fn state(&self) -> &LobbyState {
        &self.snapshot.state
    }

    fn install(&mut self, update: SessionUpdate) {
        self.snapshot = update.snapshot;
        self.effects.extend(update.news.0);
        if update.watch_feed.is_some() {
            self.watch_feed = update.watch_feed;
        }
    }

    /// What the driver published since the last take, without waiting.
    fn take(&mut self) {
        let update = self.updates.take();
        self.install(update);
    }

    /// Waits for the driver's next round; `false` once the Session has ended.
    pub async fn step(&mut self) -> bool {
        if !self.updates.changed().await {
            return false;
        }
        self.take();
        true
    }

    pub fn take_effects(&mut self) -> Vec<UiEffect> {
        self.take();
        std::mem::take(&mut self.effects)
    }

    /// The words of the latest Issue of `kind` raised so far, whoever's call raised it.
    pub fn issue(&mut self, kind: IssueKind) -> Option<String> {
        self.take();
        self.effects.iter().rev().find_map(|e| match e {
            UiEffect::Issue(issue) if issue.kind == kind => Some(issue.message.to_string()),
            _ => None,
        })
    }

    /// Waits for an Issue of `kind` and returns its words.
    pub async fn wait_issue(&mut self, kind: IssueKind) -> String {
        let waited = tokio::time::timeout(MEMBER_WAIT, async {
            loop {
                if let Some(said) = self.issue(kind) {
                    return said;
                }
                if !self.step().await {
                    panic!("session ended while waiting for {kind:?}");
                }
            }
        })
        .await;
        waited.unwrap_or_else(|_| panic!("timed out waiting for {kind:?}"))
    }

    pub async fn wait_until(&mut self, what: &str, cond: impl Fn(&LobbyState) -> bool) {
        self.wait_snapshot(MEMBER_WAIT, what, |s| cond(&s.state))
            .await;
    }

    async fn wait_snapshot(
        &mut self,
        within: Duration,
        what: &str,
        cond: impl Fn(&Snapshot) -> bool,
    ) {
        let waited = tokio::time::timeout(within, async {
            while !cond(&self.snapshot) {
                if !self.step().await {
                    panic!("session ended while waiting for {what}");
                }
            }
        })
        .await;
        if waited.is_err() {
            panic!("timed out waiting for {what}; state: {:?}", self.state());
        }
    }

    pub async fn wait_connected(&mut self) {
        self.wait_until("Connected with history", |s| {
            s.phase == Phase::Connected && s.history == lobby_client::History::Loaded
        })
        .await;
    }

    pub async fn wait_phase(&mut self, phase: Phase) {
        let label = format!("{phase:?}");
        self.wait_until(&label, |s| s.phase == phase).await;
    }

    pub async fn wait_phase_within(
        &mut self,
        within: Duration,
        what: &str,
        cond: impl Fn(&Phase) -> bool,
    ) {
        self.wait_snapshot(within, what, |s| cond(&s.state.phase))
            .await;
    }

    /// Dispatches `intent` and waits until the snapshot has taken it. The Issue the Session
    /// raised as it took it, if any, is its refusal.
    pub async fn act(&mut self, intent: Intent) -> Result<(), Issue> {
        let (n, own) = act(self.handle(), intent).await;
        self.wait_snapshot(MEMBER_WAIT, "the Intent taken", |s| s.intents >= n)
            .await;
        let refused = own.0.into_iter().find_map(|e| match e {
            UiEffect::Issue(issue) => Some(issue),
            _ => None,
        });
        refused.map_or(Ok(()), Err)
    }

    /// The nonces of this Member's Messages, on their way and confirmed.
    fn own_nonces(&self) -> std::collections::HashSet<String> {
        let s = self.state();
        let mine = s
            .chat
            .values()
            .filter(|m| s.me.as_ref() == Some(&m.author.id));
        s.pending
            .iter()
            .map(|p| p.nonce.clone())
            .chain(mine.map(|m| m.nonce.clone()))
            .collect()
    }

    fn send_failure(&self) -> Option<String> {
        self.effects.iter().rev().find_map(|e| match e {
            UiEffect::SendFailed { issue, .. } => Some(issue.message.to_string()),
            _ => None,
        })
    }

    pub async fn send(&mut self, text: &str) -> Message {
        let before = self.own_nonces();
        self.act(Intent::Send(text.to_owned()))
            .await
            .expect("send accepted");
        // The server may have confirmed it before this looks: then the Message has the nonce.
        let nonce = self
            .own_nonces()
            .into_iter()
            .find(|n| !before.contains(n))
            .unwrap_or_else(|| panic!("send failed: {:?}", self.send_failure()));
        self.wait_until("the send's terminal outcome", |s| {
            s.chat.values().any(|m| m.nonce == nonce) || !s.pending.iter().any(|p| p.nonce == nonce)
        })
        .await;
        if !self.state().chat.values().any(|m| m.nonce == nonce) {
            panic!("send failed: {:?}", self.send_failure());
        }
        self.state()
            .chat
            .values()
            .find(|m| m.nonce == nonce)
            .cloned()
            .expect("confirmed")
    }

    /// The owner's action on `user`, until the Lobby answers; a refusal is its Issue's words.
    pub async fn owner_action(&mut self, user: &UserId, action: OwnerAction) -> Result<(), String> {
        self.take();
        self.effects.clear();
        let _ = self
            .act(Intent::Owner {
                user: user.clone(),
                action,
            })
            .await;
        self.wait_until("the owner action's answer", |s| {
            !s.owner_acting.contains_key(user)
        })
        .await;
        let answer = self.effects.iter().find_map(|e| match e {
            UiEffect::OwnerActed { user: done } if done == user => Some(Ok(())),
            UiEffect::Issue(issue) => Some(Err(issue.message.to_string())),
            _ => None,
        });
        answer.expect("an answer or an Issue")
    }

    pub async fn join_voice(&mut self, media: MediaConfig) -> Result<(), Issue> {
        self.act(Intent::JoinVoice {
            media: Box::new(media),
            volumes: Default::default(),
        })
        .await
    }

    pub async fn leave_voice(&mut self) {
        let _ = self.act(Intent::LeaveVoice).await;
    }

    pub async fn toggle_mute(&mut self) -> Result<(), Issue> {
        self.act(Intent::ToggleMute).await
    }

    pub async fn start_share(
        &mut self,
        source: ScreenSource,
        quality: Quality,
    ) -> Result<(), Issue> {
        self.act(Intent::StartShare { source, quality }).await
    }

    pub async fn stop_share(&mut self) {
        let _ = self.act(Intent::StopShare).await;
    }

    pub fn own_share_feed(&mut self) -> Option<VideoFeed> {
        self.snapshot.own_share_feed()
    }

    pub fn id_of(&self, name: &str) -> UserId {
        self.state()
            .members
            .values()
            .find(|m| m.username.as_str() == name)
            .map(|m| m.id.clone())
            .expect("a Member of that name")
    }

    pub async fn watch(&mut self, name: &str) -> Result<VideoFeed, Issue> {
        let sharer = self.id_of(name);
        self.watch_feed = None;
        self.act(Intent::Watch(sharer)).await?;
        Ok(self
            .watch_feed
            .take()
            .expect("a Watch not refused has its feed"))
    }

    pub async fn ask_watch(&mut self, name: &str) -> Result<(), Issue> {
        let sharer = self.id_of(name);
        self.act(Intent::Watch(sharer)).await
    }

    pub fn watching(&self) -> Option<(String, bool)> {
        let w = self.state().share.watch.as_ref()?;
        let name = self.state().members.get(&w.sharer)?.username.to_string();
        Some((name, w.live))
    }

    pub async fn unwatch(&mut self) {
        let _ = self.act(Intent::Unwatch).await;
    }

    pub async fn inspect<T: Send + 'static>(
        &self,
        read: impl FnOnce(&Session) -> T + Send + 'static,
    ) -> T {
        inspect(&self.handle, read).await
    }

    pub async fn voice_ended(&self) -> Vec<VoiceEnded> {
        self.inspect(|s| s.voice_ended_seen().to_vec()).await
    }

    pub async fn voice_media_addr(&self) -> Option<std::net::SocketAddr> {
        self.inspect(Session::voice_media_addr).await
    }

    #[allow(dead_code, reason = "used by share_bench")]
    pub async fn share_video_sent(&self) -> Option<(u64, u64)> {
        self.inspect(Session::share_video_sent).await
    }

    #[allow(dead_code, reason = "used by share_bench")]
    pub async fn udp_bytes_sent(&self) -> Option<u64> {
        self.inspect(Session::udp_bytes_sent).await
    }

    pub async fn watched_profile_level(&self) -> Option<u32> {
        self.inspect(Session::watched_profile_level).await
    }

    #[allow(dead_code, reason = "used by share_bench")]
    pub async fn watch_loads(&self) -> Vec<(Instant, lobby_client::WatchLoad)> {
        self.inspect(|s| s.watch_loads().to_vec()).await
    }

    pub async fn run_for(&mut self, d: Duration) {
        let _ = tokio::time::timeout(d, async { while self.step().await {} }).await;
    }

    pub async fn send_raw(&mut self, text: &str) -> Result<(), Issue> {
        self.act(Intent::Send(text.to_owned())).await
    }

    pub async fn attach(&mut self, source: lobby_client::DraftSource) -> Result<(), Issue> {
        self.act(Intent::Attach(source)).await
    }

    pub async fn wait_uploaded(&mut self) {
        self.wait_until("every draft Ready", |s| {
            s.drafts.items().iter().all(|d| d.state.is_ready())
        })
        .await;
    }

    pub async fn retry_now(&mut self) {
        let _ = self.act(Intent::RetryNow).await;
    }

    pub async fn set_dnd(&mut self, on: bool) {
        let _ = self.act(Intent::SetDnd(on)).await;
    }

    pub async fn set_idle(&mut self, idle: bool) {
        let _ = self.act(Intent::SetIdle(idle)).await;
    }

    pub async fn shutdown(self) {
        self.handle.shutdown().await.expect("session shutdown");
    }

    pub async fn sign_out(self) {
        self.handle.sign_out().await.expect("sign out");
    }

    fn request_of(&self, username: &str) -> Option<lobby_protocol::AccessRequestId> {
        self.state()
            .access_requests
            .iter()
            .find(|r| r.username.as_str() == username)
            .map(|r| r.id.clone())
    }

    pub async fn vouch(&mut self, username: &str) {
        let id = self.request_of(username).expect("a pending request");
        let _ = self.act(Intent::Vouch(id)).await;
    }

    pub async fn decline(&mut self, username: &str) {
        let id = self.request_of(username).expect("a pending request");
        let _ = self.act(Intent::Decline(id)).await;
    }
}

pub async fn until(
    a: &mut Member,
    b: &mut Member,
    what: &str,
    cond: impl Fn(&LobbyState, &LobbyState) -> bool,
) {
    let waited = tokio::time::timeout(WAIT, async {
        while !cond(a.state(), b.state()) {
            tokio::select! {
                more = a.step() => assert!(more, "a's session ended"),
                more = b.step() => assert!(more, "b's session ended"),
            }
        }
    })
    .await;
    assert!(waited.is_ok(), "timed out waiting for {what}");
}
