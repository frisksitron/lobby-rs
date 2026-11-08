use std::time::Duration;

use tokio::time::Instant;

use crate::limits::TokenBucket;

/// Packets a peer's voice may send per second. Real Opus is 50 (20 ms frames); this leaves room
/// for the 10 ms frames and bursts after a stall that a client may send, and it is far below what
/// an abuser needs to make the SFU encrypt every packet for 24 listeners.
const AUDIO_PACKETS_PER_SEC: u32 = 150;
/// Payload bytes a peer's voice may send per second: 96 kB, 3x the Studio profile's 256 kbit/s.
const AUDIO_BYTES_PER_SEC: u32 = 96_000;

/// What a peer's voice uplink may send before the SFU fans it out: a second of each rate, refilled
/// at that rate. A packet over either budget is dropped.
#[derive(Debug)]
pub(super) struct AudioBudget {
    packets: TokenBucket,
    bytes: TokenBucket,
}

impl AudioBudget {
    pub(super) fn new() -> Self {
        let per_sec = |rate: u32| TokenBucket::new(Duration::from_secs(1) / rate, rate);
        Self {
            packets: per_sec(AUDIO_PACKETS_PER_SEC),
            bytes: per_sec(AUDIO_BYTES_PER_SEC),
        }
    }

    /// Whether a packet of `len` payload bytes is within both budgets, which it then spends. Both
    /// are the peer's own, so a packet the bytes refuse may still spend a packet.
    pub(super) fn admit(&mut self, len: usize, now: Instant) -> bool {
        let len = u32::try_from(len).unwrap_or(u32::MAX);
        self.packets.try_acquire(now).is_ok() && self.bytes.try_spend(len, now).is_ok()
    }
}
