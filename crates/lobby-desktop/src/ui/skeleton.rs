//! A placeholder for content on its way, sized by its caller and breathing slowly.

use std::time::Duration;

use gpui_kit::{
    Animation, AnimationExt as _, App, IntoElement, Refineable as _, RenderOnce, StyleRefinement,
    Styled, Window, bounce, div, ease_in_out,
};

use crate::kit::ActiveTheme as _;

const BREATH: Duration = Duration::from_secs(2);
const FADE: f32 = 0.5;

#[derive(Default, IntoElement)]
pub struct Skeleton {
    style: StyleRefinement,
}

impl Skeleton {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Styled for Skeleton {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for Skeleton {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let mut bar = div().w_full().h_4().bg(cx.theme().skeleton);
        bar.style().refine(&self.style);
        bar.with_animation(
            "skeleton",
            Animation::new(BREATH)
                .repeat()
                .with_easing(bounce(ease_in_out)),
            |bar, delta| bar.opacity(1. - delta * FADE),
        )
    }
}
