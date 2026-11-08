//! What the Voice controls show, and the Voice bar above the composer in a folded window. The
//! controls themselves are the [`VoiceFoot`] at the member sheet's foot.
//!
//! Share Screen stands where Join Voice's middle was, so the Lobby view drops a click on it right
//! after Voice connects (the second half of a double-click on Join).
//!
//! In a folded window the sidebar is hidden, so the same controls live in the [`VoiceStrip`] above
//! the composer, beside a [`VoiceFace`] for everyone in Voice.
use std::time::Instant;

use gpui_kit::{
    App, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce, SharedString,
    Styled as _, TestSupportExt as _, Window, component::ActiveTheme as _, div, linear_color_stop,
    linear_gradient,
};
use lobby_client::{LobbyState, Phase, VoicePhase};

use super::{FootSize, SURFACE_RADIUS, VoiceFace, VoiceFoot, auth::ClickHandler, nested_radius};
use crate::scale::u;

pub const STRIP_HEIGHT: f32 = super::voice_face::FACE_H;
pub const STRIP_PAD: f32 = 4.;
const STRIP_GAP: f32 = 4.;
/// The corners of what fills the strip's padding: a face, and the foot's buttons.
pub const STRIP_INNER_RADIUS: f32 = nested_radius(SURFACE_RADIUS, STRIP_PAD);
const STRIP_FADE: f32 = 12.;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShareControl {
    Off,
    Starting { source: SharedString },
    Live { source: SharedString },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceBarModel {
    Idle {
        can_join: bool,
    },
    Connecting,
    InVoice {
        /// What the Member asked for, as the audio already follows it: the buttons do not wait for
        /// the Lobby's answer, and go back if it refuses.
        muted: bool,
        /// Deafened plays nothing and is muted with it.
        deafened: bool,
        members: usize,
        share: ShareControl,
        /// The media path broke and is being restored: Voice goes on, quietly said so.
        reconnecting: bool,
        /// How many times the Member talked while muted in this call. The mute button's ring
        /// plays once each time it grows.
        mute_nudges: u32,
    },
}

pub fn voice_bar_model(state: &LobbyState) -> VoiceBarModel {
    match state.voice.phase {
        VoicePhase::Idle => VoiceBarModel::Idle {
            can_join: state.phase == Phase::Connected,
        },
        VoicePhase::Joining => VoiceBarModel::Connecting,
        VoicePhase::InVoice { reconnecting, .. } => match state.me_voice() {
            Some(v) if !v.joining => VoiceBarModel::InVoice {
                muted: state.voice.muted,
                deafened: state.voice.deafened,
                members: state.in_voice().len(),
                share: match &state.share.sharing {
                    None => ShareControl::Off,
                    Some(s)
                        if state
                            .me
                            .as_ref()
                            .and_then(|me| state.share_of(me))
                            .is_some() =>
                    {
                        ShareControl::Live {
                            source: s.name.clone().into(),
                        }
                    }
                    Some(s) => ShareControl::Starting {
                        source: s.name.clone().into(),
                    },
                },
                reconnecting,
                mute_nudges: state.voice.mute_nudges,
            },
            _ => VoiceBarModel::Connecting,
        },
    }
}

/// The purple of the Voice box and the Voice strip, in Voice or not.
pub fn card_surface(cx: &App) -> Hsla {
    cx.theme().sidebar_accent
}

/// What the server makes a control wait for, as of now: the Join after too many Joins, and the
/// unmute and the undeafen after too many of those.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Waits {
    /// The whole seconds left, rounded up, before Join works again.
    pub join: Option<u64>,
    pub unmute: bool,
}

impl Waits {
    pub fn of(state: &LobbyState, now: Instant) -> Self {
        Self {
            join: state
                .voice
                .join_after
                .and_then(|at| at.checked_duration_since(now))
                .filter(|left| !left.is_zero())
                .map(|left| left.as_secs_f64().ceil() as u64),
            unmute: state.voice.unmute_after.is_some_and(|at| at > now),
        }
    }
}

#[derive(Clone)]
pub struct VoiceBarActions {
    pub join: ClickHandler,
    pub leave: ClickHandler,
    pub mute: ClickHandler,
    pub deafen: ClickHandler,
    pub share: ClickHandler,
    pub stop_share: ClickHandler,
}

/// The small window's Voice bar above the composer: the faces of everyone in Voice on the left,
/// this Member's [`VoiceFoot`] on the right. Its height never changes, so nothing above or below
/// it moves when Voice does.
#[derive(IntoElement)]
pub struct VoiceStrip {
    model: VoiceBarModel,
    actions: VoiceBarActions,
    waits: Waits,
    faces: Vec<VoiceFace>,
}

impl VoiceStrip {
    pub fn new(model: VoiceBarModel, actions: VoiceBarActions) -> Self {
        Self {
            model,
            actions,
            waits: Waits::default(),
            faces: Vec::new(),
        }
    }

    pub fn waits(mut self, waits: Waits) -> Self {
        self.waits = waits;
        self
    }

    pub fn faces(mut self, faces: Vec<VoiceFace>) -> Self {
        self.faces = faces;
        self
    }
}

impl RenderOnce for VoiceStrip {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let surface = card_surface(cx);
        let who = div()
            .id("voice-faces")
            .debug_selector(|| "voice-faces".into())
            .size_full()
            .flex()
            .items_center()
            .overflow_hidden()
            .children(self.faces);
        div()
            .id("voice-strip")
            .debug_selector(|| "voice-strip".into())
            .h(u(STRIP_HEIGHT))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(u(STRIP_GAP))
            .px(u(STRIP_PAD))
            .rounded(u(SURFACE_RADIUS))
            .bg(surface)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .child(who)
                    // More faces than room: the last one shown fades out instead of being cut.
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .right_0()
                            .w(u(STRIP_FADE))
                            .bg(linear_gradient(
                                90.,
                                linear_color_stop(surface.opacity(0.), 0.),
                                linear_color_stop(surface, 1.),
                            )),
                    ),
            )
            .child(
                VoiceFoot::new(self.model, self.actions)
                    .waits(self.waits)
                    .size(FootSize::Bar),
            )
            .test_support()
    }
}
