//! The owner's Leave dialog: the Lobby's other Members, one to pick as the next owner. The pick is
//! the dialog's; the hand-over and the Leave are one Intent the Session runs.

use gpui_kit::{
    App, Context, Entity, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _,
    Window,
    component::{
        ActiveTheme as _, Disableable as _,
        button::{Button, ButtonVariants as _},
        dialog::DialogFooter,
        radio::Radio,
    },
    div,
};
use lobby_client::{LobbyState, copy};
use lobby_protocol::UserId;

use crate::components::{Avatar, AvatarSize};
use crate::scale::u;

const HEIR_ROW_H: f32 = 36.;

pub struct HandOverView {
    heirs: Vec<(UserId, SharedString)>,
    picked: Option<usize>,
}

impl HandOverView {
    /// Every other Member of `state`, by name.
    pub fn new(state: &LobbyState) -> Self {
        let mut heirs: Vec<(UserId, SharedString)> = state
            .members
            .values()
            .filter(|m| state.me.as_ref() != Some(&m.id))
            .map(|m| (m.id.clone(), m.username.to_string().into()))
            .collect();
        heirs.sort_by_key(|(_, name)| name.to_lowercase());
        Self {
            heirs,
            picked: None,
        }
    }

    fn pick(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.picked = Some(ix);
        cx.notify();
    }

    /// Cancel, and the Leave that waits for a pick and names it.
    pub fn footer(
        this: &Entity<Self>,
        cancel: impl Fn(&mut Window, &mut App) + 'static,
        leave: impl Fn(UserId, &mut Window, &mut App) + 'static,
        cx: &App,
    ) -> DialogFooter {
        let view = this.read(cx);
        let picked = view.picked.map(|ix| view.heirs[ix].clone());
        let name = picked.as_ref().map(|(_, name)| name.to_string());
        let heir = picked.map(|(id, _)| id);
        DialogFooter::new()
            .child(
                Button::new("hand-over-cancel")
                    .debug_selector(|| "hand-over-cancel".into())
                    .ghost()
                    .label("Cancel")
                    .on_click(move |_, window, cx| cancel(window, cx)),
            )
            .child(
                Button::new("hand-over-leave")
                    .debug_selector(|| "hand-over-leave".into())
                    .danger()
                    .label(copy::hand_over_and_leave(name.as_deref()))
                    .disabled(heir.is_none())
                    .on_click(move |_, window, cx| {
                        if let Some(heir) = heir.clone() {
                            leave(heir, window, cx);
                        }
                    }),
            )
    }
}

impl Render for HandOverView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, hover) = (theme.muted_foreground, theme.secondary_hover);
        let detail = if self.heirs.is_empty() {
            copy::ONLY_MEMBER
        } else {
            copy::HAND_OVER_DETAIL
        };
        let rows = self.heirs.iter().enumerate().map(|(ix, (_, name))| {
            let picked = self.picked == Some(ix);
            let name = name.clone();
            div()
                .id(SharedString::from(format!("heir-{name}")))
                .h(u(HEIR_ROW_H))
                .px_2()
                .rounded(cx.theme().radius)
                .flex()
                .items_center()
                .gap_2()
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .on_click(cx.listener(move |v, _, _, cx| v.pick(ix, cx)))
                .child(Avatar::member(name.clone()).size(AvatarSize::Bar))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child(name),
                )
                .child(
                    Radio::new(("heir-radio", ix))
                        .checked(picked)
                        .on_click(cx.listener(move |v, _, _, cx| v.pick(ix, cx))),
                )
                .test_support()
        });
        div()
            .id("hand-over")
            .debug_selector(|| "hand-over".into())
            .flex()
            .flex_col()
            .gap_3()
            .child(div().text_sm().text_color(muted).child(detail))
            .child(div().flex().flex_col().gap_0p5().children(rows))
            .test_support()
    }
}
