//! The SFU's own OS thread and its current-thread runtime.

use std::{io, net::SocketAddr, panic::AssertUnwindSafe};

use tokio::{
    net::{TcpListener, UdpSocket},
    sync::oneshot,
};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument as _, error, info_span, warn};

use super::{
    EventSink, SfuInbox,
    event_loop::Sfu,
    net::set_socket_buffers,
    tcp::{TcpLimits, TcpStreams},
};
use crate::config::SfuConfig;

pub(crate) struct SfuThread {
    local_addr: SocketAddr,
    tcp_addr: SocketAddr,
    done: Option<oneshot::Receiver<()>>,
}

impl SfuThread {
    /// Starts `lobby-sfu` and waits until its runtime owns the sockets. `stop` ends the loop;
    /// `fatal` is cancelled if the thread fails or panics.
    pub async fn spawn(
        udp: std::net::UdpSocket,
        tcp: std::net::TcpListener,
        cfg: SfuConfig,
        inbox: SfuInbox,
        sink: EventSink,
        stop: CancellationToken,
        fatal: CancellationToken,
    ) -> io::Result<Self> {
        udp.set_nonblocking(true)?;
        tcp.set_nonblocking(true)?;
        set_socket_buffers(&udp);
        let local_addr = udp.local_addr()?;
        let tcp_addr = tcp.local_addr()?;
        if local_addr.ip().is_unspecified()
            && socket2::SockRef::from(&udp).only_v6().unwrap_or(false)
        {
            warn!(%local_addr, "the SFU socket is IPv6-only: IPv4 clients cannot reach it");
        }
        let (ready_tx, ready_rx) = oneshot::channel::<io::Result<()>>();
        let (done_tx, done_rx) = oneshot::channel();
        let span = info_span!("sfu", addr = %local_addr);
        std::thread::Builder::new()
            .name("lobby-sfu".into())
            .spawn(move || {
                let _entered = span.enter();
                let run =
                    AssertUnwindSafe(|| run_thread(udp, tcp, cfg, inbox, sink, ready_tx, stop));
                match std::panic::catch_unwind(run) {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        error!(error = %e, "sfu thread failed");
                        fatal.cancel();
                    }
                    Err(_) => {
                        error!("sfu thread panicked");
                        fatal.cancel();
                    }
                }
                let _ = done_tx.send(());
            })?;
        match ready_rx.await {
            Ok(Ok(())) => Ok(Self {
                local_addr,
                tcp_addr,
                done: Some(done_rx),
            }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(io::Error::other("sfu thread exited during startup")),
        }
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn tcp_addr(&self) -> SocketAddr {
        self.tcp_addr
    }

    pub async fn join(&mut self) {
        if let Some(done) = self.done.take() {
            let _ = done.await;
        }
    }
}

fn run_thread(
    udp: std::net::UdpSocket,
    tcp: std::net::TcpListener,
    cfg: SfuConfig,
    inbox: SfuInbox,
    sink: EventSink,
    ready: oneshot::Sender<io::Result<()>>,
    stop: CancellationToken,
) -> io::Result<()> {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            let _ = ready.send(Err(io::Error::new(e.kind(), e.to_string())));
            return Err(e);
        }
    };
    rt.block_on(
        async move {
            let sockets = UdpSocket::from_std(udp).and_then(|udp| {
                let tcp = TcpListener::from_std(tcp)?;
                Ok((udp.local_addr()?, udp, tcp.local_addr()?, tcp))
            });
            let (local, socket, tcp_local, listener) = match sockets {
                Ok(s) => s,
                Err(e) => {
                    let _ = ready.send(Err(io::Error::new(e.kind(), e.to_string())));
                    return Err(e);
                }
            };
            let _ = ready.send(Ok(()));
            let tcp = TcpStreams::spawn(listener, TcpLimits::DEFAULT, stop.child_token());
            let tcp_port = cfg.tcp_advertise_port.unwrap_or(tcp_local.port());
            Sfu::new(socket, local, tcp, tcp_port, cfg, inbox, sink)
                .run(stop)
                .await;
            Ok(())
        }
        .in_current_span(),
    )
}
