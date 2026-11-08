use std::time::{Duration, Instant};

pub const AUDIO_WINDOW: Duration = Duration::from_secs(2);

/// The Voice the SFU forwards one peer, in bits per second over the last whole window. Bytes are
/// RTP payload, what str0m's BWE counts too.
#[derive(Debug)]
pub(crate) struct AudioRate {
    since: Instant,
    bytes: u64,
    bps: u64,
}

impl AudioRate {
    pub fn new(now: Instant) -> Self {
        Self {
            since: now,
            bytes: 0,
            bps: 0,
        }
    }

    pub fn sent(&mut self, bytes: usize) {
        self.bytes += bytes as u64;
    }

    /// Closes the window once [`AUDIO_WINDOW`] has passed.
    pub fn roll(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.since);
        if elapsed < AUDIO_WINDOW {
            return;
        }
        let bps = u128::from(self.bytes) * 8 * 1_000_000 / elapsed.as_micros();
        self.bps = u64::try_from(bps).unwrap_or(u64::MAX);
        self.bytes = 0;
        self.since = now;
    }

    pub fn bps(&self) -> u64 {
        self.bps
    }
}
