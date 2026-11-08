//! A TCP listener whose connections close the way HTTP servers are meant to: send FIN, then keep
//! reading for a moment. Closing a socket that still holds unread request bytes makes the OS send
//! an RST, which can discard the response the client has not read yet. An early answer (a 413 or
//! a 401 to a large upload) would then reach the client as a reset connection.

use std::{
    io,
    net::SocketAddr,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use axum::{
    extract::connect_info::Connected,
    serve::{IncomingStream, Listener},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _, ReadBuf},
    net::{TcpListener, TcpStream},
    time::{sleep, timeout},
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};
use tracing::debug;

/// How long a closed connection keeps discarding what the peer still sends.
const LINGER_TIMEOUT: Duration = Duration::from_secs(2);
/// How much it discards before giving up and letting the OS reset.
const LINGER_MAX_BYTES: usize = 16 * 1024 * 1024;
const ACCEPT_RETRY: Duration = Duration::from_secs(1);

pub struct LingerListener {
    inner: TcpListener,
    tracker: TaskTracker,
    token: CancellationToken,
}

impl LingerListener {
    pub fn new(inner: TcpListener, tracker: TaskTracker, token: CancellationToken) -> Self {
        Self {
            inner,
            tracker,
            token,
        }
    }
}

impl Listener for LingerListener {
    type Io = LingerStream;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            match self.inner.accept().await {
                Ok((io, addr)) => {
                    // Without it Nagle holds the second of two small frames written back to back
                    // (a broadcast, then the ACK) until the peer's delayed ACK, about 40 ms.
                    if let Err(e) = io.set_nodelay(true) {
                        debug!(%addr, error = %e, "tcp nodelay not set");
                    }
                    let stream = LingerStream {
                        io: Some(io),
                        tracker: self.tracker.clone(),
                        token: self.token.clone(),
                    };
                    return (stream, addr);
                }
                // A connection that died while waiting is not the listener's problem; anything
                // else (descriptor exhaustion) is given a moment to clear.
                Err(e) if is_connection_error(&e) => {}
                Err(_) => sleep(ACCEPT_RETRY).await,
            }
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.inner.local_addr()
    }
}

fn is_connection_error(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
    )
}

#[derive(Debug, Clone, Copy)]
pub struct Peer(pub SocketAddr);

impl Connected<IncomingStream<'_, LingerListener>> for Peer {
    fn connect_info(stream: IncomingStream<'_, LingerListener>) -> Self {
        Self(*stream.remote_addr())
    }
}

pub struct LingerStream {
    io: Option<TcpStream>,
    tracker: TaskTracker,
    token: CancellationToken,
}

impl LingerStream {
    fn io(&mut self) -> Pin<&mut TcpStream> {
        Pin::new(self.io.as_mut().expect("the stream lives until drop"))
    }
}

impl Drop for LingerStream {
    fn drop(&mut self) {
        let Some(io) = self.io.take() else { return };
        if self.token.is_cancelled() || tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let token = self.token.clone();
        self.tracker.spawn(async move {
            tokio::select! {
                () = token.cancelled() => {}
                () = linger(io) => {}
            }
        });
    }
}

async fn linger(mut io: TcpStream) {
    let _ = io.shutdown().await;
    let mut buf = [0u8; 16 * 1024];
    let mut left = LINGER_MAX_BYTES;
    let _ = timeout(LINGER_TIMEOUT, async {
        while left > 0 {
            match io.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => left = left.saturating_sub(n),
            }
        }
    })
    .await;
}

impl AsyncRead for LingerStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.io().poll_read(cx, buf)
    }
}

impl AsyncWrite for LingerStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.io().poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.io().poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.io().poll_shutdown(cx)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        self.io().poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.io.as_ref().is_some_and(AsyncWrite::is_write_vectored)
    }
}
