//! ICE-TCP, for clients whose network blocks UDP: the SFU's passive listener and one stream per
//! client connection, each carrying STUN, DTLS, RTP and RTCP framed per RFC 4571 (a 2-byte
//! big-endian length before every packet). It all runs on the SFU's runtime. The SFU loop reads
//! every stream's frames from one queue and writes through a bounded queue per stream, so a slow
//! client never blocks it: a full queue drops the packet, as a full UDP send buffer would.

use std::{
    collections::HashMap,
    io,
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _, BufReader},
    net::{
        TcpListener,
        tcp::{OwnedReadHalf, OwnedWriteHalf},
    },
    sync::{OwnedSemaphorePermit, Semaphore, mpsc},
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};
use tracing::{debug, warn};

use super::net::{RECV_BUFFER, canonical};

/// Frames waiting for one client's stream: a keyframe burst is a few hundred packets.
const SEND_QUEUE: usize = 512;
/// Frames from every stream waiting for the SFU loop; a full queue holds back the readers.
const INBOUND_QUEUE: usize = 1024;
/// A failing `accept` (out of file descriptors) pauses the loop, so it cannot spin.
const ACCEPT_ERROR_PAUSE: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy)]
pub(super) struct TcpLimits {
    pub max_streams: usize,
    /// A longer frame closes its stream.
    pub max_frame: usize,
    /// A stream that carried nothing a peer accepted for this long is closed: a port scanner, a
    /// client that left, or one whose path is dead. A live client checks every pair it has
    /// at least every 3 s, the nominated one included.
    pub idle_after: Duration,
}

impl TcpLimits {
    pub const DEFAULT: TcpLimits = TcpLimits {
        max_streams: 128,
        max_frame: RECV_BUFFER,
        idle_after: Duration::from_secs(5),
    };
}

/// One packet from a client's stream.
#[derive(Debug)]
pub(super) struct Frame {
    pub remote: SocketAddr,
    pub data: Vec<u8>,
}

enum Inbound {
    Opened {
        id: u64,
        remote: SocketAddr,
        writer: mpsc::Sender<Vec<u8>>,
        stop: CancellationToken,
    },
    Frame(Frame),
    Closed {
        id: u64,
        remote: SocketAddr,
    },
}

struct Stream {
    id: u64,
    writer: mpsc::Sender<Vec<u8>>,
    stop: CancellationToken,
    last_accepted: Instant,
}

/// The SFU loop's side of every TCP stream, keyed by the client's address.
pub(super) struct TcpStreams {
    inbound: mpsc::Receiver<Inbound>,
    streams: HashMap<SocketAddr, Stream>,
    idle_after: Duration,
    tracker: TaskTracker,
    stop: CancellationToken,
}

impl TcpStreams {
    /// Starts accepting on the current runtime; `stop` ends the listener and every stream.
    pub fn spawn(listener: TcpListener, limits: TcpLimits, stop: CancellationToken) -> Self {
        let (tx, inbound) = mpsc::channel(INBOUND_QUEUE);
        let tracker = TaskTracker::new();
        tracker.spawn(accept(listener, limits, tx, tracker.clone(), stop.clone()));
        Self {
            inbound,
            streams: HashMap::new(),
            idle_after: limits.idle_after,
            tracker,
            stop,
        }
    }

    /// The next frame from any stream. Cancel-safe.
    pub async fn next_frame(&mut self) -> Option<Frame> {
        loop {
            match self.inbound.recv().await? {
                Inbound::Opened {
                    id,
                    remote,
                    writer,
                    stop,
                } => {
                    debug!(%remote, "tcp stream opened");
                    let stream = Stream {
                        id,
                        writer,
                        stop,
                        last_accepted: Instant::now(),
                    };
                    if let Some(old) = self.streams.insert(remote, stream) {
                        old.stop.cancel();
                    }
                }
                Inbound::Frame(frame) => return Some(frame),
                Inbound::Closed { id, remote } => {
                    if self.streams.get(&remote).is_some_and(|s| s.id == id) {
                        self.streams.remove(&remote);
                    }
                }
            }
        }
    }

    /// Queues `data` on the stream from `to`; `false` if there is none or its queue is full.
    pub fn send(&self, to: SocketAddr, data: &[u8]) -> bool {
        let (Some(stream), Ok(len)) = (self.streams.get(&to), u16::try_from(data.len())) else {
            return false;
        };
        let mut frame = Vec::with_capacity(2 + data.len());
        frame.extend_from_slice(&len.to_be_bytes());
        frame.extend_from_slice(data);
        stream.writer.try_send(frame).is_ok()
    }

    /// A peer accepted a frame from `from`: the stream is in use.
    pub fn accepted(&mut self, from: SocketAddr, now: Instant) {
        if let Some(s) = self.streams.get_mut(&from) {
            s.last_accepted = now;
        }
    }

    /// Closes every stream idle for [`TcpLimits::idle_after`] at `now`.
    pub fn expire(&mut self, now: Instant) {
        let idle_after = self.idle_after;
        self.streams.retain(|remote, s| {
            let idle = now.saturating_duration_since(s.last_accepted) >= idle_after;
            if idle {
                debug!(%remote, "tcp stream idle");
                s.stop.cancel();
            }
            !idle
        });
    }

    pub async fn shutdown(self) {
        self.stop.cancel();
        // A reader waiting on a full inbound queue gives up once the queue is gone.
        drop(self.inbound);
        self.tracker.close();
        self.tracker.wait().await;
    }
}

async fn accept(
    listener: TcpListener,
    limits: TcpLimits,
    inbound: mpsc::Sender<Inbound>,
    tracker: TaskTracker,
    stop: CancellationToken,
) {
    let permits = Arc::new(Semaphore::new(limits.max_streams));
    let mut next_id = 0u64;
    loop {
        let accepted = tokio::select! {
            () = stop.cancelled() => return,
            r = listener.accept() => r,
        };
        let (stream, remote) = match accepted {
            Ok(v) => v,
            Err(e) => {
                warn!(error = %e, "tcp accept failed");
                tokio::select! {
                    () = stop.cancelled() => return,
                    () = tokio::time::sleep(ACCEPT_ERROR_PAUSE) => continue,
                }
            }
        };
        let remote = canonical(remote);
        let Ok(permit) = permits.clone().try_acquire_owned() else {
            debug!(%remote, max = limits.max_streams, "tcp stream refused: too many");
            continue;
        };
        if let Err(e) = stream.set_nodelay(true) {
            debug!(%remote, error = %e, "tcp nodelay not set");
        }
        next_id += 1;
        let id = next_id;
        let (read, write) = stream.into_split();
        let (writer, queue) = mpsc::channel(SEND_QUEUE);
        let stream_stop = stop.child_token();
        let opened = Inbound::Opened {
            id,
            remote,
            writer,
            stop: stream_stop.clone(),
        };
        tokio::select! {
            () = stop.cancelled() => return,
            sent = inbound.send(opened) => if sent.is_err() { return },
        }
        tracker.spawn(write_frames(write, queue, stream_stop.clone()));
        tracker.spawn(read_frames(
            read,
            id,
            remote,
            limits.max_frame,
            inbound.clone(),
            stream_stop,
            permit,
        ));
    }
}

async fn read_frames(
    read: OwnedReadHalf,
    id: u64,
    remote: SocketAddr,
    max_frame: usize,
    inbound: mpsc::Sender<Inbound>,
    stop: CancellationToken,
    _permit: OwnedSemaphorePermit,
) {
    let mut read = BufReader::new(read);
    let ended = tokio::select! {
        () = stop.cancelled() => Ok(()),
        r = forward_frames(&mut read, remote, max_frame, &inbound) => r,
    };
    match ended {
        Ok(()) => debug!(%remote, "tcp stream closed"),
        Err(e) => debug!(%remote, error = %e, "tcp stream ended"),
    }
    stop.cancel();
    // A full queue loses this; the stream then goes when it is idle.
    let _ = inbound.try_send(Inbound::Closed { id, remote });
}

/// Until the client closes the stream, it breaks the framing, or the SFU stops.
async fn forward_frames(
    read: &mut BufReader<OwnedReadHalf>,
    remote: SocketAddr,
    max_frame: usize,
    inbound: &mpsc::Sender<Inbound>,
) -> io::Result<()> {
    loop {
        let len = match read.read_u16().await {
            Ok(len) => usize::from(len),
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e),
        };
        if len > max_frame {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("a {len}-byte frame is longer than {max_frame}"),
            ));
        }
        let mut data = vec![0; len];
        read.read_exact(&mut data).await?;
        if data.is_empty() {
            continue;
        }
        if inbound
            .send(Inbound::Frame(Frame { remote, data }))
            .await
            .is_err()
        {
            return Ok(());
        }
    }
}

/// Dropping the write half at the end sends the client a FIN.
async fn write_frames(
    mut write: OwnedWriteHalf,
    mut queue: mpsc::Receiver<Vec<u8>>,
    stop: CancellationToken,
) {
    loop {
        let frame = tokio::select! {
            () = stop.cancelled() => break,
            f = queue.recv() => match f {
                Some(f) => f,
                None => break,
            },
        };
        let written = tokio::select! {
            () = stop.cancelled() => break,
            r = write.write_all(&frame) => r,
        };
        if written.is_err() {
            break;
        }
    }
    stop.cancel();
}
