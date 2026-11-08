//! Getting a socket and ending one: dial every resolved address, HELLO and IDENTIFY up to READY,
//! and the closing handshake. None of it knows about reconnect policy.

use std::time::Duration;

use futures_util::{SinkExt as _, StreamExt as _};
use lobby_protocol::{ClientFrame, ClientMsg, Identify, PROTOCOL_VERSION, ServerMsg, close};
use tokio::{net::TcpStream, time::timeout};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{
        Message,
        protocol::{CloseFrame, WebSocketConfig, frame::coding::CloseCode},
    },
};
use tracing::{debug, info, warn};

use crate::api::RESPONSE_MAX_BYTES;

/// A graceful close that takes longer is cut short: the socket is just dropped.
pub(super) const CLOSE_BUDGET: Duration = Duration::from_millis(150);

pub(super) type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DialFailure {
    NoNetwork,
    Other,
}

pub(super) enum Handshake {
    ProtocolMismatch,
    Closed(Option<u16>),
    Failed(String),
}

pub(super) struct DialError {
    pub(super) failure: DialFailure,
    pub(super) message: String,
}

impl DialError {
    fn other(message: impl ToString) -> Self {
        Self {
            failure: DialFailure::Other,
            message: message.to_string(),
        }
    }

    fn io(e: &std::io::Error) -> Self {
        use std::io::ErrorKind as K;
        let failure = match e.kind() {
            K::NetworkUnreachable | K::HostUnreachable | K::NetworkDown => DialFailure::NoNetwork,
            _ => DialFailure::Other,
        };
        Self {
            failure,
            message: e.to_string(),
        }
    }
}

/// Every resolved address is tried at once and the first to connect wins: on Windows a refused
/// `::1` (the server bound IPv4 only) takes about 2 s, which a sequential dial of `localhost`
/// would wait out on every connect.
pub(super) async fn dial(url: &str) -> Result<Socket, DialError> {
    let parsed = url::Url::parse(url).map_err(DialError::other)?;
    let host = parsed
        .host_str()
        .ok_or_else(|| DialError::other("no host"))?
        .trim_matches(['[', ']'])
        .to_owned();
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| DialError::other("no port"))?;
    let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host.as_str(), port))
        .await
        .map_err(|e| DialError {
            failure: DialFailure::NoNetwork,
            message: e.to_string(),
        })?
        .collect();
    // `select_ok` panics on an empty set.
    if addrs.is_empty() {
        return Err(DialError {
            failure: DialFailure::NoNetwork,
            message: format!("{host} resolved to no addresses"),
        });
    }
    let attempts = addrs
        .into_iter()
        .map(|addr| Box::pin(TcpStream::connect(addr)));
    let (stream, _rest) = futures_util::future::select_ok(attempts)
        .await
        .map_err(|e| DialError::io(&e))?;
    let _ = stream.set_nodelay(true);
    let config = WebSocketConfig::default()
        .max_message_size(Some(RESPONSE_MAX_BYTES))
        .max_frame_size(Some(RESPONSE_MAX_BYTES));
    tokio_tungstenite::client_async_tls_with_config(url, stream, Some(config), None)
        .await
        .map(|(ws, _response)| ws)
        .map_err(DialError::other)
}

/// HELLO (the version is checked before any credential is sent), IDENTIFY with the wanted
/// READY. Returns READY and the server's heartbeat.
pub(super) async fn handshake(
    ws: &mut Socket,
    identify: Identify,
) -> Result<(ServerMsg, Duration), Handshake> {
    let heartbeat = match next_msg(ws).await? {
        ServerMsg::Hello(hello) if hello.protocol_version == PROTOCOL_VERSION => {
            debug!(server_version = %hello.server_version, "HELLO");
            Duration::from_millis(u64::from(hello.heartbeat_ms))
        }
        ServerMsg::Hello(hello) => {
            warn!(server = hello.protocol_version, "protocol mismatch");
            close_gracefully(ws).await;
            return Err(Handshake::ProtocolMismatch);
        }
        other => {
            return Err(Handshake::Failed(format!(
                "expected HELLO, got {}",
                other.kind()
            )));
        }
    };
    let identify = ClientFrame::new(ClientMsg::Identify(identify));
    ws.send(Message::text(identify.encode()))
        .await
        .map_err(|e| Handshake::Failed(e.to_string()))?;
    loop {
        match next_msg(ws).await? {
            ready @ ServerMsg::Ready(_) => return Ok((ready, heartbeat)),
            ServerMsg::Error(e) => info!(code = ?e.code, message = %e.message, "IDENTIFY refused"),
            other => debug!(t = other.kind(), "frame before READY ignored"),
        }
    }
}

async fn next_msg(ws: &mut Socket) -> Result<ServerMsg, Handshake> {
    loop {
        match ws.next().await {
            Some(Ok(Message::Text(text))) => match ServerMsg::decode(&text) {
                Ok(msg) => return Ok(msg),
                Err(e) => return Err(Handshake::Failed(e.to_string())),
            },
            Some(Ok(Message::Close(frame))) => {
                let code = frame.map(|f| u16::from(f.code));
                finish_server_close(ws).await;
                return Err(Handshake::Closed(code));
            }
            Some(Ok(_)) => {}
            Some(Err(e)) => return Err(Handshake::Failed(e.to_string())),
            None => return Err(Handshake::Failed("connection closed".into())),
        }
    }
}

/// tungstenite only queues the reply to a server's close; reading on until the stream ends sends
/// it and completes the closing handshake, within [`CLOSE_BUDGET`].
pub(super) async fn finish_server_close(ws: &mut Socket) {
    let finished = timeout(CLOSE_BUDGET, async {
        while let Some(Ok(_)) = ws.next().await {}
    })
    .await;
    if finished.is_err() {
        debug!("server close not completed within its budget; dropping the socket");
    }
}

pub(super) async fn close_gracefully(ws: &mut Socket) {
    let frame = CloseFrame {
        code: CloseCode::from(close::NORMAL),
        reason: "".into(),
    };
    let closed = timeout(CLOSE_BUDGET, async {
        if ws.close(Some(frame)).await.is_ok() {
            while let Some(Ok(_)) = ws.next().await {}
        }
    })
    .await;
    if closed.is_err() {
        debug!("close handshake exceeded its budget; dropping the socket");
    }
}
