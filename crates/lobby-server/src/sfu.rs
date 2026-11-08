//! The SFU: its own OS thread and current-thread runtime, one UDP socket, a TCP listener for
//! clients whose network blocks UDP ([`tcp`]), and one ICE-lite str0m `Rtc` per Member In Voice.
//!
//! The server is the only SDP offerer. Each peer gets a sendrecv `voice-audio` m-line for its
//! microphone, plus pooled downlink m-lines carrying the other peers' voices. A leaving Member's
//! m-line goes inactive and is re-pointed at the next joiner, so the m-line count stays bounded.
//! Changes made while an offer is outstanding become one follow-up offer after its answer.
//!
//! `Lobby → SFU` commands are a bounded queue; `SFU → Lobby` events are synchronous calls through
//! [`EventSink`], so they are never lost.

use std::sync::{
    Arc,
    atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering},
};
#[cfg(any(test, feature = "test-support"))]
use std::{collections::HashMap, sync::Mutex};

use lobby_protocol::{Quality, Rid, RtcTrack, Sdp, SourceSize, UnixMs, UserId, VoiceSessionId};
use tokio::sync::mpsc;

mod bitrate;
mod event_loop;
mod forward;
mod ingress;
mod negotiate;
mod net;
mod peer;
mod restart;
mod speech;
mod tcp;
mod thread;
mod timeline;
pub use net::{bind_tcp, bind_udp};
pub(crate) use thread::SfuThread;

/// `/health` reports the SFU as failing once its loop has been silent this long.
const HEARTBEAT_STALE_MS: i64 = 5_000;
pub const COMMAND_QUEUE: usize = 1024;

/// A command for the peer of session `vsid` of `user`; a stale one is ignored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SfuCmd {
    pub user: UserId,
    pub vsid: VoiceSessionId,
    pub op: SfuOp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SfuOp {
    Join {
        muted: bool,
    },
    Answer {
        rev: u32,
        sdp: Sdp,
        rid: Option<Rid>,
    },
    Leave,
    SetMuted {
        muted: bool,
    },
    StartShare {
        quality: Quality,
    },
    ShareSource {
        size: SourceSize,
    },
    StopShare,
    Watch {
        sharer: UserId,
    },
    Unwatch,
    /// The client lost its media path: the next offer restarts ICE.
    Restart,
}

/// What happened to the peer of session `vsid` of `user`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SfuEvent {
    pub user: UserId,
    pub vsid: VoiceSessionId,
    pub kind: SfuEventKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SfuEventKind {
    Offer {
        rev: u32,
        sdp: Sdp,
        tracks: Vec<RtcTrack>,
    },
    AnswerApplied {
        rid: Option<Rid>,
    },
    NegotiationFailed {
        rid: Option<Rid>,
        fatal: bool,
    },
    Connected,
    /// The Member started or stopped speaking, by the V flag of their voice packets.
    Speaking(bool),
    ShareVideoStarted,
    WatchDropped {
        frames: u32,
        downlink_bps: Option<u64>,
    },
    /// The peer's media connection ended without a `Leave`.
    Lost,
}

/// How a peer's media travels: the transport of the pair its client nominated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaPath {
    Udp,
    Tcp,
}

/// Where SFU events go: `Lobby::on_sfu_event` in the server, a channel in tests.
pub(crate) type EventSink = Box<dyn Fn(SfuEvent) + Send>;

/// What the SFU thread publishes to the rest of the server: the heartbeat, and what tests observe.
struct SfuProbe {
    heartbeat: AtomicI64,
    live: AtomicUsize,
    lost: AtomicUsize,
    restarts_offered: AtomicUsize,
    #[cfg(any(test, feature = "test-support"))]
    paths: Mutex<HashMap<UserId, MediaPath>>,
    /// Bytes of every datagram sent by UDP, to every peer.
    udp_bytes_sent: AtomicU64,
}

#[derive(Clone)]
pub(crate) struct SfuHandle {
    probe: Arc<SfuProbe>,
    cmds: mpsc::Sender<SfuCmd>,
}

impl SfuHandle {
    pub fn is_alive(&self, now: UnixMs) -> bool {
        now.0 - self.probe.heartbeat.load(Ordering::Relaxed) <= HEARTBEAT_STALE_MS
    }

    /// Peers with an `Rtc` still alive, closing ones included (tests check nothing leaks).
    #[cfg(test)]
    pub fn live_peers(&self) -> usize {
        self.probe.live.load(Ordering::Relaxed)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn peers_lost(&self) -> usize {
        self.probe.lost.load(Ordering::Relaxed)
    }

    /// How many offers restarted ICE so far.
    #[cfg(any(test, feature = "test-support"))]
    pub fn ice_restarts(&self) -> usize {
        self.probe.restarts_offered.load(Ordering::Relaxed)
    }

    /// How the SFU sends `user`'s media now, once it has sent any.
    #[cfg(any(test, feature = "test-support"))]
    pub fn media_path(&self, user: &UserId) -> Option<MediaPath> {
        let paths = self.probe.paths.lock().expect("media paths");
        paths.get(user).copied()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn udp_bytes_sent(&self) -> u64 {
        self.probe.udp_bytes_sent.load(Ordering::Relaxed)
    }

    /// Never waits; the command comes back if the queue is full or the SFU stopped.
    pub fn try_send(&self, cmd: SfuCmd) -> Result<(), SfuCmd> {
        self.cmds.try_send(cmd).map_err(|e| match e {
            mpsc::error::TrySendError::Full(cmd) | mpsc::error::TrySendError::Closed(cmd) => cmd,
        })
    }
}

pub(crate) struct SfuInbox {
    probe: Arc<SfuProbe>,
    cmds: mpsc::Receiver<SfuCmd>,
}

#[cfg(test)]
impl SfuInbox {
    pub fn try_recv(&mut self) -> Option<SfuCmd> {
        self.cmds.try_recv().ok()
    }
}

/// The command queue exists before the thread, so the `Lobby` can hold the handle while the
/// SFU holds the `Lobby` (through its [`EventSink`]).
pub(crate) fn channel() -> (SfuHandle, SfuInbox) {
    let probe = Arc::new(SfuProbe {
        heartbeat: AtomicI64::new(UnixMs::now().0),
        live: AtomicUsize::new(0),
        lost: AtomicUsize::new(0),
        restarts_offered: AtomicUsize::new(0),
        #[cfg(any(test, feature = "test-support"))]
        paths: Mutex::default(),
        udp_bytes_sent: AtomicU64::new(0),
    });
    let (tx, rx) = mpsc::channel(COMMAND_QUEUE);
    (
        SfuHandle {
            probe: probe.clone(),
            cmds: tx,
        },
        SfuInbox { probe, cmds: rx },
    )
}

#[cfg(test)]
mod loopback;
