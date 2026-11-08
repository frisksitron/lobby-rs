//! The client's str0m `Rtc` and the one UDP socket it sends from, bound on the interface the OS
//! routes to the SFU through, plus its TCP streams for when UDP is blocked ([`TcpLinks`]). Every
//! offer asks the OS again and moves to a new socket when that interface changed, so an ICE
//! restart after a change of network checks from where the client is now. A real change of IP
//! usually drops the WebSocket too, which ends Voice; the move helps when the WebSocket survives
//! it (a second interface, a VPN).
//!
//! Windows reports network changes as they happen (`netwatch::NetworkWatch`). Linux would watch an
//! `AF_NETLINK`/`NETLINK_ROUTE` socket for them; not built, as ICE notices a dead path within
//! 3 s and the next offer reroutes anyway.

use std::{
    fmt, io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};

use lobby_protocol::{
    Sdp,
    encoder::{BWE_START_BPS, H264_LEVEL, desired_bps},
};
use str0m::{
    Candidate, Rtc,
    bwe::Bitrate,
    net::{Protocol, Transmit},
};
use tokio::net::UdpSocket;
use tracing::{debug, info, warn};

use super::tcp::{self, TcpLinks};
use crate::media::{MediaError, RouteChange, StunTimings};

/// Our jitter buffer owns audio reordering; str0m only holds one packet for 40 ms.
const REORDER_AUDIO_PACKETS: usize = 1;
const REORDER_AUDIO_TIMEOUT: Duration = Duration::from_millis(40);
/// How long a video frame may take to assemble, from its first packet. A keyframe paced at a low
/// bandwidth estimate (146 kbps was measured on an idle link) spreads 10-20 KB over up to ~1.1 s
/// and must still arrive whole. A complete frame leaves at once, so only a frame missing a packet
/// that is never repaired, or the frame after such a gap, waits this long. RTX repairs most losses
/// well inside it.
const REORDER_VIDEO_TIMEOUT: Duration = Duration::from_millis(1500);
/// The one video codec, as the SFU offers it.
const H264_PT: u8 = 108;
const H264_RTX_PT: u8 = 109;
/// The media socket's kernel buffers: a keyframe arrives as one burst of up to a few hundred
/// datagrams, more than the 64 KiB Windows default holds.
const UDP_SOCKET_BUFFER: usize = 4 * 1024 * 1024;
/// The most a new socket's offer waits to see it writable: the first STUN check is retransmitted
/// after 100 ms anyway, and the TCP path does not wait for UDP.
const UDP_READY_WAIT: Duration = Duration::from_millis(100);
const AUDIO_BPS: u64 = 256_000;
pub(super) fn sharing_bps(share_bps: u32) -> u64 {
    desired_bps(u64::from(share_bps)) + AUDIO_BPS
}

/// Which local address the OS routes to an SFU address through. Tests stand in for the OS.
#[derive(Clone)]
pub struct RouteProbe(Arc<dyn Fn(SocketAddr) -> io::Result<IpAddr> + Send + Sync>);

impl RouteProbe {
    #[cfg(any(test, feature = "test-support"))]
    pub fn new(probe: impl Fn(SocketAddr) -> io::Result<IpAddr> + Send + Sync + 'static) -> Self {
        Self(Arc::new(probe))
    }

    fn route(&self, to: SocketAddr) -> io::Result<IpAddr> {
        (self.0)(to)
    }
}

impl Default for RouteProbe {
    fn default() -> Self {
        Self(Arc::new(route_to))
    }
}

impl fmt::Debug for RouteProbe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RouteProbe")
    }
}

pub(super) struct Transport {
    pub rtc: Rtc,
    pub socket: UdpSocket,
    pub local: SocketAddr,
    pub tcp: TcpLinks,
    /// The SFU's host candidates in the latest offer.
    candidates: Vec<SocketAddr>,
    probe: RouteProbe,
    /// The local address a network change last reported as new, so a burst reports it once.
    reported: Option<IpAddr>,
    /// What the uplink estimate was last told to probe for.
    desired_bps: u64,
}

impl Transport {
    /// For the first offer: the socket goes on the interface the first routable SFU candidate is
    /// reached through, and is our host candidate; our TCP candidate is on the same address.
    pub fn open(
        sdp: &Sdp,
        stun: StunTimings,
        probe: RouteProbe,
        tcp: TcpLinks,
    ) -> Result<Transport, MediaError> {
        let candidates = sfu_candidates(sdp.as_str());
        let SfuRoute { sfu, local: ip } = first_route(&candidates, |to| probe.route(to))?;
        let (socket, local) = bind_media(ip)?;
        let mut config = Rtc::builder()
            .clear_codecs()
            .enable_opus(true, false)
            .set_reordering_size_audio(REORDER_AUDIO_PACKETS)
            .set_reordering_timeout_audio(Some(REORDER_AUDIO_TIMEOUT))
            .set_reordering_timeout_video(Some(REORDER_VIDEO_TIMEOUT))
            .set_stats_interval(Some(super::STATS_EVERY))
            // Paces the video at the estimate; audio is never held back.
            .enable_bwe(Some(Bitrate::bps(BWE_START_BPS)));
        config.codec_config().add_h264(
            H264_PT.into(),
            Some(H264_RTX_PT.into()),
            true,
            H264_LEVEL.profile_level_id(),
        );
        config.set_initial_stun_rto(stun.initial_rto);
        config.set_max_stun_rto(stun.max_rto);
        config.set_max_stun_retransmits(stun.retransmits);
        let mut rtc = config.build(Instant::now());
        for candidate in candidates_on(local)? {
            rtc.add_local_candidate(candidate);
        }
        info!(%local, %sfu, "voice transport bound");
        Ok(Transport {
            rtc,
            socket,
            local,
            tcp,
            candidates,
            probe,
            reported: None,
            desired_bps: 0,
        })
    }

    /// Has the uplink estimate probe up to `bps`. str0m probes at once on each new value, so an
    /// unchanged one is not passed on.
    pub fn desire(&mut self, bps: u64) {
        if bps != self.desired_bps {
            self.desired_bps = bps;
            self.rtc.bwe().set_desired_bitrate(Bitrate::bps(bps));
        }
    }

    /// Until tokio has seen the socket writable once, `try_send_to` refuses without trying. A new
    /// socket's first STUN check would be lost, and the TCP path would connect first. A socket
    /// still not writable after [`UDP_READY_WAIT`] is left to ICE's retransmits.
    pub async fn writable(&self) {
        if tokio::time::timeout(UDP_READY_WAIT, self.socket.writable())
            .await
            .is_err()
        {
            debug!(local = %self.local, "the media socket is not writable yet");
        }
    }

    /// Sends what str0m wants sent, by UDP or the TCP stream to its destination.
    pub fn send(&mut self, out: &Transmit) -> Sent {
        match out.proto {
            Protocol::Tcp if out.source.ip() == self.local.ip() => {
                if self
                    .tcp
                    .send(self.local.ip(), out.destination, &out.contents)
                {
                    Sent::Yes
                } else {
                    Sent::Failed(io::ErrorKind::WouldBlock.into())
                }
            }
            Protocol::Udp if out.source == self.local => {
                match self.socket.try_send_to(&out.contents, out.destination) {
                    Ok(_) => Sent::Yes,
                    Err(e) => Sent::Failed(e),
                }
            }
            _ => Sent::Stale,
        }
    }

    /// Where a packet that came by TCP or UDP arrived, as str0m knows our candidates.
    pub fn local_for(&self, proto: Protocol) -> SocketAddr {
        match proto {
            Protocol::Tcp => tcp::local(self.local.ip()),
            _ => self.local,
        }
    }

    /// Before a later offer is answered: `true` if the socket moved. A move that fails is not
    /// fatal; the old socket stays and the next offer tries again.
    pub fn reroute(&mut self, sdp: &Sdp) -> bool {
        let candidates = sfu_candidates(sdp.as_str());
        if !candidates.is_empty() {
            self.candidates = candidates;
        }
        let Some(ip) = self.moved() else {
            return false;
        };
        match self.rebind(ip) {
            Ok(()) => true,
            Err(e) => {
                warn!(error = %e, "voice transport not moved");
                false
            }
        }
    }

    /// The OS reported a change of network: does it change our route to the SFU?
    pub fn network_changed(&mut self) -> RouteChange {
        match self.moved() {
            Some(ip) if self.reported != Some(ip) => {
                self.reported = Some(ip);
                RouteChange::Moved
            }
            _ => RouteChange::Same,
        }
    }

    /// The local address the OS routes to the SFU through now, if it is not ours.
    fn moved(&self) -> Option<IpAddr> {
        match first_route(&self.candidates, |to| self.probe.route(to)) {
            Ok(route) if route.local != self.local.ip() => Some(route.local),
            Ok(_) => None,
            Err(e) => {
                debug!(error = %e, "no route to the SFU right now");
                None
            }
        }
    }

    /// The old candidates are invalidated so ICE stops checking from an address that is gone.
    fn rebind(&mut self, ip: IpAddr) -> Result<(), MediaError> {
        let (socket, local) = bind_media(ip)?;
        let candidates = candidates_on(local)?;
        for old in candidates_on(self.local).into_iter().flatten() {
            self.rtc.direct_api().invalidate_candidate(&old);
        }
        for candidate in candidates {
            self.rtc.add_local_candidate(candidate);
        }
        info!(from = %self.local, to = %local, "voice transport moved");
        self.socket = socket;
        self.local = local;
        self.tcp.clear();
        self.reported = None;
        Ok(())
    }
}

pub(super) enum Sent {
    Yes,
    /// From an address the transport moved away from.
    Stale,
    Failed(io::Error),
}

/// Our UDP host candidate on the media socket, and the active TCP one on its address.
fn candidates_on(local: SocketAddr) -> Result<[Candidate; 2], MediaError> {
    let udp = Candidate::host(local, "udp")
        .map_err(|e| MediaError::Transport(format!("host candidate {local}: {e}")))?;
    let tcp = tcp::active_candidate(local.ip())
        .map_err(|e| MediaError::Transport(format!("tcp candidate {local}: {e}")))?;
    Ok([udp, tcp])
}

/// The media socket on `ip`, any port, with buffers large enough for a keyframe burst.
fn bind_media(ip: IpAddr) -> Result<(UdpSocket, SocketAddr), MediaError> {
    let std_socket = std::net::UdpSocket::bind(SocketAddr::new(ip, 0))
        .map_err(|e| MediaError::NoRoute(format!("bind {ip}: {e}")))?;
    std_socket
        .set_nonblocking(true)
        .map_err(|e| MediaError::Transport(e.to_string()))?;
    let sock = socket2::SockRef::from(&std_socket);
    for (what, set) in [
        ("recv", sock.set_recv_buffer_size(UDP_SOCKET_BUFFER)),
        ("send", sock.set_send_buffer_size(UDP_SOCKET_BUFFER)),
    ] {
        if let Err(e) = set {
            warn!(what, error = %e, "media socket buffer not enlarged");
        }
    }
    let socket =
        UdpSocket::from_std(std_socket).map_err(|e| MediaError::Transport(e.to_string()))?;
    let local = socket
        .local_addr()
        .map_err(|e| MediaError::Transport(e.to_string()))?;
    Ok((socket, local))
}

fn sfu_candidates(sdp: &str) -> Vec<SocketAddr> {
    sdp.lines()
        .filter_map(|l| l.trim().strip_prefix("a=candidate:"))
        .filter_map(|c| {
            // foundation component transport priority address port "typ" type …
            let f: Vec<&str> = c.split_whitespace().collect();
            if f.len() < 8 || !f[2].eq_ignore_ascii_case("udp") || f[7] != "host" {
                return None;
            }
            let ip: IpAddr = f[4].parse().ok()?;
            let port: u16 = f[5].parse().ok()?;
            Some(SocketAddr::new(ip, port))
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SfuRoute {
    sfu: SocketAddr,
    local: IpAddr,
}

/// The first candidate, in offer order (the Operator's `advertise_ips` order), that `probe` finds
/// a route to; its address family is the media socket's (one socket). `NoRoute` names
/// every failure.
fn first_route(
    candidates: &[SocketAddr],
    probe: impl Fn(SocketAddr) -> io::Result<IpAddr>,
) -> Result<SfuRoute, MediaError> {
    if candidates.is_empty() {
        return Err(MediaError::NoRoute(
            "the offer has no UDP host candidate".into(),
        ));
    }
    let mut failures = Vec::new();
    for &sfu in candidates {
        match probe(sfu) {
            Ok(local) => return Ok(SfuRoute { sfu, local }),
            Err(e) => {
                debug!(%sfu, error = %e, "no route to this SFU candidate");
                failures.push(format!("{sfu}: {e}"));
            }
        }
    }
    Err(MediaError::NoRoute(failures.join("; ")))
}

/// The local address the OS routes to `to`: a connected UDP socket learns it without sending.
fn route_to(to: SocketAddr) -> io::Result<IpAddr> {
    let any: IpAddr = match to {
        SocketAddr::V4(_) => Ipv4Addr::UNSPECIFIED.into(),
        SocketAddr::V6(_) => Ipv6Addr::UNSPECIFIED.into(),
    };
    let probe = std::net::UdpSocket::bind(SocketAddr::new(any, 0))?;
    probe.connect(to)?;
    Ok(probe.local_addr()?.ip())
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    };

    use super::*;

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    fn v4_only(to: SocketAddr) -> io::Result<IpAddr> {
        match to {
            SocketAddr::V4(_) => Ok("192.168.1.5".parse().unwrap()),
            SocketAddr::V6(_) => Err(io::ErrorKind::NetworkUnreachable.into()),
        }
    }

    fn offer(candidates: &[&str]) -> Sdp {
        let lines: Vec<String> = candidates
            .iter()
            .map(|c| {
                let a = addr(c);
                format!(
                    "a=candidate:1 1 udp 2130706431 {} {} typ host",
                    a.ip(),
                    a.port()
                )
            })
            .collect();
        Sdp::new(format!("v=0\r\n{}\r\n", lines.join("\r\n")))
    }

    fn transport(candidates: &[&str], probe: RouteProbe) -> Transport {
        let links = TcpLinks::new(tcp::channel().0);
        let t = Transport::open(&offer(candidates), StunTimings::default(), probe, links).unwrap();
        assert_eq!(t.local.ip(), "127.0.0.1".parse::<IpAddr>().unwrap());
        t
    }

    /// Routes through 127.0.0.1 until `moved`, then through 127.0.0.2, as after joining
    /// another network; never over IPv6.
    fn switchable() -> (RouteProbe, Arc<AtomicBool>) {
        let moved = Arc::new(AtomicBool::new(false));
        let m = moved.clone();
        let probe = RouteProbe::new(move |to| match to {
            SocketAddr::V6(_) => Err(io::ErrorKind::NetworkUnreachable.into()),
            SocketAddr::V4(_) if m.load(Ordering::SeqCst) => Ok("127.0.0.2".parse().unwrap()),
            SocketAddr::V4(_) => Ok("127.0.0.1".parse().unwrap()),
        });
        (probe, moved)
    }

    #[test]
    fn a_v6_first_offer_on_a_host_without_v6_routes_over_v4() {
        let offer = [addr("[2001:db8::7]:50000"), addr("203.0.113.7:50000")];
        assert_eq!(
            first_route(&offer, v4_only).unwrap(),
            SfuRoute {
                sfu: addr("203.0.113.7:50000"),
                local: "192.168.1.5".parse().unwrap()
            }
        );
    }

    /// Each offer asks the OS again: a new local address moves the transport to a new socket
    /// there, the same one (or none at all) keeps the socket it has.
    #[tokio::test]
    async fn an_offer_after_a_change_of_route_moves_the_transport_to_a_new_socket() {
        let (probe, moved) = switchable();
        let sfu = ["127.0.0.1:50000"];
        let mut t = transport(&sfu, probe);
        let first = t.local;
        assert!(!t.reroute(&offer(&sfu)), "the route is unchanged");
        assert!(!t.reroute(&offer(&["[2001:db8::7]:50000"])), "no route");
        assert_eq!(t.local, first);

        moved.store(true, Ordering::SeqCst);
        assert!(t.reroute(&offer(&sfu)));
        assert_eq!(t.local.ip(), "127.0.0.2".parse::<IpAddr>().unwrap());
        assert_eq!(t.socket.local_addr().unwrap(), t.local);
        let now = t.local;
        assert!(!t.reroute(&offer(&sfu)), "already there");
        assert_eq!(t.local, now);

        let sender = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        sender.send_to(b"still listening?", now).unwrap();
        let mut buf = [0u8; 32];
        let (n, _) = t.socket.recv_from(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"still listening?", "the new socket receives");
    }

    /// The route choice runs over the SFU candidates of the latest offer, not the one picked at
    /// open: an SFU first reached over IPv6 is reached over IPv4 once IPv6 is gone, through the
    /// IPv4 candidate a later offer added.
    #[tokio::test]
    async fn the_route_is_chosen_again_over_the_latest_offers_candidates() {
        let v6_gone = Arc::new(AtomicBool::new(false));
        let gone = v6_gone.clone();
        let probes = Arc::new(Mutex::new(Vec::new()));
        let seen = probes.clone();
        let probe = RouteProbe::new(move |to| {
            seen.lock().unwrap().push(to);
            match to {
                SocketAddr::V6(_) if gone.load(Ordering::SeqCst) => {
                    Err(io::ErrorKind::NetworkUnreachable.into())
                }
                SocketAddr::V6(_) => Ok("127.0.0.1".parse().unwrap()),
                SocketAddr::V4(_) => Ok("127.0.0.2".parse().unwrap()),
            }
        });
        let both = ["[2001:db8::7]:50000", "127.0.0.9:50000"];
        let mut t = transport(&both[..1], probe);
        v6_gone.store(true, Ordering::SeqCst);
        probes.lock().unwrap().clear();
        assert!(t.reroute(&offer(&both)));
        assert_eq!(t.local.ip(), "127.0.0.2".parse::<IpAddr>().unwrap());
        assert_eq!(*probes.lock().unwrap(), [addr(both[0]), addr(both[1])]);
    }

    /// A burst of changes reports a new route once; the move itself waits for the next offer.
    #[tokio::test]
    async fn a_network_change_reports_a_new_route_once() {
        let (probe, moved) = switchable();
        let sfu = ["127.0.0.1:50000"];
        let mut t = transport(&sfu, probe);
        assert_eq!(t.network_changed(), RouteChange::Same);
        moved.store(true, Ordering::SeqCst);
        assert_eq!(t.network_changed(), RouteChange::Moved);
        assert_eq!(t.network_changed(), RouteChange::Same, "the same burst");
        assert_eq!(t.local.ip(), "127.0.0.1".parse::<IpAddr>().unwrap());
        assert!(t.reroute(&offer(&sfu)));
        assert_eq!(t.network_changed(), RouteChange::Same, "moved already");
    }
}
