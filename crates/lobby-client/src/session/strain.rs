use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use lobby_protocol::{WatchDropped, encoder::bps_for};

use crate::{
    media::{ShareLoad, VoiceStats, WatchLoad},
    state::{Shortfall, StrainCause, StrainRole, StrainWarning},
};

pub(super) const WINDOW: Duration = Duration::from_secs(5);
pub(super) const SHOW_AT: usize = 3;
pub(super) const HEALTHY_FOR: Duration = Duration::from_secs(3);
const UNSHOWN_FRAMES: u64 = 6;
/// A Sharer whose estimate is below the quality's target is short only when what it sends fills
/// this share of the estimate: a still screen sends little and fits a thin uplink.
const UPLINK_FULL_PERCENT: u64 = 80;
/// A capture that makes this many pictures a second is moving at the share's pace; the encoder
/// is judged only then, because a still screen makes few.
const MOVING_FPS: u32 = 50;
const ENCODER_MIN_FPS: u32 = 50;

#[derive(Debug, Default)]
struct Gauge {
    on: bool,
    seconds: VecDeque<(Instant, bool)>,
}

impl Gauge {
    fn feed(&mut self, now: Instant, bad: bool) {
        self.seconds.push_back((now, bad));
        while self
            .seconds
            .front()
            .is_some_and(|(t, _)| now.saturating_duration_since(*t) >= WINDOW)
        {
            self.seconds.pop_front();
        }
        let mut bad = self.seconds.iter().filter(|(_, bad)| *bad);
        self.on = if self.on {
            bad.any(|(t, _)| now.saturating_duration_since(*t) < HEALTHY_FOR)
        } else {
            bad.count() >= SHOW_AT
        };
    }
}

#[derive(Debug)]
pub(super) struct Strain {
    role: StrainRole,
    network: Gauge,
    computer: Gauge,
    shortfall: Option<Shortfall>,
    dropped: u32,
    downlink_bps: Option<u64>,
}

impl Strain {
    pub fn viewer() -> Strain {
        Strain::new(StrainRole::Viewer)
    }

    pub fn sharer() -> Strain {
        Strain::new(StrainRole::Sharer)
    }

    fn new(role: StrainRole) -> Strain {
        Strain {
            role,
            network: Gauge::default(),
            computer: Gauge::default(),
            shortfall: None,
            dropped: 0,
            downlink_bps: None,
        }
    }

    /// The warning to show: the link's before the computer's, since a computer starved by the
    /// link is not the cause.
    pub fn warning(&self) -> Option<StrainWarning> {
        let cause = if self.network.on {
            StrainCause::Network(self.shortfall)
        } else if self.computer.on {
            StrainCause::Computer
        } else {
            return None;
        };
        Some(StrainWarning {
            role: self.role,
            cause,
        })
    }

    pub fn on_dropped(&mut self, dropped: &WatchDropped) {
        self.dropped += dropped.frames;
        self.downlink_bps = dropped.downlink_bps;
    }

    pub fn on_stats(&mut self, now: Instant, stats: &VoiceStats) {
        let (video, reaching, lost) = match self.role {
            StrainRole::Viewer => {
                let dropped = std::mem::take(&mut self.dropped) > 0;
                let reaching = std::mem::take(&mut self.downlink_bps).filter(|_| dropped);
                let video = stats.share_in.as_ref();
                (
                    video,
                    reaching,
                    dropped || video.is_some_and(|v| v.lost > 0),
                )
            }
            StrainRole::Sharer => (stats.share_out.as_ref(), stats.uplink_estimate, false),
        };
        let needs = video.and_then(|v| v.size).map(|s| u64::from(bps_for(s)));
        let short = reaching
            .zip(needs)
            .map(|(reaching, needs)| Shortfall { reaching, needs })
            .filter(|s| s.reaching < s.needs);
        let uplink_full = video
            .and_then(|v| v.bits_per_second)
            .zip(reaching)
            .is_some_and(|(sending, reaching)| sending * 100 >= reaching * UPLINK_FULL_PERCENT);
        let bad = lost || (self.role == StrainRole::Sharer && short.is_some() && uplink_full);
        match short {
            Some(short) if bad => self.shortfall = Some(short),
            _ if self.shortfall.map(|s| s.needs) != needs => self.shortfall = None,
            _ => {}
        }
        self.network.feed(now, bad);
        if !self.network.on {
            self.shortfall = None;
        }
    }

    pub fn on_watch_load(&mut self, now: Instant, load: &WatchLoad) {
        let undecoded = load.frames.saturating_sub(load.pictures);
        let unshown = u64::from(undecoded) + load.ui_dropped;
        self.computer
            .feed(now, load.lost > 0 || unshown >= UNSHOWN_FRAMES);
    }

    pub fn on_share_load(&mut self, now: Instant, load: &ShareLoad) {
        let behind = load.offered_fps() >= MOVING_FPS && load.encoded_fps() < ENCODER_MIN_FPS;
        self.computer.feed(now, behind);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::{MediaPath, VideoStats};

    fn stats(size: (u32, u32), lost: u32, uplink: Option<u64>) -> VoiceStats {
        let video = VideoStats {
            bits_per_second: Some(1_000_000),
            size: Some(size),
            encoder: None,
            hardware_decode: Some(true),
            lost,
        };
        VoiceStats {
            path: MediaPath::Udp,
            sfu: "127.0.0.1:1".parse().unwrap(),
            rtt: None,
            loss_in: Some(0),
            loss_out: None,
            jitter: None,
            audio_out: None,
            audio_in: None,
            uplink_estimate: uplink,
            share_out: Some(video.clone()),
            share_in: Some(video),
        }
    }

    const UHD: (u32, u32) = (3840, 2160);

    fn dropped(frames: u32) -> WatchDropped {
        WatchDropped {
            frames,
            downlink_bps: Some(4_700_000),
        }
    }

    fn network(shortfall: Option<Shortfall>, role: StrainRole) -> Option<StrainWarning> {
        Some(StrainWarning {
            role,
            cause: StrainCause::Network(shortfall),
        })
    }

    #[test]
    fn a_warning_shows_after_most_of_the_window_is_bad_and_clears_once_it_is_healthy() {
        let start = Instant::now();
        let at = |s: u64| start + Duration::from_secs(s);
        let short = Some(Shortfall {
            reaching: 4_700_000,
            needs: 20_000_000,
        });
        let mut viewer = Strain::viewer();
        for (s, sfu_drops) in [(0, true), (1, false), (2, true)] {
            if sfu_drops {
                viewer.on_dropped(&dropped(58));
            }
            viewer.on_stats(at(s), &stats(UHD, 0, None));
            assert_eq!(viewer.warning(), None, "{s} s in");
        }
        viewer.on_dropped(&dropped(60));
        viewer.on_stats(at(3), &stats(UHD, 0, None));
        assert_eq!(viewer.warning(), network(short, StrainRole::Viewer));
        viewer.on_stats(at(4), &stats(UHD, 3, None));
        assert_eq!(viewer.warning(), network(short, StrainRole::Viewer));
        viewer.on_stats(at(5), &stats((1920, 1080), 2, None));
        assert_eq!(viewer.warning(), network(None, StrainRole::Viewer));
        for s in 6..8 {
            viewer.on_stats(at(s), &stats((1920, 1080), 0, None));
            assert!(viewer.warning().is_some(), "healthy since 6 s, at {s} s");
        }
        viewer.on_stats(at(8), &stats((1920, 1080), 0, None));
        assert_eq!(viewer.warning(), None, "healthy for three seconds");

        let decoding = |frames, pictures, lost| WatchLoad {
            window: Duration::from_secs(1),
            decode_busy: Duration::ZERO,
            convert_busy: Duration::ZERO,
            frames,
            pictures,
            lost,
            ui_dropped: 0,
        };
        let mut viewer = Strain::viewer();
        viewer.on_watch_load(at(0), &decoding(60, 40, 0));
        viewer.on_watch_load(at(1), &decoding(5, 5, 30));
        viewer.on_watch_load(at(2), &decoding(60, 52, 0));
        assert_eq!(
            viewer.warning().map(|w| w.cause),
            Some(StrainCause::Computer)
        );

        let mut viewer = Strain::viewer();
        let slideshow = WatchLoad {
            ui_dropped: 50,
            ..decoding(60, 60, 0)
        };
        for s in 0..SHOW_AT as u64 {
            viewer.on_watch_load(at(s), &slideshow);
        }
        assert_eq!(
            viewer.warning().map(|w| w.cause),
            Some(StrainCause::Computer),
            "pictures the UI never showed"
        );

        let sending = |bits_per_second, uplink| {
            let mut voice = stats(UHD, 0, Some(uplink));
            voice.share_out.as_mut().unwrap().bits_per_second = Some(bits_per_second);
            voice
        };
        let mut sharer = Strain::sharer();
        for s in 0..5 {
            sharer.on_stats(at(s), &sending(1_000_000, 4_000_000));
            assert_eq!(sharer.warning(), None, "a still screen fits, at {s} s");
        }
        for s in 5..5 + SHOW_AT as u64 {
            sharer.on_stats(at(s), &sending(8_000_000, 8_000_000));
        }
        let upload = Some(Shortfall {
            reaching: 8_000_000,
            needs: 20_000_000,
        });
        assert_eq!(sharer.warning(), network(upload, StrainRole::Sharer));

        let encoding = |offered, encoded| ShareLoad {
            window: Duration::from_secs(1),
            offered,
            encoded,
        };
        let mut sharer = Strain::sharer();
        for s in 0..SHOW_AT as u64 {
            sharer.on_share_load(at(s), &encoding(60, 31));
        }
        assert_eq!(
            sharer.warning().map(|w| w.cause),
            Some(StrainCause::Computer)
        );
        for s in 3..5 {
            sharer.on_share_load(at(s), &encoding(3, 3));
            assert!(sharer.warning().is_some(), "still since 3 s, at {s} s");
        }
        sharer.on_share_load(at(5), &encoding(3, 3));
        assert_eq!(sharer.warning(), None, "a still screen says nothing");
    }
}
