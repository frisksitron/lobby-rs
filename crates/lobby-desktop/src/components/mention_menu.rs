//! What is offered while an `@name` is typed in the composer: a small sheet that floats over the
//! feed above the composer, so nothing moves when it comes and goes.

use gpui_kit::{
    App, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, SharedString,
    Styled as _, TestSupportExt as _,
    assets::IconName,
    component::{ActiveTheme as _, Icon},
    div,
    prelude::FluentBuilder as _,
};
use lobby_client::compose::MentionTarget;

use super::{
    Avatar, AvatarSize, Callback, ICON_SM, LINE_H, floating_sheet,
    mention_chip::fill,
    tokens::{BORDER, nested_radius},
};
use crate::scale::u;

const WIDTH: f32 = 200.;
const PAD: f32 = 4.;
const RADIUS: f32 = 11.;
const SELECTED_WASH: f32 = 0.1;

/// The sheet over `targets`, the `selected` one lit. A press on a row calls `on_pick` with its
/// place in `targets`.
pub fn mention_menu(
    targets: &[MentionTarget],
    selected: usize,
    on_pick: Callback<usize>,
    cx: &App,
) -> impl IntoElement {
    let fg = cx.theme().foreground;
    let everyone_fill = fill(cx);
    let accent = cx.theme().primary;
    floating_sheet(cx)
        .id("mention-menu")
        .test_support()
        .w(u(WIDTH))
        .p(u(PAD))
        .rounded(u(RADIUS))
        .flex()
        .flex_col()
        .children(targets.iter().enumerate().map(|(i, target)| {
            let on_pick = on_pick.clone();
            let mark = match target {
                MentionTarget::Member(name) => Avatar::member(name.to_string())
                    .size(AvatarSize::Bar)
                    .into_any_element(),
                MentionTarget::Everyone => div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .flex_shrink_0()
                    .size(u(AvatarSize::Bar.px()))
                    .rounded_full()
                    .bg(everyone_fill)
                    .text_color(accent)
                    .child(Icon::new(IconName::Users).size(u(ICON_SM)))
                    .into_any_element(),
            };
            div()
                .id(SharedString::from(format!("mention-{}", target.name())))
                .test_support()
                .flex()
                .items_center()
                .gap(u(8.))
                .h(u(LINE_H))
                .px(u(6.))
                .rounded(u(nested_radius(RADIUS, BORDER + PAD)))
                .text_sm()
                .text_color(fg)
                .when(i == selected, |d| d.bg(fg.opacity(SELECTED_WASH)))
                .hover(move |d| d.bg(fg.opacity(SELECTED_WASH)))
                .child(mark)
                .child(target.name().to_owned())
                .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    cx.stop_propagation();
                    on_pick(i, window, cx);
                })
        }))
}
