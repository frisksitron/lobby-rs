use std::{panic::AssertUnwindSafe, time::Duration};

use axum::extract::ws::{Message, Utf8Bytes};
use futures_util::{FutureExt as _, Sink, Stream, StreamExt as _};
use lobby_protocol::{
    Ack, ClientMsg, DecodeError, ErrorBody, ErrorCode, Hello, Identify, PROTOCOL_VERSION,
    PresenceSet, RawClientFrame, Rid, ServerMsg, UnixMs, UserId, limits::INK_POINTS_MAX,
};
use tokio::{
    sync::mpsc,
    time::{Instant, sleep_until, timeout, timeout_at},
};
use tracing::{Instrument as _, Span, debug, error, info, info_span, warn};

use super::{PreAuthPermit, budget::Budgets, writer};
use crate::{
    auth::{self, AuthError},
    chat::{self, ChatError},
    domain::{Authenticated, WriteError},
    limits::{SlidingWindow, retry_error},
    lobby::{CloseReason, Conn, HistoryRead, INK_QUEUE_BATCHES, InkQueue, Kick, RegisterError},
    refusal,
    server::App,
    tiers::ConnId,
    voice::VoiceCommand,
};

const WRITER_JOIN: Duration = Duration::from_secs(10);
/// After a frame the server could not read, how long the socket stays open once the close frame is
/// out, so the peer reads it before the reset that closing a socket with unread bytes causes.
const UNREAD_LINGER: Duration = Duration::from_millis(250);
const IDENTIFY_DB_BUDGET: Duration = Duration::from_secs(5);
/// Offline means not connected; nobody can choose it.
const MSG_PRESENCE_NOT_SETTABLE: &str = "Presence offline cannot be set";

/// Generic over the socket halves so tests can drive it with channels. `permit` is the socket's
/// pre-auth slot, given back once it identifies.
pub(crate) async fn run<W, R>(app: App, id: ConnId, permit: PreAuthPermit, sink: W, mut stream: R)
where
    W: Sink<Message> + Unpin + Send + 'static,
    R: Stream<Item = Result<Message, axum::Error>> + Unpin,
{
    let ws = &app.cfg.ws;
    let kick = Kick::child_of(&app.conns.tier.token);
    let (out_tx, out_rx) = mpsc::channel::<Utf8Bytes>(ws.outbound_queue);
    let ink = InkQueue::new(INK_QUEUE_BATCHES);
    let writer = app.conns.tier.tracker.spawn(
        writer::run(
            sink,
            out_rx,
            ink.clone(),
            kick.clone(),
            ws.ping_interval,
            ws.write_timeout,
        )
        .in_current_span(),
    );
    let hello = ServerMsg::Hello(Hello {
        protocol_version: PROTOCOL_VERSION,
        server_version: env!("CARGO_PKG_VERSION").into(),
        heartbeat_ms: millis(ws.ping_interval),
        identify_timeout_ms: millis(ws.identify_timeout),
    });
    let out = Out {
        tx: out_tx,
        ink,
        kick: kick.clone(),
    };
    out.send(&hello);

    // Set once registered, so cleanup can unregister even after a panic.
    let mut registered: Option<UserId> = None;
    let conn = Connection {
        app: &app,
        id,
        out: &out,
    };
    // A panic here closes this connection with 1011 and still runs the cleanup below.
    let served = AssertUnwindSafe(conn.serve(&mut stream, &mut registered, permit))
        .catch_unwind()
        .await;
    if served.is_err() {
        error!("connection handler panicked");
        kick.kick(CloseReason::InternalError);
    }
    if let Some(user) = &registered {
        app.lobby.unregister(user, id);
    }

    drop(out);
    let deadline = Instant::now() + writer::CLOSE_BUDGET;
    let mut writer = writer;
    let (joined, unreadable) = tokio::join!(
        timeout(WRITER_JOIN, &mut writer),
        closing_handshake(&mut stream, deadline),
    );
    if joined.is_err() {
        // Dropping the JoinHandle would only detach it; the tracker would then report it as
        // leaked at shutdown with nothing pointing here.
        writer.abort();
        warn!("writer did not finish in time; aborted");
    }
    if unreadable && kick.reason() == CloseReason::ProtocolViolation {
        sleep_until(deadline.min(Instant::now() + UNREAD_LINGER)).await;
    }
    debug!(reason = ?kick.reason(), "connection closed");
}

/// Reads until the peer's close frame or EOF, by `deadline`. A socket dropped with unread bytes is
/// reset, and on Windows the reset discards what the peer has not read yet, our close frame and its
/// code included. `true` if the stream had already ended, which after a kick means a frame the
/// server could not read.
async fn closing_handshake<R>(stream: &mut R, deadline: Instant) -> bool
where
    R: Stream<Item = Result<Message, axum::Error>> + Unpin,
{
    let mut frames = 0usize;
    let _ = timeout_at(deadline, async {
        while let Some(Ok(frame)) = stream.next().await {
            frames += 1;
            if matches!(frame, Message::Close(_)) {
                break;
            }
        }
    })
    .await;
    frames == 0
}

struct Out {
    tx: mpsc::Sender<Utf8Bytes>,
    ink: InkQueue,
    kick: Kick,
}

impl Out {
    /// Never awaits: a full queue means the client is not reading, which is a 4008.
    fn send(&self, msg: &ServerMsg) {
        if self.tx.try_send(Utf8Bytes::from(msg.encode())).is_err() {
            self.kick.kick(CloseReason::SlowConsumer);
        }
    }

    fn error(&self, code: ErrorCode, message: impl Into<String>, rid: Option<Rid>) {
        self.send(&ServerMsg::Error(ErrorBody::new(code, message, rid)));
    }

    fn fail(
        &self,
        code: ErrorCode,
        message: impl Into<String>,
        rid: Option<Rid>,
        why: CloseReason,
    ) {
        self.error(code, message, rid);
        self.kick.kick(why);
    }

    fn over_budget(&self, code: ErrorCode, message: &str, rid: Option<Rid>, retry_at: Instant) {
        let payload = retry_error(code, message, rid, retry_at, Instant::now());
        self.send(&ServerMsg::Error(payload));
    }
}

struct Connection<'a> {
    app: &'a App,
    id: ConnId,
    out: &'a Out,
}

struct Identity {
    user: UserId,
    deadline: Instant,
}

enum Next {
    Continue,
    Stop,
}

impl Connection<'_> {
    async fn serve<R>(&self, stream: &mut R, registered: &mut Option<UserId>, permit: PreAuthPermit)
    where
        R: Stream<Item = Result<Message, axum::Error>> + Unpin,
    {
        let mut budgets = Budgets::new();
        let Some(identity) = self.unidentified(stream, &mut budgets).await else {
            return;
        };
        drop(permit);
        *registered = Some(identity.user.clone());
        self.identified(stream, identity, &mut budgets).await;
    }

    async fn unidentified<R>(&self, stream: &mut R, budgets: &mut Budgets) -> Option<Identity>
    where
        R: Stream<Item = Result<Message, axum::Error>> + Unpin,
    {
        let deadline = Instant::now() + self.app.cfg.ws.identify_timeout;
        loop {
            let frame = tokio::select! {
                biased;
                _ = self.out.kick.kicked() => return None,
                _ = sleep_until(deadline) => {
                    self.out.kick.kick(CloseReason::IdentifyTimeout);
                    return None;
                }
                frame = stream.next() => frame,
            };
            let Some(text) = read(frame, &self.out.kick)? else {
                continue;
            };
            let raw = match RawClientFrame::parse(text.as_str()) {
                Ok(raw) => raw,
                Err(e) => {
                    debug!(error = %e, "malformed frame before IDENTIFY");
                    self.out.fail(
                        ErrorCode::InvalidRequest,
                        "Malformed frame",
                        None,
                        CloseReason::ProtocolViolation,
                    );
                    return None;
                }
            };
            let rid = raw.rid.clone();
            let frame = match raw.decode() {
                Ok(frame) => frame,
                Err(DecodeError::UnknownType(t)) => {
                    debug!(%t, "unknown command before IDENTIFY");
                    self.out.fail(
                        ErrorCode::InvalidRequest,
                        "Unknown command",
                        rid,
                        CloseReason::ProtocolViolation,
                    );
                    return None;
                }
                Err(e) => {
                    debug!(error = %e, "bad payload before IDENTIFY");
                    let message = if matches!(&e, DecodeError::BadPayload { t, .. } if t == "IDENTIFY")
                    {
                        "Invalid IDENTIFY payload"
                    } else {
                        "IDENTIFY first"
                    };
                    self.out.fail(
                        ErrorCode::InvalidRequest,
                        message,
                        rid,
                        CloseReason::ProtocolViolation,
                    );
                    return None;
                }
            };
            let ClientMsg::Identify(identify) = frame.msg else {
                debug!(t = frame.msg.kind(), "command before IDENTIFY");
                self.out.fail(
                    ErrorCode::InvalidRequest,
                    "IDENTIFY first",
                    rid,
                    CloseReason::ProtocolViolation,
                );
                return None;
            };
            // The first IDENTIFY counts against the budget too; it always fits.
            let _ = budgets.identify.try_acquire(Instant::now());
            return self.identify(identify, rid).await;
        }
    }

    async fn identify(&self, identify: Identify, rid: Option<Rid>) -> Option<Identity> {
        let presence = identify.presence;
        if !presence.is_settable() {
            self.out.fail(
                ErrorCode::InvalidRequest,
                "Invalid IDENTIFY payload",
                rid,
                CloseReason::ProtocolViolation,
            );
            return None;
        }
        // Read before the lookup: a revocation committed after it refuses the registration.
        let auth_epoch = self.app.lobby.auth_epoch();
        let authed = self.authenticate(&identify, rid.clone()).await?;
        let history = self.ready_history(&identify, rid.clone()).await?;
        let conn = Conn {
            id: self.id,
            session: authed.session,
            tx: self.out.tx.clone(),
            ink: self.out.ink.clone(),
            kick: self.out.kick.clone(),
            auth_epoch,
        };
        match self
            .app
            .lobby
            .register(&authed.user, conn, presence, history)
        {
            Ok(()) => {}
            Err(RegisterError::Revoked) => {
                info!(
                    session = authed.session.0,
                    "IDENTIFY refused: session revoked meanwhile"
                );
                self.out.fail(
                    ErrorCode::AuthFailed,
                    "Session revoked",
                    rid,
                    CloseReason::SessionRevoked,
                );
                return None;
            }
            Err(e @ RegisterError::NotAMember) => {
                debug!(error = %e, "IDENTIFY refused");
                self.out.fail(
                    ErrorCode::AuthFailed,
                    "Invalid or expired access token",
                    rid,
                    CloseReason::AuthInvalid,
                );
                return None;
            }
        }
        Span::current().record("user", tracing::field::display(&authed.user));
        info!(session = authed.session.0, "identified");
        Some(Identity {
            user: authed.user,
            deadline: deadline_of(authed.access_expires_at),
        })
    }

    async fn authenticate(&self, identify: &Identify, rid: Option<Rid>) -> Option<Authenticated> {
        let looked_up = timeout(
            IDENTIFY_DB_BUDGET,
            auth::authenticate(self.app, identify.token.expose()),
        )
        .await;
        match looked_up {
            Ok(Ok(authed)) => return Some(authed),
            Ok(Err(e @ (AuthError::InvalidToken | AuthError::AccessExpired))) => {
                self.out
                    .fail(e.code(), e.to_string(), rid, CloseReason::AuthInvalid);
                return None;
            }
            Ok(Err(e)) => {
                error!(error = %e, "IDENTIFY lookup failed");
            }
            Err(_) => {
                error!("IDENTIFY lookup exceeded its DB budget");
            }
        }
        self.out.fail(
            ErrorCode::InternalError,
            "An internal error occurred",
            rid,
            CloseReason::InternalError,
        );
        None
    }

    async fn ready_history(&self, identify: &Identify, rid: Option<Rid>) -> Option<HistoryRead> {
        let read = timeout(
            IDENTIFY_DB_BUDGET,
            chat::ready_history(self.app, identify.held.as_ref()),
        )
        .await;
        match read {
            Ok(Ok(history)) => return Some(history),
            Ok(Err(e)) => error!(error = %e, "READY history failed"),
            Err(_) => error!("READY history exceeded its DB budget"),
        }
        self.out.fail(
            ErrorCode::InternalError,
            "An internal error occurred",
            rid,
            CloseReason::InternalError,
        );
        None
    }

    async fn identified<R>(&self, stream: &mut R, mut identity: Identity, budgets: &mut Budgets)
    where
        R: Stream<Item = Result<Message, axum::Error>> + Unpin,
    {
        let pong_timeout = self.app.cfg.ws.pong_timeout;
        let mut alive_until = Instant::now() + pong_timeout;
        loop {
            let frame = tokio::select! {
                biased;
                _ = self.out.kick.kicked() => return,
                _ = sleep_until(identity.deadline) => {
                    info!("access token expired without a re-IDENTIFY");
                    self.out.fail(
                        ErrorCode::AuthExpired,
                        "Access token expired",
                        None,
                        CloseReason::AuthInvalid,
                    );
                    return;
                }
                _ = sleep_until(alive_until) => {
                    info!(?pong_timeout, "no pong from the peer: closing 4002");
                    self.out.kick.kick(CloseReason::PongTimeout);
                    return;
                }
                frame = stream.next() => frame,
            };
            alive_until = Instant::now() + pong_timeout;
            let Some(text) = read(frame, &self.out.kick) else {
                return;
            };
            let Some(text) = text else {
                continue;
            };
            if let Next::Stop = self.command(text.as_str(), &mut identity, budgets).await {
                return;
            }
        }
    }

    async fn command(&self, text: &str, identity: &mut Identity, budgets: &mut Budgets) -> Next {
        let raw = match RawClientFrame::parse(text) {
            Ok(raw) => raw,
            Err(e) => {
                debug!(error = %e, "malformed frame");
                self.out
                    .error(ErrorCode::InvalidRequest, "Malformed frame", None);
                return Next::Continue;
            }
        };
        let span = info_span!("cmd", t = %raw.t, rid = raw.rid.as_ref().map(Rid::as_str));
        let rid = raw.rid.clone();
        async move {
            let frame = match raw.decode() {
                Ok(frame) => frame,
                Err(DecodeError::UnknownType(_)) => {
                    self.out
                        .error(ErrorCode::InvalidRequest, "Unknown command", rid);
                    return Next::Continue;
                }
                Err(DecodeError::BadPayload { t, .. }) if t == "INK" => {
                    debug!("malformed ink batch dropped");
                    return Next::Continue;
                }
                Err(e) => {
                    debug!(error = %e, "bad payload");
                    let message = match &e {
                        DecodeError::BadPayload { t, .. } => format!("Invalid {t} payload"),
                        _ => "Malformed frame".to_owned(),
                    };
                    self.out.error(ErrorCode::InvalidRequest, message, rid);
                    return Next::Continue;
                }
            };
            if let Some(next) = self.over_budget(&frame.msg, rid.clone(), budgets, Instant::now()) {
                return next;
            }
            let cmd = match frame.msg {
                ClientMsg::Identify(identify) => {
                    return self.reidentify(identify, rid, identity).await;
                }
                ClientMsg::PresenceSet(PresenceSet { presence }) if !presence.is_settable() => {
                    self.out
                        .error(ErrorCode::InvalidRequest, MSG_PRESENCE_NOT_SETTABLE, rid);
                    return Next::Continue;
                }
                ClientMsg::PresenceSet(PresenceSet { presence }) => {
                    let changed = self
                        .app
                        .lobby
                        .set_presence(&identity.user, self.id, presence);
                    debug!(?presence, changed, "presence set");
                    if let Some(rid) = rid {
                        self.out.send(&ServerMsg::Ack(Ack::new(rid)));
                    }
                    return Next::Continue;
                }
                ClientMsg::MessageSend(cmd) => {
                    let sent =
                        chat::send(self.app, &identity.user, self.id, rid.as_ref(), cmd).await;
                    match sent {
                        Ok(()) => budgets.message_sent(),
                        Err(e) => self.chat_error(&e, rid),
                    }
                    return Next::Continue;
                }
                ClientMsg::MessageEdit(cmd) => {
                    let edited = chat::edit(self.app, &identity.user, cmd).await;
                    self.chat_outcome(edited, rid);
                    return Next::Continue;
                }
                ClientMsg::MessageDelete(cmd) => {
                    let deleted = chat::delete(self.app, &identity.user, cmd).await;
                    self.chat_outcome(deleted, rid);
                    return Next::Continue;
                }
                ClientMsg::Typing => {
                    self.app.lobby.typing(&identity.user, self.id);
                    return Next::Continue;
                }
                ClientMsg::VoiceJoin(flags) => VoiceCommand::Join {
                    muted: flags.muted,
                    deafened: flags.deafened,
                },
                ClientMsg::VoiceLeave => VoiceCommand::Leave,
                ClientMsg::VoiceStateSet(flags) => VoiceCommand::SetState {
                    muted: flags.muted,
                    deafened: flags.deafened,
                },
                ClientMsg::RtcAnswer(answer) => VoiceCommand::Answer {
                    vsid: answer.voice_session_id,
                    rev: answer.rev,
                    sdp: answer.sdp,
                },
                ClientMsg::RtcRestart(restart) => VoiceCommand::Restart {
                    vsid: restart.voice_session_id,
                },
                ClientMsg::ScreenShareStart(s) => VoiceCommand::ShareStart(s),
                ClientMsg::ScreenShareSource(s) => VoiceCommand::ShareSource(s.size),
                ClientMsg::ScreenShareStop => VoiceCommand::ShareStop,
                ClientMsg::ScreenShareWatch(w) => VoiceCommand::Watch {
                    sharer: w.sharer_id,
                },
                ClientMsg::ScreenShareUnwatch => VoiceCommand::Unwatch,
                ClientMsg::DrawingSet(d) => VoiceCommand::SetDrawing(d.enabled),
                ClientMsg::Ink(batch) if batch.points.len() > INK_POINTS_MAX => {
                    debug!(points = batch.points.len(), "oversized ink batch dropped");
                    return Next::Continue;
                }
                ClientMsg::Ink(batch) => VoiceCommand::Ink(batch),
            };
            self.voice(identity, rid, cmd)
        }
        .instrument(span)
        .await
    }

    /// The per-connection budgets. `Some` when the command is refused (answered, or dropped when it
    /// has no rid) instead of being handled.
    fn over_budget(
        &self,
        msg: &ClientMsg,
        rid: Option<Rid>,
        budgets: &mut Budgets,
        now: Instant,
    ) -> Option<Next> {
        let take = |window: &mut SlidingWindow| window.try_acquire(now).err();
        match msg {
            ClientMsg::Identify(_) => {
                let retry_at = take(&mut budgets.identify)?;
                warn!("IDENTIFY flood: closing 4029");
                let message = "Too many IDENTIFY attempts";
                let payload = retry_error(ErrorCode::RateLimited, message, rid, retry_at, now);
                self.out.send(&ServerMsg::Error(payload));
                self.out.kick.kick(CloseReason::IdentifyRateLimited);
                Some(Next::Stop)
            }
            ClientMsg::RtcAnswer(_) | ClientMsg::RtcRestart(_) => {
                let retry_at = take(&mut budgets.rtc)?;
                debug!("RTC budget exceeded");
                let message = "Too many voice signaling messages";
                let code = ErrorCode::SignalingRateLimited;
                self.out.over_budget(code, message, rid, retry_at);
                Some(Next::Continue)
            }
            ClientMsg::ScreenShareStart(_)
            | ClientMsg::ScreenShareStop
            | ClientMsg::ScreenShareWatch(_)
            | ClientMsg::ScreenShareUnwatch
            | ClientMsg::ScreenShareSource(_)
            | ClientMsg::DrawingSet(_) => {
                let retry_at = take(&mut budgets.share)?;
                debug!("Screen Share budget exceeded");
                let message = "Too many Screen Share commands";
                let code = ErrorCode::SignalingRateLimited;
                self.out.over_budget(code, message, rid, retry_at);
                Some(Next::Continue)
            }
            ClientMsg::PresenceSet(_) => {
                let retry_at = take(&mut budgets.presence)?;
                // Without a rid the excess is dropped silently.
                if rid.is_some() {
                    let message = "Too many presence changes";
                    self.out
                        .over_budget(ErrorCode::RateLimited, message, rid, retry_at);
                }
                Some(Next::Continue)
            }
            ClientMsg::Typing => take(&mut budgets.typing).map(|_| Next::Continue),
            ClientMsg::Ink(_) => (!budgets.take_ink(now)).then_some(Next::Continue),
            ClientMsg::MessageSend(_)
            | ClientMsg::MessageEdit(_)
            | ClientMsg::MessageDelete(_)
            | ClientMsg::VoiceJoin(_)
            | ClientMsg::VoiceLeave
            | ClientMsg::VoiceStateSet(_) => None,
        }
    }

    /// re-IDENTIFY after a token refresh: `ACK{accessExpiresAt}` moves the auth deadline.
    async fn reidentify(
        &self,
        identify: Identify,
        rid: Option<Rid>,
        identity: &mut Identity,
    ) -> Next {
        let Some(rid) = rid else {
            self.out
                .error(ErrorCode::InvalidRequest, "re-IDENTIFY needs a rid", None);
            return Next::Continue;
        };
        let auth_epoch = self.app.lobby.auth_epoch();
        let Some(authed) = self.authenticate(&identify, Some(rid.clone())).await else {
            return Next::Stop;
        };
        if authed.user != identity.user {
            warn!(other = %authed.user, "re-IDENTIFY as a different user");
            self.out.fail(
                ErrorCode::AuthFailed,
                "Invalid or expired access token",
                Some(rid),
                CloseReason::AuthInvalid,
            );
            return Next::Stop;
        }
        if !self
            .app
            .lobby
            .set_session(&identity.user, self.id, authed.session, auth_epoch)
        {
            info!(
                session = authed.session.0,
                "re-IDENTIFY refused: session revoked meanwhile"
            );
            self.out.fail(
                ErrorCode::AuthFailed,
                "Session revoked",
                Some(rid),
                CloseReason::SessionRevoked,
            );
            return Next::Stop;
        }
        identity.deadline = deadline_of(authed.access_expires_at);
        info!(
            access_expires_at = authed.access_expires_at.0,
            "re-identified"
        );
        self.out.send(&ServerMsg::Ack(Ack {
            access_expires_at: Some(authed.access_expires_at),
            ..Ack::new(rid)
        }));
        Next::Continue
    }

    /// A Voice command goes to the `Lobby`, whose voice machine answers with `ACK`/`ERROR`.
    fn voice(&self, identity: &Identity, rid: Option<Rid>, cmd: VoiceCommand) -> Next {
        if cmd.needs_rid() && rid.is_none() {
            self.out
                .error(ErrorCode::InvalidRequest, "This command needs a rid", None);
            return Next::Continue;
        }
        self.app
            .lobby
            .voice(&identity.user, self.id, rid, cmd, Instant::now());
        Next::Continue
    }

    fn chat_error(&self, e: &ChatError, rid: Option<Rid>) {
        let payload = refusal::chat(e, rid, Instant::now());
        match e {
            ChatError::RateLimited(_) => {
                debug!(retry_after_ms = payload.retry_after_ms, "send throttled");
            }
            ChatError::Write(WriteError::Db(db)) => error!(error = %db, "chat command failed"),
            _ => debug!(error = %e, "chat command refused"),
        }
        self.out.send(&ServerMsg::Error(payload));
    }

    /// An edit's or a delete's terminal outcome, queued after its broadcast.
    fn chat_outcome(&self, outcome: Result<(), ChatError>, rid: Option<Rid>) {
        match (outcome, rid) {
            (Ok(()), Some(rid)) => self.out.send(&ServerMsg::Ack(Ack::new(rid))),
            (Ok(()), None) => {}
            (Err(e), rid) => self.chat_error(&e, rid),
        }
    }
}

/// `None`: the connection is over. `Some(None)`: a control frame, keep reading.
fn read(frame: Option<Result<Message, axum::Error>>, kick: &Kick) -> Option<Option<Utf8Bytes>> {
    match frame {
        None | Some(Ok(Message::Close(_))) => None,
        Some(Err(e)) => {
            if is_protocol_violation(&e) {
                debug!(error = %e, "unreadable frame");
                kick.kick(CloseReason::ProtocolViolation);
            } else {
                debug!(error = %e, "connection lost");
            }
            None
        }
        Some(Ok(Message::Text(text))) => Some(Some(text)),
        Some(Ok(Message::Binary(_))) => {
            kick.kick(CloseReason::ProtocolViolation);
            None
        }
        Some(Ok(Message::Ping(_) | Message::Pong(_))) => Some(None),
    }
}

fn deadline_of(expires_at: UnixMs) -> Instant {
    let left = u64::try_from(expires_at.0 - UnixMs::now().0).unwrap_or(0);
    Instant::now() + Duration::from_millis(left)
}

/// A frame the peer got wrong (over `max_frame_bytes`, invalid UTF-8, bad framing) is a 4000. A
/// transport error or a reset without a close handshake means the peer is gone.
fn is_protocol_violation(e: &axum::Error) -> bool {
    use tungstenite::{Error, error::ProtocolError};
    let Some(e) = std::error::Error::source(e).and_then(|s| s.downcast_ref::<Error>()) else {
        return false;
    };
    match e {
        Error::Capacity(_) | Error::Utf8(_) | Error::AttackAttempt => true,
        Error::Protocol(p) => !matches!(p, ProtocolError::ResetWithoutClosingHandshake),
        _ => false,
    }
}

fn millis(d: Duration) -> u32 {
    u32::try_from(d.as_millis()).unwrap_or(u32::MAX)
}
