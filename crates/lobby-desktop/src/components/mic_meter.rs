//! The microphone's level meter: twenty segments on a dBFS scale, lit in lavender (the live
//! voice) and in coral where the level nears clipping, with the held peak as one lone lit segment
//! past the level. It takes the width it is given. Under it, [`mic_hint`] is one line kept for what
//! is wrong, so a hint or a failure moves nothing.

use gpui_kit::{
    App, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce, SharedString,
    Styled as _, TestSupportExt as _, Window, component::ActiveTheme as _, div,
};
use lobby_client::{MicQuality, MicReading, Zone, copy};

use super::truncated_text;
use crate::scale::u;

pub const SEGMENTS: usize = 20;
/// The level of the first segment's lower edge.
const SCALE_FLOOR_DB: f32 = -60.0;
/// What each segment adds: the scale runs from the floor to full scale.
const SEGMENT_DB: f32 = -SCALE_FLOOR_DB / SEGMENTS as f32;
const SEGMENT_HEIGHT: f32 = 8.;
const SEGMENT_GAP: f32 = 2.;
/// The hint's line, a `text_xs` line, reserved whether or not it says anything.
const HINT_HEIGHT: f32 = 16.;

/// How many segments the level lights, from the left.
pub fn lit_segments(db: f32) -> usize {
    (((db - SCALE_FLOOR_DB) / SEGMENT_DB).floor().max(0.0) as usize).min(SEGMENTS)
}

/// The segment a held peak marks, if it is on the scale.
fn peak_segment(db: f32) -> Option<usize> {
    (db >= SCALE_FLOOR_DB).then(|| lit_segments(db).min(SEGMENTS - 1))
}

/// The zone a segment belongs to, by the level at its middle.
fn zone_of(segment: usize) -> Zone {
    Zone::of(SCALE_FLOOR_DB + (segment as f32 + 0.5) * SEGMENT_DB)
}

/// What to tell the person about the level, if anything.
fn hint(quality: MicQuality) -> Option<&'static str> {
    match quality {
        MicQuality::Good => None,
        MicQuality::TooQuiet => Some(copy::MIC_TOO_QUIET),
        MicQuality::TooLoud => Some(copy::MIC_TOO_LOUD),
        MicQuality::Clipping => Some(copy::MIC_CLIPPING),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Meter {
    Level(MicReading),
    /// The microphone cannot be listened to: the bar stays dark and the reason takes the hint's
    /// line.
    Failed(SharedString),
}

#[derive(IntoElement)]
pub struct MicMeterBar {
    meter: Meter,
}

impl MicMeterBar {
    pub fn new(meter: Meter) -> Self {
        Self { meter }
    }
}

impl RenderOnce for MicMeterBar {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let (lit, peak) = match &self.meter {
            Meter::Level(reading) => (
                lit_segments(reading.level_db),
                peak_segment(reading.peak_db),
            ),
            Meter::Failed(_) => (0, None),
        };
        let colour = |segment: usize| match zone_of(segment) {
            Zone::Danger => theme.danger,
            Zone::Quiet | Zone::Good | Zone::Loud => theme.primary,
        };
        div()
            .id("mic-level")
            .flex_1()
            .min_w_0()
            .h(u(SEGMENT_HEIGHT))
            .flex()
            .gap(u(SEGMENT_GAP))
            .children((0..SEGMENTS).map(|i| {
                let on = i < lit || Some(i) == peak;
                div()
                    .flex_1()
                    .h_full()
                    .rounded(u(1.))
                    .bg(if on { colour(i) } else { theme.muted })
            }))
            .test_support()
    }
}

/// The line under the meter: why the microphone cannot be heard, else what is wrong with its
/// level, else, while Hear yourself is on, the reminder to use headphones.
pub fn mic_hint(meter: &Meter, monitoring: bool, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let line = div().w_full().h(u(HINT_HEIGHT)).text_xs();
    let (id, text, tone) = match meter {
        Meter::Failed(why) => ("mic-failure", Some(why.clone()), theme.danger),
        Meter::Level(reading) => match hint(reading.quality) {
            Some(text) if reading.quality == MicQuality::Clipping => {
                ("mic-hint", Some(text.into()), theme.danger)
            }
            Some(text) => ("mic-hint", Some(text.into()), theme.warning),
            None if monitoring => (
                "monitor-headphones",
                Some(copy::MONITOR_HEADPHONES.into()),
                theme.muted_foreground,
            ),
            None => ("mic-hint", None, theme.muted_foreground),
        },
    };
    line.children(text.map(|text| {
        div()
            .id(id)
            .text_color(tone)
            .child(truncated_text(format!("{id}-text"), text))
            .test_support()
    }))
}
