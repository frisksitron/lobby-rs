//! The `lobby-rtc` thread: a current-thread Tokio runtime owning the client
//! str0m `Rtc` and its one UDP socket. It answers the server's offers, routes inbound Opus by
//! mid through the offer's `tracks` table to a playout slot, and sends the encoded microphone
//! frames on the Member's own m-line.
//!
//! Media goes by UDP, or by TCP where the network blocks UDP ([`tcp`]): ICE picks the pair.
//!
//! Transport errors are classified instead of propagated: an unparseable datagram, a
//! `ConnectionReset` (an ICMP port-unreachable after an SFU restart) or a transient send or
//! receive error is counted and ignored. Only an `RtcError` from str0m ends Voice.
//!
//! ICE tells the Session when the path to the SFU goes down and comes back
//! ([`VoiceEvent::Path`]), and an OS network change is reported with whether it changed the
//! route to the SFU ([`VoiceEvent::NetworkChanged`]); the Session decides on restarts.

mod stats;
mod tcp;
mod transport;
mod video;
mod voice;

use std::{
    collections::HashMap,
    io,
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use lobby_protocol::{RtcTrack, Sdp, UserId, VoiceSessionId, encoder::bps_for};
use str0m::{
    Event, IceConnectionState, Input, Output, Rtc,
    bwe::BweKind,
    change::SdpOffer,
    format::Codec,
    media::{Pt, Writer},
    net::{Protocol, Receive},
};
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument as _, debug, info, info_span, trace, warn};

use stats::{Mids, StatsTally};
use tcp::{TcpEvent, TcpLinks};
pub use transport::RouteProbe;
use transport::{Sent, Transport};
use video::VideoPath;
use voice::VoiceRoutes;

pub(crate) use video::{EncodedVideo, KeyframeRequests};

use super::{
    MediaError, PathState, StunTimings, VoiceEvent, VoiceEvents, VoiceStats,
    opus_codec::MAX_PACKET, playout::PlayoutFeed, watch::WatchInput,
};
use crate::Volume;

/// Larger than any datagram str0m accepts.
const RECV_BUFFER: usize = 2048;
pub(super) const STATS_EVERY: Duration = Duration::from_secs(1);
/// How often the playout's speaking flags are read for the Session: one frame.
const SPEAKERS_EVERY: Duration = Duration::from_millis(20);
/// A recurring local socket error is logged at most this often, and pauses the loop briefly.
const ERROR_LOG_EVERY: Duration = Duration::from_secs(10);
const ERROR_PAUSE: Duration = Duration::from_millis(10);

pub(crate) enum RtcCmd {
    Offer {
        vsid: VoiceSessionId,
        rev: u32,
        sdp: Sdp,
        tracks: Vec<RtcTrack>,
    },
    #[cfg_attr(
        not(windows),
        expect(dead_code, reason = "only Windows starts a share")
    )]
    Share {
        frames: mpsc::Receiver<EncodedVideo>,
        keyframes: Arc<KeyframeRequests>,
    },
    #[cfg_attr(
        not(windows),
        expect(dead_code, reason = "only Windows starts a share")
    )]
    StopShare,
    Watch(WatchInput),
    Unwatch,
    /// How loud `user` plays from now on, wherever it is bound and whenever it next binds.
    Volume {
        user: UserId,
        volume: Volume,
    },
    /// The OS reported an interface or address added or removed.
    NetworkChanged,
}

/// Fixed size, so `lobby-voice-tx` hands it over without allocating.
pub(crate) struct EncodedFrame {
    pub ts: u64,
    /// The frame's RFC 6464 level (dB below full scale) and V flag: the VAD's verdict.
    pub dbov: u8,
    pub voiced: bool,
    pub len: u16,
    pub data: [u8; MAX_PACKET],
}

impl EncodedFrame {
    pub fn payload(&self) -> &[u8] {
        &self.data[..usize::from(self.len)]
    }
}

/// What the transport shows outside its thread; tests read it.
#[derive(Debug, Default)]
pub(crate) struct TransportView {
    pub local: Mutex<Option<SocketAddr>>,
    pub stale_source: AtomicU64,
    pub video_frames: AtomicU64,
    pub video_bytes: AtomicU64,
    #[cfg(feature = "test-support")]
    pub watched_profile_level: std::sync::atomic::AtomicU32,
    pub udp_bytes_sent: AtomicU64,
}

impl TransportView {
    fn set_local(&self, local: SocketAddr) {
        *self.local.lock().expect("transport view lock") = Some(local);
    }
}

#[derive(Debug, Default)]
struct Counters {
    ignored_datagrams: u64,
    socket_errors: u64,
}

pub(crate) struct RtcTask {
    me: UserId,
    cmds: mpsc::Receiver<RtcCmd>,
    events: VoiceEvents,
    cancel: CancellationToken,
    view: Arc<TransportView>,
    stun: StunTimings,
    route: RouteProbe,
    transport: Option<Transport>,
    tcp_events: mpsc::Receiver<TcpEvent>,
    tcp_events_tx: mpsc::Sender<TcpEvent>,
    connected: bool,
    /// The latest ICE path state the Session has not taken yet, so a full queue delays it but
    /// never loses it.
    path_unsent: Option<PathState>,
    counters: Counters,
    last_error_log: Option<Instant>,
    tally: StatsTally,
    voice: VoiceRoutes,
    video: VideoPath,
}

pub(crate) fn spawn(task: RtcTask) -> io::Result<std::thread::JoinHandle<()>> {
    let span = info_span!("rtc");
    std::thread::Builder::new()
        .name("lobby-rtc".into())
        .spawn(move || {
            let _entered = span.enter();
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    task.events
                        .blocking_send(VoiceEvent::Failed(MediaError::Transport(e.to_string())));
                    return;
                }
            };
            rt.block_on(task.run().in_current_span());
        })
}

impl RtcTask {
    #[expect(
        clippy::too_many_arguments,
        reason = "the transport thread's inputs, each made by `VoiceHandle::start`"
    )]
    pub fn new(
        me: UserId,
        cmds: mpsc::Receiver<RtcCmd>,
        frames: rtrb::Consumer<EncodedFrame>,
        frames_ready: Arc<Notify>,
        feed: PlayoutFeed,
        events: VoiceEvents,
        cancel: CancellationToken,
        view: Arc<TransportView>,
        stun: StunTimings,
        route: RouteProbe,
        volumes: HashMap<UserId, Volume>,
    ) -> Self {
        let (tcp_events_tx, tcp_events) = tcp::channel();
        Self {
            me,
            cmds,
            events,
            cancel,
            view,
            stun,
            route,
            transport: None,
            tcp_events,
            tcp_events_tx,
            connected: false,
            path_unsent: None,
            counters: Counters::default(),
            last_error_log: None,
            tally: StatsTally::default(),
            voice: VoiceRoutes::new(frames, frames_ready, feed, volumes),
            video: VideoPath::default(),
        }
    }

    async fn run(mut self) {
        let mut buf = vec![0u8; RECV_BUFFER];
        let mut stats = tokio::time::interval(STATS_EVERY);
        stats.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut speakers = tokio::time::interval(SPEAKERS_EVERY);
        speakers.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let ended = loop {
            let next = match self.drain() {
                Ok(next) => next,
                Err(e) => break Some(e),
            };
            if let Some(path) = self.path_unsent
                && self.events.try_send(VoiceEvent::Path(path))
            {
                self.path_unsent = None;
            }
            // Windows timers round an elapsed deadline up to the next tick: handle it now.
            if let (Some(t), Some(tr)) = (next, self.transport.as_mut())
                && t <= Instant::now()
            {
                if let Err(e) = tr.rtc.handle_input(Input::Timeout(Instant::now())) {
                    break Some(MediaError::Transport(e.to_string()));
                }
                continue;
            }
            let sleep = async {
                match next {
                    Some(t) => tokio::time::sleep_until(t.into()).await,
                    None => std::future::pending().await,
                }
            };
            let recv = async {
                match &self.transport {
                    Some(t) => t.socket.recv_from(&mut buf).await,
                    None => std::future::pending().await,
                }
            };
            let video = async {
                match &mut self.video.sharing {
                    Some(s) => s.frames.recv().await,
                    None => std::future::pending().await,
                }
            };
            let step = tokio::select! {
                biased;
                () = self.cancel.cancelled() => break None,
                Some(cmd) = self.cmds.recv() => self.on_cmd(cmd).await,
                () = self.voice.frames_ready.notified() => {
                    let rtc = connected_rtc(&mut self.transport, self.connected);
                    self.voice.send_frames(rtc);
                    Ok(())
                }
                frame = video => {
                    match frame {
                        Some(frame) => {
                            let rtc = connected_rtc(&mut self.transport, self.connected);
                            self.video.send(frame, rtc, &self.view);
                            self.desire_bitrate();
                        }
                        None => {
                            self.video.sharing = None;
                            self.desire_bitrate();
                        }
                    }
                    Ok(())
                }
                r = recv => self.on_recv(r, &buf).await,
                Some(ev) = self.tcp_events.recv() => self.on_tcp(ev),
                () = sleep => self.timeout(),
                _ = speakers.tick() => {
                    self.voice.tell_speakers(&self.events);
                    Ok(())
                }
                _ = stats.tick() => {
                    self.log_stats();
                    if let Some(stats) = self.snapshot() {
                        // A full queue drops it: the next one is a second away.
                        let _ = self.events.try_send(VoiceEvent::Stats(stats));
                    }
                    Ok(())
                }
            };
            if let Err(e) = step {
                break Some(e);
            }
        };
        if let Some(e) = ended {
            warn!(error = %e, "voice transport failed");
            self.events.send(VoiceEvent::Failed(e)).await;
        }
        // No DTLS close_notify: it would beat the VOICE_LEAVE on the WebSocket and the SFU would
        // take the leave for a lost connection. The server closes the peer on the leave, or when
        // the WebSocket or the media goes silent.
        info!(
            sent = self.voice.counters.sent,
            unsent = self.voice.counters.unsent,
            overflow = self.voice.counters.overflow,
            ignored_datagrams = self.counters.ignored_datagrams,
            socket_errors = self.counters.socket_errors,
            stale_source = self.view.stale_source.load(Ordering::Relaxed),
            "voice transport stopped"
        );
    }

    fn drain(&mut self) -> Result<Option<Instant>, MediaError> {
        loop {
            let Some(t) = self.transport.as_mut() else {
                return Ok(None);
            };
            if !t.rtc.is_alive() {
                return Err(MediaError::Transport("the media connection closed".into()));
            }
            match t.rtc.poll_output() {
                Ok(Output::Timeout(at)) => return Ok(Some(at)),
                // Never waits; a refused send is a lost datagram that RTCP and ICE recover.
                Ok(Output::Transmit(out)) => match t.send(&out) {
                    Sent::Yes => {
                        if out.proto == Protocol::Udp {
                            self.view
                                .udp_bytes_sent
                                .fetch_add(out.contents.len() as u64, Ordering::Relaxed);
                        }
                    }
                    Sent::Stale => {
                        self.view.stale_source.fetch_add(1, Ordering::Relaxed);
                    }
                    Sent::Failed(e) => {
                        self.counters.socket_errors += 1;
                        self.log_socket_error(&e, "send");
                    }
                },
                Ok(Output::Event(ev)) => self.on_event(ev),
                Err(e) => return Err(MediaError::Transport(e.to_string())),
            }
        }
    }

    fn on_event(&mut self, ev: Event) {
        match ev {
            Event::Connected => {
                info!("voice media connected");
                self.connected = true;
            }
            Event::IceConnectionStateChange(state) => {
                debug!(?state, "ice");
                // A full agent reports Completed once no other pair is left to check.
                if state == IceConnectionState::Disconnected {
                    info!("voice media path down");
                    self.path_unsent = Some(PathState::Down);
                } else if state.is_connected() {
                    self.path_unsent = Some(PathState::Up);
                }
            }
            Event::MediaData(data) if self.video.watches(data.mid) => {
                let rtc = self.transport.as_mut().map(|t| &mut t.rtc);
                self.video.on_media(data, rtc, &self.view);
            }
            Event::MediaData(data) => self.voice.on_media(&data),
            Event::PeerStats(s) => self.tally.peer(s),
            Event::MediaIngressStats(s) => self.tally.ingress(&s),
            Event::MediaEgressStats(s) => self.tally.egress(&s),
            Event::KeyframeRequest(req) => self.video.on_keyframe_request(req.mid),
            Event::EgressBitrateEstimate(BweKind::Twcc { estimate, .. }) => {
                trace!(%estimate, "uplink estimate");
                self.tally.uplink_estimate(estimate.as_u64());
            }
            _ => {}
        }
    }

    fn timeout(&mut self) -> Result<(), MediaError> {
        if let Some(t) = self.transport.as_mut() {
            t.rtc
                .handle_input(Input::Timeout(Instant::now()))
                .map_err(|e| MediaError::Transport(e.to_string()))?;
        }
        Ok(())
    }

    async fn on_recv(
        &mut self,
        r: io::Result<(usize, SocketAddr)>,
        buf: &[u8],
    ) -> Result<(), MediaError> {
        match classify(r) {
            Received::Datagram(n, source) => self.on_datagram(Protocol::Udp, &buf[..n], source),
            Received::Ignored => {
                self.counters.ignored_datagrams += 1;
                Ok(())
            }
            Received::Error(e) => {
                self.counters.socket_errors += 1;
                self.log_socket_error(&e, "recv");
                tokio::time::sleep(ERROR_PAUSE).await;
                Ok(())
            }
        }
    }

    fn on_tcp(&mut self, ev: TcpEvent) -> Result<(), MediaError> {
        match ev {
            TcpEvent::Frame { remote, data } => self.on_datagram(Protocol::Tcp, &data, remote),
            TcpEvent::Closed { remote, id } => {
                if let Some(t) = &mut self.transport {
                    t.tcp.closed(remote, id);
                }
                Ok(())
            }
        }
    }

    fn on_datagram(
        &mut self,
        proto: Protocol,
        data: &[u8],
        source: SocketAddr,
    ) -> Result<(), MediaError> {
        let Some(t) = self.transport.as_mut() else {
            return Ok(());
        };
        let Ok(contents) = data.try_into() else {
            // Not STUN, DTLS, RTP or RTCP: somebody's junk.
            self.counters.ignored_datagrams += 1;
            return Ok(());
        };
        let input = Input::Receive(
            Instant::now(),
            Receive {
                proto,
                source,
                destination: t.local_for(proto),
                contents,
            },
        );
        if !t.rtc.accepts(&input) {
            self.counters.ignored_datagrams += 1;
            return Ok(());
        }
        t.rtc
            .handle_input(input)
            .map_err(|e| MediaError::Transport(e.to_string()))
    }

    fn log_socket_error(&mut self, e: &io::Error, what: &str) {
        if self
            .last_error_log
            .is_none_or(|t| t.elapsed() >= ERROR_LOG_EVERY)
        {
            debug!(error = %e, what, count = self.counters.socket_errors, "voice socket error");
            self.last_error_log = Some(Instant::now());
        }
    }

    async fn on_cmd(&mut self, cmd: RtcCmd) -> Result<(), MediaError> {
        match cmd {
            RtcCmd::Offer {
                vsid,
                rev,
                sdp,
                tracks,
            } => {
                let new_socket = match &mut self.transport {
                    Some(t) => {
                        let moved = t.reroute(&sdp);
                        if moved {
                            self.view.set_local(t.local);
                        }
                        moved
                    }
                    None => {
                        let links = TcpLinks::new(self.tcp_events_tx.clone());
                        let t = Transport::open(&sdp, self.stun, self.route.clone(), links)?;
                        self.view.set_local(t.local);
                        self.transport = Some(t);
                        self.desire_bitrate();
                        true
                    }
                };
                let t = self.transport.as_mut().expect("opened above");
                if new_socket {
                    tokio::select! {
                        biased;
                        () = self.cancel.cancelled() => return Ok(()),
                        () = t.writable() => {}
                    }
                }
                trace!(sdp = sdp.as_str(), "offer");
                let offer = SdpOffer::from_sdp_string(sdp.as_str())
                    .map_err(|e| MediaError::Transport(format!("bad offer: {e}")))?;
                let answer = t
                    .rtc
                    .sdp_api()
                    .accept_offer(offer)
                    .map_err(|e| MediaError::Transport(format!("offer refused: {e}")))?;
                self.video.route(&self.me, &tracks);
                self.voice.route(&self.me, &tracks);
                debug!(rev, tracks = tracks.len(), "answering");
                let answer = VoiceEvent::Answer {
                    vsid,
                    rev,
                    sdp: Sdp::new(answer.to_sdp_string()),
                };
                tokio::select! {
                    biased;
                    () = self.cancel.cancelled() => {}
                    () = self.events.send(answer) => {}
                }
            }
            RtcCmd::Share { frames, keyframes } => {
                self.video.share(frames, keyframes);
                self.desire_bitrate();
            }
            RtcCmd::StopShare => {
                self.video.sharing = None;
                self.desire_bitrate();
            }
            RtcCmd::Watch(input) => {
                self.video.watch(input);
            }
            RtcCmd::Unwatch => {
                self.video.unwatch();
            }
            RtcCmd::Volume { user, volume } => {
                self.voice.set_volume(user, volume);
            }
            RtcCmd::NetworkChanged => {
                if let Some(t) = &mut self.transport {
                    let change = t.network_changed();
                    debug!(?change, "network changed");
                    // A full queue drops it: ICE still notices a dead path on its own.
                    let _ = self.events.try_send(VoiceEvent::NetworkChanged(change));
                }
            }
        }
        Ok(())
    }

    /// str0m probes the uplink up to twice the bitrate wanted, and takes a probe's result as its
    /// estimate even when lower: probes at Voice's bitrate would pull the estimate down to it.
    /// So nothing is wanted, and nothing probed, unless sharing, from the share's first picture.
    fn desire_bitrate(&mut self) {
        let Some(t) = self.transport.as_mut() else {
            return;
        };
        let bps = match (&self.video.sharing, &self.video.share_out) {
            (Some(_), Some((size, _))) => transport::sharing_bps(bps_for(*size)),
            _ => 0,
        };
        t.desire(bps);
    }

    fn snapshot(&mut self) -> Option<VoiceStats> {
        let (share, watch) = self.video.live_mids();
        let mids = Mids {
            uplink: self.voice.uplink,
            audio_in: self.voice.audio_in(),
            share,
            watch,
        };
        let share = self.video.share_out.clone();
        let lost = self.video.take_lost();
        self.tally.snapshot(mids, share, self.video.watched(), lost)
    }

    fn log_stats(&mut self) {
        self.voice.log_slots();
        debug!(
            sent = self.voice.counters.sent,
            unsent = self.voice.counters.unsent,
            ignored_datagrams = self.counters.ignored_datagrams,
            "voice uplink"
        );
        self.video.log_stats();
    }
}

/// str0m's `Rtc` once the media connection is up: no media is written to it before.
fn connected_rtc(transport: &mut Option<Transport>, connected: bool) -> Option<&mut Rtc> {
    transport.as_mut().filter(|_| connected).map(|t| &mut t.rtc)
}

/// The payload type the m-line negotiated for `codec`, kept once found.
fn payload_type(writer: &Writer<'_>, codec: Codec, cached: &mut Option<Pt>) -> Option<Pt> {
    if cached.is_none() {
        *cached = writer
            .payload_params()
            .find(|p| p.spec().codec == codec)
            .map(|p| p.pt());
    }
    *cached
}

enum Received {
    Datagram(usize, SocketAddr),
    Ignored,
    Error(io::Error),
}

/// `ConnectionReset` on a UDP receive is Windows reporting an ICMP port-unreachable for an
/// earlier send (an SFU restart, NAT rebinding): it must not end Voice.
fn classify(r: io::Result<(usize, SocketAddr)>) -> Received {
    match r {
        Ok((n, source)) => Received::Datagram(n, source),
        Err(e) if e.kind() == io::ErrorKind::ConnectionReset => Received::Ignored,
        #[cfg(windows)]
        // WSAEMSGSIZE: a datagram larger than the buffer.
        Err(e) if e.raw_os_error() == Some(10040) => Received::Ignored,
        Err(e) => Received::Error(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_connection_reset_or_oversized_datagram_is_ignored_and_other_errors_reported() {
        let reset = io::Error::from(io::ErrorKind::ConnectionReset);
        assert!(matches!(classify(Err(reset)), Received::Ignored));
        #[cfg(windows)]
        assert!(matches!(
            classify(Err(io::Error::from_raw_os_error(10040))),
            Received::Ignored
        ));
        let other = io::Error::from(io::ErrorKind::NetworkUnreachable);
        assert!(matches!(classify(Err(other)), Received::Error(_)));
        let ok = classify(Ok((12, "127.0.0.1:9".parse().unwrap())));
        assert!(matches!(ok, Received::Datagram(12, _)));
    }
}
