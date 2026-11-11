//! The owner's Leave dialog: the Lobby's other Members, one to pick as the next owner. The pick is
//! the dialog's; the hand-over and the Leave are one Intent the Session runs.

use std::rc::Rc;

use gpui_kit::{
    App, ClickEvent, Context, ElementId, Entity, FocusHandle, FontWeight, InteractiveElement as _,
    IntoElement, KeyDownEvent, MouseButton, ParentElement as _, Render, RenderOnce, SharedString,
    StatefulInteractiveElement as _, StyleRefinement, Styled as _, TestSupportExt as _, Window,
    assets::IconName,
    base::{Radio, RadioGroup, spring},
    div,
    prelude::FluentBuilder as _,
    svg,
};
use lobby_client::{LobbyState, copy};
use lobby_protocol::UserId;

use crate::components::{Avatar, AvatarSize, ROW_HEIGHT};
use crate::kit::{ActiveTheme as _, dialog::DialogFooter};
use crate::scale::u;
use crate::ui;

const HEIR_ROW_H: f32 = ROW_HEIGHT;
/// The radio's circle and the tick inside it, at 100%.
const RADIO: f32 = 16.;
const TICK: f32 = 12.;

pub struct HandOverView {
    heirs: Vec<(UserId, SharedString)>,
    picked: Option<usize>,
    /// One per heir, so an arrow key can move the focus along with the pick.
    focus: Vec<FocusHandle>,
}

/// One heir's radio: a circle that fills and ticks when it is the pick. Presses do not take the
/// focus, so the ring means the keyboard.
#[derive(IntoElement)]
struct HeirRadio {
    id: ElementId,
    checked: bool,
    tab_stop: bool,
    focus: FocusHandle,
    position: (usize, usize),
    name: SharedString,
    on_pick: Pick,
}

type Pick = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

impl RenderOnce for HeirRadio {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let (primary, input, ring_color) = (theme.primary, theme.input, theme.ring);
        let (tick_ink, rest_fill) = (theme.primary_foreground, theme.input_background());
        let (round, spring_control) = (theme.radius_full(), theme.motion_tokens().spring_control);
        let tick = spring(
            (self.id.clone(), "tick"),
            if self.checked { 1. } else { 0. },
            spring_control,
            window,
            cx,
        );
        let focused = self.focus.is_focused(window);
        let on_pick = self.on_pick;
        let circle = div()
            .relative()
            .size(u(RADIO))
            .flex_shrink_0()
            .rounded(round)
            .border_1()
            .border_color(if self.checked { primary } else { input })
            .bg(if self.checked { primary } else { rest_fill })
            .child(
                svg()
                    .absolute()
                    .top_px()
                    .left_px()
                    .size(u(TICK))
                    .text_color(tick_ink)
                    .when(tick > 0., |tick_mark| {
                        tick_mark.path(IconName::Check.path()).opacity(tick)
                    }),
            );
        let circle = if focused {
            let shape = StyleRefinement::default().border_1().rounded(round);
            circle
                .border_color(ring_color)
                .child(ui::focus_ring(&shape, window, cx))
        } else {
            circle
        };
        Radio::new(self.id)
            .checked(self.checked)
            .tab_stop(self.tab_stop)
            .track_focus(&self.focus)
            .accessibility_label(self.name)
            .set_position(self.position.0, self.position.1)
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .on_change(move |_, event, window, cx| on_pick(event, window, cx))
            .child(circle)
    }
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
            focus: Vec::new(),
        }
    }

    /// Who is picked now.
    pub fn heir(&self) -> Option<UserId> {
        self.picked.map(|ix| self.heirs[ix].0.clone())
    }

    fn pick(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.picked = Some(ix);
        cx.notify();
    }

    /// An arrow key moves the pick `by` places along the list, round its ends, and the focus with
    /// it, as a group of radios does: from the focused heir, or from the pick when the focus is
    /// elsewhere, or to the first or the last of the list when there is neither.
    fn step(&mut self, by: isize, window: &mut Window, cx: &mut Context<Self>) {
        let len = self.heirs.len() as isize;
        if len == 0 {
            return;
        }
        let from = (self.focus.iter().position(|h| h.is_focused(window))).or(self.picked);
        let ix = match from {
            Some(at) => (at as isize + by).rem_euclid(len) as usize,
            None if by > 0 => 0,
            None => (len - 1) as usize,
        };
        self.pick(ix, cx);
        if let Some(handle) = self.focus.get(ix) {
            window.focus(handle, cx);
        }
    }

    /// Cancel, and the Leave that waits for a pick and names it.
    pub fn footer(
        this: &Entity<Self>,
        cancel: impl Fn(&mut Window, &mut App) + 'static,
        leave: impl Fn(UserId, &mut Window, &mut App) + 'static,
        cx: &App,
    ) -> DialogFooter {
        let view = this.read(cx);
        let name = view.picked.map(|ix| view.heirs[ix].1.to_string());
        let heir = view.heir();
        DialogFooter::new()
            .child(
                ui::Button::new("hand-over-cancel")
                    .debug_selector(|| "hand-over-cancel".into())
                    .size(ui::Size::Page)
                    .label("Cancel")
                    .on_click(move |_, window, cx| cancel(window, cx)),
            )
            .child(
                ui::Button::new("hand-over-leave")
                    .debug_selector(|| "hand-over-leave".into())
                    .variant(ui::Variant::Danger)
                    .size(ui::Size::Page)
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
        let count = self.heirs.len();
        self.focus.resize_with(count, || cx.focus_handle());
        // Tab enters the group at its pick, or at its first heir while there is none.
        let tab_ix = self.picked.unwrap_or(0);
        let rows = self.heirs.iter().enumerate().map(|(ix, (_, name))| {
            let picked = self.picked == Some(ix);
            let (name, label) = (name.clone(), name.clone());
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
                .child(Avatar::member(name.clone()).size(AvatarSize::Row))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child(name),
                )
                .child(HeirRadio {
                    id: ("heir-radio", ix).into(),
                    checked: picked,
                    tab_stop: ix == tab_ix,
                    focus: self.focus[ix].clone(),
                    position: (ix + 1, count),
                    name: label,
                    on_pick: Rc::new(cx.listener(move |v, _: &ClickEvent, _, cx| v.pick(ix, cx))),
                })
                .test_support()
        });
        let on_key = cx.listener(|v, event: &KeyDownEvent, window, cx| {
            let by = match event.keystroke.key.as_str() {
                "up" | "left" => -1,
                "down" | "right" => 1,
                _ => return,
            };
            v.step(by, window, cx);
            cx.stop_propagation();
        });
        div()
            .id("hand-over")
            .debug_selector(|| "hand-over".into())
            .flex()
            .flex_col()
            .gap_3()
            .child(div().text_sm().text_color(muted).child(detail))
            .child(
                RadioGroup::new("heirs")
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .on_key_down(on_key)
                    .children(rows),
            )
            .test_support()
    }
}
