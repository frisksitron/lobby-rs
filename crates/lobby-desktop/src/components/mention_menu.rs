//! The Members offered while an `@name` is typed in the composer: a small sheet that floats over
//! the feed above the composer, so nothing moves when it comes and goes.

use gpui_kit::{
    App, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, SharedString,
    Styled as _, TestSupportExt as _, component::ActiveTheme as _, div,
    prelude::FluentBuilder as _,
};
use lobby_protocol::Username;

use super::{
    Avatar, AvatarSize, Callback, floating_sheet,
    tokens::{BORDER, nested_radius},
};
use crate::scale::u;

const WIDTH: f32 = 200.;
const ROW_HEIGHT: f32 = 30.;
const PAD: f32 = 4.;
const RADIUS: f32 = 11.;
const SELECTED_WASH: f32 = 0.1;

/// The sheet over `matches`, the `selected` one lit. A press on a row calls `on_pick` with its
/// place in `matches`.
pub fn mention_menu(
    matches: &[Username],
    selected: usize,
    on_pick: Callback<usize>,
    cx: &App,
) -> impl IntoElement {
    let fg = cx.theme().foreground;
    floating_sheet(cx)
        .id("mention-menu")
        .test_support()
        .w(u(WIDTH))
        .p(u(PAD))
        .rounded(u(RADIUS))
        .flex()
        .flex_col()
        .children(matches.iter().enumerate().map(|(i, name)| {
            let on_pick = on_pick.clone();
            div()
                .id(SharedString::from(format!("mention-{name}")))
                .test_support()
                .flex()
                .items_center()
                .gap(u(8.))
                .h(u(ROW_HEIGHT))
                .px(u(6.))
                .rounded(u(nested_radius(RADIUS, BORDER + PAD)))
                .text_sm()
                .text_color(fg)
                .when(i == selected, |d| d.bg(fg.opacity(SELECTED_WASH)))
                .hover(move |d| d.bg(fg.opacity(SELECTED_WASH)))
                .child(Avatar::member(name.to_string()).size(AvatarSize::Bar))
                .child(name.to_string())
                .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    cx.stop_propagation();
                    on_pick(i, window, cx);
                })
        }))
}
