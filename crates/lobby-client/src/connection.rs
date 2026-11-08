use std::{
    sync::{
        Arc,
        atomic::{AtomicU8, AtomicU64, Ordering},
    },
    time::Duration,
};

use futures_util::{FutureExt as _, SinkExt as _, StreamExt as _, future::BoxFuture};
use lobby_protocol::{
    AccessToken, ClientFrame, ClientMsg, HeldHistory, Identify, Presence, Rid, ServerMsg, UnixMs,
    close,
};
use tokio::{
    runtime::Handle,
    sync::{Notify, mpsc, oneshot},
    time::{Instant, sleep, sleep_until, timeout},
};
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument as _, debug, info, info_span, warn};

use self::transport::{
    CLOSE_BUDGET, DialFailure, Handshake, Socket, close_gracefully, dial, finish_server_close,
    handshake,
};
use crate::{
    ClientTask,
    api::{ApiClient, ApiError, ApiKind},
    netwatch::NetworkWatch,
    session::SessionEvent,
};

mod ink_lane;
mod transport;

use self::ink_lane::InkLane;

const HANDSHAKE_BUDGET: Duration = Duration::from_secs(20);
/// A token that takes longer than this (a refresh on a dead pooled connection) gets a fresh
/// socket: the one dialed beside it is near the Lobby's 10 s IDENTIFY deadline.
const SLOW_TOKEN: Duration = Duration::from_secs(5);
/// Inbound events are awaited, never dropped: chat and control must not be lost.
pub(crate) const EVENT_QUEUE: usize = 256;
/// `send` fails with `QueueFull` beyond this, rather than buffer without bound.
const COMMAND_QUEUE: usize = 64;
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// The refresh and re-IDENTIFY happen this long before the access token expires,
/// so the socket never meets an expired token.
const REFRESH_LEAD: Duration = Duration::from_secs(120);
/// A proactive refresh is never scheduled sooner than this, so a short token
/// cannot spin.
const REFRESH_FLOOR: Duration = Duration::from_secs(5);
const REFRESH_RETRY: Duration = Duration::from_secs(15);
/// The actor's own re-IDENTIFY rids; their ACK is consumed here.
const REIDENTIFY_PREFIX: &str = "reid-";
/// A socket is stale after this many server heartbeats without a frame: the server pings every
/// `HELLO.heartbeatMs` (10 s), so 25 s by default.
const STALE_HEARTBEATS: f64 = 2.5;
/// The stale deadline never drops below this, whatever the server's heartbeat.
const STALE_FLOOR: Duration = Duration::from_secs(1);
/// After an OS resume: no frame within this long of our ping ⇒ the socket is dead.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
/// A session that lasted this long was a real recovery: the attempt count starts over. One that
/// ended sooner keeps counting, so a Lobby that accepts and then drops every connection backs off
/// to Unavailable instead of being redialed in a loop.
const STABLE_AFTER: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq)]
pub struct ReconnectPolicy {
    pub tiers: [Duration; 4],
    pub max_attempts: u32,
    /// ±this fraction of the wait, so a Lobby's clients do not all return at the same instant
    /// after a server restart.
    pub jitter: f64,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            tiers: [2, 5, 10, 30].map(Duration::from_secs),
            max_attempts: 10,
            jitter: 0.2,
        }
    }
}

impl ReconnectPolicy {
    pub fn delay_before(&self, attempt: u32, unit: f64) -> Option<Duration> {
        if attempt == 0 || attempt > self.max_attempts {
            return None;
        }
        let tier = usize::try_from(attempt - 1)
            .unwrap_or(usize::MAX)
            .min(self.tiers.len() - 1);
        let factor = 1.0 + self.jitter * (2.0 * unit.clamp(0.0, 1.0) - 1.0);
        Some(self.tiers[tier].mul_f64(factor.max(0.0)))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnEvent {
    Phase(Phase),
    Msg(ServerMsg),
    Disconnected { code: Option<u16> },
}

/// Durations, not instants: the actor runs on Tokio's clock and the UI on its own, so the UI
/// turns `retry_in` into a deadline when the event arrives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    Connecting {
        attempt: u32,
    },
    Connected,
    Reconnecting {
        attempt: u32,
        max: u32,
        retry_in: Duration,
    },
    Offline {
        retry_in: Duration,
    },
    Unavailable {
        attempts: u32,
    },
    ProtocolMismatch,
    NeedsAuth,
    SessionReplaced,
}

impl Phase {
    pub fn retry_in(&self) -> Option<Duration> {
        match self {
            Phase::Reconnecting { retry_in, .. } | Phase::Offline { retry_in } => Some(*retry_in),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConnError {
    #[error("not connected")]
    NotConnected,
    #[error("too many commands queued")]
    QueueFull,
}

#[derive(Debug)]
pub(crate) struct Command {
    pub(crate) socket: u64,
    pub(crate) text: String,
}

/// The identified socket's number, or [`NO_SOCKET`]. Each identified socket gets a new number,
/// so a command accepted for one socket never goes out on the next: the Session failed it when
/// that socket went away, and a late copy would duplicate a re-sent draft.
pub(crate) type SocketId = Arc<AtomicU64>;
pub(crate) const NO_SOCKET: u64 = 0;

#[derive(Default)]
pub(crate) struct Signals {
    pub(crate) retry: Notify,
    pub(crate) probe: Notify,
    pub(crate) presence: WantedPresence,
    /// What the Session's feed holds, read by every first IDENTIFY so READY carries what follows.
    pub(crate) held: std::sync::Mutex<Option<HeldHistory>>,
    /// Ink waits here, not among the commands.
    pub(crate) ink: InkLane,
}

/// What the Session wants others to see, read by every first IDENTIFY so a reconnect never
/// shows the Member online for a moment first.
#[derive(Default)]
pub(crate) struct WantedPresence(AtomicU8);

impl WantedPresence {
    pub(crate) fn set(&self, presence: Presence) {
        let v = match presence {
            Presence::Idle => 1,
            Presence::Dnd => 2,
            Presence::Online | Presence::Offline => 0,
        };
        self.0.store(v, Ordering::SeqCst);
    }

    pub(crate) fn get(&self) -> Presence {
        match self.0.load(Ordering::SeqCst) {
            1 => Presence::Idle,
            2 => Presence::Dnd,
            _ => Presence::Online,
        }
    }
}

pub struct Connection {
    cmds: mpsc::Sender<Command>,
    socket: SocketId,
    cancel: CancellationToken,
    signals: Arc<Signals>,
    done: Option<oneshot::Receiver<()>>,
    rt: Option<Handle>,
    /// Calls [`Self::probe_now`] on every network change, so a Wi-Fi switch is not found out by
    /// the stale-heartbeat timer or a failed send. Bursts of changes coalesce in the signal.
    _network: Option<NetworkWatch>,
}

impl Connection {
    pub(crate) fn spawn(
        rt: &Handle,
        api: Arc<ApiClient>,
        policy: ReconnectPolicy,
        events: mpsc::Sender<SessionEvent>,
        held: Option<HeldHistory>,
        watch_network: bool,
    ) -> Connection {
        let (cmds_tx, cmds_rx) = mpsc::channel(COMMAND_QUEUE);
        let cancel = CancellationToken::new();
        let signals = Arc::new(Signals {
            held: std::sync::Mutex::new(held),
            ..Signals::default()
        });
        let network = watch_network
            .then(|| {
                let signals = signals.clone();
                NetworkWatch::start(move || signals.probe.notify_one())
            })
            .flatten();
        let socket = Arc::new(AtomicU64::new(NO_SOCKET));
        let (done_tx, done_rx) = oneshot::channel();
        let span = info_span!("conn", lobby = %api.origin());
        let actor = Actor {
            url: api.origin().ws_url(lobby_protocol::routes::WS),
            api,
            policy,
            events,
            cmds: cmds_rx,
            cancel: cancel.clone(),
            signals: signals.clone(),
            socket: socket.clone(),
            sockets: 0,
            reidentify: 0,
        };
        rt.spawn(
            async move {
                actor.run().await;
                drop(done_tx);
            }
            .instrument(span),
        );
        Connection {
            cmds: cmds_tx,
            socket,
            cancel,
            signals,
            done: Some(done_rx),
            rt: Some(rt.clone()),
            _network: network,
        }
    }

    #[cfg(feature = "test-support")]
    pub(crate) fn detached(
        cmds: mpsc::Sender<Command>,
        socket: SocketId,
        cancel: CancellationToken,
        signals: Arc<Signals>,
    ) -> Connection {
        Connection {
            cmds,
            socket,
            cancel,
            signals,
            done: None,
            rt: None,
            _network: None,
        }
    }

    pub fn send(&self, frame: &ClientFrame) -> Result<(), ConnError> {
        let socket = self.socket.load(Ordering::SeqCst);
        if socket == NO_SOCKET {
            return Err(ConnError::NotConnected);
        }
        let cmd = Command {
            socket,
            text: frame.encode(),
        };
        self.cmds.try_send(cmd).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => ConnError::QueueFull,
            mpsc::error::TrySendError::Closed(_) => ConnError::NotConnected,
        })
    }

    /// Ink is lossy: it waits apart from the commands, only the newest two frames are kept, and
    /// a frame still waiting when the socket is replaced is dropped.
    pub fn send_ink(&self, frame: &ClientFrame) -> Result<(), ConnError> {
        let socket = self.socket.load(Ordering::SeqCst);
        if socket == NO_SOCKET {
            return Err(ConnError::NotConnected);
        }
        self.signals.ink.push(Command {
            socket,
            text: frame.encode(),
        });
        Ok(())
    }

    pub fn retry_now(&self) {
        self.signals.retry.notify_one();
    }

    pub fn probe_now(&self) {
        self.signals.probe.notify_one();
    }

    pub fn set_wanted_presence(&self, presence: Presence) {
        self.signals.presence.set(presence);
    }

    pub(crate) fn set_held(&self, held: Option<HeldHistory>) {
        *self.signals.held.lock().expect("held history") = held;
    }

    /// Cancels the actor and resolves once it has closed the socket (with 1000, bounded at
    /// 150 ms) and exited. Every await of the actor races the cancel, so this stays bounded.
    pub fn shutdown(mut self) -> ClientTask<()> {
        self.cancel.cancel();
        let done = self.done.take();
        match (self.rt.as_ref(), done) {
            (Some(rt), Some(done)) => ClientTask::spawn(rt, async move {
                let _ = done.await;
            }),
            _ => ClientTask::ready(()),
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

struct Actor {
    url: String,
    api: Arc<ApiClient>,
    policy: ReconnectPolicy,
    events: mpsc::Sender<SessionEvent>,
    cmds: mpsc::Receiver<Command>,
    cancel: CancellationToken,
    signals: Arc<Signals>,
    socket: SocketId,
    sockets: u64,
    reidentify: u32,
}

struct Gone;

enum End {
    Closed {
        code: Option<u16>,
        token: AccessToken,
        up_for: Option<Duration>,
    },
    Failed(DialFailure),
    Dead {
        up_for: Duration,
    },
    NeedsAuth,
    ProtocolMismatch,
}

enum Next {
    Stop(Phase),
    Hold(Phase),
    Now,
    Backoff { offline: bool },
}

impl Actor {
    /// Waits for room in the queue, but never past a cancel: an owner that stopped draining
    /// must still get its close 1000 and a `shutdown()` that resolves.
    async fn emit(&self, ev: ConnEvent) -> Result<(), Gone> {
        tokio::select! {
            biased;
            () = self.cancel.cancelled() => Err(Gone),
            sent = self.events.send(SessionEvent::Conn(ev)) => sent.map_err(|_| Gone),
        }
    }

    async fn run(mut self) {
        let mut attempt = 0;
        // One immediate retry after a 4003 (refresh, then reconnect), not counted as an attempt.
        let mut auth_retry = true;
        let mut violations = 0;
        // Redials without a wait since the last stable session: one is allowed, then backoff.
        let mut quick = 0;
        loop {
            if self
                .emit(ConnEvent::Phase(Phase::Connecting { attempt }))
                .await
                .is_err()
            {
                return;
            }
            let Ok(end) = self.attempt().await else {
                return;
            };
            self.socket.store(NO_SOCKET, Ordering::SeqCst);
            // Commands queued for a socket that is gone are dropped; the Session fails them. One
            // that slips in after this drain carries the old number and `session` drops it.
            while self.cmds.try_recv().is_ok() {}
            let next = match end {
                End::NeedsAuth => Next::Stop(Phase::NeedsAuth),
                End::ProtocolMismatch => Next::Stop(Phase::ProtocolMismatch),
                End::Failed(failure) => {
                    if self
                        .emit(ConnEvent::Disconnected { code: None })
                        .await
                        .is_err()
                    {
                        return;
                    }
                    Next::Backoff {
                        offline: failure == DialFailure::NoNetwork,
                    }
                }
                End::Dead { up_for } => {
                    if self
                        .emit(ConnEvent::Disconnected { code: None })
                        .await
                        .is_err()
                    {
                        return;
                    }
                    if up_for >= STABLE_AFTER {
                        (attempt, quick) = (0, 0);
                    }
                    redial(&mut quick)
                }
                End::Closed {
                    code,
                    token,
                    up_for,
                } => {
                    if self.emit(ConnEvent::Disconnected { code }).await.is_err() {
                        return;
                    }
                    let after_ready = up_for.is_some();
                    let stable = up_for.is_some_and(|d| d >= STABLE_AFTER);
                    if stable {
                        (attempt, quick) = (0, 0);
                    }
                    if code == Some(close::PROTOCOL_VIOLATION) {
                        violations += 1;
                    } else if after_ready {
                        violations = 0;
                    }
                    match code {
                        Some(close::REPLACED) => Next::Hold(Phase::SessionReplaced),
                        Some(close::SESSION_REVOKED | close::REMOVED) => {
                            // The in-memory pair goes first; the keychain delete runs on a
                            // blocking thread and finishes even if this is cancelled.
                            tokio::select! {
                                biased;
                                () = self.cancel.cancelled() => return,
                                () = self.api.clear() => {}
                            }
                            Next::Stop(Phase::NeedsAuth)
                        }
                        Some(close::AUTH_INVALID) if auth_retry || after_ready => {
                            auth_retry = false;
                            let refreshed = tokio::select! {
                                biased;
                                () = self.cancel.cancelled() => return,
                                r = self.api.force_refresh(&token) => r,
                            };
                            match refreshed {
                                Ok(_) => {
                                    info!("access token refused (4003): refreshed, reconnecting");
                                    redial(&mut quick)
                                }
                                Err(e) if e.kind == ApiKind::SessionExpired => {
                                    Next::Stop(Phase::NeedsAuth)
                                }
                                Err(e) => {
                                    warn!(error = %e, "refresh after 4003 failed");
                                    Next::Backoff { offline: false }
                                }
                            }
                        }
                        // The server was not reading fast enough for us: READY resyncs.
                        Some(close::SLOW_CONSUMER) => redial(&mut quick),
                        // Once may be a fluke; twice is this client's bug, and retrying would
                        // only repeat it.
                        Some(close::PROTOCOL_VIOLATION) if violations >= 2 => {
                            warn!(
                                "the server closed 4000 twice: this client sends something it refuses"
                            );
                            Next::Hold(Phase::Unavailable { attempts: attempt })
                        }
                        // A reset, an end without a close, or a server restart, on a session that
                        // was stable: the path or the Lobby is likely back already.
                        None | Some(close::GOING_AWAY) if stable => redial(&mut quick),
                        _ => Next::Backoff { offline: false },
                    }
                }
            };
            match next {
                Next::Stop(phase) => {
                    let _ = self.emit(ConnEvent::Phase(phase)).await;
                    return;
                }
                Next::Hold(phase) => {
                    if self.emit(ConnEvent::Phase(phase)).await.is_err() {
                        return;
                    }
                    // Only a click ends a hold: a resume would take a replaced session back from
                    // the device the Member uses now, and a refused one would only be refused
                    // again.
                    if self.wait(None, Wake::RetryOnly).await.is_err() {
                        return;
                    }
                    (attempt, quick, violations) = (0, 0, 0);
                    auth_retry = true;
                }
                Next::Now => {}
                Next::Backoff { offline } => {
                    let upcoming = attempt + 1;
                    let Some(delay) = self.policy.delay_before(upcoming, rand::random()) else {
                        let failed = attempt;
                        warn!(attempts = failed, "giving up until Try now");
                        let phase = Phase::Unavailable { attempts: failed };
                        if self.emit(ConnEvent::Phase(phase)).await.is_err() {
                            return;
                        }
                        if self.wait(None, Wake::RetryOrResume).await.is_err() {
                            return;
                        }
                        (attempt, quick) = (0, 0);
                        auth_retry = true;
                        continue;
                    };
                    let phase = if offline {
                        Phase::Offline { retry_in: delay }
                    } else {
                        Phase::Reconnecting {
                            attempt: upcoming,
                            max: self.policy.max_attempts,
                            retry_in: delay,
                        }
                    };
                    info!(
                        attempt = upcoming,
                        ?delay,
                        offline,
                        "reconnecting after a wait"
                    );
                    if self.emit(ConnEvent::Phase(phase)).await.is_err() {
                        return;
                    }
                    match self.wait(Some(delay), Wake::RetryOrResume).await {
                        Err(Gone) => return,
                        Ok(Woken::Timer) => attempt = upcoming,
                        Ok(Woken::Asked) => {
                            (attempt, quick) = (0, 0);
                            auth_retry = true;
                        }
                    }
                }
            }
        }
    }

    async fn wait(&self, delay: Option<Duration>, wake: Wake) -> Result<Woken, Gone> {
        let timer = async {
            match delay {
                Some(d) => sleep(d).await,
                None => std::future::pending().await,
            }
        };
        let resumed = async {
            match wake {
                Wake::RetryOrResume => self.signals.probe.notified().await,
                Wake::RetryOnly => std::future::pending().await,
            }
        };
        tokio::select! {
            biased;
            () = self.cancel.cancelled() => Err(Gone),
            () = self.signals.retry.notified() => {
                info!("retry requested");
                Ok(Woken::Asked)
            }
            () = resumed => {
                info!("the OS resumed or the network changed: retrying now");
                Ok(Woken::Asked)
            }
            () = timer => Ok(Woken::Timer),
        }
    }

    /// A "Try now" or a resume that arrived while no wait listened (a click while dialing, a
    /// second click on Unavailable, a resume during a hold) would otherwise end the next wait at
    /// once and restart the attempt count: a skipped backoff the anti-hammer rule
    /// does not allow. The dial about to start is what was asked for, so both are spent here.
    fn forget_stale_signals(&self) {
        let _ = self.signals.retry.notified().now_or_never();
        let _ = self.signals.probe.notified().now_or_never();
    }

    async fn attempt(&mut self) -> Result<End, Gone> {
        self.forget_stale_signals();
        let asked = Instant::now();
        // The token is needed only at IDENTIFY, so a refresh runs beside the dial, not before it.
        let (token, dialed) = tokio::select! {
            biased;
            () = self.cancel.cancelled() => return Err(Gone),
            both = async {
                tokio::join!(
                    self.api.access_token(),
                    timeout(HANDSHAKE_BUDGET, dial(&self.url)),
                )
            } => both,
        };
        let token = match token {
            Ok((token, _expires)) => token,
            Err(e) => {
                let end = if matches!(
                    e.kind,
                    ApiKind::SessionExpired | ApiKind::KeychainUnavailable
                ) {
                    info!(error = %e, "no usable session");
                    End::NeedsAuth
                } else {
                    // A refresh the server did not answer: it may just be down, so not Offline.
                    warn!(error = %e, "could not get an access token");
                    End::Failed(DialFailure::Other)
                };
                if let Ok(Ok(mut ws)) = dialed {
                    close_gracefully(&mut ws).await;
                }
                return Ok(end);
            }
        };
        let dialed = match dialed {
            // The Lobby's IDENTIFY deadline may have passed for a socket dialed that long ago.
            Ok(Ok(mut ws)) if asked.elapsed() >= SLOW_TOKEN => {
                info!(took = ?asked.elapsed(), "the token was slow: dialing again");
                close_gracefully(&mut ws).await;
                tokio::select! {
                    biased;
                    () = self.cancel.cancelled() => return Err(Gone),
                    r = timeout(HANDSHAKE_BUDGET, dial(&self.url)) => r,
                }
            }
            dialed => dialed,
        };
        let deadline = Instant::now() + HANDSHAKE_BUDGET;
        let mut ws = match dialed {
            Ok(Ok(ws)) => ws,
            Ok(Err(e)) => {
                warn!(error = %e.message, no_network = e.failure == DialFailure::NoNetwork, "connect failed");
                return Ok(End::Failed(e.failure));
            }
            Err(_) => {
                warn!("connect timed out");
                return Ok(End::Failed(DialFailure::Other));
            }
        };
        // From here the socket exists, so a cancel still closes it with 1000.
        let identify = Identify {
            token: token.clone(),
            presence: self.signals.presence.get(),
            held: self.signals.held.lock().expect("held history").clone(),
        };
        let greeted = tokio::select! {
            biased;
            () = self.cancel.cancelled() => None,
            r = tokio::time::timeout_at(deadline, handshake(&mut ws, identify)) => {
                Some(r.unwrap_or(Err(Handshake::Failed("timed out".into()))))
            }
        };
        let Some(greeted) = greeted else {
            close_gracefully(&mut ws).await;
            return Err(Gone);
        };
        let (ready, heartbeat) = match greeted {
            Ok(greeted) => greeted,
            Err(Handshake::ProtocolMismatch) => return Ok(End::ProtocolMismatch),
            Err(Handshake::Closed(code)) => {
                info!(?code, "closed during the handshake");
                return Ok(End::Closed {
                    code,
                    token,
                    up_for: None,
                });
            }
            Err(Handshake::Failed(why)) => {
                warn!(%why, "connect failed");
                return Ok(End::Failed(DialFailure::Other));
            }
        };
        info!("connected");
        self.sockets += 1;
        let socket = self.sockets;
        self.socket.store(socket, Ordering::SeqCst);
        let greeted = async {
            self.emit(ConnEvent::Msg(ready)).await?;
            self.emit(ConnEvent::Phase(Phase::Connected)).await
        };
        if greeted.await.is_err() {
            close_gracefully(&mut ws).await;
            return Err(Gone);
        }
        let stale_after = heartbeat.mul_f64(STALE_HEARTBEATS).max(STALE_FLOOR);
        let identified = Instant::now();
        let ended = self.session(&mut ws, socket, stale_after).await;
        let up_for = identified.elapsed();
        match ended {
            Ok(SessionEnd::Closed(code)) => Ok(End::Closed {
                code,
                token,
                up_for: Some(up_for),
            }),
            Ok(SessionEnd::Dead) => Ok(End::Dead { up_for }),
            Ok(SessionEnd::NeedsAuth) => Ok(End::NeedsAuth),
            Err(Gone) => {
                self.flush_queued(&mut ws, socket).await;
                close_gracefully(&mut ws).await;
                Err(Gone)
            }
        }
    }

    /// The owner left with commands still queued for this socket: the last thing it said
    /// (`VOICE_LEAVE` on a Lobby switch or quit) goes out before the close, within
    /// [`CLOSE_BUDGET`]. Usually nothing is queued and this costs nothing.
    async fn flush_queued(&mut self, ws: &mut Socket, socket: u64) {
        let mut queued = Vec::new();
        while let Ok(cmd) = self.cmds.try_recv() {
            if cmd.socket == socket {
                queued.push(cmd.text);
            }
        }
        if queued.is_empty() {
            return;
        }
        let n = queued.len();
        let sent = timeout(CLOSE_BUDGET, async {
            for text in queued {
                ws.feed(Message::text(text)).await?;
            }
            ws.flush().await
        })
        .await;
        match sent {
            Ok(Ok(())) => debug!(commands = n, "queued commands sent before the close"),
            _ => debug!(commands = n, "queued commands not sent before the close"),
        }
    }

    async fn session(
        &mut self,
        ws: &mut Socket,
        socket: u64,
        stale_after: Duration,
    ) -> Result<SessionEnd, Gone> {
        // Waits for the token mutex, which a refresh elsewhere may hold.
        let mut refresh_at = tokio::select! {
            biased;
            () = self.cancel.cancelled() => return Err(Gone),
            at = self.refresh_deadline() => at,
        };
        let mut last_frame = Instant::now();
        let mut probe_deadline: Option<Instant> = None;
        let mut refreshing: Option<BoxFuture<'static, Result<(AccessToken, UnixMs), ApiError>>> =
            None;
        loop {
            let probing = async {
                match probe_deadline {
                    Some(at) => sleep_until(at).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                biased;
                () = self.cancel.cancelled() => return Err(Gone),
                cmd = self.cmds.recv() => {
                    let Some(cmd) = cmd else { return Err(Gone) };
                    if cmd.socket != socket {
                        debug!(socket = cmd.socket, "dropped a command queued for an earlier socket");
                        continue;
                    }
                    if !self.write(ws, Message::text(cmd.text)).await? {
                        return Ok(SessionEnd::Closed(None));
                    }
                }
                () = self.signals.ink.ready() => {
                    while let Some(text) = self.signals.ink.pop(socket) {
                        if !self.write(ws, Message::text(text)).await? {
                            return Ok(SessionEnd::Closed(None));
                        }
                    }
                }
                frame = ws.next() => {
                    // Anything at all, the server's pings included, proves the path alive.
                    last_frame = Instant::now();
                    if probe_deadline.take().is_some() {
                        debug!("the resume probe was answered");
                    }
                    match frame {
                        Some(Ok(Message::Text(text))) => match ServerMsg::decode(&text) {
                            Ok(msg) => {
                                if !self.consumed_by_actor(&msg) {
                                    self.emit(ConnEvent::Msg(msg)).await?;
                                }
                            }
                            Err(e) => warn!(error = %e, "undecodable frame ignored"),
                        },
                        Some(Ok(Message::Close(frame))) => {
                            let code = frame.map(|f| u16::from(f.code));
                            info!(?code, "server closed the connection");
                            finish_server_close(ws).await;
                            return Ok(SessionEnd::Closed(code));
                        }
                        Some(Ok(_)) => {}
                        Some(Err(e)) => {
                            warn!(error = %e, "connection lost");
                            return Ok(SessionEnd::Closed(None));
                        }
                        None => return Ok(SessionEnd::Closed(None)),
                    }
                }
                () = sleep_until(last_frame + stale_after) => {
                    warn!(?stale_after, "no frame from the server: the connection is stale");
                    return Ok(SessionEnd::Dead);
                }
                () = self.signals.probe.notified() => {
                    info!("the OS resumed or the network changed: probing the connection");
                    if !self.write(ws, Message::Ping(Default::default())).await? {
                        return Ok(SessionEnd::Dead);
                    }
                    probe_deadline = Some(Instant::now() + PROBE_TIMEOUT);
                }
                () = probing => {
                    warn!("no answer to the probe: the connection is dead");
                    return Ok(SessionEnd::Dead);
                }
                () = sleep_until(refresh_at), if refreshing.is_none() => {
                    // Polled beside the socket, so chat and Voice keep flowing while it waits on
                    // HTTP or the keychain. Dropping it with the socket is safe: a rotation whose
                    // response was never stored is reissued by the server on the next refresh.
                    let api = self.api.clone();
                    refreshing = Some(Box::pin(async move { api.fresh_within(REFRESH_LEAD).await }));
                }
                refreshed = async { refreshing.as_mut().expect("guarded").await }, if refreshing.is_some() => {
                    refreshing = None;
                    match refreshed {
                        Ok((token, expires)) => {
                            self.reidentify += 1;
                            let n = self.reidentify;
                            let rid: Rid = format!("{REIDENTIFY_PREFIX}{n}").parse().expect("valid rid");
                            // The server reads presence and held history only on a socket's first
                            // IDENTIFY, so these are ignored.
                            let frame = ClientFrame::with_rid(rid, ClientMsg::Identify(Identify {
                                token,
                                presence: Presence::Online,
                                held: None,
                            }));
                            info!(access_expires_at = expires.0, "re-IDENTIFY after refresh");
                            if !self.write(ws, Message::text(frame.encode())).await? {
                                return Ok(SessionEnd::Closed(None));
                            }
                            refresh_at = deadline_before(expires);
                        }
                        Err(e) if matches!(e.kind, ApiKind::SessionExpired | ApiKind::KeychainUnavailable) => {
                            warn!(error = %e, "session can no longer be refreshed");
                            close_gracefully(ws).await;
                            return Ok(SessionEnd::NeedsAuth);
                        }
                        Err(e) => {
                            warn!(error = %e, retry_in = ?REFRESH_RETRY, "refresh failed; will retry");
                            refresh_at = Instant::now() + REFRESH_RETRY;
                        }
                    }
                }
            }
        }
    }

    async fn write(&self, ws: &mut Socket, msg: Message) -> Result<bool, Gone> {
        tokio::select! {
            biased;
            () = self.cancel.cancelled() => Err(Gone),
            r = timeout(WRITE_TIMEOUT, ws.send(msg)) => match r {
                Ok(Ok(())) => Ok(true),
                Ok(Err(e)) => {
                    warn!(error = %e, "write failed");
                    Ok(false)
                }
                Err(_) => {
                    warn!("write timed out");
                    Ok(false)
                }
            },
        }
    }

    async fn refresh_deadline(&self) -> Instant {
        match self.api.access_expires_at().await {
            Some(expires) => deadline_before(expires),
            None => Instant::now() + REFRESH_RETRY,
        }
    }

    fn consumed_by_actor(&self, msg: &ServerMsg) -> bool {
        match msg {
            ServerMsg::Ack(ack) if ack.rid.as_str().starts_with(REIDENTIFY_PREFIX) => {
                info!(
                    access_expires_at = ack.access_expires_at.map(|t| t.0),
                    "re-IDENTIFY acknowledged"
                );
                true
            }
            ServerMsg::Error(e)
                if e.rid
                    .as_ref()
                    .is_some_and(|r| r.as_str().starts_with(REIDENTIFY_PREFIX)) =>
            {
                warn!(code = ?e.code, message = %e.message, "re-IDENTIFY refused");
                true
            }
            _ => false,
        }
    }
}

/// Dial again at once, unless that was already done since the last stable session: a path that
/// keeps failing right after READY gets the normal backoff.
fn redial(quick: &mut u32) -> Next {
    *quick += 1;
    if *quick > 1 {
        Next::Backoff { offline: false }
    } else {
        Next::Now
    }
}

#[derive(Clone, Copy)]
enum Wake {
    RetryOrResume,
    RetryOnly,
}

enum Woken {
    Timer,
    Asked,
}

enum SessionEnd {
    Closed(Option<u16>),
    Dead,
    NeedsAuth,
}

fn deadline_before(expires: UnixMs) -> Instant {
    let left = u64::try_from(expires.0 - UnixMs::now().0).unwrap_or(0);
    let wait = Duration::from_millis(left)
        .saturating_sub(REFRESH_LEAD)
        .max(REFRESH_FLOOR);
    Instant::now() + wait
}
