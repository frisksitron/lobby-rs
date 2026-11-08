use std::time::Instant;

/// Maps a source's media times onto a downlink's monotonic one, re-anchored at each re-point.
#[derive(Debug, Default)]
pub(super) struct Timeline {
    offset: Option<i64>,
    last: Option<(u64, Instant)>,
}

impl Timeline {
    const STEP: i64 = 3000;
    /// The watch downlink carries H.264 only, whose RTP clock is 90 kHz.
    const RATE: f64 = 90_000.0;

    pub(super) fn reanchor(&mut self) {
        self.offset = None;
    }

    /// A re-anchored downlink resumes as far on as wallclock moved since its last frame (at
    /// least one frame), so its RTP clock keeps tracking real time across pauses.
    pub(super) fn map(&mut self, source: u64, now: Instant) -> u64 {
        let source = i64::try_from(source).unwrap_or(i64::MAX);
        let resume = |last: Option<(u64, Instant)>| {
            last.map(|(time, at)| {
                let elapsed = (now.saturating_duration_since(at).as_secs_f64() * Self::RATE) as i64;
                i64::try_from(time)
                    .unwrap_or(i64::MAX)
                    .saturating_add(elapsed.max(Self::STEP))
            })
        };
        let last = self.last;
        let offset = *self
            .offset
            .get_or_insert_with(|| resume(last).map_or(0, |at| at - source));
        let mapped = u64::try_from(source.saturating_add(offset)).unwrap_or(0);
        let mapped = match (self.last, resume(self.last)) {
            (Some((time, _)), Some(at)) if mapped <= time => {
                self.offset = Some(at - source);
                u64::try_from(at).unwrap_or(0)
            }
            _ => mapped,
        };
        self.last = Some((mapped, now));
        mapped
    }
}
