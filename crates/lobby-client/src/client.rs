//! The `Client` facade: owns a runtime handle, one `reqwest::Client` and the keychain, and every
//! method spawns on that handle.

use std::{sync::Arc, time::Duration};

use lobby_protocol::LobbyInfo;
use tokio::{runtime::Handle, sync::mpsc};
use tracing::{Instrument as _, info_span};

use crate::{
    ClientTask, Clock, Connection, EntryHandle, EntryUpdates, HttpPolicy, LobbyInfoError,
    LobbyOrigin, LobbyState, Paths, ReconnectPolicy, SessionHandle, TokenStore, TokenStoreError,
    Updates,
    api::{self, ApiClient},
    connection::EVENT_QUEUE,
    entry::{self, Entry},
    media_cache::MediaCache,
    session::{RuntimeClock, Session, drive},
    tokens::Keychain,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// An idle HTTP/2 connection is pinged this often, so one on a path that died silently (sleep, a
/// network change) is dropped within the interval and the timeout, before the 25 s stale deadline
/// sends the WebSocket back to REST. That keeps the pool warm for minutes: a request after a quiet
/// spell skips the TCP and TLS round trips.
const H2_PING_INTERVAL: Duration = Duration::from_secs(10);
const H2_PING_TIMEOUT: Duration = Duration::from_secs(5);
/// HTTP/1.1 has no pings, but it is only plain `http`, which is loopback unless allowed for
/// development: every Lobby on the internet is HTTPS behind Caddy, which speaks HTTP/2.
const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, thiserror::Error)]
#[error("could not initialize the HTTP client: {0}")]
pub struct BuildError(#[source] reqwest::Error);

/// A Lobby's tokens, read from the store once, for [`Client::resume`].
pub struct StoredSession {
    api: Arc<ApiClient>,
}

#[derive(Clone)]
pub struct Client {
    rt: Handle,
    http: reqwest::Client,
    keychain: Keychain,
    policy: HttpPolicy,
    reconnect: ReconnectPolicy,
    clock: Arc<dyn Clock>,
    watch_network: bool,
}

impl Client {
    pub fn new(
        rt: Handle,
        store: Arc<dyn TokenStore>,
        policy: HttpPolicy,
    ) -> Result<Client, BuildError> {
        let http = reqwest::Client::builder()
            .use_rustls_tls()
            .connect_timeout(CONNECT_TIMEOUT)
            .pool_idle_timeout(POOL_IDLE_TIMEOUT)
            .http2_keep_alive_interval(H2_PING_INTERVAL)
            .http2_keep_alive_timeout(H2_PING_TIMEOUT)
            .http2_keep_alive_while_idle(true)
            .user_agent(concat!("lobby/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(BuildError)?;
        Ok(Client {
            rt,
            http,
            keychain: Keychain::new(store),
            policy,
            reconnect: ReconnectPolicy::default(),
            clock: Arc::new(RuntimeClock),
            watch_network: false,
        })
    }

    /// Each Session's connection probes at once when the OS reports a network change (Windows and
    /// Linux). Off by default so tests leave the machine's network alone.
    pub fn with_network_watch(mut self) -> Client {
        self.watch_network = true;
        self
    }

    pub fn with_reconnect_policy(mut self, policy: ReconnectPolicy) -> Client {
        self.reconnect = policy;
        self
    }

    /// The clock the Sessions and Entries this client starts live by, instead of the runtime's.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Client {
        self.clock = clock;
        self
    }

    /// How long a keychain call may take before it counts as unavailable.
    pub fn with_store_timeout(mut self, timeout: Duration) -> Client {
        self.keychain = self.keychain.with_timeout(timeout);
        self
    }

    /// Checks `feed` for an app newer than `current` and downloads it into `dir`, the running
    /// app's folder, until the [`Updater`](crate::update::Updater) is dropped.
    pub fn updater(
        &self,
        feed: crate::update::ReleaseFeed,
        current: crate::update::Version,
        dir: std::path::PathBuf,
        timing: crate::update::UpdateTiming,
    ) -> crate::update::Updater {
        crate::update::Updater::start(&self.rt, self.http.clone(), feed, current, dir, timing)
    }

    /// Where plain `http` is accepted. Origins read back from `state.json` are only structurally
    /// checked, so whoever connects to one checks it against this first.
    pub fn policy(&self) -> HttpPolicy {
        self.policy
    }

    pub fn lobby_info(
        &self,
        origin: &LobbyOrigin,
    ) -> ClientTask<Result<LobbyInfo, LobbyInfoError>> {
        let http = self.http.clone();
        let origin = origin.clone();
        let span = info_span!("lobby_info", lobby = %origin);
        ClientTask::spawn(
            &self.rt,
            async move { api::checked_lobby_info(&http, &origin).await }.instrument(span),
        )
    }

    /// Looks for what Screen Share needs off the caller's thread: [`crate::share_blocked`] says
    /// what it found from when it ends.
    pub fn probe_share(&self) -> ClientTask<Option<crate::MediaError>> {
        ClientTask::spawn(&self.rt, async {
            tokio::task::spawn_blocking(crate::media::probe_share)
                .await
                .ok()
                .flatten()
        })
    }

    pub fn has_session(&self, origin: &LobbyOrigin) -> ClientTask<Result<bool, TokenStoreError>> {
        let loaded = self.keychain.load(origin);
        ClientTask::spawn(&self.rt, async move { loaded.await.map(|t| t.is_some()) })
    }

    /// The Lobby's tokens from the store, `None` when it has none: [`Client::resume`] starts its
    /// Session without reading them again.
    pub fn stored_session(
        &self,
        origin: &LobbyOrigin,
    ) -> ClientTask<Result<Option<StoredSession>, TokenStoreError>> {
        let loaded = self.keychain.load(origin);
        let (http, origin, keychain) = (self.http.clone(), origin.clone(), self.keychain.clone());
        ClientTask::spawn(&self.rt, async move {
            Ok(loaded.await?.map(|tokens| StoredSession {
                api: Arc::new(ApiClient::with_tokens(http, origin, keychain, tokens)),
            }))
        })
    }

    /// Done once the keychain calls made so far have run, such as the clear of a sign in that was
    /// left behind its save. Quit awaits it: a clear cut off with the app, behind a save that
    /// still lands, would sign the Member in again at the next start.
    pub fn shutdown(&self) -> ClientTask<()> {
        match self.keychain.settled() {
            Some(settled) => ClientTask::spawn(&self.rt, settled),
            None => ClientTask::ready(()),
        }
    }

    /// Entry at the Lobby address. `here` is the Lobby the person is in, when they add one.
    pub fn entry(&self, here: Option<(LobbyOrigin, String)>) -> (EntryHandle, EntryUpdates) {
        entry::drive(self.entry_io(), Entry::at_address(self.policy, here))
    }

    /// Entry at the email step of a Lobby that has answered.
    pub fn entry_at(&self, origin: &LobbyOrigin, info: LobbyInfo) -> (EntryHandle, EntryUpdates) {
        let entry = Entry::reached(self.policy, origin.clone(), info);
        entry::drive(self.entry_io(), entry)
    }

    fn entry_io(&self) -> entry::EntryIo {
        entry::EntryIo {
            rt: self.rt.clone(),
            http: self.http.clone(),
            keychain: self.keychain.clone(),
            clock: self.clock.clone(),
        }
    }

    /// The blob cache of one Lobby: one per origin, on this
    /// client's runtime and HTTP client. Its disk eviction starts at once and runs hourly.
    pub fn media(&self, origin: &LobbyOrigin, paths: &Paths) -> MediaCache {
        MediaCache::new(self.rt.clone(), self.http.clone(), origin.clone(), paths)
    }

    /// The running session of a Lobby whose tokens are in the store, on this client's runtime:
    /// Intents go in through the handle, snapshots and news come out of the updates. Dropping
    /// the handle cancels everything the Session started.
    pub fn connect(&self, origin: &LobbyOrigin) -> (SessionHandle, Updates) {
        self.start(self.api(origin), LobbyState::default())
    }

    /// [`Client::connect`] with tokens already read, from what an earlier Session of the Lobby
    /// left (`last`): its first snapshot shows that while it connects, and its READY carries what
    /// followed, as after a reconnect.
    pub fn resume(
        &self,
        stored: StoredSession,
        last: Option<&LobbyState>,
    ) -> (SessionHandle, Updates) {
        let state = last.map(LobbyState::resumed).unwrap_or_default();
        self.start(stored.api, state)
    }

    fn start(&self, api: Arc<ApiClient>, state: LobbyState) -> (SessionHandle, Updates) {
        let (events_tx, events_rx) = mpsc::channel(EVENT_QUEUE);
        let conn = Connection::spawn(
            &self.rt,
            api.clone(),
            self.reconnect.clone(),
            events_tx.clone(),
            state.held_history(),
            self.watch_network,
        );
        let origin = api.origin().clone();
        let session = Session::new(origin, state, conn, self.rt.clone(), api, events_tx);
        drive(&self.rt, self.clock.clone(), session, events_rx)
    }

    /// A Session over `conn`, whose events go to `events`, with this client's REST and media.
    #[cfg(feature = "test-support")]
    pub(crate) fn session_over(
        &self,
        origin: &LobbyOrigin,
        conn: Connection,
        events: mpsc::Sender<crate::SessionEvent>,
    ) -> Session {
        Session::new(
            origin.clone(),
            LobbyState::default(),
            conn,
            self.rt.clone(),
            self.api(origin),
            events,
        )
    }

    /// The driver of a Session made over a test's connection, on this client's runtime and clock.
    #[cfg(feature = "test-support")]
    pub(crate) fn drive(
        &self,
        session: Session,
        events: mpsc::Receiver<crate::SessionEvent>,
    ) -> (SessionHandle, Updates) {
        drive(&self.rt, self.clock.clone(), session, events)
    }

    fn api(&self, origin: &LobbyOrigin) -> Arc<ApiClient> {
        Arc::new(ApiClient::new(
            self.http.clone(),
            origin.clone(),
            self.keychain.clone(),
        ))
    }
}
