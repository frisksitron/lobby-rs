use std::{
    collections::VecDeque,
    io,
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use futures_util::{SinkExt as _, StreamExt as _};
use lobby_protocol::{
    ClientFrame, ErrorBody, ErrorCode, Hello, LobbyInfo, LobbyInstanceId, Message, MessageChanges,
    PROTOCOL_VERSION, Presence, RawClientFrame, ServerMsg,
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::mpsc,
    task::JoinSet,
};
use tokio_tungstenite::tungstenite::{
    Message as WsMessage,
    protocol::{CloseFrame, frame::coding::CloseCode},
};
use tokio_util::task::AbortOnDropHandle;

use super::{
    WAIT, frames,
    http::{
        bearer_of, envelope, read_request, read_request_and_body, request_line, respond,
        respond_with,
    },
    tokens_for_tests,
};
use crate::{HttpPolicy, LobbyOrigin};

#[derive(Debug, Clone)]
pub enum LobbyInfoReply {
    Ok,
    Error {
        status: u16,
        code: ErrorCode,
    },
    /// The Lobby's info followed by blanks, one byte over what the client reads of a response:
    /// it parses as it is if the size is not checked.
    Oversized,
}

#[derive(Debug, Clone)]
pub enum RefreshReply {
    Ok,
    Status(u16),
}

#[derive(Debug, Clone)]
pub struct FakeBehavior {
    pub lobby_info: LobbyInfoReply,
    pub protocol_version: u32,
    pub ready: bool,
    pub flood: usize,
    pub close_with: Option<u16>,
    pub refresh: RefreshReply,
    /// `HELLO.heartbeatMs`; the client's stale deadline is 2.5 of these.
    pub heartbeat_ms: u32,
    /// After READY, never read or write again but keep the socket: a half-open path.
    pub stall_after_ready: bool,
    pub refuse_ws: bool,
    pub upload_max_bytes: Option<u64>,
    /// Each media response is held this long before its bytes go, so fetches overlap.
    pub media_delay: Duration,
    /// Who the Member `me` finds in READY, besides themselves.
    pub members: Vec<(String, Presence)>,
    pub me: String,
    /// What `GET /api/v1/messages` answers, newest page.
    pub history: Vec<Message>,
    /// When set, each history answer waits for a `notify_one` on it.
    pub history_gate: Option<Arc<tokio::sync::Notify>>,
    /// What `GET /api/v1/messages/changes` answers, whatever its `after`.
    pub changes: MessageChanges,
    /// The first this many changes requests fail with a 500.
    pub changes_failures: u64,
    /// When set, each changes answer waits for a `notify_one` on it.
    pub changes_gate: Option<Arc<tokio::sync::Notify>>,
}

impl Default for FakeBehavior {
    fn default() -> Self {
        Self {
            lobby_info: LobbyInfoReply::Ok,
            protocol_version: PROTOCOL_VERSION,
            ready: true,
            flood: 0,
            close_with: None,
            refresh: RefreshReply::Ok,
            heartbeat_ms: 10_000,
            stall_after_ready: false,
            refuse_ws: false,
            upload_max_bytes: None,
            media_delay: Duration::ZERO,
            members: Vec::new(),
            me: "fake".into(),
            history: Vec::new(),
            history_gate: None,
            changes: MessageChanges::default(),
            changes_failures: 0,
            changes_gate: None,
        }
    }
}

pub struct FakeLobby {
    addr: SocketAddr,
    observed: mpsc::UnboundedReceiver<Observed>,
    /// Observations a test has not asked for yet, set aside while `sent` reads the frames.
    kept: VecDeque<Observed>,
    shared: Arc<Shared>,
    _accept: AbortOnDropHandle<()>,
}

/// What a test makes the Lobby do on its live WebSocket.
enum Push {
    Msg(ServerMsg),
    Close(u16),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observed {
    LobbyInfoRequested,
    RefreshRequested,
    MessagesRequested {
        bearer: String,
    },
    Identified,
    Flooded,
    /// A WebSocket ended; the client's close code, or `None` without a close frame. After a
    /// [`FakeBehavior::close_with`], the code is the client's reply to the fake's close.
    WsClosed(Option<u16>),
    Text(String),
    WsRefused,
    UploadRequested {
        name: String,
        bytes: usize,
        bearer: String,
    },
    MediaRequested(String),
}

struct Shared {
    behavior: FakeBehavior,
    tx: mpsc::UnboundedSender<Observed>,
    issued: AtomicU64,
    refusing: AtomicBool,
    changes_asked: AtomicU64,
    live: Mutex<Vec<mpsc::UnboundedSender<Push>>>,
}

impl FakeLobby {
    pub const SERVER_VERSION: &'static str = "0.0.0-fake";
    pub const NAME: &'static str = "Fake Lobby";

    pub async fn start(behavior: FakeBehavior) -> FakeLobby {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake lobby");
        let addr = listener.local_addr().expect("fake lobby address");
        let (tx, observed) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            behavior,
            tx,
            issued: AtomicU64::new(0),
            refusing: AtomicBool::new(false),
            changes_asked: AtomicU64::new(0),
            live: Mutex::new(Vec::new()),
        });
        let serving = shared.clone();
        let accept = tokio::spawn(async move {
            // Owned here, so dropping the FakeLobby aborts every connection task too.
            let mut conns = JoinSet::new();
            while let Ok((stream, _)) = listener.accept().await {
                conns.spawn(serve(stream, serving.clone()));
            }
        });
        FakeLobby {
            addr,
            observed,
            kept: VecDeque::new(),
            shared,
            _accept: AbortOnDropHandle::new(accept),
        }
    }

    /// `msg` goes to the client on its WebSocket now, as if the Lobby had sent it.
    pub fn emit(&self, msg: ServerMsg) {
        let mut live = self.shared.live.lock().expect("live sockets");
        live.retain(|socket| socket.send(Push::Msg(msg.clone())).is_ok());
    }

    /// Closes the live WebSocket (1001, going away): the client reconnects, and the next READY
    /// is the same as the first.
    pub fn drop_socket(&self) {
        self.drop_socket_with(CloseCode::Away.into());
    }

    pub fn drop_socket_with(&self, code: u16) {
        for live in self.shared.live.lock().expect("live sockets").drain(..) {
            let _ = live.send(Push::Close(code));
        }
    }

    /// While on, a new WebSocket is answered with 503, as a Lobby that is down answers.
    pub fn refuse_ws(&self, on: bool) {
        self.shared.refusing.store(on, Ordering::SeqCst);
    }

    /// The frames the client sent on its WebSocket since the last call, oldest first. Other
    /// observations stay for [`Self::next_observed`].
    pub fn sent(&mut self) -> Vec<ClientFrame> {
        let mut frames = Vec::new();
        let mut rest = VecDeque::new();
        while let Some(seen) = self
            .kept
            .pop_front()
            .or_else(|| self.observed.try_recv().ok())
        {
            match seen {
                Observed::Text(text) => frames.push(
                    RawClientFrame::parse(&text)
                        .and_then(RawClientFrame::decode)
                        .expect("the client sends valid frames"),
                ),
                other => rest.push_back(other),
            }
        }
        self.kept = rest;
        frames
    }

    pub fn origin(&self) -> LobbyOrigin {
        LobbyOrigin::parse(&format!("http://{}", self.addr), HttpPolicy::LoopbackOnly)
            .expect("loopback origin")
    }

    pub async fn next_observed(&mut self) -> Observed {
        if let Some(kept) = self.kept.pop_front() {
            return kept;
        }
        tokio::time::timeout(WAIT, self.observed.recv())
            .await
            .expect("an observation within 5 s")
            .expect("fake lobby running")
    }

    pub fn observed_now(&mut self) -> Vec<Observed> {
        let mut out: Vec<Observed> = self.kept.drain(..).collect();
        while let Ok(o) = self.observed.try_recv() {
            out.push(o);
        }
        out
    }

    pub async fn next_close(&mut self) -> Option<u16> {
        loop {
            if let Observed::WsClosed(code) = self.next_observed().await {
                return code;
            }
        }
    }

    pub fn media_png() -> Vec<u8> {
        png(4, 3)
    }

    fn preview_png() -> Vec<u8> {
        png(2, 2)
    }

    pub fn lobby_info(protocol_version: u32) -> LobbyInfo {
        LobbyInfo {
            instance_id: "lby_0123456789abcdef01234567"
                .parse::<LobbyInstanceId>()
                .expect("valid id"),
            name: Self::NAME.into(),
            icon: None,
            upload_max_bytes: 10 * 1024 * 1024,
            magic_code_sends: 3,
            join_requests_enabled: true,
            set_up: true,
            owner: None,
            protocol_version,
            server_version: Self::SERVER_VERSION.into(),
        }
    }
}

async fn serve(stream: TcpStream, shared: Arc<Shared>) {
    let Ok(line) = request_line(&stream).await else {
        return;
    };
    if line.starts_with("GET /api/v1/ws ")
        && (shared.behavior.refuse_ws || shared.refusing.load(Ordering::SeqCst))
    {
        let mut stream = stream;
        let _ = read_request(&mut stream).await;
        let _ = shared.tx.send(Observed::WsRefused);
        let _ = respond(stream, 503, &envelope(ErrorCode::Unavailable, "down")).await;
    } else if line.starts_with("GET /api/v1/ws ") {
        serve_ws(stream, &shared).await;
    } else if line.starts_with("GET /api/v1/lobby ") {
        let _ = serve_lobby_info(stream, &shared).await;
    } else if line.starts_with("POST /api/v1/auth/refresh ") {
        let _ = serve_refresh(stream, &shared).await;
    } else if line.starts_with("GET /api/v1/messages/changes") {
        let _ = serve_changes(stream, &shared).await;
    } else if line.starts_with("GET /api/v1/messages") {
        let _ = serve_messages(stream, &shared).await;
    } else if line.starts_with("POST /api/v1/uploads") {
        let _ = serve_upload(stream, &line, &shared).await;
    } else if line.starts_with("GET /media/") {
        let _ = serve_media(stream, &line, &shared).await;
    } else {
        let _ = respond(
            stream,
            404,
            br#"{"error":{"code":"NOT_FOUND","message":"Not found"}}"#,
        )
        .await;
    }
}

async fn serve_ws(stream: TcpStream, shared: &Shared) {
    let behavior = &shared.behavior;
    let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
        return;
    };
    // Registered before the client can see anything: a test that acts on Connected must find
    // this socket live, whatever the runtime threads are doing.
    let (push_tx, mut push) = mpsc::unbounded_channel();
    shared.live.lock().expect("live sockets").push(push_tx);
    let hello = ServerMsg::Hello(Hello {
        protocol_version: behavior.protocol_version,
        server_version: FakeLobby::SERVER_VERSION.into(),
        heartbeat_ms: behavior.heartbeat_ms,
        identify_timeout_ms: 10_000,
    });
    if ws.send(WsMessage::text(hello.encode())).await.is_err() {
        return;
    }
    if behavior.ready {
        loop {
            match ws.next().await {
                Some(Ok(WsMessage::Text(t))) if t.contains("\"IDENTIFY\"") => break,
                Some(Ok(WsMessage::Close(frame))) => {
                    let _ = shared
                        .tx
                        .send(Observed::WsClosed(frame.map(|f| u16::from(f.code))));
                    return;
                }
                Some(Ok(_)) => {}
                Some(Err(_)) | None => {
                    let _ = shared.tx.send(Observed::WsClosed(None));
                    return;
                }
            }
        }
        let others: Vec<(&str, Presence)> = behavior
            .members
            .iter()
            .map(|(name, presence)| (name.as_str(), *presence))
            .collect();
        let mut ready = frames::ready(&behavior.me, &others);
        if let ServerMsg::Ready(r) = &mut ready {
            r.last_message_seq = behavior.history.last().map_or(0, |m| m.id.get() as u64);
            r.history = lobby_protocol::ReadyHistory::Latest(lobby_protocol::MessagePage {
                messages: behavior.history.clone(),
                has_more: false,
            });
        }
        if ws.send(WsMessage::text(ready.encode())).await.is_err() {
            return;
        }
        let _ = shared.tx.send(Observed::Identified);
        if behavior.stall_after_ready {
            std::future::pending::<()>().await;
        }
    }
    if behavior.flood > 0 {
        let error = ServerMsg::Error(ErrorBody::new(ErrorCode::Unavailable, "flood", None));
        for _ in 0..behavior.flood {
            if ws.feed(WsMessage::text(error.encode())).await.is_err() {
                return;
            }
        }
        if ws.flush().await.is_err() {
            return;
        }
        let _ = shared.tx.send(Observed::Flooded);
    }
    if let Some(code) = behavior.close_with {
        let frame = CloseFrame {
            code: CloseCode::from(code),
            reason: "".into(),
        };
        if ws.close(Some(frame)).await.is_err() {
            return;
        }
    }
    let mut pushable = true;
    let code = loop {
        tokio::select! {
            pushed = push.recv(), if pushable => match pushed {
                Some(Push::Msg(msg)) => {
                    if ws.send(WsMessage::text(msg.encode())).await.is_err() {
                        break None;
                    }
                }
                Some(Push::Close(code)) => {
                    let frame = CloseFrame {
                        code: CloseCode::from(code),
                        reason: "".into(),
                    };
                    let _ = ws.close(Some(frame)).await;
                    pushable = false;
                }
                None => pushable = false,
            },
            read = ws.next() => match read {
                Some(Ok(WsMessage::Close(frame))) => break frame.map(|f| u16::from(f.code)),
                Some(Ok(WsMessage::Text(text))) => {
                    let _ = shared.tx.send(Observed::Text(text.to_string()));
                }
                Some(Ok(_)) => {}
                Some(Err(_)) | None => break None,
            },
        }
    };
    let _ = shared.tx.send(Observed::WsClosed(code));
}

async fn serve_lobby_info(mut stream: TcpStream, shared: &Shared) -> io::Result<()> {
    read_request(&mut stream).await?;
    let _ = shared.tx.send(Observed::LobbyInfoRequested);
    match &shared.behavior.lobby_info {
        LobbyInfoReply::Ok => {
            let body = serde_json::to_vec(&FakeLobby::lobby_info(shared.behavior.protocol_version))
                .expect("LobbyInfo serializes");
            respond(stream, 200, &body).await
        }
        LobbyInfoReply::Error { status, code } => {
            respond(stream, *status, &envelope(*code, "scripted failure")).await
        }
        LobbyInfoReply::Oversized => {
            let mut body =
                serde_json::to_vec(&FakeLobby::lobby_info(shared.behavior.protocol_version))
                    .expect("LobbyInfo serializes");
            body.resize(crate::api::RESPONSE_MAX_BYTES + 1, b' ');
            respond(stream, 200, &body).await
        }
    }
}

async fn serve_refresh(mut stream: TcpStream, shared: &Shared) -> io::Result<()> {
    read_request(&mut stream).await?;
    let _ = shared.tx.send(Observed::RefreshRequested);
    match shared.behavior.refresh {
        RefreshReply::Ok => {
            let n = shared.issued.fetch_add(1, Ordering::SeqCst);
            let body = serde_json::to_vec(&tokens_for_tests(&format!("fresh{n}")))
                .expect("tokens serialize");
            respond(stream, 200, &body).await
        }
        RefreshReply::Status(status) => {
            let code = match status {
                401 => ErrorCode::AuthFailed,
                403 => ErrorCode::Forbidden,
                _ => ErrorCode::InternalError,
            };
            respond(stream, status, &envelope(code, "scripted refresh failure")).await
        }
    }
}

async fn serve_messages(mut stream: TcpStream, shared: &Shared) -> io::Result<()> {
    let head = read_request(&mut stream).await?;
    let bearer = bearer_of(&head);
    let _ = shared.tx.send(Observed::MessagesRequested { bearer });
    if let Some(gate) = &shared.behavior.history_gate {
        gate.notified().await;
    }
    let page = lobby_protocol::MessagePage {
        messages: shared.behavior.history.clone(),
        has_more: false,
    };
    let body = serde_json::to_vec(&page).expect("a page serializes");
    respond(stream, 200, &body).await
}

async fn serve_changes(mut stream: TcpStream, shared: &Shared) -> io::Result<()> {
    read_request(&mut stream).await?;
    if let Some(gate) = &shared.behavior.changes_gate {
        gate.notified().await;
    }
    let asked = shared.changes_asked.fetch_add(1, Ordering::SeqCst);
    if asked < shared.behavior.changes_failures {
        let body = envelope(ErrorCode::InternalError, "scripted changes failure");
        return respond(stream, 500, &body).await;
    }
    let body = serde_json::to_vec(&shared.behavior.changes).expect("changes serialize");
    respond(stream, 200, &body).await
}

async fn serve_upload(mut stream: TcpStream, line: &str, shared: &Shared) -> io::Result<()> {
    let (head, body) = read_request_and_body(&mut stream).await?;
    let name = line
        .split_once("?name=")
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .map(|raw| {
            url::form_urlencoded::parse(raw.as_bytes())
                .map(|(k, v)| format!("{k}{v}"))
                .collect::<String>()
        })
        .unwrap_or_default();
    let _ = shared.tx.send(Observed::UploadRequested {
        name: name.clone(),
        bytes: body.len(),
        bearer: bearer_of(&head),
    });
    if shared
        .behavior
        .upload_max_bytes
        .is_some_and(|max| body.len() as u64 > max)
    {
        return respond(
            stream,
            413,
            &envelope(ErrorCode::PayloadTooLarge, "too large"),
        )
        .await;
    }
    let is_png = body.starts_with(b"\x89PNG");
    let attachment = lobby_protocol::Attachment {
        id: lobby_protocol::BlobId::random(),
        name,
        mime_type: if is_png {
            "image/png"
        } else {
            "application/octet-stream"
        }
        .into(),
        size: body.len() as u64,
        preview: is_png.then_some(lobby_protocol::Preview {
            width: 2,
            height: 2,
        }),
    };
    let body = serde_json::to_vec(&attachment).expect("attachment serializes");
    respond(stream, 201, &body).await
}

async fn serve_media(mut stream: TcpStream, line: &str, shared: &Shared) -> io::Result<()> {
    read_request(&mut stream).await?;
    let path = line
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_owned();
    let _ = shared.tx.send(Observed::MediaRequested(path.clone()));
    tokio::time::sleep(shared.behavior.media_delay).await;
    let bytes = if path.ends_with("/preview") {
        FakeLobby::preview_png()
    } else {
        FakeLobby::media_png()
    };
    respond_with(stream, 200, "image/png", "", &bytes).await
}

fn png(width: u32, height: u32) -> Vec<u8> {
    let img = image::RgbaImage::from_pixel(width, height, image::Rgba([200, 30, 30, 255]));
    let mut out = io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut out, image::ImageFormat::Png)
        .expect("png encodes");
    out.into_inner()
}
