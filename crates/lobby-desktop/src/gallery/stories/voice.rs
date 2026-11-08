use std::{rc::Rc, time::Duration};

use gpui_kit::{
    AnyElement, App, Context, IntoElement as _, ParentElement as _, Styled as _, Window, div, px,
};

use lobby_client::{MediaPath, VideoStats, VoiceStats};

use crate::components::{
    FootSize, STATS_DIALOG_W, SURFACE_RADIUS, ShareControl, VoiceBarActions, VoiceBarModel,
    VoiceFoot, VoiceStatsList, Waits, card_surface, floating_sheet,
    voice_bar::{STRIP_HEIGHT, STRIP_PAD},
};
use crate::gallery::{Gallery, Section};
use crate::lobby_view::{SIDEBAR_SLIM_W, SIDEBAR_W};
use crate::scale::u;

use super::{caption, popover_frame, wrap};

/// The Voice bar's width in a 480 px window, less its two 16 px gutters.
pub(super) const BAR_W: f32 = 448.;
/// The room the bar leaves its foot in these stories: every state of it fits.
const BAR_FOOT_W: f32 = 232.;

pub(super) fn voice_actions() -> VoiceBarActions {
    let none: crate::components::auth::ClickHandler = Rc::new(|_, _, _| {});
    VoiceBarActions {
        join: none.clone(),
        leave: none.clone(),
        mute: none.clone(),
        deafen: none.clone(),
        share: none.clone(),
        stop_share: none,
    }
}

pub(super) fn out_of_voice() -> VoiceBarModel {
    VoiceBarModel::Idle { can_join: true }
}

pub(super) fn in_voice(muted: bool, share: ShareControl) -> VoiceBarModel {
    VoiceBarModel::InVoice {
        muted,
        deafened: false,
        members: 3,
        share,
        reconnecting: false,
        mute_nudges: 0,
    }
}

fn reconnecting(mut model: VoiceBarModel) -> VoiceBarModel {
    if let VoiceBarModel::InVoice { reconnecting, .. } = &mut model {
        *reconnecting = true;
    }
    model
}

/// Deafened is muted too.
pub(super) fn deafened_voice() -> VoiceBarModel {
    VoiceBarModel::InVoice {
        muted: true,
        deafened: true,
        members: 3,
        share: ShareControl::Off,
        reconnecting: false,
        mute_nudges: 0,
    }
}

pub(super) fn live() -> ShareControl {
    ShareControl::Live {
        source: "Display 1".into(),
    }
}

/// The foot where it stands: at the foot of a member sheet `sheet_w` wide, or at the right end of
/// the small window's Voice bar.
fn placed(
    model: VoiceBarModel,
    waits: Waits,
    size: FootSize,
    sheet_w: f32,
    cx: &App,
) -> AnyElement {
    let foot = VoiceFoot::new(model, voice_actions())
        .waits(waits)
        .size(size);
    match size {
        FootSize::Sheet => floating_sheet(cx)
            .w(u(sheet_w))
            .p_2()
            .child(foot)
            .into_any_element(),
        FootSize::Bar => div()
            .w(u(BAR_FOOT_W))
            .h(u(STRIP_HEIGHT))
            .px(u(STRIP_PAD))
            .flex()
            .items_center()
            .justify_end()
            .rounded(u(SURFACE_RADIUS))
            .bg(card_surface(cx))
            .child(foot)
            .into_any_element(),
    }
}

pub(super) fn voice_foot(
    _: &mut Gallery,
    _: &mut Window,
    cx: &mut Context<Gallery>,
) -> Vec<Section> {
    let none = Waits::default();
    let unmute_waits = Waits {
        unmute: true,
        ..none
    };
    let starting = ShareControl::Starting {
        source: "Display 1".into(),
    };
    let states = [
        ("Join", out_of_voice(), none),
        (
            "Join waits (not connected)",
            VoiceBarModel::Idle { can_join: false },
            none,
        ),
        (
            "Join waits (cooldown)",
            out_of_voice(),
            Waits {
                join: Some(15),
                ..none
            },
        ),
        ("Connecting", VoiceBarModel::Connecting, none),
        ("In Voice", in_voice(false, ShareControl::Off), none),
        ("Muted", in_voice(true, ShareControl::Off), none),
        ("Deafened", deafened_voice(), none),
        ("Undeafen waits (cooldown)", deafened_voice(), unmute_waits),
        (
            "Reconnecting",
            reconnecting(in_voice(false, ShareControl::Off)),
            none,
        ),
        (
            "Reconnecting while sharing",
            reconnecting(in_voice(false, live())),
            none,
        ),
        (
            "Unmute waits (cooldown)",
            in_voice(true, ShareControl::Off),
            unmute_waits,
        ),
        ("Share starting", in_voice(false, starting), none),
        ("Sharing", in_voice(false, live()), none),
        ("Muted and sharing", in_voice(true, live()), none),
    ];
    let cells = |size: FootSize, cx: &App| {
        wrap(states.clone().map(|(name, model, waits)| {
            caption(name, placed(model, waits, size, SIDEBAR_SLIM_W, cx), cx)
        }))
    };
    vec![
        Section::new(
            "sheet",
            "At the foot of the member sheet, 220 wide: one 36 px row in every state. Leave and Join \
             Voice fill what the group leaves",
            cells(FootSize::Sheet, cx),
        ),
        Section::new(
            "bar",
            "At the right end of the small window's Voice bar: 34 px, with Leave and Join Voice as \
             wide as their word and the group in the watch tile's plum",
            cells(FootSize::Bar, cx),
        ),
    ]
}

pub(super) fn join_to_leave(
    _: &mut Gallery,
    _: &mut Window,
    cx: &mut Context<Gallery>,
) -> Vec<Section> {
    let none = Waits::default();
    let ends = |size: FootSize, sheet_w: f32, cx: &App| {
        wrap([
            caption("Out", placed(out_of_voice(), none, size, sheet_w, cx), cx),
            caption(
                "Connecting",
                placed(VoiceBarModel::Connecting, none, size, sheet_w, cx),
                cx,
            ),
            caption(
                "In Voice",
                placed(in_voice(false, ShareControl::Off), none, size, sheet_w, cx),
                cx,
            ),
        ])
    };
    vec![
        Section::new(
            "sheet",
            "In the member sheet, 220 wide. Out is one Join Voice button. In is the group of mute, \
             deafen and Share Screen with Leave, which stands where Join Voice's right end was",
            ends(FootSize::Sheet, SIDEBAR_SLIM_W, cx),
        ),
        Section::new(
            "sheet-wide",
            "The same in the wider sheet, 264, where Join Voice and Leave have more to fill",
            ends(FootSize::Sheet, SIDEBAR_W, cx),
        ),
        Section::new(
            "bar",
            "In the small window's Voice bar, where Join Voice and Leave are as wide as their word",
            ends(FootSize::Bar, SIDEBAR_SLIM_W, cx),
        ),
    ]
}

pub(super) fn source_picker(
    g: &mut Gallery,
    _: &mut Window,
    cx: &mut Context<Gallery>,
) -> Vec<Section> {
    let [loading, sources, boxes, failed] = &g.held.pickers;
    let framed = |p: &gpui_kit::Entity<crate::components::PickerView>, cx: &App| {
        popover_frame(div().w(px(480.)).child(p.clone()), cx)
    };
    vec![
        Section::new(
            "sources",
            "Screens, windows and test patterns with their thumbnails; the long-titled window gave none and keeps its icon, and Signal, which blocks capture, is greyed (scroll)",
            framed(sources, cx),
        ),
        Section::new(
            "boxes",
            "The moment the list shows: every picture box is reserved and holds its icon until a thumbnail arrives",
            framed(boxes, cx),
        ),
        Section::new("loading", "Looking for sources", framed(loading, cx)),
        Section::new("failed", "Capture not available", framed(failed, cx)),
    ]
}

fn sample_stats() -> VoiceStats {
    VoiceStats {
        path: MediaPath::Udp,
        sfu: "203.0.113.7:50000".parse().expect("an address"),
        rtt: Some(Duration::from_millis(24)),
        loss_in: Some(0),
        loss_out: Some(4),
        jitter: Some(Duration::from_micros(3_200)),
        audio_out: Some(131_000),
        audio_in: Some(262_400),
        uplink_estimate: None,
        share_out: None,
        share_in: None,
    }
}

pub(super) fn voice_stats(
    _: &mut Gallery,
    _: &mut Window,
    cx: &mut Context<Gallery>,
) -> Vec<Section> {
    let dialog = |stats: Option<VoiceStats>, cx: &App| {
        popover_frame(
            div()
                .w(px(STATS_DIALOG_W - 24.))
                .flex()
                .flex_col()
                .gap_3()
                .child(
                    div()
                        .text_base()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .child("Voice stats"),
                )
                .child(VoiceStatsList::new(stats)),
            cx,
        )
    };
    let sharing = VoiceStats {
        path: MediaPath::Tcp,
        rtt: Some(Duration::from_micros(8_400)),
        loss_in: None,
        share_out: Some(VideoStats {
            bits_per_second: Some(2_450_000),
            size: Some((1920, 1080)),
            encoder: Some("NVIDIA H.264 Encoder MFT".into()),
            hardware_decode: None,
            lost: 0,
        }),
        share_in: Some(VideoStats {
            bits_per_second: Some(1_180_000),
            size: Some((1280, 720)),
            encoder: None,
            hardware_decode: Some(true),
            lost: 0,
        }),
        ..sample_stats()
    };
    vec![
        Section::new(
            "stats-voice",
            "In Voice over UDP",
            dialog(Some(sample_stats()), cx),
        ),
        Section::new(
            "stats-share",
            "Over TCP, sharing and watching",
            dialog(Some(sharing), cx),
        ),
        Section::new(
            "stats-measuring",
            "Before the first measurement",
            dialog(None, cx),
        ),
    ]
}
