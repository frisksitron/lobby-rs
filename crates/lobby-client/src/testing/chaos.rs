use std::{
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    sync::Notify,
    task::JoinSet,
};
use tokio_util::{sync::CancellationToken, task::AbortOnDropHandle};

use super::http::request_line;
use crate::{HttpPolicy, LobbyOrigin};

/// A TCP proxy between a client and a real server that breaks the path on demand. A frozen
/// connection stops forwarding in both directions but keeps both sockets open: to both ends it
/// looks like a pulled cable or a laptop lid, a half-open socket.
pub struct ChaosProxy {
    addr: SocketAddr,
    shared: Arc<ChaosShared>,
    _accept: AbortOnDropHandle<()>,
}

#[derive(Default)]
struct ChaosShared {
    blackhole: AtomicBool,
    refusing: AtomicBool,
    links: Mutex<Vec<Arc<Link>>>,
    accepted: AtomicUsize,
    /// REST requests whose request line starts with this get through to the server, but the
    /// server's response never reaches the client: the connection is cut once it arrives.
    drop_responses_to: Mutex<Option<String>>,
    dropped: AtomicUsize,
    /// The request line each connection opened with, in order.
    requests: Mutex<Vec<String>>,
}

#[derive(Default)]
struct Link {
    frozen: AtomicBool,
    thawed: Notify,
    cut: CancellationToken,
    websocket: AtomicBool,
}

impl ChaosProxy {
    pub async fn start(upstream: SocketAddr) -> ChaosProxy {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind proxy");
        let addr = listener.local_addr().expect("proxy address");
        let shared = Arc::new(ChaosShared::default());
        let accepting = shared.clone();
        let accept = tokio::spawn(async move {
            let mut links = JoinSet::new();
            while let Ok((client, _)) = listener.accept().await {
                accepting.accepted.fetch_add(1, Ordering::SeqCst);
                if accepting.refusing.load(Ordering::SeqCst) {
                    drop(client);
                    continue;
                }
                let link = Arc::new(Link::default());
                link.frozen
                    .store(accepting.blackhole.load(Ordering::SeqCst), Ordering::SeqCst);
                accepting.links.lock().expect("links").push(link.clone());
                links.spawn(proxy(client, upstream, link, accepting.clone()));
            }
        });
        ChaosProxy {
            addr,
            shared,
            _accept: AbortOnDropHandle::new(accept),
        }
    }

    pub fn origin(&self) -> LobbyOrigin {
        LobbyOrigin::parse(&format!("http://{}", self.addr), HttpPolicy::LoopbackOnly)
            .expect("loopback origin")
    }

    pub fn freeze_existing(&self) {
        for link in self.shared.links.lock().expect("links").iter() {
            link.frozen.store(true, Ordering::SeqCst);
        }
    }

    pub fn freeze_websockets(&self) {
        for link in self.shared.links.lock().expect("links").iter() {
            if link.websocket.load(Ordering::SeqCst) {
                link.frozen.store(true, Ordering::SeqCst);
            }
        }
    }

    pub fn set_blackhole(&self, on: bool) {
        self.shared.blackhole.store(on, Ordering::SeqCst);
        for link in self.shared.links.lock().expect("links").iter() {
            link.frozen.store(on, Ordering::SeqCst);
            if !on {
                link.thawed.notify_waiters();
            }
        }
    }

    /// New connections are accepted and closed at once (`true`), as a stopped server's closed
    /// port answers on Linux; Windows takes about 2 s to refuse a loopback connection, which
    /// would stretch every reconnect attempt of a test. Existing connections are untouched.
    pub fn set_refusing(&self, on: bool) {
        self.shared.refusing.store(on, Ordering::SeqCst);
    }

    pub fn cut_all(&self) {
        for link in self.shared.links.lock().expect("links").drain(..) {
            link.cut.cancel();
        }
    }

    pub fn accepted(&self) -> usize {
        self.shared.accepted.load(Ordering::SeqCst)
    }

    /// From now on, requests whose request line starts with `line` (`"POST /api/v1/auth/refresh"`)
    /// reach the server, which handles them, but their responses are lost on the way back, as
    /// when a client dies between the server's commit and its own write. `None` stops it.
    pub fn drop_responses_to(&self, line: Option<&str>) {
        *self.shared.drop_responses_to.lock().expect("drop rule") = line.map(str::to_owned);
    }

    pub fn dropped(&self) -> usize {
        self.shared.dropped.load(Ordering::SeqCst)
    }

    /// What each connection through the proxy asked for first (`"GET /api/v1/messages?limit=50"`),
    /// oldest first. A kept-alive connection shows only its first request.
    pub fn requests(&self) -> Vec<String> {
        self.shared.requests.lock().expect("requests").clone()
    }
}

async fn proxy(client: TcpStream, upstream: SocketAddr, link: Arc<Link>, shared: Arc<ChaosShared>) {
    let Ok(server) = TcpStream::connect(upstream).await else {
        return;
    };
    let mut drop_response = false;
    if let Ok(line) = request_line(&client).await {
        shared.requests.lock().expect("requests").push(line.clone());
        link.websocket
            .store(line.starts_with("GET /api/v1/ws"), Ordering::SeqCst);
        drop_response = shared
            .drop_responses_to
            .lock()
            .expect("drop rule")
            .as_deref()
            .is_some_and(|prefix| line.starts_with(prefix));
    }
    if drop_response {
        let (mut client_rx, _client_tx) = client.into_split();
        let (mut server_rx, mut server_tx) = server.into_split();
        let mut buf = vec![0u8; 16 * 1024];
        let forwarded = async {
            loop {
                match client_rx.read(&mut buf).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => {
                        if server_tx.write_all(&buf[..n]).await.is_err() {
                            return;
                        }
                    }
                }
            }
        };
        let mut answer = vec![0u8; 16 * 1024];
        tokio::select! {
            () = forwarded => {}
            read = server_rx.read(&mut answer) => {
                if matches!(read, Ok(n) if n > 0) {
                    shared.dropped.fetch_add(1, Ordering::SeqCst);
                }
            }
        }
        return;
    }
    let _ = client.set_nodelay(true);
    let _ = server.set_nodelay(true);
    let (client_rx, client_tx) = client.into_split();
    let (server_rx, server_tx) = server.into_split();
    let cut = link.cut.clone();
    tokio::select! {
        () = cut.cancelled() => {}
        () = pump(client_rx, server_tx, &link) => {}
        () = pump(server_rx, client_tx, &link) => {}
    }
}

async fn pump(
    mut from: tokio::net::tcp::OwnedReadHalf,
    mut to: tokio::net::tcp::OwnedWriteHalf,
    link: &Link,
) {
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        while link.frozen.load(Ordering::SeqCst) {
            let thawed = link.thawed.notified();
            if !link.frozen.load(Ordering::SeqCst) {
                break;
            }
            thawed.await;
        }
        let n = match from.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        while link.frozen.load(Ordering::SeqCst) {
            let thawed = link.thawed.notified();
            if !link.frozen.load(Ordering::SeqCst) {
                break;
            }
            thawed.await;
        }
        if to.write_all(&buf[..n]).await.is_err() {
            return;
        }
    }
}
