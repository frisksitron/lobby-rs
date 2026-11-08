//! One peer's ICE restart, from the client's `RTC_RESTART`, a nomination from a new client
//! address or a client that fell back to TCP, until ICE completes again; and how long a silent
//! client may live meanwhile.

use std::{
    collections::VecDeque,
    net::SocketAddr,
    time::{Duration, Instant},
};

use str0m::{ice::StunMessage, net::Protocol};

use super::{MediaPath, net::is_stun};

/// More than a client nominates from in one session: its host address, or a NAT's per SFU address.
const NOMINATED_FROM_KEPT: usize = 8;

/// How long after a client's media moved to UDP its media over TCP is what it sent before the
/// move, overtaken on the way. A client falls back only once its UDP pair has failed its checks,
/// which takes it 2.6 s (1 s with the tests' quick timings).
const STRAGGLERS_FOR: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    /// The next offer restarts ICE.
    Wanted,
    /// The offer that restarts ICE is out.
    Offered,
    /// Its answer is applied; the client's checks are under way.
    Checking,
}

/// How long a client may be silent before its peer is lost.
#[derive(Debug, Clone, Copy)]
pub(super) struct DeadPeer {
    pub after: Duration,
    pub after_restart: Duration,
}

#[derive(Debug)]
pub(super) struct IceRestart {
    state: State,
    /// Every address the client nominated a pair from, the newest last.
    nominated_from: VecDeque<(Protocol, SocketAddr)>,
    /// How the client's DTLS and media came last, and since when, since ICE last completed a
    /// restart.
    media_over: Option<(Protocol, Instant)>,
}

impl IceRestart {
    pub fn new() -> Self {
        Self {
            state: State::Idle,
            nominated_from: VecDeque::with_capacity(NOMINATED_FROM_KEPT),
            media_over: None,
        }
    }

    /// `true` if no restart was wanted yet.
    pub fn want(&mut self) -> bool {
        let fresh = matches!(self.state, State::Idle | State::Checking);
        self.state = State::Wanted;
        fresh
    }

    /// A datagram the peer accepted. A binding request with USE-CANDIDATE from an address this
    /// client never nominated from is a NAT rebinding or a network switch, which the client's
    /// own ICE does not notice; without a restart, str0m keeps sending to the old address until
    /// its pair expires 15 s later. So is a fall back from UDP to TCP, whose pair ranks lower
    /// than the dead one. A move up from TCP to UDP needs none: the better pair wins at once.
    ///
    /// A client sends its media by the pair it nominated: its media over TCP after it came over
    /// UDP, while we send over UDP, means its UDP pair died and it fell back to a TCP pair it
    /// nominated before, which str0m alone would not follow until the dead pair expires. Media
    /// over TCP before any over UDP is no fall back: a client moving up keeps sending over TCP
    /// until its UDP nomination is answered, and we send over UDP from the nomination on. Nor is
    /// media over TCP within [`STRAGGLERS_FOR`] of the move to UDP.
    ///
    /// `true` if the datagram wants a restart now.
    pub fn on_datagram(
        &mut self,
        data: &[u8],
        proto: Protocol,
        source: SocketAddr,
        connected: bool,
        sending: Option<MediaPath>,
        now: Instant,
    ) -> bool {
        if is_nomination(data) {
            return self.nominated(proto, source, connected);
        }
        if is_stun(data) {
            return false;
        }
        let since = match self.media_over {
            Some((over, since)) if over == proto => since,
            _ => now,
        };
        let before = self.media_over.replace((proto, since));
        let fell_back = proto == Protocol::Tcp
            && before.is_some_and(|(over, since)| {
                over == Protocol::Udp && now.duration_since(since) >= STRAGGLERS_FOR
            })
            && sending == Some(MediaPath::Udp);
        fell_back && connected && self.state == State::Idle && self.want()
    }

    fn nominated(&mut self, proto: Protocol, source: SocketAddr, connected: bool) -> bool {
        if self.nominated_from.contains(&(proto, source)) {
            return false;
        }
        let outranked = self
            .nominated_from
            .iter()
            .any(|(p, _)| *p == proto || *p == Protocol::Udp);
        // During a restart the client nominates afresh, from wherever it is now.
        let moved = connected && self.state == State::Idle && outranked;
        if self.nominated_from.len() == NOMINATED_FROM_KEPT {
            self.nominated_from.pop_front();
        }
        self.nominated_from.push_back((proto, source));
        moved && self.want()
    }

    pub fn wanted(&self) -> bool {
        self.state == State::Wanted
    }

    pub fn offered(&mut self) {
        self.state = State::Offered;
    }

    pub fn answered(&mut self) {
        if self.state == State::Offered {
            self.state = State::Checking;
        }
    }

    /// ICE reports a connected state. `true` if that completed a restart.
    pub fn ice_connected(&mut self) -> bool {
        let done = self.state == State::Checking;
        if done {
            self.state = State::Idle;
            // The restart's pair may be TCP, which we have not sent over yet.
            self.media_over = None;
        }
        done
    }

    pub fn dead_after(&self, limits: DeadPeer) -> Duration {
        match self.state {
            State::Idle => limits.after,
            State::Wanted | State::Offered | State::Checking => limits.after_restart,
        }
    }
}

/// A binding request that nominates its pair.
fn is_nomination(data: &[u8]) -> bool {
    if !is_stun(data) {
        return false;
    }
    StunMessage::parse(data).is_ok_and(|m| m.is_binding_request() && m.use_candidate())
}
