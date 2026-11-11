//! A turning loader, in the text colour around it unless given its own.

use std::time::Duration;

use gpui_kit::{
    Animation, AnimationExt as _, App, Hsla, IntoElement, ParentElement as _, Rems, RenderOnce,
    Styled as _, Transformation, Window, assets::IconName, div, ease_in_out, percentage,
};

use super::{ICON_MD, Icon};
use crate::scale::u;

/// One turn.
const TURN: Duration = Duration::from_millis(800);

#[derive(IntoElement)]
pub struct Spinner {
    icon: IconName,
    size: Rems,
    color: Option<Hsla>,
}

impl Default for Spinner {
    fn default() -> Self {
        Self {
            icon: IconName::Loader,
            size: u(ICON_MD),
            color: None,
        }
    }
}

impl Spinner {
    pub fn new() -> Self {
        Self::default()
    }

    /// The glyph that turns, where the default does not suit its place.
    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = icon;
        self
    }

    pub fn size(mut self, size: Rems) -> Self {
        self.size = size;
        self
    }

    pub fn color(mut self, color: Hsla) -> Self {
        self.color = Some(color);
        self
    }
}

impl RenderOnce for Spinner {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let mut icon = Icon::new(self.icon).size(self.size);
        if let Some(color) = self.color {
            icon = icon.text_color(color);
        }
        div().child(icon.with_animation(
            "spin",
            Animation::new(TURN).repeat().with_easing(ease_in_out),
            |icon, delta| icon.transform(Transformation::rotate(percentage(delta))),
        ))
    }
}
