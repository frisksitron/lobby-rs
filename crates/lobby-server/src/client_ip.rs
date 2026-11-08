//! Who is asking: the client IP behind a trusted reverse proxy, which every per-IP limit keys on.
//!
//! Only when the socket peer is a trusted proxy is `X-Forwarded-For` read, and then **from the
//! right**: each proxy appends the address it saw, so the right-most entries are our own proxies'
//! and the first untrusted one is the client. A forged left-most entry is therefore ignored.
//! `X-Real-IP` is never read: a proxy that passed a client's own value through would let it pick
//! its bucket.

use std::net::{IpAddr, Ipv6Addr};

use axum::{
    extract::{ConnectInfo, Request, State},
    http::{Extensions, HeaderMap},
    middleware::Next,
    response::Response,
};
use ipnet::IpNet;

use crate::{linger::Peer, server::App};

/// The client of a request. Only [`resolve`] makes one, so a limit cannot be keyed on a socket
/// peer that is really the proxy, or on an IPv4 client in its IPv6-mapped form. The test server's
/// own sign-ins, which cross no socket, are the one exception.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientIp(IpAddr);

impl ClientIp {
    #[cfg(feature = "test-support")]
    pub fn this_machine() -> Self {
        Self(IpAddr::from([127, 0, 0, 1]))
    }

    pub fn ip(self) -> IpAddr {
        self.0
    }

    /// What a per-IP limit counts under. One host is given a whole IPv6 /64 and can use any
    /// address in it, so the /64 is one client.
    pub fn key(self) -> IpAddr {
        match self.0 {
            IpAddr::V6(v6) => IpAddr::V6(Ipv6Addr::from_bits(v6.to_bits() & (u128::MAX << 64))),
            v4 => v4,
        }
    }

    /// What the Magic Code send caps count under. A free tunnel broker hands anyone an IPv6 /48,
    /// 65536 /64s, so the /48 is one client: each client's caps bound its guesses at codes.
    pub fn site_key(self) -> IpAddr {
        match self.0 {
            IpAddr::V6(v6) => IpAddr::V6(Ipv6Addr::from_bits(v6.to_bits() & (u128::MAX << 80))),
            v4 => v4,
        }
    }
}

/// IPv4-mapped IPv6 peers (`::ffff:a.b.c.d`) count as their IPv4 address.
fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    }
}

fn trusted(ip: IpAddr, proxies: &[IpNet]) -> bool {
    proxies.iter().any(|c| c.contains(&ip))
}

/// The client behind `peer`: `peer` itself unless it is a trusted proxy; then the right-most
/// `X-Forwarded-For` entry that is not a trusted proxy, else `peer`.
pub fn client_ip(peer: IpAddr, headers: &HeaderMap, proxies: &[IpNet]) -> IpAddr {
    let peer = canonical(peer);
    if !trusted(peer, proxies) {
        return peer;
    }
    let forwarded = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .collect::<Vec<_>>();
    for entry in forwarded.iter().rev() {
        match entry.trim().parse::<IpAddr>() {
            Ok(ip) if trusted(canonical(ip), proxies) => continue,
            Ok(ip) => return canonical(ip),
            // A garbled entry: nothing to its left can be trusted either.
            Err(_) => break,
        }
    }
    peer
}

pub fn peer_of(extensions: &Extensions) -> IpAddr {
    extensions
        .get::<ConnectInfo<Peer>>()
        .map_or(IpAddr::from([127, 0, 0, 1]), |ConnectInfo(Peer(a))| a.ip())
}

pub async fn resolve(State(app): State<App>, mut req: Request, next: Next) -> Response {
    let ip = client_ip(
        peer_of(req.extensions()),
        req.headers(),
        &app.cfg.http.trusted_proxy_cidrs,
    );
    req.extensions_mut().insert(ClientIp(ip));
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(*k, HeaderValue::from_str(v).unwrap());
        }
        h
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn xff_is_read_right_to_left_and_only_from_trusted_proxies() {
        let caddy: Vec<IpNet> = vec!["172.30.0.2/32".parse().unwrap()];
        // Caddy appends what it saw: a spoofed left-most entry stays ignored.
        let spoofed = headers(&[("x-forwarded-for", "1.2.3.4, 203.0.113.9")]);
        assert_eq!(
            client_ip(ip("172.30.0.2"), &spoofed, &caddy),
            ip("203.0.113.9")
        );
        assert_eq!(
            client_ip(ip("198.51.100.7"), &spoofed, &caddy),
            ip("198.51.100.7")
        );
        // Chained trusted proxies are skipped; several header lines count as one list.
        let two: Vec<IpNet> = vec!["172.30.0.0/24".parse().unwrap()];
        let chained = headers(&[
            ("x-forwarded-for", "9.9.9.9"),
            ("x-forwarded-for", "203.0.113.9, 172.30.0.5"),
        ]);
        assert_eq!(
            client_ip(ip("172.30.0.2"), &chained, &two),
            ip("203.0.113.9")
        );
        // No usable XFF: the peer; a client's own X-Real-IP is never believed.
        let real = headers(&[("x-real-ip", "203.0.113.10")]);
        assert_eq!(client_ip(ip("172.30.0.2"), &real, &caddy), ip("172.30.0.2"));
        assert_eq!(
            client_ip(ip("172.30.0.2"), &HeaderMap::new(), &caddy),
            ip("172.30.0.2")
        );
        let garbled = headers(&[("x-forwarded-for", "203.0.113.9, nonsense")]);
        assert_eq!(
            client_ip(ip("172.30.0.2"), &garbled, &caddy),
            ip("172.30.0.2")
        );
        let local: Vec<IpNet> = vec!["127.0.0.1/32".parse().unwrap()];
        let xff = headers(&[("x-forwarded-for", "203.0.113.9")]);
        assert_eq!(
            client_ip(ip("::ffff:127.0.0.1"), &xff, &local),
            ip("203.0.113.9")
        );
        // A dual-stack listener with no proxy names an IPv4 peer `::ffff:a.b.c.d`.
        assert_eq!(
            client_ip(ip("::ffff:198.51.100.7"), &xff, &caddy),
            ip("198.51.100.7")
        );
    }
}
