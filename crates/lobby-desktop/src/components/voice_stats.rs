//! The Voice stats: how this client's media reaches the SFU, as the last second measured it.
//! Opened from the Member's own panel while In Voice.

use std::time::Duration;

use gpui_kit::{
    App, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    RenderOnce, SharedString, Styled as _, TestSupportExt as _, Window, div,
    prelude::FluentBuilder as _,
};
use lobby_client::{MediaPath, VideoStats, VoiceStats};

use crate::kit::ActiveTheme as _;
use crate::scale::u;
use crate::session_entity::SessionEntity;

pub const STATS_DIALOG_W: f32 = 360.;
const LABEL_W: f32 = 92.;

pub fn stats_dialog_width(window_w: f32) -> f32 {
    (window_w - 32.).min(STATS_DIALOG_W)
}

/// Each line of the list, label and value, in the order shown.
pub fn stats_lines(stats: &VoiceStats) -> Vec<(&'static str, String)> {
    let path = match stats.path {
        MediaPath::Udp => "UDP",
        MediaPath::Tcp => "TCP",
    };
    let loss =
        |l: Option<u16>| l.map_or("–".to_owned(), |l| format!("{:.1} %", f32::from(l) / 10.));
    let mut lines = vec![
        ("Connection", format!("{path} to {}", stats.sfu)),
        ("Round trip", stats.rtt.map_or("–".to_owned(), millis)),
        (
            "Packet loss",
            format!("{} in · {} out", loss(stats.loss_in), loss(stats.loss_out)),
        ),
        ("Jitter", stats.jitter.map_or("–".to_owned(), millis)),
        ("Audio out", bitrate(stats.audio_out)),
        ("Audio in", bitrate(stats.audio_in)),
    ];
    if let Some(v) = &stats.share_out {
        lines.push(("Sharing", video(v)));
        if let Some(encoder) = &v.encoder {
            lines.push(("Encoder", encoder.to_string()));
        }
    }
    if let Some(v) = &stats.share_in {
        lines.push(("Watching", video(v)));
        if let Some(hardware) = v.hardware_decode {
            lines.push((
                "Decoder",
                if hardware { "Hardware" } else { "Software" }.into(),
            ));
        }
    }
    lines
}

fn millis(d: Duration) -> String {
    let ms = d.as_secs_f64() * 1000.;
    if ms < 10. {
        format!("{ms:.1} ms")
    } else {
        format!("{ms:.0} ms")
    }
}

fn bitrate(bits_per_second: Option<u64>) -> String {
    match bits_per_second {
        None => "–".to_owned(),
        Some(bps) if bps >= 1_000_000 => format!("{:.1} Mbps", bps as f64 / 1e6),
        Some(bps) => format!("{} kbps", (bps as f64 / 1e3).round()),
    }
}

fn video(v: &VideoStats) -> String {
    let mut parts = vec!["H.264".to_owned()];
    parts.extend(v.size.map(|(w, h)| format!("{w}×{h}")));
    parts.extend(v.bits_per_second.map(|bps| bitrate(Some(bps))));
    parts.join(" · ")
}

/// `None` stats: Voice has no path to the SFU yet, or not for a second.
#[derive(IntoElement)]
pub struct VoiceStatsList {
    stats: Option<VoiceStats>,
}

impl VoiceStatsList {
    pub fn new(stats: Option<VoiceStats>) -> Self {
        Self { stats }
    }
}

impl RenderOnce for VoiceStatsList {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let body = match &self.stats {
            None => div()
                .py_4()
                .text_sm()
                .text_color(muted)
                .child("Measuring…")
                .into_any_element(),
            Some(stats) => {
                let theme = cx.theme();
                let (edge, label_ink) = (theme.border, theme.description_list_label_foreground);
                let label_fill = theme.tokens.description_list_label;
                let lines = stats_lines(stats);
                let last = lines.len().saturating_sub(1);
                div()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(edge)
                    .children(lines.into_iter().enumerate().map(|(ix, (label, value))| {
                        div()
                            .flex()
                            .when(ix != last, |row| row.border_b_1().border_color(edge))
                            .child(
                                div()
                                    .w(u(LABEL_W))
                                    .flex_shrink_0()
                                    .px_1()
                                    .py_0p5()
                                    .text_sm()
                                    .text_color(label_ink)
                                    .bg(label_fill)
                                    .border_r_1()
                                    .border_color(edge)
                                    .child(label),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .overflow_hidden()
                                    .px_1()
                                    .py_0p5()
                                    .text_sm()
                                    .child(SharedString::from(value)),
                            )
                    }))
                    .into_any_element()
            }
        };
        div()
            .id("voice-stats")
            .debug_selector(|| "voice-stats".into())
            .w_full()
            .child(body)
            .test_support()
    }
}

/// The dialog's body: redrawn as the Session takes each second's stats.
pub struct VoiceStatsDialog {
    session: Entity<SessionEntity>,
}

impl VoiceStatsDialog {
    pub fn new(session: Entity<SessionEntity>, cx: &mut Context<Self>) -> Self {
        cx.observe(&session, |_, _, cx| cx.notify()).detach();
        Self { session }
    }
}

impl Render for VoiceStatsDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let stats = self
            .session
            .read(cx)
            .state()
            .and_then(|s| s.voice.stats.clone());
        VoiceStatsList::new(stats)
    }
}
