//! What a Screen Share sends: one H.264 stream at the quality its Sharer picks, at 60 fps.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The most a share sends: the source scaled down to fit the quality's box, never up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Quality {
    #[serde(rename = "1080")]
    UpTo1080,
    #[serde(rename = "1440")]
    UpTo1440,
    #[serde(rename = "2160")]
    UpTo2160,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub quality: Quality,
    pub box_size: (u32, u32),
    /// Where more bits stop buying sharpness on NVIDIA's encoder at 60 fps, for scrolling text
    /// and a moving texture.
    pub bps: u32,
}

/// Smallest first, one row per [`Quality`] in its order.
pub const QUALITIES: [Row; 3] = [
    Row {
        quality: Quality::UpTo1080,
        box_size: (1920, 1080),
        bps: 6_000_000,
    },
    Row {
        quality: Quality::UpTo1440,
        box_size: (2560, 1440),
        bps: 10_000_000,
    },
    Row {
        quality: Quality::UpTo2160,
        box_size: (3840, 2160),
        bps: 20_000_000,
    },
];

const _: () = {
    let mut i = 0;
    while i < QUALITIES.len() {
        assert!(
            QUALITIES[i].quality as usize == i,
            "QUALITIES in the order of Quality"
        );
        i += 1;
    }
};

impl Quality {
    pub const fn row(self) -> Row {
        QUALITIES[self as usize]
    }

    pub const fn box_size(self) -> (u32, u32) {
        self.row().box_size
    }

    pub const fn bps(self) -> u32 {
        self.row().bps
    }

    pub fn bps_from(self, size: (u32, u32)) -> u32 {
        bps_for(fit(size, self.box_size()))
    }
}

/// The bitrate of a stream encoded at `size`: the bitrate of the smallest box that holds it, so
/// a 1440p monitor shared at up to 4K sends 1440p's.
pub fn bps_for(size: (u32, u32)) -> u32 {
    QUALITIES
        .iter()
        .find(|r| size.0 <= r.box_size.0 && size.1 <= r.box_size.1)
        .unwrap_or(&QUALITIES[QUALITIES.len() - 1])
        .bps
}

/// The size `src` is encoded at within `max`: scaled down to fit, never up, in even pixels.
pub fn fit(src: (u32, u32), max: (u32, u32)) -> (u32, u32) {
    let scale = (f64::from(max.0) / f64::from(src.0.max(1)))
        .min(f64::from(max.1) / f64::from(src.1.max(1)))
        .min(1.0);
    let even = |v: f64| ((v as u32) & !1).max(2);
    (
        even(f64::from(src.0) * scale),
        even(f64::from(src.1) * scale),
    )
}

pub const FPS: u32 = 60;
pub const FRAME_INTERVAL: Duration = Duration::from_micros(16_667);
/// A picture captured at least this long after the last one sent goes out at once: three
/// quarters of the interval lets a 60 fps source through whole, however late the encoder's
/// thread wakes.
pub const MIN_SPACING: Duration = Duration::from_nanos(FRAME_INTERVAL.as_nanos() as u64 * 3 / 4);

/// The H.264 levels a share's encoder and both SDP ends name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum H264Level {
    L4_2,
    L5_2,
}

/// 2160p at 60 fps is 1,944,000 macroblocks a second: over level 5.1's 983,040, within 5.2's
/// 2,073,600 (H.264 Table A-1).
pub const H264_LEVEL: H264Level = H264Level::L5_2;

impl H264Level {
    pub const fn level_idc(self) -> u8 {
        match self {
            H264Level::L4_2 => 42,
            H264Level::L5_2 => 52,
        }
    }

    /// The `profile-level-id` both SDP ends advertise: constrained baseline at this level.
    pub const fn profile_level_id(self) -> u32 {
        0x42e000 | self.level_idc() as u32
    }
}

/// A bandwidth estimate is told to probe for this share of the bitrate it should carry. str0m's
/// estimate settles near 1.3 × what it is told on an open link, and its pacer sends at about
/// 1.1 × the estimate, so a hardware encoder's bursts get room over its mean rate.
pub const DESIRE_PERCENT: u64 = 150;

/// Where every peer's bandwidth estimate starts: what the top quality probes for. str0m's pacer
/// holds video to the estimate, so one that started lower held a share's first frames back.
pub const BWE_START_BPS: u64 = desired_bps(QUALITIES[QUALITIES.len() - 1].bps as u64);

pub const fn desired_bps(bps: u64) -> u64 {
    bps * DESIRE_PERCENT / 100
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stream_is_sent_at_the_bitrate_of_the_smallest_box_that_holds_it() {
        let sent = [
            ((1280, 720), 6_000_000),
            ((1920, 1080), 6_000_000),
            ((1920, 1200), 10_000_000),
            ((2560, 1440), 10_000_000),
            ((3440, 1440), 20_000_000),
            ((3840, 2160), 20_000_000),
        ];
        for (size, bps) in sent {
            assert_eq!(bps_for(size), bps, "{size:?}");
        }
    }

    #[test]
    fn the_level_names_itself_in_the_sdp() {
        assert_eq!(H264_LEVEL.profile_level_id(), 0x42e034);
        assert_eq!(H264Level::L4_2.profile_level_id(), 0x42e02a);
    }
}
