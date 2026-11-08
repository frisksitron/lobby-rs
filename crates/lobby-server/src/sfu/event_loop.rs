//! The SFU's state and its loop: commands, datagrams and timers in, str0m output and events out.

use std::{
    net::{IpAddr, SocketAddr},
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};

use lobby_protocol::{UnixMs, UserId, VoiceSessionId};
use str0m::{
    Event, Input, Output,
    bwe::BweKind,
    media::MediaData,
    net::{Protocol, Receive},
};
use tokio::{net::UdpSocket, sync::mpsc};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, trace, warn};

use super::{
    EventSink, MediaPath, SfuCmd, SfuEvent, SfuEventKind, SfuInbox, SfuOp, SfuProbe,
    net::{RECV_BUFFER, canonical, is_remote_noise, is_stun, local_ip_for, on_wire},
    peer::{Peer, ShareWanted},
    restart::DeadPeer,
    tcp::TcpStreams,
};
use crate::config::SfuConfig;

const HEARTBEAT_EVERY: Duration = Duration::from_secs(1);
/// A local socket error that keeps recurring is logged at most this often, and each one pauses
/// the loop briefly so it cannot spin.
const RECV_ERROR_LOG_EVERY: Duration = Duration::from_secs(10);
const RECV_ERROR_PAUSE: Duration = Duration::from_millis(10);
const STATS_EVERY: Duration = Duration::from_secs(5);

#[derive(Debug, Default)]
struct Counters {
    ignored_datagrams: u64,
    dropped_datagrams: u64,
    recv_errors: u64,
}

pub(super) struct Sfu {
    socket: UdpSocket,
    /// The port in this SFU's candidates: where clients send, which a port mapping can make
    /// differ from the socket's.
    pub(super) port: u16,
    tcp: TcpStreams,
    /// As `port`, for the TCP candidates.
    pub(super) tcp_port: u16,
    /// An IPv6 socket (dual-stack when bound to `[::]`) sends to IPv4 peers v4-mapped.
    v6: bool,
    pub(super) advertise: Vec<IpAddr>,
    dead_peer: DeadPeer,
    pub(super) peers: Vec<Peer>,
    cmds: mpsc::Receiver<SfuCmd>,
    pub(super) probe: Arc<SfuProbe>,
    sink: EventSink,
    pub(super) events: Vec<SfuEvent>,
    pub(super) media: Vec<(usize, MediaData)>,
    pub(super) sharers_asked_for_keyframes: Vec<UserId>,
    counters: Counters,
}

impl Sfu {
    pub(super) fn new(
        socket: UdpSocket,
        local: SocketAddr,
        tcp: TcpStreams,
        tcp_port: u16,
        cfg: SfuConfig,
        inbox: SfuInbox,
        sink: EventSink,
    ) -> Self {
        Self {
            socket,
            port: cfg.advertise_port.unwrap_or(local.port()),
            tcp,
            tcp_port,
            v6: local.is_ipv6(),
            advertise: cfg.advertise_ips,
            dead_peer: DeadPeer {
                after: cfg.dead_peer_after,
                after_restart: cfg.dead_peer_after_restart,
            },
            peers: Vec::new(),
            cmds: inbox.cmds,
            probe: inbox.probe,
            sink,
            events: Vec::new(),
            media: Vec::new(),
            sharers_asked_for_keyframes: Vec::new(),
            counters: Counters::default(),
        }
    }

    pub(super) async fn run(mut self, stop: CancellationToken) {
        let mut buf = vec![0u8; RECV_BUFFER];
        let mut beat = tokio::time::interval(HEARTBEAT_EVERY);
        beat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut stats = tokio::time::interval(STATS_EVERY);
        stats.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last_error_log: Option<Instant> = None;
        info!(
            advertise = ?self.advertise,
            port = self.port,
            tcp_port = self.tcp_port,
            "sfu running"
        );
        loop {
            self.probe
                .heartbeat
                .store(UnixMs::now().0, Ordering::Relaxed);
            self.drain_all();
            if !self.media.is_empty() {
                self.forward_media();
                continue;
            }
            self.expire_silent(Instant::now());
            self.expire_speech(Instant::now());
            self.reap();
            self.renegotiate();
            self.ask_keyframes();
            self.set_desired_bitrates(Instant::now());
            for ev in self.events.drain(..) {
                (self.sink)(ev);
            }
            let dead_peer = self.dead_peer;
            let next = self
                .peers
                .iter()
                .flat_map(|p| [Some(p.next_timeout), p.dead_at(dead_peer), p.speaking_until])
                .flatten()
                .min();
            // Windows timers round an elapsed deadline up to the next tick: handle it now.
            if next.is_some_and(|t| t <= Instant::now()) {
                self.timeout_all(Instant::now());
                continue;
            }
            let sleep = async {
                match next {
                    Some(t) => tokio::time::sleep_until(t.into()).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                biased;
                _ = stop.cancelled() => break,
                Some(cmd) = self.cmds.recv() => self.handle_cmd(cmd),
                r = self.socket.recv_from(&mut buf) => match r {
                    Ok((n, source)) => {
                        self.handle_datagram(Protocol::Udp, &buf[..n], source);
                    }
                    Err(e) if is_remote_noise(&e) => self.counters.ignored_datagrams += 1,
                    Err(e) => {
                        self.counters.recv_errors += 1;
                        if last_error_log.is_none_or(|t| t.elapsed() >= RECV_ERROR_LOG_EVERY) {
                            warn!(error = %e, recv_errors = self.counters.recv_errors, "udp recv failed");
                            last_error_log = Some(Instant::now());
                        }
                        tokio::time::sleep(RECV_ERROR_PAUSE).await;
                    }
                },
                Some(frame) = self.tcp.next_frame() => {
                    if self.handle_datagram(Protocol::Tcp, &frame.data, frame.remote) {
                        self.tcp.accepted(frame.remote, Instant::now());
                    }
                }
                () = sleep => self.timeout_all(Instant::now()),
                _ = beat.tick() => {
                    self.report_dropped();
                    self.retry_keyframes(Instant::now());
                    self.tcp.expire(Instant::now());
                }
                _ = stats.tick() => self.log_stats(),
            }
        }
        for p in &mut self.peers {
            let _ = p.rtc.close();
        }
        // One last drain sends the DTLS close_notify of every peer.
        self.drain_all();
        info!(
            peers = self.peers.len(),
            ignored_datagrams = self.counters.ignored_datagrams,
            dropped_datagrams = self.counters.dropped_datagrams,
            recv_errors = self.counters.recv_errors,
            "sfu stopped"
        );
        self.tcp.shutdown().await;
    }

    fn handle_cmd(&mut self, SfuCmd { user, vsid, op }: SfuCmd) {
        match op {
            SfuOp::Join { muted } => return self.join(user, vsid, muted),
            SfuOp::Answer { rev, sdp, rid } => return self.answer(user, vsid, rev, sdp, rid),
            _ => {}
        }
        let Some(p) = self.peer_mut(&user, vsid) else {
            return;
        };
        match op {
            SfuOp::Join { .. } | SfuOp::Answer { .. } => unreachable!("handled above"),
            SfuOp::Leave => {
                info!(%user, vsid = vsid.get(), "peer leaving");
                p.close();
                self.forget_viewers_of(&user);
            }
            SfuOp::SetMuted { muted } => {
                debug!(%user, muted, "mute");
                p.muted = muted;
                // The Lobby ends the Member's speaking with the mute itself.
                if muted {
                    p.speaking_until = None;
                }
            }
            SfuOp::StartShare { quality } => {
                info!(%user, ?quality, "share starting");
                p.share_wanted = Some(ShareWanted {
                    quality,
                    source: None,
                });
                if let Some(s) = &mut p.share {
                    s.video_seen = false;
                }
            }
            SfuOp::ShareSource { size } => {
                if let Some(wanted) = &mut p.share_wanted {
                    wanted.source = Some(size);
                }
            }
            SfuOp::StopShare => {
                info!(%user, "share stopping");
                p.share_wanted = None;
                self.forget_viewers_of(&user);
            }
            SfuOp::Watch { sharer } => {
                info!(viewer = %user, %sharer, "watch");
                // A line still pointed at this Sharer resumes without an offer.
                if p.watch_asked(&sharer, Instant::now()) {
                    self.sharers_asked_for_keyframes.push(sharer);
                }
            }
            SfuOp::Unwatch => {
                info!(viewer = %user, "unwatch");
                p.unwatch();
            }
            SfuOp::Restart => {
                if p.restart.want() {
                    p.restart_wanted("client asked");
                }
            }
        }
    }

    fn forget_viewers_of(&mut self, sharer: &UserId) {
        for p in &mut self.peers {
            p.share_ended(sharer);
        }
    }

    fn peer_mut(&mut self, user: &UserId, vsid: VoiceSessionId) -> Option<&mut Peer> {
        self.peers
            .iter_mut()
            .find(|p| &p.user == user && p.vsid == vsid && !p.closing)
    }

    /// A UDP datagram or a TCP frame; `true` if a peer accepted it.
    fn handle_datagram(&mut self, proto: Protocol, data: &[u8], source: SocketAddr) -> bool {
        let Ok(contents) = data.try_into() else {
            self.counters.ignored_datagrams += 1;
            return false;
        };
        let source = canonical(source);
        let port = match proto {
            Protocol::Tcp => self.tcp_port,
            _ => self.port,
        };
        let destination = SocketAddr::new(local_ip_for(&self.advertise, source.ip()), port);
        let now = Instant::now();
        let input = Input::Receive(
            now,
            Receive {
                proto,
                source,
                destination,
                contents,
            },
        );
        match self.peers.iter_mut().find(|p| p.rtc.accepts(&input)) {
            Some(p) => {
                p.last_heard = Some(now);
                if p.restart
                    .on_datagram(data, proto, source, p.connected, p.path, now)
                {
                    p.restart_wanted("client moved to another pair");
                }
                if let Err(e) = p.rtc.handle_input(input) {
                    warn!(user = %p.user, error = %e, "peer input failed");
                    p.rtc.disconnect();
                }
                true
            }
            None => {
                self.counters.ignored_datagrams += 1;
                false
            }
        }
    }

    /// A silent peer is disconnected, so [`Self::reap`] reports it lost.
    fn expire_silent(&mut self, now: Instant) {
        for p in &mut self.peers {
            if p.dead_at(self.dead_peer).is_some_and(|t| t <= now) {
                info!(user = %p.user, vsid = p.vsid.get(), restart = ?p.restart, "peer silent");
                p.rtc.disconnect();
            }
        }
    }

    /// A Member whose voiced packets stopped coming (muted, DTX, loss) stops speaking.
    fn expire_speech(&mut self, now: Instant) {
        for p in &mut self.peers {
            if p.speech_expired(now) && !p.closing {
                self.events.push(SfuEvent {
                    user: p.user.clone(),
                    vsid: p.vsid,
                    kind: SfuEventKind::Speaking(false),
                });
            }
        }
    }

    fn timeout_all(&mut self, now: Instant) {
        for p in &mut self.peers {
            if p.rtc.is_alive()
                && let Err(e) = p.rtc.handle_input(Input::Timeout(now))
            {
                warn!(user = %p.user, error = %e, "peer timeout failed");
                p.rtc.disconnect();
            }
        }
    }

    fn drain_all(&mut self) {
        for i in 0..self.peers.len() {
            self.drain(i);
        }
    }

    /// Polls one peer until it yields a timeout. Media is collected for forwarding afterwards,
    /// since two peers cannot be borrowed at once.
    fn drain(&mut self, i: usize) {
        loop {
            let p = &mut self.peers[i];
            if !p.rtc.is_alive() {
                return;
            }
            match p.rtc.poll_output() {
                Ok(Output::Timeout(t)) => {
                    p.next_timeout = t;
                    return;
                }
                Ok(Output::Transmit(t)) => {
                    // ICE answers each check on the path it came by; the rest goes by the
                    // nominated pair.
                    if !is_stun(&t.contents) {
                        let path = match t.proto {
                            Protocol::Tcp => MediaPath::Tcp,
                            _ => MediaPath::Udp,
                        };
                        if p.path != Some(path) {
                            info!(user = %p.user, vsid = p.vsid.get(), ?path, "media path");
                            p.path = Some(path);
                            #[cfg(any(test, feature = "test-support"))]
                            self.probe
                                .paths
                                .lock()
                                .expect("media paths")
                                .insert(p.user.clone(), path);
                        }
                    }
                    // Never waits: a full send buffer or stream queue drops the datagram, which
                    // RTP/RTCP recover with NACK and ICE/DTLS with retransmits.
                    let sent = match t.proto {
                        Protocol::Tcp => self.tcp.send(t.destination, &t.contents),
                        _ => {
                            let to = on_wire(self.v6, t.destination);
                            let sent = self.socket.try_send_to(&t.contents, to).is_ok();
                            if sent {
                                self.probe
                                    .udp_bytes_sent
                                    .fetch_add(t.contents.len() as u64, Ordering::Relaxed);
                            }
                            sent
                        }
                    };
                    if !sent {
                        self.counters.dropped_datagrams += 1;
                    }
                }
                Ok(Output::Event(e)) => match e {
                    Event::Connected if !p.connected && !p.closing => {
                        p.connected = true;
                        info!(user = %p.user, vsid = p.vsid.get(), "peer connected");
                        self.events.push(SfuEvent {
                            user: p.user.clone(),
                            vsid: p.vsid,
                            kind: SfuEventKind::Connected,
                        });
                    }
                    Event::IceConnectionStateChange(state) => {
                        debug!(user = %p.user, ?state, "ice");
                        // An ice-lite agent reports Completed, never Connected.
                        if state.is_connected() && p.restart.ice_connected() {
                            info!(user = %p.user, vsid = p.vsid.get(), "ice restarted");
                        }
                    }
                    Event::MediaData(data) => self.media.push((i, data)),
                    Event::KeyframeRequest(req) => {
                        if let Some(w) = p.watch.iter().find(|w| w.mid == req.mid)
                            && let Some(source) = w.source.assigned()
                        {
                            debug!(viewer = %p.user, sharer = %source, kind = ?req.kind, "viewer keyframe request");
                            self.sharers_asked_for_keyframes.push(source.clone());
                        }
                    }
                    Event::EgressBitrateEstimate(BweKind::Twcc { estimate, .. }) => {
                        trace!(user = %p.user, %estimate, "downlink estimate");
                        p.downlink_bps = Some(estimate.as_u64());
                    }
                    _ => {}
                },
                Err(e) => {
                    warn!(user = %p.user, error = %e, "peer output failed");
                    p.rtc.disconnect();
                    return;
                }
            }
        }
    }

    fn reap(&mut self) {
        let events = &mut self.events;
        let lost = &self.probe.lost;
        let mut gone = Vec::new();
        self.peers.retain(|p| {
            if p.rtc.is_alive() {
                return true;
            }
            if !p.closing {
                info!(user = %p.user, vsid = p.vsid.get(), "peer lost");
                lost.fetch_add(1, Ordering::Relaxed);
                events.push(SfuEvent {
                    user: p.user.clone(),
                    vsid: p.vsid,
                    kind: SfuEventKind::Lost,
                });
            }
            gone.push(p.user.clone());
            false
        });
        for user in gone {
            self.forget_viewers_of(&user);
        }
        self.probe.live.store(self.peers.len(), Ordering::Relaxed);
    }

    fn log_stats(&self) {
        for p in self.peers.iter().filter(|p| !p.closing) {
            info!(
                user = %p.user,
                vsid = p.vsid.get(),
                connected = p.connected,
                muted = p.muted,
                received = p.received,
                forwarded = p.forwarded,
                muted_dropped = p.muted_dropped,
                sharing = ?p.share_wanted.filter(|_| p.share.is_some()),
                watching = ?p.watched(),
                video_received = p.video_received,
                video_forwarded = p.video_forwarded,
                keyframe_requests = p.keyframe_requests,
                mlines = p.mlines(),
                "sfu peer"
            );
        }
    }
}
