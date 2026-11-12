//! The Lobby switcher: the Lobby's mark and name at the left of the title bar, opening a small menu
//! of the Lobbies on this device and "Add Lobby". With no Lobby saved, the app's mark stands alone
//! in the same place.
//!
//! Every Lobby's row has its own menu, from its ⋯ or a right-click: Change icon… (with Remove
//! while there is one) and Leave Lobby…, for that Lobby whichever one is open. It is drawn beside
//! the kit's menu, not in it: the kit's submenus open on hover, and its rows switch on a click. A
//! press in it is outside the kit's menu, which closes under it, so it stays for its own click.
//!
//! Long names never push the title bar's other controls away. The trigger shrinks and ellipsizes up
//! to [`TRIGGER_MAX`], and in the menu every row has the menu's inner width.

use std::{cell::Cell, rc::Rc};

use gpui_kit::{
    Anchor, AnyElement, App, Bounds, ClickEvent, Context, Entity, Hsla, ImageSource,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Pixels, Point,
    RenderOnce, SharedString, Styled as _, TestSupportExt as _, Window,
    assets::IconName,
    base::{ElementExt as _, POPUP_PRIORITY, Positioner},
    deferred, div, point,
    prelude::FluentBuilder as _,
};
use lobby_client::{LobbyOrigin, copy};
use lobby_protocol::BlobId;

use crate::components::{
    Avatar, AvatarSize, BAR_CONTROL_H, BarButton, Callback, ICON_SM, ICON_XS, MEMBER_CARD_PAD,
    member_panel::{card_line, line_with_end, separator},
    truncated_text,
};
use crate::kit::{
    ActiveTheme as _,
    menu::{DropdownMenu as _, PopupMenuItem},
};
use crate::scale::{design_viewport, device, u};
use crate::ui::{
    self,
    motion::{Reveal, Side},
};
/// Keeps the connection label near the name; narrower windows shrink it further.
pub const TRIGGER_MAX: f32 = 260.;
/// The trigger's padding, also kept by the lone app mark so it never moves between routes.
const TRIGGER_PAD_X: f32 = 4.;
const ROW_MARK: AvatarSize = AvatarSize::Menu;
const MENU_WIDTH: f32 = 288.;
/// In a window too narrow for the menu and a Lobby's menu side by side at full width.
const MENU_WIDTH_NARROW: f32 = 264.;
/// What a row leaves of the menu's width: the kit menu's 4 px padding and each item's 8 px, on
/// both sides. The kit's item does not constrain its content, so the row says its width itself.
const ROW_INSET: f32 = 2. * 4. + 2. * 8.;
const MORE_SIZE: f32 = 24.;
const LOBBY_MENU_WIDTH: f32 = 184.;
/// From a row's right edge to its Lobby menu's left edge: the rest of the kit menu (its item
/// padding, its own padding and border) and a gap.
const LOBBY_MENU_OFFSET: f32 = 8. + 4. + 1. + 2.;
const WINDOW_MARGIN: f32 = 8.;
const DISABLED_OPACITY: f32 = 0.5;

#[derive(Debug, Clone, PartialEq)]
pub struct SwitcherEntry {
    pub origin: LobbyOrigin,
    pub name: SharedString,
    pub icon: Option<BlobId>,
    pub color: Hsla,
    pub current: bool,
    pub last_used: bool,
    pub signed_in: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SwitcherCurrent {
    pub name: SharedString,
    pub origin: LobbyOrigin,
    pub icon: Option<BlobId>,
    pub color: Hsla,
}

/// A Lobby's icon as the picture its mark draws, once it is decoded; the letter mark stands in
/// until then and where there is none.
pub type IconOf = Rc<dyn Fn(&LobbyOrigin, Option<&BlobId>, &mut App) -> Option<ImageSource>>;

fn no_icons() -> IconOf {
    Rc::new(|_, _, _| None)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwitcherPick {
    Lobby(LobbyOrigin),
    Add,
    /// From the Lobby's own menu.
    Act(LobbyOrigin, LobbyAction),
}

/// What a Lobby's own menu does to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LobbyAction {
    ChangeIcon,
    RemoveIcon,
    Leave,
}

/// Whose Lobby menu is open, and where its corner goes.
#[derive(Default)]
struct LobbyMenu {
    open: Option<(LobbyOrigin, Point<Pixels>)>,
    /// The menu as last opened, which it shows while it fades out.
    drawn: Option<(LobbyOrigin, Point<Pixels>)>,
    /// A press landed in it: the kit's menu closing under that press leaves it open.
    pressed: bool,
}

impl LobbyMenu {
    fn show(&mut self, origin: LobbyOrigin, at: Point<Pixels>, cx: &mut Context<Self>) {
        self.open = Some((origin.clone(), at));
        self.drawn = Some((origin, at));
        self.pressed = false;
        cx.notify();
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        if self.open.take().is_some() {
            cx.notify();
        }
        self.pressed = false;
    }
}

pub fn shown_address(origin: &LobbyOrigin) -> SharedString {
    let s = origin.as_str();
    s.strip_prefix("https://").unwrap_or(s).to_owned().into()
}

/// The menu's width: narrower where a Lobby's menu beside it would not fit the window.
fn menu_width(window: &Window) -> f32 {
    let room = 2. * WINDOW_MARGIN + MENU_WIDTH + LOBBY_MENU_OFFSET + LOBBY_MENU_WIDTH;
    if design_viewport(window).0 < room {
        MENU_WIDTH_NARROW
    } else {
        MENU_WIDTH
    }
}

#[derive(IntoElement)]
pub struct LobbySwitcher {
    current: Option<SwitcherCurrent>,
    entries: Vec<SwitcherEntry>,
    on_pick: Callback<SwitcherPick>,
    icons: IconOf,
}

impl LobbySwitcher {
    pub fn new(
        current: Option<SwitcherCurrent>,
        entries: Vec<SwitcherEntry>,
        on_pick: Callback<SwitcherPick>,
    ) -> Self {
        Self {
            current,
            entries,
            on_pick,
            icons: no_icons(),
        }
    }

    pub fn icons(mut self, icons: IconOf) -> Self {
        self.icons = icons;
        self
    }

    pub fn still(current: Option<SwitcherCurrent>) -> Self {
        Self::new(current, Vec::new(), Rc::new(|_, _, _| {}))
    }
}

impl RenderOnce for LobbySwitcher {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let icon = self
            .current
            .as_ref()
            .and_then(|c| (self.icons)(&c.origin, c.icon.as_ref(), cx));
        let theme = cx.theme();
        let (fg, muted) = (theme.foreground, theme.muted_foreground);
        let name: SharedString = self
            .current
            .as_ref()
            .map(|c| c.name.clone())
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| "Lobby".into());
        let mark = match &self.current {
            Some(c) => Avatar::lobby(c.name.clone(), c.color).image(icon),
            None => Avatar::app(),
        }
        .size(AvatarSize::Bar);
        let label = div()
            .debug_selector(|| "lobby-switcher-name".into())
            .min_w_0()
            .truncate()
            .text_sm()
            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
            .text_color(fg)
            .child(name.clone());
        let content = div()
            .min_w_0()
            .flex()
            .items_center()
            .gap_2()
            .child(mark)
            .child(label);
        // Every ancestor of the name shrinks (min_w_0, no flex_shrink_0), so the name ellipsizes
        // instead of growing the title bar.
        let wrapper = || {
            div()
                .debug_selector(|| "lobby-switcher-trigger".into())
                .min_w_0()
                .max_w(u(TRIGGER_MAX))
        };
        if self.entries.is_empty() {
            // Same box as the trigger, so the mark stays put when the first Lobby is saved.
            return wrapper()
                .h(u(BAR_CONTROL_H))
                .px(u(TRIGGER_PAD_X))
                .flex()
                .items_center()
                .child(content)
                .into_any_element();
        }
        let accessible: SharedString = match &self.current {
            Some(_) => format!("{name} · {}", copy::SWITCHER_SHORTCUT).into(),
            None => format!("{} · {}", copy::LOBBIES, copy::SWITCHER_SHORTCUT).into(),
        };
        let trigger = ui::Button::new("lobby-switcher")
            .size(ui::Size::Bar)
            .accessibility_label(accessible)
            .debug_selector(|| "lobby-switcher".into())
            .px(u(TRIGGER_PAD_X))
            .flex_shrink(1.)
            .min_w_0()
            .max_w_full()
            // The name is already on the trigger.
            .tooltip(copy::SWITCHER_SHORTCUT)
            .child(
                content.child(
                    ui::Icon::new(IconName::ChevronsUpDown)
                        .size(u(ICON_XS))
                        .flex_shrink_0()
                        .text_color(muted),
                ),
            );
        let state = window.use_keyed_state("lobby-menu", cx, |_, _| LobbyMenu::default());
        let reveal = Reveal::new("lobby-menu", state.read(cx).open.is_some(), window, cx);
        let lobby_menu = reveal
            .is_drawn()
            .then(|| state.read(cx).drawn.clone())
            .flatten()
            .and_then(|(origin, at)| {
                let entry = self.entries.iter().find(|e| e.origin == origin)?;
                Some(lobby_menu(
                    entry,
                    at,
                    &state,
                    reveal,
                    &self.on_pick,
                    window,
                    cx,
                ))
            });
        let (entries, on_pick, icons) = (self.entries, self.on_pick, self.icons);
        let rows_state = state.clone();
        let menu = trigger
            .dropdown_menu_with_anchor(Anchor::TopLeft, move |menu, window, _| {
                // No heading row: the kit's first Down selects index 0 even when it is a label, so
                // a heading would swallow it.
                let width = menu_width(window);
                let mut menu = menu
                    .min_w(device(width, window))
                    .max_w(device(width, window));
                for (ix, entry) in entries.iter().enumerate() {
                    let pick = on_pick.clone();
                    let origin = entry.origin.clone();
                    let row = lobby_row(ix, entry.clone(), width, icons.clone(), &rows_state);
                    menu = menu.item(row.on_click(move |_, window, cx| {
                        pick(SwitcherPick::Lobby(origin.clone()), window, cx)
                    }));
                }
                let pick = on_pick.clone();
                menu.separator().item(
                    add_row(width)
                        .on_click(move |_, window, cx| pick(SwitcherPick::Add, window, cx)),
                )
            })
            // A press in the Lobby's menu closes the kit's under it; anything else closes both.
            .on_open_change(move |open, _, cx| {
                state.update(cx, |m, cx| {
                    if *open || !m.pressed {
                        m.close(cx);
                    }
                })
            });
        BarButton::slot(menu, None)
            .debug_selector(|| "lobby-switcher-trigger".into())
            .min_w_0()
            .max_w(u(TRIGGER_MAX))
            .children(lobby_menu)
            .into_any_element()
    }
}

/// A Lobby's own menu, its corner at `at`. A Lobby that needs a new Sign In has both lines
/// disabled.
fn lobby_menu(
    entry: &SwitcherEntry,
    at: Point<Pixels>,
    state: &Entity<LobbyMenu>,
    reveal: Reveal,
    on_pick: &Callback<SwitcherPick>,
    window: &Window,
    cx: &App,
) -> AnyElement {
    let off = !entry.signed_in;
    let act = |action: LobbyAction| {
        let (state, pick, origin) = (state.clone(), on_pick.clone(), entry.origin.clone());
        move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            state.update(cx, |m, cx| m.close(cx));
            pick(SwitcherPick::Act(origin.clone(), action), window, cx);
        }
    };
    // The lines keep their own colours, so the kit's disabled look does not show on them.
    let dim = |b: ui::Button| b.disabled(true).opacity(DISABLED_OPACITY);
    let icon = card_line("lobby-menu-icon", copy::CHANGE_ICON, false, cx)
        .tooltip(if off {
            copy::SIGN_IN_FIRST
        } else {
            copy::ICON_SEEN_BY_ALL
        })
        .map(|b| {
            if off {
                dim(b)
            } else {
                b.on_click(act(LobbyAction::ChangeIcon))
            }
        });
    let remove = (entry.icon.is_some() && !off).then(|| {
        ui::Button::new("lobby-menu-icon-remove")
            .debug_selector(|| "lobby-menu-icon-remove".into())
            .accessibility_label("Remove icon")
            .label(copy::REMOVE)
            .on_click(act(LobbyAction::RemoveIcon))
    });
    let leave = card_line("lobby-menu-leave", copy::LEAVE_LOBBY_LINE, true, cx).map(|b| {
        if off {
            dim(b.tooltip(copy::SIGN_IN_FIRST))
        } else {
            b.on_click(act(LobbyAction::Leave))
        }
    });
    let (pressed, closed) = (state.clone(), state.clone());
    let panel = div()
        .id("lobby-menu")
        .debug_selector(|| "lobby-menu".into())
        .w(u(LOBBY_MENU_WIDTH));
    let panel = ui::popover_surface(panel, cx)
        .p(u(MEMBER_CARD_PAD))
        .when(!reveal.is_closing(), |panel| {
            // A disabled line stops its press before it bubbles here, so pressing one closes
            // both.
            panel
                .on_any_mouse_down(move |_, _, cx| pressed.update(cx, |m, _| m.pressed = true))
                .on_mouse_down_out(move |_, _, cx| closed.update(cx, |m, cx| m.close(cx)))
        })
        .child(line_with_end(icon, remove, cx))
        .child(separator(cx))
        .child(leave)
        .test_support();
    let panel = reveal.apply(panel, Side::Left);
    deferred(
        Positioner::corner(Anchor::TopLeft, at)
            .margin(device(WINDOW_MARGIN, window))
            .occlude()
            .child(panel),
    )
    .with_priority(POPUP_PRIORITY)
    .into_any_element()
}

/// Where a row's Lobby menu goes: beside the kit's menu, its first line level with the row.
fn lobby_menu_at(row: Bounds<Pixels>, window: &Window) -> Point<Pixels> {
    point(
        row.right() + device(LOBBY_MENU_OFFSET, window),
        row.top() - device(MEMBER_CARD_PAD, window),
    )
}

fn lobby_row(
    ix: usize,
    entry: SwitcherEntry,
    menu_width: f32,
    icons: IconOf,
    state: &Entity<LobbyMenu>,
) -> PopupMenuItem {
    let state = state.clone();
    // Where the row was drawn, for the Lobby menu it opens.
    let drawn = Rc::new(Cell::new(Bounds::default()));
    PopupMenuItem::element(move |_, cx| {
        let icon = icons(&entry.origin, entry.icon.as_ref(), cx);
        let theme = cx.theme();
        let (fg, muted, primary) = (theme.foreground, theme.muted_foreground, theme.primary);
        let name = if entry.name.trim().is_empty() {
            shown_address(&entry.origin)
        } else {
            entry.name.clone()
        };
        let caption: SharedString = if entry.signed_in {
            shown_address(&entry.origin)
        } else {
            format!(
                "{} · {}",
                copy::SIGN_IN_NEEDED,
                shown_address(&entry.origin)
            )
            .into()
        };
        let open = state
            .read(cx)
            .open
            .as_ref()
            .is_some_and(|(o, _)| *o == entry.origin);
        let show = {
            let (state, drawn, origin) = (state.clone(), drawn.clone(), entry.origin.clone());
            move |window: &mut Window, cx: &mut App| {
                let at = lobby_menu_at(drawn.get(), window);
                state.update(cx, |m, cx| m.show(origin.clone(), at, cx));
            }
        };
        let right_click = show.clone();
        let more = ui::Button::new(("switcher-more", ix))
            .debug_selector(move || format!("switcher-more-{ix}"))
            .accessibility_label(copy::LOBBY_MENU)
            .size(ui::Size::Corner)
            .w(u(MORE_SIZE))
            .h(u(MORE_SIZE))
            .flex_shrink_0()
            .text_color(muted)
            .icon(IconName::Ellipsis)
            .tooltip(copy::LOBBY_MENU)
            .selected(open)
            .on_click(move |_, window, cx| {
                // The row's own click switches to the Lobby.
                cx.stop_propagation();
                show(window, cx);
            });
        let drawn = drawn.clone();
        div()
            .id(("switcher-lobby", ix))
            .debug_selector(move || format!("switcher-lobby-{ix}"))
            .w(u(menu_width - ROW_INSET))
            .flex()
            .items_center()
            .gap_2p5()
            .py(u(3.))
            .on_mouse_down(MouseButton::Right, move |_, window, cx| {
                right_click(window, cx)
            })
            .on_prepaint(move |bounds, _, _| drawn.set(bounds))
            .child(
                Avatar::lobby(name.clone(), entry.color)
                    .size(ROW_MARK)
                    .image(icon),
            )
            .child(
                div()
                    .debug_selector(move || format!("switcher-text-{ix}"))
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .child(
                        truncated_text(("switcher-name", ix), name)
                            .text_sm()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(fg),
                    )
                    .child(div().truncate().text_xs().text_color(muted).child(caption)),
            )
            .child(
                div()
                    .w(u(16.))
                    .flex_shrink_0()
                    .flex()
                    .justify_center()
                    .when(entry.current, |d| {
                        d.debug_selector(move || format!("switcher-current-{ix}"))
                            .child(
                                ui::Icon::new(IconName::Check)
                                    .size(u(ICON_SM))
                                    .text_color(primary),
                            )
                    })
                    .when(!entry.current && entry.last_used, |d| {
                        d.debug_selector(move || format!("switcher-last-used-{ix}"))
                            .child(div().size(u(6.)).rounded_full().bg(muted))
                    }),
            )
            .child(more)
            .test_support()
    })
}

fn add_row(menu_width: f32) -> PopupMenuItem {
    PopupMenuItem::element(move |_, cx| {
        let theme = cx.theme();
        let (fg, muted, border) = (theme.foreground, theme.muted_foreground, theme.border);
        div()
            .id("switcher-add")
            .debug_selector(|| "switcher-add".into())
            .w(u(menu_width - ROW_INSET))
            .flex()
            .items_center()
            .gap_2p5()
            .py(u(3.))
            .child(
                div()
                    .size(u(ROW_MARK.px()))
                    .flex_shrink_0()
                    .rounded(u(ROW_MARK.px() * 0.28))
                    .border_1()
                    .border_dashed()
                    .border_color(border)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        ui::Icon::new(IconName::Plus)
                            .size(u(ICON_SM))
                            .text_color(muted),
                    ),
            )
            .child(div().text_sm().text_color(fg).child(copy::ADD_LOBBY))
            .test_support()
    })
}
