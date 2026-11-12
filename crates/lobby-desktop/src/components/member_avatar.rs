//! A Member's avatar with a pulsing ring while they speak, dimming while their Voice connects, and
//! the presence badge. Presence is told by shape as much as colour: a dot for online and idle, a
//! bar for Do Not Disturb, a hollow ring for offline.

use std::time::Duration;

use gpui_kit::{
    Animation, AnimationExt as _, AnyElement, App, ElementId, Hsla, ImageSource, IntoElement,
    ParentElement as _, RenderOnce, SharedString, Styled as _, Window, base::spring, div,
    prelude::FluentBuilder as _, pulsating_between,
};
use lobby_protocol::{Presence, VoiceState};

use super::{
    Avatar,
    tokens::{AvatarSize, PULSE_FPS, idle_color, voice_accent},
};
use crate::kit::ActiveTheme as _;
use crate::scale::u;
use crate::ui::motion;

/// The speaking ring sits one pixel outside the avatar.
const RING: f32 = 2.;

/// The pulse's range, and where a still ring holds: the middle, as with reduced motion.
const RING_OPACITY: (f32, f32) = (0.35, 1.0);
const STILL_RING_OPACITY: f32 = (RING_OPACITY.0 + RING_OPACITY.1) / 2.;

fn presence_badge_on(
    presence: Presence,
    known: bool,
    size: f32,
    background: Hsla,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let (success, idle, muted) = (theme.success, idle_color(theme), theme.muted_foreground);
    let (dnd, bar) = (theme.red, theme.foreground);
    let base = div()
        .size(u(size))
        .flex_shrink_0()
        .rounded_full()
        .flex()
        .items_center()
        .justify_center();
    let base = base.when(!known, |b| b.opacity(0.6));
    match presence {
        Presence::Online => base.bg(success).into_any_element(),
        Presence::Idle => base.bg(idle).into_any_element(),
        Presence::Dnd => base
            .bg(dnd)
            .child(
                div()
                    .w(u((size * 0.55).round()))
                    .h(u((size * 0.18).max(1.5)))
                    .rounded_full()
                    .bg(bar),
            )
            .into_any_element(),
        Presence::Offline => base
            .border(u(1.5))
            .border_color(muted.opacity(0.7))
            .bg(background)
            .into_any_element(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AvatarState {
    pub presence: Option<Presence>,
    pub known: bool,
    pub voice: Option<VoiceState>,
}

#[derive(IntoElement)]
pub struct MemberAvatar {
    id: ElementId,
    name: SharedString,
    state: AvatarState,
    size: AvatarSize,
    image: Option<ImageSource>,
    surface: Option<Hsla>,
    still: bool,
}

impl MemberAvatar {
    pub fn new(
        id: impl Into<ElementId>,
        name: impl Into<SharedString>,
        state: AvatarState,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            state,
            size: AvatarSize::Row,
            image: None,
            surface: None,
            still: false,
        }
    }

    /// The speaking ring holds still: while a share is on screen its frames are motion enough,
    /// and a pulse on its own timer would draw the window between them.
    pub fn still(mut self, still: bool) -> Self {
        self.still = still;
        self
    }

    pub fn size(mut self, size: AvatarSize) -> Self {
        self.size = size;
        self
    }

    pub fn image(mut self, image: Option<ImageSource>) -> Self {
        self.image = image;
        self
    }

    pub fn surface(mut self, color: Hsla) -> Self {
        self.surface = Some(color);
        self
    }

    pub fn outer(size: AvatarSize) -> f32 {
        size.px() + 2. * (RING + 1.)
    }
}

impl RenderOnce for MemberAvatar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let surface = self.surface.unwrap_or(cx.theme().background);
        let voice = self.state.voice;
        // Speaking is live; the last known one would ring for nobody.
        let speaking =
            self.state.known && voice.is_some_and(|v| v.speaking && !v.muted && !v.joining);
        let ring = spring(
            (self.id.clone(), "ring"),
            if speaking { 1. } else { 0. },
            motion::QUICK,
            window,
            cx,
        );
        let joining = voice.is_some_and(|v| v.joining);
        let offline = self.state.known && self.state.presence == Some(Presence::Offline);
        let badge_size = (self.size.px() / 3.).round();
        let badge = self
            .state
            .presence
            .map(|p| presence_badge_on(p, self.state.known, badge_size, surface, cx));
        let outer = Self::outer(self.size);
        div()
            .relative()
            .flex_shrink_0()
            .size(u(outer))
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .when(joining, |d| d.opacity(0.45))
                    .when(offline, |d| d.opacity(0.5))
                    .child(
                        Avatar::member(self.name.clone())
                            .size(self.size)
                            .image(self.image),
                    ),
            )
            .when(ring > 0., |d| {
                let circle = div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size(u(outer))
                    .rounded_full()
                    .border(u(RING))
                    .border_color(voice_accent(cx.theme()));
                d.child(if self.still {
                    circle.opacity(STILL_RING_OPACITY * ring).into_any_element()
                } else {
                    circle
                        .with_animation(
                            self.id.clone(),
                            Animation::new(Duration::from_millis(1100))
                                .repeat()
                                .with_max_fps(PULSE_FPS)
                                .with_easing(pulsating_between(RING_OPACITY.0, RING_OPACITY.1)),
                            move |circle, pulse| circle.opacity(pulse * ring),
                        )
                        .into_any_element()
                })
            })
            .children(badge.map(|badge| {
                div()
                    .absolute()
                    .bottom(u(-1.))
                    .right(u(-1.))
                    .p(u(2.))
                    .rounded_full()
                    .bg(surface)
                    .child(badge)
            }))
    }
}
