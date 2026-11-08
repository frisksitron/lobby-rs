//! The microphone's level meter on the rack's MIC module: twenty segments on a dBFS scale,
//! coloured by where the level stands (quiet, good, loud, near clipping), with the held peak as
//! one lone lit segment past the level. Under it, one reserved line says what is wrong with the
//! level, only while it is. When the microphone cannot be listened to, the reason takes the
//! meter's place, in the same room.

use gpui_kit::{
    App, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce, SharedString,
    Styled as _, TestSupportExt as _, Window, component::ActiveTheme as _, div,
};
use lobby_client::{MicQuality, MicReading, Zone, copy};

use crate::scale::u;

pub const SEGMENTS: usize = 20;
/// The level of the first segment's lower edge.
const SCALE_FLOOR_DB: f32 = -60.0;
/// What each segment adds: the scale runs from the floor to full scale.
const SEGMENT_DB: f32 = -SCALE_FLOOR_DB / SEGMENTS as f32;
const SEGMENT_HEIGHT: f32 = 6.;
const SEGMENT_GAP: f32 = 2.;
const HINT_GAP: f32 = 4.;
/// The bar, the gap and the hint's line: reserved whole, so a hint or a failure moves nothing.
pub const METER_HEIGHT: f32 = 30.;

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
    /// The microphone cannot be listened to: the reason replaces the bar.
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
        let room = div().w_full().h(u(METER_HEIGHT)).flex_shrink_0();
        let reading = match self.meter {
            Meter::Failed(why) => {
                return room.child(
                    div()
                        .id("mic-failure")
                        .text_xs()
                        .line_clamp(2)
                        .text_color(theme.danger)
                        .child(why)
                        .test_support(),
                );
            }
            Meter::Level(reading) => reading,
        };
        let lit = lit_segments(reading.level_db);
        let peak = peak_segment(reading.peak_db);
        let colour = |segment: usize| match zone_of(segment) {
            Zone::Quiet => theme.muted_foreground,
            Zone::Good => theme.success,
            Zone::Loud => theme.warning,
            Zone::Danger => theme.danger,
        };
        let segments = div()
            .id("mic-level")
            .w_full()
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
            .test_support();
        let hint = hint(reading.quality).map(|text| {
            let tone = if reading.quality == MicQuality::Clipping {
                theme.danger
            } else {
                theme.warning
            };
            div()
                .id("mic-hint")
                .text_xs()
                .truncate()
                .text_color(tone)
                .child(text)
                .test_support()
        });
        room.flex()
            .flex_col()
            .gap(u(HINT_GAP))
            .child(segments)
            .children(hint)
    }
}
