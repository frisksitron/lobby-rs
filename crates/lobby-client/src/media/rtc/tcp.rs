//! The client's side of ICE-TCP, for networks that block UDP. Our one active TCP candidate lets
//! str0m check the SFU's passive TCP candidates; the first packet for an SFU address connects a
//! stream to it, framed per RFC 4571 (a 2-byte big-endian length before every packet), and later
//! packets for that address share it. A stream that fails or closes is forgotten, and the next
//! packet connects again. ICE prefers UDP pairs, so where UDP works TCP carries only checks.

use std::{
    collections::HashMap,
    io,
    net::{IpAddr, SocketAddr},
    time::Duration,
};

use str0m::{Candidate, net::TcpType};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _, BufReader},
    net::{
        TcpSocket,
        tcp::{OwnedReadHalf, OwnedWriteHalf},
    },
    sync::mpsc,
};
use tokio_util::task::AbortOnDropHandle;
use tracing::debug;

/// RFC 6544: an active candidate has no listening port, and says 9.
const ACTIVE_PORT: u16 = 9;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Packets waiting for one stream: a keyframe burst is a few hundred.
const SEND_QUEUE: usize = 512;
/// Packets from every stream waiting for the transport loop.
const INBOUND_QUEUE: usize = 1024;
/// Larger than any packet str0m sends; a longer frame is a broken stream.
const MAX_FRAME: usize = 2048;

/// Where str0m's TCP packets come from: our active candidate on the routed address.
pub(super) fn local(ip: IpAddr) -> SocketAddr {
    SocketAddr::new(ip, ACTIVE_PORT)
}

pub(super) fn active_candidate(ip: IpAddr) -> Result<Candidate, String> {
    Candidate::builder()
        .tcp()
        .host(local(ip))
        .tcptype(TcpType::Active)
        .build()
        .map_err(|e| e.to_string())
}

pub(crate) enum TcpEvent {
    Frame { remote: SocketAddr, data: Vec<u8> },
    Closed { remote: SocketAddr, id: u64 },
}

pub(super) fn channel() -> (mpsc::Sender<TcpEvent>, mpsc::Receiver<TcpEvent>) {
    mpsc::channel(INBOUND_QUEUE)
}

struct Link {
    id: u64,
    queue: mpsc::Sender<Vec<u8>>,
    _task: AbortOnDropHandle<()>,
}

/// One stream per SFU address. Dropping it closes them all.
pub(super) struct TcpLinks {
    links: HashMap<SocketAddr, Link>,
    events: mpsc::Sender<TcpEvent>,
    next_id: u64,
}

impl TcpLinks {
    pub fn new(events: mpsc::Sender<TcpEvent>) -> Self {
        Self {
            links: HashMap::new(),
            events,
            next_id: 0,
        }
    }

    /// Queues one packet for `to`, connecting from `from` first if no stream is there; `false`
    /// if it was dropped (a full queue).
    pub fn send(&mut self, from: IpAddr, to: SocketAddr, data: &[u8]) -> bool {
        let Ok(len) = u16::try_from(data.len()) else {
            return false;
        };
        let link = self.links.entry(to).or_insert_with(|| {
            self.next_id += 1;
            let (queue, rx) = mpsc::channel(SEND_QUEUE);
            let task = tokio::spawn(run_link(from, to, self.next_id, rx, self.events.clone()));
            Link {
                id: self.next_id,
                queue,
                _task: AbortOnDropHandle::new(task),
            }
        });
        let mut frame = Vec::with_capacity(2 + data.len());
        frame.extend_from_slice(&len.to_be_bytes());
        frame.extend_from_slice(data);
        link.queue.try_send(frame).is_ok()
    }

    pub fn closed(&mut self, remote: SocketAddr, id: u64) {
        if self.links.get(&remote).is_some_and(|l| l.id == id) {
            self.links.remove(&remote);
        }
    }

    pub fn clear(&mut self) {
        self.links.clear();
    }
}

async fn run_link(
    from: IpAddr,
    to: SocketAddr,
    id: u64,
    mut queue: mpsc::Receiver<Vec<u8>>,
    events: mpsc::Sender<TcpEvent>,
) {
    let ended = match connect(from, to).await {
        Ok((read, write)) => {
            debug!(%to, "voice tcp stream connected");
            tokio::select! {
                r = read_frames(read, to, &events) => r,
                r = write_frames(write, &mut queue) => r,
            }
        }
        Err(e) => Err(e),
    };
    match ended {
        Ok(()) => debug!(%to, "voice tcp stream closed"),
        Err(e) => debug!(%to, error = %e, "voice tcp stream failed"),
    }
    let _ = events.send(TcpEvent::Closed { remote: to, id }).await;
}

/// From `from`, the address our candidate and the UDP socket are on.
async fn connect(from: IpAddr, to: SocketAddr) -> io::Result<(OwnedReadHalf, OwnedWriteHalf)> {
    let socket = match to {
        SocketAddr::V4(_) => TcpSocket::new_v4()?,
        SocketAddr::V6(_) => TcpSocket::new_v6()?,
    };
    socket.bind(SocketAddr::new(from, 0))?;
    let stream = tokio::time::timeout(CONNECT_TIMEOUT, socket.connect(to))
        .await
        .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))??;
    stream.set_nodelay(true)?;
    Ok(stream.into_split())
}

async fn read_frames(
    read: OwnedReadHalf,
    remote: SocketAddr,
    events: &mpsc::Sender<TcpEvent>,
) -> io::Result<()> {
    let mut read = BufReader::new(read);
    loop {
        let len = match read.read_u16().await {
            Ok(len) => usize::from(len),
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e),
        };
        if len > MAX_FRAME {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("a {len}-byte frame"),
            ));
        }
        let mut data = vec![0; len];
        read.read_exact(&mut data).await?;
        if data.is_empty() {
            continue;
        }
        if events.send(TcpEvent::Frame { remote, data }).await.is_err() {
            return Ok(());
        }
    }
}

async fn write_frames(
    mut write: OwnedWriteHalf,
    queue: &mut mpsc::Receiver<Vec<u8>>,
) -> io::Result<()> {
    while let Some(frame) = queue.recv().await {
        write.write_all(&frame).await?;
    }
    Ok(())
}
