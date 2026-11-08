//! Sockets and addresses: binding, buffer sizes, and the address rules between str0m and the
//! socket.

use std::{
    io,
    net::{IpAddr, SocketAddr},
};

use tracing::warn;

/// Larger than any datagram str0m accepts: a 1500-byte MTU plus headroom.
pub(super) const RECV_BUFFER: usize = 2048;
const TCP_BACKLOG: i32 = 128;
/// A 1080p keyframe is 50–250 packets sent at once with no pacing, more than the 64 KiB Windows
/// default holds.
pub const UDP_SOCKET_BUFFER: usize = 4 * 1024 * 1024;
/// WSAEMSGSIZE: Windows fails `recv_from` for a datagram larger than the buffer (Linux truncates).
#[cfg(windows)]
const WSAEMSGSIZE: i32 = 10040;

/// Binds `sfu.udp_bind`. An IPv6 address is bound dual-stack, so `[::]:port` also serves IPv4
/// peers: Windows defaults to IPv6-only and Linux follows `net.ipv6.bindv6only`.
pub fn bind_udp(addr: SocketAddr) -> io::Result<std::net::UdpSocket> {
    let socket = socket2::Socket::new(
        socket2::Domain::for_address(addr),
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )?;
    if addr.is_ipv6() {
        socket.set_only_v6(false)?;
    }
    socket.bind(&addr.into())?;
    Ok(socket.into())
}

/// Binds `sfu.tcp_bind`, dual-stack for an IPv6 address as [`bind_udp`] does.
pub fn bind_tcp(addr: SocketAddr) -> io::Result<std::net::TcpListener> {
    let socket = socket2::Socket::new(
        socket2::Domain::for_address(addr),
        socket2::Type::STREAM,
        Some(socket2::Protocol::TCP),
    )?;
    if addr.is_ipv6() {
        socket.set_only_v6(false)?;
    }
    // A restart rebinds at once despite the old streams in TIME_WAIT. On Windows this would let
    // another process take the port instead.
    #[cfg(not(windows))]
    socket.set_reuse_address(true)?;
    socket.bind(&addr.into())?;
    socket.listen(TCP_BACKLOG)?;
    Ok(socket.into())
}

/// Larger kernel buffers; a refusal (some systems cap them) only costs packets NACK recovers.
pub(super) fn set_socket_buffers(udp: &std::net::UdpSocket) {
    let sock = socket2::SockRef::from(udp);
    for (what, set) in [
        ("recv", sock.set_recv_buffer_size(UDP_SOCKET_BUFFER)),
        ("send", sock.set_send_buffer_size(UDP_SOCKET_BUFFER)),
    ] {
        if let Err(e) = set {
            warn!(what, error = %e, "udp socket buffer not enlarged");
        }
    }
}

/// Which advertised address a datagram from `source` was sent to. str0m answers STUN only on a
/// local candidate address, and a socket bound to `0.0.0.0` or `[::]` cannot tell which one the
/// peer used. str0m pairs candidates of one address family only, so the family must match; then
/// the scope: loopback for loopback, a private (LAN) address for a private sender, a public one
/// otherwise. Failing both, the same family, then the first address.
pub(super) fn local_ip_for(advertise: &[IpAddr], source: IpAddr) -> IpAddr {
    fn scope(ip: IpAddr) -> u8 {
        match ip {
            ip if ip.is_loopback() => 0,
            IpAddr::V4(v4) if v4.is_private() || v4.is_link_local() => 1,
            IpAddr::V6(v6) if v6.is_unique_local() || v6.is_unicast_link_local() => 1,
            _ => 2,
        }
    }
    let source = source.to_canonical();
    let family = |ip: &IpAddr| ip.is_ipv4() == source.is_ipv4();
    advertise
        .iter()
        .copied()
        .find(|ip| family(ip) && scope(*ip) == scope(source))
        .or_else(|| advertise.iter().copied().find(family))
        .or_else(|| advertise.first().copied())
        .unwrap_or(source)
}

/// What str0m sees: a dual-stack socket reports an IPv4 peer as `::ffff:a.b.c.d`, which would
/// pair with no IPv4 candidate.
pub(super) fn canonical(addr: SocketAddr) -> SocketAddr {
    SocketAddr::new(addr.ip().to_canonical(), addr.port())
}

/// What the socket sends to: an IPv6 socket reaches an IPv4 peer at its v4-mapped address.
pub(super) fn on_wire(socket_is_v6: bool, to: SocketAddr) -> SocketAddr {
    match to {
        SocketAddr::V4(v4) if socket_is_v6 => {
            SocketAddr::new(v4.ip().to_ipv6_mapped().into(), v4.port())
        }
        _ => to,
    }
}

/// RFC 7983: a STUN message starts with 0 to 3; DTLS, RTP and RTCP never do.
pub(super) fn is_stun(data: &[u8]) -> bool {
    data.first().is_some_and(|b| *b <= 3)
}

/// Errors a remote peer can provoke at will: an ICMP port-unreachable (Windows reports it as
/// ConnectionReset on the next recv) and an oversized datagram.
pub(super) fn is_remote_noise(e: &io::Error) -> bool {
    #[cfg(windows)]
    if e.raw_os_error() == Some(WSAEMSGSIZE) {
        return true;
    }
    e.kind() == io::ErrorKind::ConnectionReset
}
