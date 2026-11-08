use std::time::Instant;

use lobby_protocol::SPEAKING_HANGOVER;

/// The server's one verdict on whether a Member speaks, from the V flag of their voice packets.
///
/// A packet's flag cannot flip the verdict at will: speaking ends only after the hangover, or on
/// an unvoiced packet once it has lasted that long, so it changes at most a few times a second
/// however fast the Member sends. A real client's flag already stays set through its own
/// hangover, so the second rule never delays a real end.
#[derive(Debug, Default)]
pub(super) struct Speech {
    speaking: Option<Speaking>,
}

#[derive(Debug, Clone, Copy)]
struct Speaking {
    since: Instant,
    until: Instant,
}

impl Speech {
    /// Returns the change, if any.
    pub(super) fn heard(&mut self, voiced: bool, now: Instant) -> Option<bool> {
        match (&mut self.speaking, voiced) {
            (None, true) => {
                self.speaking = Some(Speaking {
                    since: now,
                    until: now + SPEAKING_HANGOVER,
                });
                Some(true)
            }
            (Some(s), true) => {
                s.until = now + SPEAKING_HANGOVER;
                None
            }
            (Some(s), false) if now >= s.since + SPEAKING_HANGOVER => {
                self.speaking = None;
                Some(false)
            }
            _ => None,
        }
    }

    /// Whether the hangover after the last voiced packet ran out with nothing heard since.
    pub(super) fn expired(&mut self, now: Instant) -> bool {
        let expired = self.speaking.is_some_and(|s| s.until <= now);
        if expired {
            self.speaking = None;
        }
        expired
    }

    pub(super) fn clear(&mut self) {
        self.speaking = None;
    }

    pub(super) fn expires_at(&self) -> Option<Instant> {
        self.speaking.map(|s| s.until)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    const PACKET: Duration = Duration::from_millis(1);

    #[test]
    fn a_flag_flipping_every_packet_changes_speaking_a_few_times_a_second() {
        let start = Instant::now();
        let mut speech = Speech::default();
        let mut changes = 0;
        for n in 0..5000u32 {
            let now = start + PACKET * n;
            changes += usize::from(speech.heard(n % 2 == 0, now).is_some());
            changes += usize::from(speech.expired(now));
        }
        // 5 s of packets, one every millisecond: a change per hangover at most, where a change
        // per packet would be 5000.
        assert!(changes <= 5 * 10, "{changes} changes");
    }
}
