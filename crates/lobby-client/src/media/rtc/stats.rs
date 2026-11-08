//! What str0m measures of the call, once a second, folded into the [`VoiceStats`] the Session
//! shows. str0m reports bytes as running totals per m-line; a rate is the change since its last
//! report.

use std::{collections::HashMap, sync::Arc, time::Instant};

use str0m::{
    media::Mid,
    net::Protocol,
    stats::{MediaEgressStats, MediaIngressStats, PeerStats},
};

use crate::media::{MediaPath, VideoStats, VoiceStats};

/// Opus runs its RTP clock at 48 kHz: str0m reports jitter in its ticks.
const AUDIO_CLOCK_HZ: u64 = 48_000;

/// The m-lines a snapshot reads, as the latest offer routed them.
pub(super) struct Mids {
    pub uplink: Option<Mid>,
    pub audio_in: Vec<Mid>,
    pub share: Option<Mid>,
    pub watch: Option<Mid>,
}

#[derive(Debug, Clone, Copy)]
struct Total {
    bytes: u64,
    at: Instant,
    bits_per_second: Option<u64>,
}

impl Total {
    fn next(previous: Option<&Total>, bytes: u64, at: Instant) -> Total {
        let bits_per_second = previous.filter(|p| at > p.at && bytes >= p.bytes).map(|p| {
            let secs = at.duration_since(p.at).as_secs_f64();
            ((bytes - p.bytes) as f64 * 8.0 / secs) as u64
        });
        Total {
            bytes,
            at,
            bits_per_second,
        }
    }
}

#[derive(Default)]
pub(super) struct StatsTally {
    peer: Option<PeerStats>,
    ingress: HashMap<Mid, (Total, u32)>,
    egress: HashMap<Mid, Total>,
    uplink_estimate: Option<u64>,
}

pub(super) type ShareOut = Option<((u32, u32), Arc<str>)>;

impl StatsTally {
    pub fn peer(&mut self, stats: PeerStats) {
        self.peer = Some(stats);
    }

    pub fn uplink_estimate(&mut self, bits_per_second: u64) {
        self.uplink_estimate = Some(bits_per_second);
    }

    pub fn ingress(&mut self, s: &MediaIngressStats) {
        let total = Total::next(
            self.ingress.get(&s.mid).map(|(t, _)| t),
            s.bytes,
            s.timestamp,
        );
        self.ingress.insert(s.mid, (total, s.jitter));
    }

    pub fn egress(&mut self, s: &MediaEgressStats) {
        let total = Total::next(self.egress.get(&s.mid), s.bytes, s.timestamp);
        self.egress.insert(s.mid, total);
    }

    pub fn snapshot(
        &self,
        mids: Mids,
        share: ShareOut,
        watched: Option<((u32, u32), bool)>,
        lost: u32,
    ) -> Option<VoiceStats> {
        let peer = self.peer.as_ref()?;
        let pair = peer.selected_candidate_pair.as_ref()?;
        let path = match pair.protocol {
            Protocol::Udp => MediaPath::Udp,
            _ => MediaPath::Tcp,
        };
        let rate_out = |mid: Mid| self.egress.get(&mid).and_then(|t| t.bits_per_second);
        let mut audio_in = Some(0);
        let mut jitter = None;
        for mid in mids.audio_in {
            if let Some((total, ticks)) = self.ingress.get(&mid) {
                audio_in = audio_in.zip(total.bits_per_second).map(|(a, b)| a + b);
                jitter = jitter.max(Some(u64::from(*ticks)));
            }
        }
        let video = |bits_per_second, size, encoder, hardware_decode, lost| VideoStats {
            bits_per_second,
            size,
            encoder,
            hardware_decode,
            lost,
        };
        Some(VoiceStats {
            path,
            sfu: pair.remote.addr,
            rtt: pair.current_round_trip_time.or(peer.rtt),
            loss_in: peer.ingress_loss_fraction.map(permille),
            loss_out: peer.egress_loss_fraction.map(permille),
            jitter: jitter
                .map(|t| std::time::Duration::from_micros(t * 1_000_000 / AUDIO_CLOCK_HZ)),
            audio_out: mids.uplink.and_then(rate_out),
            audio_in,
            uplink_estimate: self.uplink_estimate,
            share_out: mids.share.map(|mid| {
                let (size, encoder) = share.unzip();
                video(rate_out(mid), size, encoder, None, 0)
            }),
            share_in: mids.watch.map(|mid| {
                let rate = self.ingress.get(&mid).and_then(|(t, _)| t.bits_per_second);
                let (size, hardware) = watched.unzip();
                video(rate, size, None, hardware, lost)
            }),
        })
    }
}

fn permille(fraction: f32) -> u16 {
    (fraction.clamp(0.0, 1.0) * 1000.0).round() as u16
}
