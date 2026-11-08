pub mod admission;

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use futures_util::{SinkExt as _, StreamExt as _};
use lobby_server::testing::TestServer;
use serde_json::Value;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpStream,
};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::Message};

const WAIT: Duration = Duration::from_secs(5);

pub fn files_under(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if dir == root && e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => panic!("cannot read {}: {e}", dir.display()),
        };
        for e in entries {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out
}

/// Every file under the blob root, once the deletes the server started are done.
pub async fn blob_files(env: &TestServer) -> Vec<PathBuf> {
    env.server.files_settled().await;
    files_under(&env.config.storage.blob_root)
}

pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl HttpResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn json(&self) -> Value {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|e| panic!("body is not JSON ({e}): {:?}", self.body))
    }
}

pub struct RawResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl RawResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("body is not JSON ({e}): {:?}", self.body))
    }
}

pub async fn http_bytes(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> RawResponse {
    let (mut reader, mut writer) = TcpStream::connect(addr).await.unwrap().into_split();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    // A test may declare its own length (a large one with no body, for an early 413) or frame
    // the body itself (chunked).
    if !headers.iter().any(|(k, _)| {
        k.eq_ignore_ascii_case("content-length") || k.eq_ignore_ascii_case("transfer-encoding")
    }) {
        req.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    let mut wire = req.into_bytes();
    wire.extend_from_slice(body);
    // Read while writing: the server may answer before it has read a whole body (a 413).
    let response = tokio::spawn(async move {
        let mut raw = Vec::new();
        let _ = reader.read_to_end(&mut raw).await;
        raw
    });
    let _ = writer.write_all(&wire).await;
    let raw = tokio::time::timeout(WAIT, response)
        .await
        .expect("response within 5 s")
        .unwrap();
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("http response");
    let head = String::from_utf8(raw[..split].to_vec()).unwrap();
    let body = raw[split + 4..].to_vec();
    let mut lines = head.lines();
    let status = lines
        .next()
        .unwrap()
        .split(' ')
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .collect();
    RawResponse {
        status,
        headers,
        body,
    }
}

pub async fn http(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> HttpResponse {
    let raw = http_bytes(addr, method, path, headers, body.as_bytes()).await;
    HttpResponse {
        status: raw.status,
        headers: raw.headers,
        body: String::from_utf8(raw.body).unwrap(),
    }
}

pub async fn get(addr: SocketAddr, path: &str) -> HttpResponse {
    http(addr, "GET", path, &[], "").await
}

pub fn assert_envelope(res: &HttpResponse, status: u16, code: &str) {
    assert_eq!(res.status, status, "{}", res.body);
    assert!(
        res.header("content-type")
            .is_some_and(|v| v.starts_with("application/json")),
        "{:?}",
        res.headers
    );
    let body = res.json();
    assert_eq!(body["error"]["code"], code, "{body}");
    assert!(body["error"]["message"].is_string(), "{body}");
}

pub type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub async fn ws_connect(url: &str) -> Ws {
    let (ws, resp) = tokio_tungstenite::connect_async(url).await.unwrap();
    assert_eq!(resp.status(), 101);
    ws
}

pub async fn next_json(ws: &mut Ws) -> Value {
    loop {
        let msg = tokio::time::timeout(WAIT, ws.next())
            .await
            .expect("frame within 5 s")
            .expect("stream open")
            .unwrap();
        match msg {
            Message::Text(t) => return serde_json::from_str(&t).unwrap(),
            Message::Ping(_) | Message::Pong(_) => continue,
            other => panic!("expected a text frame, got {other:?}"),
        }
    }
}

pub async fn expect_close(ws: &mut Ws) -> u16 {
    loop {
        match tokio::time::timeout(WAIT, ws.next())
            .await
            .expect("close within 5 s")
        {
            Some(Ok(Message::Close(Some(f)))) => return f.code.into(),
            Some(Ok(Message::Close(None))) => return 1005,
            Some(Ok(_)) => continue,
            other => panic!("expected a close frame, got {other:?}"),
        }
    }
}

pub async fn send_text(ws: &mut Ws, text: &str) {
    ws.send(Message::text(text)).await.unwrap();
}

pub async fn post_json(addr: SocketAddr, path: &str, body: &Value) -> HttpResponse {
    http(
        addr,
        "POST",
        path,
        &[("Content-Type", "application/json")],
        &body.to_string(),
    )
    .await
}

pub async fn get_auth(addr: SocketAddr, path: &str, token: &str) -> HttpResponse {
    let bearer = format!("Bearer {token}");
    http(addr, "GET", path, &[("Authorization", bearer.as_str())], "").await
}

/// A connection of the test's own to the running server's database, to age a row.
pub async fn sqlite(env: &TestServer) -> sqlx::SqliteConnection {
    use sqlx::Connection as _;
    let options =
        sqlx::sqlite::SqliteConnectOptions::new().filename(env.data_dir().join("lobby.db"));
    sqlx::SqliteConnection::connect_with(&options)
        .await
        .unwrap()
}

/// The `SignedIn` of a Magic Code sign in, as the wire carries it.
pub async fn sign_in(env: &TestServer, name: &str) -> Value {
    serde_json::to_value(env.sign_in(name).await).unwrap()
}

pub fn access(signed_in: &Value) -> String {
    signed_in["tokens"]["accessToken"]
        .as_str()
        .expect("accessToken")
        .to_owned()
}

pub fn refresh_token(tokens: &Value) -> String {
    tokens["refreshToken"]
        .as_str()
        .expect("refreshToken")
        .to_owned()
}

pub async fn refresh(addr: SocketAddr, refresh: &str) -> HttpResponse {
    post_json(
        addr,
        "/api/v1/auth/refresh",
        &serde_json::json!({"refreshToken": refresh}),
    )
    .await
}

pub async fn identify(url: &str, token: &str) -> (Ws, Value) {
    identify_with(
        url,
        serde_json::json!({"token": token, "presence": "online"}),
    )
    .await
}

pub async fn identify_with(url: &str, d: Value) -> (Ws, Value) {
    let mut ws = ws_connect(url).await;
    assert_eq!(next_json(&mut ws).await["t"], "HELLO");
    send_text(
        &mut ws,
        &serde_json::json!({"t": "IDENTIFY", "d": d}).to_string(),
    )
    .await;
    let ready = next_json(&mut ws).await;
    assert_eq!(ready["t"], "READY", "{ready}");
    (ws, ready["d"].clone())
}

pub async fn next_of(ws: &mut Ws, t: &str) -> Value {
    loop {
        let frame = next_json(ws).await;
        if frame["t"] == t {
            return frame["d"].clone();
        }
    }
}

pub fn message_send(rid: &str, content: &str) -> String {
    serde_json::json!({"t": "MESSAGE_SEND", "rid": rid, "d": {"content": content, "attachmentIds": []}})
        .to_string()
}

pub fn nonce() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(1);
    format!(
        "{:016x}{:016x}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    )
}

pub async fn expect_quiet(ws: &mut Ws, wait: Duration) {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        match tokio::time::timeout_at(deadline, ws.next()).await {
            Err(_) => return,
            Ok(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => continue,
            Ok(other) => panic!("expected no frame, got {other:?}"),
        }
    }
}

pub async fn send_and_confirm(ws: &mut Ws, content: &str) -> Value {
    let rid = nonce();
    send_text(ws, &message_send(&rid, content)).await;
    loop {
        let frame = next_json(ws).await;
        match frame["t"].as_str() {
            Some("MESSAGE_CREATE") if frame["d"]["message"]["nonce"] == rid.as_str() => {
                return frame["d"]["message"].clone();
            }
            Some("ERROR") if frame["d"]["rid"] == rid.as_str() => panic!("send refused: {frame}"),
            _ => {}
        }
    }
}
