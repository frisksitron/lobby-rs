//! A glyph on a soft disc, tinted by an accent or the theme's quiet secondary.

use std::time::Duration;

use crate::scale::u;
use gpui_kit::{
    Animation, AnimationExt as _, App, Hsla, IntoElement, ParentElement as _, RenderOnce,
    SharedString, Styled as _, Transformation, Window,
    assets::IconName,
    component::{ActiveTheme as _, Icon},
    div, size,
};

#[derive(IntoElement)]
pub struct IconDisc {
    icon: IconName,
    size: f32,
    accent: Option<Hsla>,
    hello: Option<SharedString>,
}

impl IconDisc {
    pub fn new(icon: IconName, size: f32) -> Self {
        Self {
            icon,
            size,
            accent: None,
            hello: None,
        }
    }

    pub fn accent(mut self, color: Hsla) -> Self {
        self.accent = Some(color);
        self
    }

    /// The glyph swells once as the disc appears. Only the glyph moves.
    pub fn hello(mut self, key: impl Into<SharedString>) -> Self {
        self.hello = Some(key.into());
        self
    }
}

impl RenderOnce for IconDisc {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let (disc, glyph) = match self.accent {
            Some(accent) => (accent.opacity(0.14), accent),
            None => (theme.secondary, theme.muted_foreground),
        };
        let icon = Icon::new(self.icon)
            .size(u((self.size * 0.5).round()))
            .text_color(glyph);
        let icon = match self.hello {
            Some(key) => icon
                .with_animation(
                    key,
                    Animation::new(Duration::from_millis(900)),
                    |icon, t| {
                        let swell = 1. + 0.14 * (t * std::f32::consts::PI).sin();
                        icon.transform(Transformation::scale(size(swell, swell)))
                    },
                )
                .into_any_element(),
            None => icon.into_any_element(),
        };
        div()
            .size(u(self.size))
            .flex_shrink_0()
            .rounded_full()
            .bg(disc)
            .flex()
            .items_center()
            .justify_center()
            .child(icon)
    }
}
