//! The composition root and the staged shutdown.

use std::{
    io,
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};

use lobby_protocol::{LobbyInfo, PROTOCOL_VERSION, UserId};
use tokio::{net::TcpListener, sync::Semaphore, task::JoinHandle, time::timeout_at};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::{
    blobs::BlobStore,
    config::Config,
    cpu::CPU_PERMITS,
    db::{self, Db, DbError, DbLock, DbOpenError, LockError},
    domain::{Domain, Writer},
    jobs,
    limits::{CodeSends, RouteLimits},
    linger::{LingerListener, Peer},
    lobby::{Lobby, Seed},
    mail::{MailError, Mailer},
    router::router,
    sfu::{self, SfuHandle, SfuThread},
    tiers::{ConnTier, Tiers},
    ws::PreAuthBudget,
};

/// Listeners are bound by the caller and handed in, so tests can bind `127.0.0.1:0`.
pub struct Sockets {
    pub tcp: TcpListener,
    pub udp: std::net::UdpSocket,
    /// `sfu.tcp_bind`.
    pub sfu_tcp: std::net::TcpListener,
}

#[derive(Clone)]
pub struct App {
    pub(crate) cfg: Arc<Config>,
    /// Readers only: every write goes through `domain`.
    pub(crate) db: Db,
    pub(crate) domain: Domain,
    pub(crate) blobs: BlobStore,
    pub(crate) lobby: Lobby,
    pub(crate) cpu: Arc<Semaphore>,
    pub(crate) conns: ConnTier,
    pub(crate) sfu: SfuHandle,
    pub(crate) preauth: PreAuthBudget,
    pub(crate) limits: Arc<RouteLimits>,
}

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("{0}")]
    Locked(#[source] LockError),
    #[error("{0}")]
    Db(#[source] DbOpenError),
    #[error("database: {0}")]
    Seed(#[source] DbError),
    #[error("listener: {0}")]
    Bind(#[source] io::Error),
    #[error("sfu: {0}")]
    Sfu(#[source] io::Error),
    #[error("blob store: {0}")]
    Blobs(#[source] io::Error),
    #[error("mail: {0}")]
    Mail(#[source] MailError),
}

pub async fn start(cfg: Config, sockets: Sockets) -> Result<RunningServer, StartError> {
    let cfg = Arc::new(cfg);
    let local_addr = sockets.tcp.local_addr().map_err(StartError::Bind)?;
    // Held until the writer connection closed: one server per database.
    let lock = DbLock::acquire(&cfg.database.path).map_err(StartError::Locked)?;
    let (db, writer) = Db::open(&cfg.database.path).await.map_err(StartError::Db)?;
    let tiers = Tiers::default();
    let blobs =
        BlobStore::new(&cfg.storage, tiers.services.tracker.clone()).map_err(StartError::Blobs)?;
    let seed = load_seed(&cfg, &db).await.map_err(StartError::Seed)?;
    if seed.profiles.is_empty() {
        if let Ok(email) = lobby_protocol::Email::parse(&cfg.auth.bootstrap_email) {
            info!(
                domain = email.domain(),
                "no Members yet: the first one joins with auth.bootstrap_email"
            );
        } else {
            warn!(
                "no Members yet and no auth.bootstrap_email: set it so the first Member can join"
            );
        }
    }
    let mailer = Mailer::new(
        &cfg.smtp,
        &cfg.lobby.name,
        cfg.auth.magic_code_ttl,
        tiers.services.tracker.clone(),
        tiers.services.token.clone(),
    )
    .map_err(StartError::Mail)?;
    let fatal = CancellationToken::new();
    let (sfu_handle, sfu_inbox) = sfu::channel();
    let lobby = Lobby::new(seed, sfu_handle.clone(), cfg.voice);
    // The SFU reports into the Lobby synchronously, from its own thread.
    let sink_lobby = lobby.clone();
    let sink: sfu::EventSink = Box::new(move |ev| {
        sink_lobby.on_sfu_event(ev, tokio::time::Instant::now());
    });
    let sfu = SfuThread::spawn(
        sockets.udp,
        sockets.sfu_tcp,
        cfg.sfu.clone(),
        sfu_inbox,
        sink,
        tiers.services.token.clone(),
        fatal.clone(),
    )
    .await
    .map_err(StartError::Sfu)?;
    mailer.check_reachable(&cfg.smtp);
    let code_sends = Arc::new(CodeSends::new(cfg.auth.magic_code_sends));
    // After the last step that can fail, so a failed start leaves no writer task behind.
    let domain = Domain::start(
        Writer {
            conn: writer,
            lock,
            lobby: lobby.clone(),
            blobs: blobs.clone(),
            cfg: cfg.clone(),
            code_sends: code_sends.clone(),
            mailer,
        },
        &tiers.writes,
    );
    let limits = Arc::new(RouteLimits::new(cfg.http.rate_limits));
    let jobs = jobs::Jobs {
        lobby: lobby.clone(),
        db: db.clone(),
        domain: domain.clone(),
        blobs: blobs.clone(),
        limits: limits.clone(),
        code_sends: code_sends.clone(),
        cleanup_every: cfg.jobs.cleanup_interval,
    };
    #[cfg(feature = "test-support")]
    let cleanup = jobs.clone();
    tiers.services.tracker.spawn(supervised(
        jobs::run(jobs, tiers.services.token.clone()),
        fatal.clone(),
    ));
    let app = App {
        cfg: cfg.clone(),
        blobs,
        db: db.clone(),
        domain,
        lobby,
        cpu: Arc::new(Semaphore::new(CPU_PERMITS)),
        conns: tiers.conns.clone(),
        sfu: sfu_handle,
        preauth: PreAuthBudget::new(
            cfg.ws.max_unidentified_per_ip,
            cfg.ws.max_unidentified_global,
        ),
        limits,
    };
    let make_service = router(app.clone()).into_make_service_with_connect_info::<Peer>();
    let http = tiers.http.tracker.spawn(
        axum::serve(
            LingerListener::new(
                sockets.tcp,
                tiers.http.tracker.clone(),
                tiers.http.token.clone(),
            ),
            make_service,
        )
        .with_graceful_shutdown(tiers.http.token.clone().cancelled_owned())
        .into_future(),
    );
    info!(
        http = %local_addr,
        sfu = %sfu.local_addr(),
        sfu_tcp = %sfu.tcp_addr(),
        "server listening"
    );
    Ok(RunningServer {
        local_addr,
        tiers,
        http,
        sfu,
        db,
        app,
        #[cfg(feature = "test-support")]
        cleanup,
        fatal,
        shutdown_timeout: cfg.shutdown_timeout,
        shut_down: false,
    })
}

async fn load_seed(cfg: &Config, db: &Db) -> Result<Seed, DbError> {
    let (set_up, owner, icon) = {
        let mut conn = db.readers().acquire().await?;
        (
            db::users::any_user(&mut conn).await?,
            db::owner::owner(&mut conn).await?,
            db::icon::get(&mut conn).await?,
        )
    };
    Ok(Seed {
        info: LobbyInfo {
            instance_id: db.instance_id().await?,
            name: cfg.lobby.name.clone(),
            icon,
            upload_max_bytes: cfg.storage.upload_max_bytes,
            magic_code_sends: cfg.auth.magic_code_sends.per_email_per_10m,
            join_requests_enabled: cfg.auth.join_requests_enabled,
            set_up,
            owner,
            protocol_version: PROTOCOL_VERSION,
            server_version: env!("CARGO_PKG_VERSION").into(),
        },
        profiles: db::users::active_profiles(db).await?,
        access_requests: db::admission::pending(db).await?,
        last_message_seq: db::messages::last_seq(db).await?,
        last_change_rev: db::messages::last_rev(db).await?,
        send_every: cfg.ws.send_every,
    })
}

/// A started server. Call [`RunningServer::shutdown`]; dropping it instead cancels every task
/// without waiting and logs an error.
pub struct RunningServer {
    local_addr: SocketAddr,
    tiers: Tiers,
    http: JoinHandle<io::Result<()>>,
    sfu: SfuThread,
    db: Db,
    app: App,
    /// A copy of the cleanup job the services tier runs, for [`RunningServer::run_cleanup_at`].
    #[cfg(feature = "test-support")]
    cleanup: jobs::Jobs,
    fatal: CancellationToken,
    shutdown_timeout: Duration,
    shut_down: bool,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ShutdownReport {
    pub http: bool,
    pub conns: bool,
    /// Every accepted write finished.
    pub writes: bool,
    pub services: bool,
    pub sfu: bool,
    pub leaked_tasks: usize,
    pub elapsed: Duration,
}

impl ShutdownReport {
    pub fn is_clean(&self) -> bool {
        self.http
            && self.conns
            && self.writes
            && self.services
            && self.sfu
            && self.leaked_tasks == 0
    }
}

impl RunningServer {
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn sfu_addr(&self) -> SocketAddr {
        self.sfu.local_addr()
    }

    pub fn sfu_tcp_addr(&self) -> SocketAddr {
        self.sfu.tcp_addr()
    }

    /// Cancelled when a service failed or panicked (crash-only): `main` shuts down and exits 70.
    pub fn fatal(&self) -> CancellationToken {
        self.fatal.clone()
    }

    /// `bytes` staged as an upload of `kind` would be, without a request.
    #[cfg(feature = "test-support")]
    async fn stage(
        &self,
        kind: crate::blobs::BlobKind,
        bytes: &[u8],
        name: &str,
    ) -> crate::domain::Staged {
        let body = futures_util::stream::iter([Ok::<_, io::Error>(
            axum::body::Bytes::copy_from_slice(bytes),
        )]);
        crate::domain::Staged::upload(&self.app.blobs, &self.app.cpu, kind, Some(name), body)
            .await
            .expect("the seeded file is staged")
    }

    /// Holds the writer task: writes accepted meanwhile wait, in order, until the hold drops.
    #[cfg(feature = "test-support")]
    pub async fn hold_writes(&self) -> crate::domain::HeldWrites {
        self.app.domain.hold().await
    }

    /// Waits until at least `n` accepted writes have not finished.
    #[cfg(feature = "test-support")]
    pub async fn writes_in_flight(&self, n: usize) {
        self.app.domain.in_flight_at_least(n).await;
    }

    /// Waits until every accepted write has finished.
    #[cfg(feature = "test-support")]
    pub async fn writes_settled(&self) {
        self.app.domain.settled().await;
    }

    /// Waits until the files the server let go of so far are deleted.
    #[cfg(feature = "test-support")]
    pub async fn files_settled(&self) {
        self.app.blobs.discarded().await;
    }

    /// `POST /auth/magic-code` from this machine, through its own handler.
    #[cfg(feature = "test-support")]
    pub async fn request_code(
        &self,
        email: lobby_protocol::Email,
    ) -> lobby_protocol::MagicChallenge {
        let req = crate::error::AppJson(lobby_protocol::MagicCodeRequest { email });
        let client = axum::Extension(crate::client_ip::ClientIp::this_machine());
        crate::auth::magic::request_code(axum::extract::State(self.app.clone()), client, req)
            .await
            .expect("a code is requested")
            .0
            .challenge
    }

    /// `POST /auth/magic-code/verify`, through its own handler.
    #[cfg(feature = "test-support")]
    pub async fn verify_code(
        &self,
        challenge: lobby_protocol::MagicChallenge,
        code: lobby_protocol::MagicCode,
    ) -> lobby_protocol::VerifyOutcome {
        let req = crate::error::AppJson(lobby_protocol::MagicCodeVerify { challenge, code });
        crate::auth::magic::verify_code(axum::extract::State(self.app.clone()), req)
            .await
            .expect("the code is verified")
            .0
    }

    #[cfg(feature = "test-support")]
    pub async fn user_id_of(&self, username: &str) -> UserId {
        let id: String = sqlx::query_scalar("SELECT id FROM users WHERE username = ?1")
            .bind(username)
            .fetch_one(self.app.db.readers())
            .await
            .expect("the Member exists");
        id.parse().expect("a stored user id parses")
    }

    /// Stores one Message by `author` claiming a chat attachment of `bytes`, uploaded as `name`
    /// (sanitized as an upload is), as an upload and a `MESSAGE_SEND` would.
    #[cfg(feature = "test-support")]
    pub async fn seed_file_message(
        &self,
        author: &UserId,
        bytes: &[u8],
        name: &str,
        content: &str,
        nonce: &str,
    ) -> lobby_protocol::Attachment {
        let staged = self
            .stage(crate::blobs::BlobKind::ChatAttachment, bytes, name)
            .await;
        let blob = self
            .app
            .domain
            .store_upload(author.clone(), staged)
            .await
            .expect("the seeded upload is stored");
        self.app
            .domain
            .send_message(
                author.clone(),
                crate::tiers::ConnId(0),
                nonce.parse().expect("a seeded nonce is a rid"),
                content.to_owned(),
                vec![blob.id.clone()],
            )
            .await
            .expect("the seeded Message is stored");
        blob
    }

    /// One cleanup pass as of `now`, so a test ages expiries without waiting.
    #[cfg(feature = "test-support")]
    pub async fn run_cleanup_at(&self, now: lobby_protocol::UnixMs) -> jobs::CleanupReport {
        jobs::run_cleanup(
            &self.cleanup,
            now,
            tokio::time::Instant::now(),
            &CancellationToken::new(),
        )
        .await
        .expect("the cleanup runs")
    }

    #[cfg(feature = "test-support")]
    pub fn counts(&self) -> crate::lobby::Counts {
        self.app.lobby.counts()
    }

    /// How many Voice media connections the SFU saw end without a leave.
    #[cfg(feature = "test-support")]
    pub fn sfu_peers_lost(&self) -> usize {
        self.app.sfu.peers_lost()
    }

    /// How many offers restarted ICE.
    #[cfg(feature = "test-support")]
    pub fn sfu_ice_restarts(&self) -> usize {
        self.app.sfu.ice_restarts()
    }

    /// Bytes of every datagram the SFU sent by UDP, to every peer.
    #[cfg(feature = "test-support")]
    pub fn sfu_udp_bytes_sent(&self) -> u64 {
        self.app.sfu.udp_bytes_sent()
    }

    /// How the SFU sends `user`'s media now, once it has sent any.
    #[cfg(feature = "test-support")]
    pub fn sfu_media_path(&self, user: &UserId) -> Option<sfu::MediaPath> {
        self.app.sfu.media_path(user)
    }

    #[cfg(feature = "test-support")]
    pub fn fake_media_connected(&self, user: &UserId, vsid: lobby_protocol::VoiceSessionId) {
        self.fake_sfu_event(user, vsid, sfu::SfuEventKind::Connected);
    }

    #[cfg(feature = "test-support")]
    pub fn fake_share_video_started(&self, user: &UserId, vsid: lobby_protocol::VoiceSessionId) {
        self.fake_sfu_event(user, vsid, sfu::SfuEventKind::ShareVideoStarted);
    }

    #[cfg(feature = "test-support")]
    fn fake_sfu_event(
        &self,
        user: &UserId,
        vsid: lobby_protocol::VoiceSessionId,
        kind: sfu::SfuEventKind,
    ) {
        let ev = sfu::SfuEvent {
            user: user.clone(),
            vsid,
            kind,
        };
        self.app.lobby.on_sfu_event(ev, tokio::time::Instant::now());
    }

    pub async fn shutdown(mut self) -> ShutdownReport {
        let started = Instant::now();
        let deadline = tokio::time::Instant::now() + self.shutdown_timeout;
        let mut r = ShutdownReport::default();
        // 0. `/health` reports draining; new requests and upgrades get 503.
        self.tiers.conns.set_draining();
        // 1. Stop accepting. 2. End every Voice session (VOICE_ENDED{shutdown} queued first),
        // then kick every connection (1001); each awaits its own cleanup.
        self.tiers.http.token.cancel();
        self.app.lobby.end_all_voice();
        self.tiers.conns.tier.token.cancel();
        self.tiers.conns.tier.tracker.close();
        r.http = matches!(timeout_at(deadline, &mut self.http).await, Ok(Ok(Ok(()))));
        self.tiers.http.tracker.close();
        r.conns = timeout_at(deadline, self.tiers.conns.tier.tracker.wait())
            .await
            .is_ok();
        // 3. No more writes, the jobs' included. The ones accepted meanwhile finish and tell the
        // live Lobby; then the writer task closes its connection and lets go of the database's
        // lock. Past the deadline the task keeps both until it is done.
        self.app.domain.stop_admitting();
        self.tiers.writes.close();
        r.writes = timeout_at(deadline, self.tiers.writes.wait()).await.is_ok();
        // 4. Services last: connections may still need them while they clean up.
        self.tiers.services.token.cancel();
        self.tiers.services.tracker.close();
        r.services = timeout_at(deadline, self.tiers.services.tracker.wait())
            .await
            .is_ok();
        r.sfu = timeout_at(deadline, self.sfu.join()).await.is_ok();
        // Connections the kicked sockets closed meanwhile may have started to linger.
        r.http &= timeout_at(deadline, self.tiers.http.tracker.wait())
            .await
            .is_ok();
        r.leaked_tasks = self.tiers.http.tracker.len()
            + self.tiers.conns.tier.tracker.len()
            + self.tiers.writes.len()
            + self.tiers.services.tracker.len();
        // 5. Last: the readers.
        self.db.close().await;
        r.elapsed = started.elapsed();
        self.shut_down = true;
        if r.is_clean() {
            info!(
                elapsed_ms = r.elapsed.as_millis() as u64,
                "shutdown complete"
            );
        } else {
            warn!(report = ?r, "unclean shutdown");
        }
        r
    }
}

/// The jobs task is crash-only: a panic in it cancels `fatal`.
async fn supervised(job: impl Future<Output = ()>, fatal: CancellationToken) {
    use futures_util::FutureExt as _;
    if std::panic::AssertUnwindSafe(job)
        .catch_unwind()
        .await
        .is_err()
    {
        error!("jobs task panicked");
        fatal.cancel();
    }
}

impl Drop for RunningServer {
    fn drop(&mut self) {
        if !self.shut_down {
            self.tiers.cancel_all();
            self.app.domain.stop_admitting();
            error!("unclean shutdown: RunningServer dropped without shutdown()");
        }
    }
}
