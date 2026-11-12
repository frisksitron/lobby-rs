//! The Voice foot: Mute, Deafen and Share Screen as one button group with Leave beside it, or Join
//! Voice in their place. One row of one height in every state, so nothing moves when Voice
//! connects. While it connects the group is already there, dimmed and inert, and Leave is live.
//!
//! The member sheet ends in it at [`FootSize::Sheet`]. The small window's Voice bar has it at
//! [`FootSize::Bar`].

use std::time::Duration;

use gpui_kit::{
    Animation, AnimationExt as _, App, ClickEvent, Div, FontWeight, Hsla, InteractiveElement as _,
    IntoElement, ParentElement as _, RenderOnce, Stateful, Styled as _, TestSupportExt as _,
    Window, assets::IconName, base::Selectable as _, div, prelude::FluentBuilder as _, px,
};

use super::{
    SURFACE_INNER_RADIUS, ShareControl, VoiceBarActions, VoiceBarModel, Waits, auth::ClickHandler,
    voice_bar::STRIP_INNER_RADIUS,
};
use crate::kit::{ActiveTheme as _, Colorize as _, Theme};
use crate::scale::{device_in, u};
use crate::ui::{self, Spinner};

const FOOT_GAP: f32 = 6.;
/// In the narrowest sheet Leave is 74 px wide: its icon and word fit only with less padding than
/// the kit's.
const SHEET_WORD_PAD: f32 = 4.;
/// Leave and Join in the bar, where they are as wide as their word.
const BAR_LEAVE_PAD: f32 = 14.;
const BAR_JOIN_PAD: f32 = 16.;
/// How much of the Voice lavender lifts the bar's plum buttons under the pointer and pressed.
const BAR_HOVER_TINT: f32 = 0.12;
const BAR_ACTIVE_TINT: f32 = 0.2;
/// The watch tile's plum is nearly the bar's own, so the bar's buttons lift it toward the text
/// colour enough to read as buttons (1.6:1 against the bar).
const BAR_REST_LIFT: f32 = 0.1;
/// Mute's ring when the Member talks muted: how far outside the button it sits, how long it plays
/// and how many times it swells.
const NUDGE_RING_GAP: f32 = 3.;
const NUDGE_DURATION: Duration = Duration::from_millis(2400);
const NUDGE_BREATHS: f32 = 3.;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FootSize {
    /// The member sheet's foot: Leave and Join fill the row and carry their phone icon.
    #[default]
    Sheet,
    /// The small window's Voice bar: a little shorter, with Leave and Join as wide as their word.
    Bar,
}

impl FootSize {
    pub const fn height(self) -> f32 {
        match self {
            Self::Sheet => 36.,
            Self::Bar => 34.,
        }
    }

    /// The group, Leave and Join round their outer corners alike, to fit what holds the foot.
    const fn radius(self) -> f32 {
        match self {
            Self::Sheet => SURFACE_INNER_RADIUS,
            Self::Bar => STRIP_INNER_RADIUS,
        }
    }

    const fn button_w(self) -> f32 {
        match self {
            Self::Sheet => 40.,
            Self::Bar => 38.,
        }
    }

    const fn fills(self) -> bool {
        matches!(self, Self::Sheet)
    }

    /// The group's fill at rest: the sheet's warm grey, or on the bar's purple a lifted plum.
    fn rest(self, theme: &Theme) -> Hsla {
        match self {
            Self::Sheet => theme.secondary,
            Self::Bar => theme
                .foreground
                .mix_oklab(theme.sidebar_primary, BAR_REST_LIFT),
        }
    }

    fn sharing_tint(self) -> f32 {
        match self {
            Self::Sheet => 0.24,
            Self::Bar => 0.3,
        }
    }
}

fn handler(h: &ClickHandler) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    let h = h.clone();
    move |e, w, cx| h(e, w, cx)
}

fn voice_group(
    model: &VoiceBarModel,
    actions: &VoiceBarActions,
    waits: Waits,
    size: FootSize,
    deafen_press: &ui::PressLink,
    cx: &App,
) -> Option<Stateful<Div>> {
    let radius = device_in(size.radius(), cx);
    let theme = cx.theme();
    let rest = match size {
        FootSize::Sheet => {
            ui::Custom::new(theme.button_secondary, theme.button_secondary_foreground)
                .hover(theme.button_secondary_hover)
                .active(theme.button_secondary_active)
                .disabled(theme.button_secondary)
        }
        FootSize::Bar => {
            let plum = FootSize::Bar.rest(theme);
            ui::Custom::new(plum, theme.sidebar_primary_foreground)
                .hover(theme.primary.mix_oklab(plum, BAR_HOVER_TINT))
                .active(theme.primary.mix_oklab(plum, BAR_ACTIVE_TINT))
                .disabled(plum)
        }
    };
    let switched_off = ui::Custom::new(theme.danger, theme.danger_foreground)
        .hover(theme.danger_hover)
        .disabled(theme.danger);
    let button = |id: &'static str| {
        ui::Button::new(id)
            .variant(ui::Variant::Custom(rest))
            .w(u(size.button_w()))
            .h(u(size.height()))
            .p_0()
    };
    let buttons = match model {
        VoiceBarModel::Idle { .. } => return None,
        VoiceBarModel::Connecting => [
            button("voice-mute")
                .accessibility_label("Mute")
                .icon(IconName::Mic)
                .tooltip("Mute"),
            button("voice-deafen")
                .accessibility_label("Deafen")
                .icon(IconName::Headphones)
                .tooltip("Deafen"),
            button("share-screen")
                .accessibility_label("Share Screen")
                .icon(IconName::ScreenShare)
                .tooltip("Share Screen"),
        ]
        .map(|b| b.disabled(true)),
        VoiceBarModel::InVoice {
            muted,
            deafened,
            share,
            reconnecting,
            ..
        } => {
            let (muted, deafened) = (*muted, *deafened);
            let mute = button("voice-mute")
                .accessibility_label(if muted { "Unmute" } else { "Mute" })
                .icon(if muted {
                    IconName::MicOff
                } else {
                    IconName::Mic
                })
                .when(muted, |b| {
                    b.variant(ui::Variant::Custom(switched_off)).selected(true)
                })
                .disabled(muted && waits.unmute)
                .tooltip(match (muted, waits.unmute) {
                    (true, true) => "You can unmute again in a moment",
                    (true, false) if deafened => "Unmute and undeafen",
                    (true, false) => "Unmute",
                    (false, _) => "Mute",
                })
                // Deafening mutes, so Deafen takes Mute down with it while Mute is up.
                .when(!muted, |b| b.pressed_with(deafen_press))
                .on_click(handler(&actions.mute));
            let deafen = button("voice-deafen")
                .accessibility_label(if deafened { "Undeafen" } else { "Deafen" })
                .icon(if deafened {
                    IconName::HeadphoneOff
                } else {
                    IconName::Headphones
                })
                .when(deafened, |b| {
                    b.variant(ui::Variant::Custom(switched_off)).selected(true)
                })
                .disabled(deafened && waits.unmute)
                .tooltip(match (deafened, waits.unmute) {
                    (true, true) => "You can undeafen again in a moment",
                    (true, false) => "Undeafen",
                    (false, _) => "Deafen",
                })
                .presses(deafen_press)
                .on_click(handler(&actions.deafen));
            [
                mute,
                deafen,
                share_button(share, *reconnecting, actions, size, button, cx),
            ]
        }
    };
    let [first, middle, last] = buttons.map(|b| {
        let on = b.is_selected();
        b.toggled(on)
    });
    Some(
        div()
            .id("voice-group")
            .flex()
            .items_center()
            .flex_shrink_0()
            .gap(px(1.)) // px: the hairline seam between the buttons
            .child(first.rounded_l(radius).rounded_r_none())
            .child(middle.rounded_none())
            .child(last.rounded_r(radius).rounded_l_none()),
    )
}

fn mute_nudges(model: &VoiceBarModel) -> u32 {
    match model {
        VoiceBarModel::InVoice { mute_nudges, .. } => *mute_nudges,
        _ => 0,
    }
}

/// Talking into a muted microphone rings Mute once, in the danger colour Mute already wears: three
/// breaths over 2.4 s that end at nothing. The ring is an overlay, so no control moves, and each
/// nudge is a new animation id, so it plays once per nudge however often the foot redraws.
fn nudged(group: Stateful<Div>, nudges: u32, size: FootSize, cx: &App) -> impl IntoElement {
    let ring = (nudges > 0).then(|| {
        let theme = cx.theme();
        div()
            .absolute()
            .top(u(-NUDGE_RING_GAP))
            .left(u(-NUDGE_RING_GAP))
            .w(u(size.button_w() + 2. * NUDGE_RING_GAP))
            .h(u(size.height() + 2. * NUDGE_RING_GAP))
            .rounded(device_in(size.radius() + NUDGE_RING_GAP, cx))
            .border_2()
            .border_color(theme.danger)
            .with_animation(
                ("mute-nudge", nudges as usize),
                Animation::new(NUDGE_DURATION),
                |ring, delta| {
                    ring.opacity((delta * std::f32::consts::PI * NUDGE_BREATHS).sin().abs())
                },
            )
    });
    div().relative().flex_shrink_0().child(group).children(ring)
}

/// Share Screen says what it does in its tooltip, and its icon stays. While a share starts, or
/// the media path is restored, a spinner takes the icon's place. A running share is stopped from
/// it in every state, so it is never a loading kit button, which would ignore the click.
fn share_button(
    share: &ShareControl,
    reconnecting: bool,
    actions: &VoiceBarActions,
    size: FootSize,
    button: impl Fn(&'static str) -> ui::Button,
    cx: &App,
) -> ui::Button {
    const DROPPED: &str = "The connection dropped. Getting Voice back…";
    let theme = cx.theme();
    let tint = theme
        .primary
        .mix_oklab(size.rest(theme), size.sharing_tint());
    let sharing = ui::Custom::new(tint, theme.primary);
    let spinner = || Spinner::new().size(u(ui::ICON_XS));
    let stop = |b: ui::Button| {
        b.accessibility_label("Stop sharing")
            .variant(ui::Variant::Custom(sharing))
            .selected(true)
            .on_click(handler(&actions.stop_share))
    };
    match share {
        ShareControl::Off if reconnecting => button("voice-reconnecting")
            .accessibility_label("Reconnecting")
            .child(spinner())
            .disabled(true)
            .tooltip(DROPPED),
        ShareControl::Starting { source } | ShareControl::Live { source } if reconnecting => {
            stop(button("share-stop"))
                .child(spinner())
                .tooltip(format!("{DROPPED} Click to stop sharing {source}."))
        }
        ShareControl::Off => {
            let b = button("share-screen")
                .accessibility_label("Share Screen")
                .icon(IconName::ScreenShare);
            match lobby_client::share_blocked() {
                Some(why) => b.disabled(true).tooltip(why.to_string()),
                None => b.tooltip("Share Screen").on_click(handler(&actions.share)),
            }
        }
        ShareControl::Starting { source } => stop(button("share-stop"))
            .child(spinner())
            .tooltip(format!("Starting {source}. Click to stop.")),
        ShareControl::Live { source } => stop(button("share-stop"))
            .icon(IconName::ScreenShare)
            .tooltip(format!(
                "Everyone in Voice can watch {source}. Click to stop."
            )),
    }
}

/// The words on Leave and Join say what they do, so only a reason they wait gets a tooltip.
fn word_button(id: &'static str, word: &'static str, icon: IconName, size: FootSize) -> ui::Button {
    ui::Button::new(id)
        .label(word)
        .h(u(size.height()))
        .font_weight(FontWeight::SEMIBOLD)
        .when(size.fills(), |b| {
            b.icon(icon).flex_1().min_w_0().px(u(SHEET_WORD_PAD))
        })
}

#[derive(IntoElement)]
pub struct VoiceFoot {
    model: VoiceBarModel,
    actions: VoiceBarActions,
    waits: Waits,
    size: FootSize,
}

impl VoiceFoot {
    pub fn new(model: VoiceBarModel, actions: VoiceBarActions) -> Self {
        Self {
            model,
            actions,
            waits: Waits::default(),
            size: FootSize::default(),
        }
    }

    pub fn waits(mut self, waits: Waits) -> Self {
        self.waits = waits;
        self
    }

    pub fn size(mut self, size: FootSize) -> Self {
        self.size = size;
        self
    }
}

impl RenderOnce for VoiceFoot {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (size, waits, actions) = (self.size, self.waits, &self.actions);
        let radius = device_in(size.radius(), cx);
        let row = div()
            .id("voice-controls")
            .debug_selector(|| "voice-controls".into())
            .h(u(size.height()))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(u(FOOT_GAP));
        match &self.model {
            VoiceBarModel::Idle { can_join } => row.child(
                word_button("voice-join", "Join Voice", IconName::Phone, size)
                    .variant(ui::Variant::Primary)
                    .rounded(radius)
                    .when(!size.fills(), |b| b.px(u(BAR_JOIN_PAD)))
                    .disabled(!can_join || waits.join.is_some())
                    .when(!can_join, |b| b.tooltip("Voice needs the Lobby connected"))
                    .when_some(waits.join.filter(|_| *can_join), |b, secs| {
                        b.tooltip(format!("You can join again in under {secs} s"))
                    })
                    .on_click(handler(&actions.join)),
            ),
            model => row
                .children({
                    let deafen_press = ui::PressLink::new("deafen-press", window, cx);
                    voice_group(model, actions, waits, size, &deafen_press, cx)
                        .map(|group| nudged(group, mute_nudges(model), size, cx))
                })
                .child(
                    word_button("voice-leave", "Leave", IconName::PhoneOff, size)
                        .accessibility_label("Leave Voice")
                        .variant(ui::Variant::Danger)
                        .rounded(radius)
                        .when(!size.fills(), |b| b.px(u(BAR_LEAVE_PAD)))
                        .on_click({
                            let leave = actions.leave.clone();
                            // Leave takes Join's place at once, so the second half of a
                            // double-click on Join lands here; it must not end the call it began.
                            move |e, w, cx| {
                                if e.click_count() < 2 {
                                    leave(e, w, cx);
                                }
                            }
                        }),
                ),
        }
        .test_support()
    }
}
