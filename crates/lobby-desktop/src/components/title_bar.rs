//! The window's title bar, drawn by gpui-kit's `TitleBar`, which also moves the window. Every
//! interactive child occludes, so a click on it is a click and not the start of a window drag.

use std::time::Duration;

use gpui_kit::{
    Animation, AnimationExt as _, AnyElement, App, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, RenderOnce, SharedString, Styled as _, Window, div, ease_out_quint, hsla,
};

use super::{avatar::stable_hash, tokens::MARK_COLORS};
use crate::kit::{ActiveTheme as _, TitleBar};
use crate::scale::u;

/// Taller than the kit's 34 px, so the Lobby's name and the label breathe;
/// toasts start below it.
pub const TITLE_BAR_HEIGHT: f32 = 40.;

#[derive(IntoElement, Default)]
pub struct AppTitleBar {
    left: Vec<AnyElement>,
    right: Vec<AnyElement>,
    tint: Option<(SharedString, Hsla)>,
}

impl AppTitleBar {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn left(mut self, child: impl IntoElement) -> Self {
        self.left.push(child.into_any_element());
        self
    }

    pub fn right(mut self, child: impl IntoElement) -> Self {
        self.right.push(child.into_any_element());
        self
    }

    pub fn tint(mut self, key: impl Into<SharedString>, color: Option<Hsla>) -> Self {
        self.tint = color.map(|c| (key.into(), c));
        self
    }
}

/// The colour slot an origin prefers, so origins that differ in one character (adjacent ports) land
/// on unrelated slots.
pub fn preferred_slot(origin: &str) -> usize {
    (stable_hash(origin) % MARK_COLORS.len() as u64) as usize
}

/// The slots of the Lobbies on this device, in saved order: each takes its preferred slot or, when
/// an earlier Lobby holds it, the next free one. Adding a Lobby never recolours the ones before it.
pub fn mark_slots<'a>(origins: impl IntoIterator<Item = &'a str>) -> Vec<usize> {
    let mut taken = [false; MARK_COLORS.len()];
    origins
        .into_iter()
        .map(|origin| {
            let preferred = preferred_slot(origin);
            let slot = (0..MARK_COLORS.len())
                .map(|i| (preferred + i) % MARK_COLORS.len())
                .find(|s| !taken[*s])
                .unwrap_or(preferred);
            taken[slot] = true;
            slot
        })
        .collect()
}

pub fn slot_color(slot: usize) -> Hsla {
    let (h, s, l) = MARK_COLORS[slot % MARK_COLORS.len()];
    hsla(h / 360., s, l, 1.)
}

pub fn mark_color(origin: &str) -> Hsla {
    slot_color(preferred_slot(origin))
}

impl RenderOnce for AppTitleBar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let base = cx.theme().title_bar;
        // The kit's bar never shrinks below its content, so a long Lobby name pushed the bell and
        // the window controls off the window. Laid out absolutely, our row adds nothing to the
        // bar's width: it gets the space left of the window controls.
        let row = div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .child(
                div()
                    .debug_selector(|| "title-bar-left".into())
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .gap_2()
                    .children(self.left),
            )
            .child(
                div()
                    .debug_selector(|| "title-bar-right".into())
                    .flex_shrink_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .gap_1()
                    .pl_2()
                    .pr_2()
                    .children(self.right),
            );
        let bar = TitleBar::new()
            .h(u(TITLE_BAR_HEIGHT))
            .child(div().relative().flex_1().h_full().child(row));
        match self.tint {
            None => bar.into_any_element(),
            Some((key, color)) => bar
                .with_animation(
                    key,
                    Animation::new(Duration::from_millis(400)).with_easing(ease_out_quint()),
                    move |bar, delta| bar.bg(base.blend(color.opacity(0.14 * delta))),
                )
                .into_any_element(),
        }
    }
}
