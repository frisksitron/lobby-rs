//! The Magic Code field: eight cells in two groups of four, on gpui-base's unstyled OTP root,
//! which keeps the value, the focus and the typed digits. Keys, text input and paste all fill the
//! cells and submit once the last digit is there.
//!
//! Text input (IME, on-screen keyboards, password-manager auto-type, remote-desktop clients) goes
//! through [`CodeTyping`]. Windows also sends each digit key as a character right after the key.
//! That one is skipped, so a key never counts twice.
//!
//! A wrong code keeps its digits for a moment with red cells and a small shake, before the view
//! clears them.

use super::{
    Callback,
    context_menu::{menu_frame, menu_item},
};
use crate::scale::u;
use gpui_kit::{
    Animation, AnimationExt as _, App, Bounds, Context, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, Focusable as _, InteractiveElement as _, IntoElement,
    MouseButton, ParentElement as _, Pixels, RenderOnce, SharedString, Styled as _,
    TestSupportExt as _, UTF16Selection, Window,
    base::OtpInput,
    canvas,
    component::{
        ActiveTheme as _,
        input::OtpState,
        menu::{ContextMenuExt as _, PopupMenu},
    },
    div,
    prelude::FluentBuilder as _,
};
use lobby_client::copy;
use lobby_protocol::MagicCode;

/// The code in pasted text: a whole code's digits once spaces and dashes are gone, else the first
/// group of exactly that many digits, where one space or dash may split them.
pub fn code_from(text: &str) -> Option<String> {
    let squeezed: String = text
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect();
    if squeezed.len() == MagicCode::LEN && squeezed.bytes().all(|b| b.is_ascii_digit()) {
        return Some(squeezed);
    }
    let chars: Vec<char> = text.chars().collect();
    let mut digits = String::new();
    let mut i = 0;
    while i <= chars.len() {
        match chars.get(i) {
            Some(c) if c.is_ascii_digit() => digits.push(*c),
            // A single separator between two digits keeps the group going.
            Some(' ' | '-')
                if !digits.is_empty()
                    && chars.get(i + 1).is_some_and(char::is_ascii_digit)
                    && chars.get(i - 1).is_some_and(char::is_ascii_digit) => {}
            _ => {
                if digits.len() == MagicCode::LEN {
                    return Some(digits);
                }
                digits.clear();
            }
        }
        i += 1;
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeTyped {
    Change,
    Complete,
}

pub struct CodeTyping {
    otp: Entity<OtpState>,
    /// The character of the digit key the OTP root just handled. Windows sends it again as a
    /// character, which must not count twice.
    skip: Option<char>,
}

impl EventEmitter<CodeTyped> for CodeTyping {}

impl CodeTyping {
    pub fn new(otp: &Entity<OtpState>) -> Self {
        Self {
            otp: otp.clone(),
            skip: None,
        }
    }

    pub fn type_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let mut chars = text.chars();
        if let (Some(c), None, Some(skip)) = (chars.next(), chars.next(), self.skip.take())
            && c == skip
        {
            return;
        }
        let len = self.otp.read(cx).len();
        let current = self.otp.read(cx).value().to_string();
        let next = match code_from(text) {
            Some(code) if text.chars().count() > 1 => code,
            _ => {
                let mut next = current.clone();
                for d in text.chars().filter_map(digit) {
                    if next.chars().count() < len {
                        next.push(d);
                    }
                }
                next
            }
        };
        if next == current {
            return;
        }
        self.otp
            .update(cx, |o, cx| o.set_value(next.clone(), window, cx));
        cx.emit(CodeTyped::Change);
        if next.chars().count() == len {
            cx.emit(CodeTyped::Complete);
        }
    }
}

fn digit(c: char) -> Option<char> {
    if c.is_ascii_digit() {
        return Some(c);
    }
    let d = (c as u32).checked_sub('０' as u32)?;
    char::from_digit(d, 10)
}

impl EntityInputHandler for CodeTyping {
    fn text_for_range(
        &mut self,
        range: std::ops::Range<usize>,
        adjusted: &mut Option<std::ops::Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        let value = self.otp.read(cx).value().to_string();
        let end = range.end.min(value.len());
        let start = range.start.min(end);
        *adjusted = Some(start..end);
        Some(value[start..end].to_owned())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let end = self.otp.read(cx).value().len();
        Some(UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<std::ops::Range<usize>> {
        None
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}

    fn replace_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.type_text(text, window, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        _new_text: &str,
        _new_selected_range: Option<std::ops::Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: std::ops::Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        Some(element_bounds)
    }

    fn character_index_for_point(
        &mut self,
        _point: gpui_kit::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

/// One cell's side and the gaps: the kit's large OTP cells, as tall as the step's other controls.
/// Eight cells and their gaps (396) fit the small window's column (448).
const CELL: f32 = 44.;
const CELL_GAP: f32 = 4.;
const GROUP_GAP: f32 = 20.;

#[derive(IntoElement)]
pub struct CodeInput {
    state: Entity<OtpState>,
    typing: Entity<CodeTyping>,
    disabled: bool,
    wrong: Option<u64>,
    on_paste: Callback<String>,
}

impl CodeInput {
    pub fn new(
        state: &Entity<OtpState>,
        typing: &Entity<CodeTyping>,
        on_paste: Callback<String>,
    ) -> Self {
        Self {
            state: state.clone(),
            typing: typing.clone(),
            disabled: false,
            wrong: None,
            on_paste,
        }
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn wrong(mut self, attempt: Option<u64>) -> Self {
        self.wrong = attempt;
        self
    }
}

impl RenderOnce for CodeInput {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let (input, ring, danger, bg, fg, caret) = (
            theme.input,
            theme.ring,
            theme.danger,
            theme.background,
            theme.foreground,
            theme.caret,
        );
        let radius = u(super::SURFACE_INNER_RADIUS);
        let state = self.state.read(cx);
        let focus = state.focus_handle(cx);
        let focused = focus.is_focused(window) && !self.disabled;
        let blink = state.cursor_visible(cx);
        let len = state.len();
        let value: Vec<char> = state.value().chars().collect();
        let cursor = value.len().min(len.saturating_sub(1));
        let wrong = self.wrong.is_some();
        let cells: Vec<_> = (0..len)
            .map(|ix| {
                let c = value.get(ix).copied();
                let here = focused && ix == cursor && !wrong;
                let border = if wrong {
                    danger
                } else if here {
                    ring
                } else {
                    input
                };
                let state = self.state.clone();
                div()
                    .id(("code-cell", ix))
                    .size(u(CELL))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(radius)
                    .border_1()
                    .border_color(border)
                    .bg(bg)
                    .text_color(if wrong { danger } else { fg })
                    .text_xl()
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .when(self.disabled, |d| d.opacity(0.5))
                    .when(here, |d| d.shadow_xs())
                    .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                        state.read(cx).focus_handle(cx).focus(window, cx)
                    })
                    .map(|d| match c {
                        Some(c) => d.child(c.to_string()),
                        None => d.when(here && blink, |d| {
                            d.child(div().h(u(20.)).w_0().border_l_2().border_color(caret))
                        }),
                    })
                    .into_any_element()
            })
            .collect();
        let half = len.div_ceil(2);
        let mut cells = cells.into_iter();
        let group = |n: usize, cells: &mut dyn Iterator<Item = gpui_kit::AnyElement>| {
            div().flex().gap(u(CELL_GAP)).children(cells.take(n))
        };
        let first = group(half, &mut cells);
        let second = group(len - half, &mut cells);
        let row = div()
            .flex()
            .items_center()
            .gap(u(GROUP_GAP))
            .child(first)
            .child(second);
        let row = match self.wrong {
            Some(n) => row
                .with_animation(
                    SharedString::from(format!("code-wrong-{n}")),
                    Animation::new(std::time::Duration::from_millis(420)),
                    |row, t| {
                        let sway = (t * std::f32::consts::PI * 6.).sin() * 7. * (1. - t);
                        row.ml(u(sway)).mr(u(-sway))
                    },
                )
                .into_any_element(),
            None => row.into_any_element(),
        };
        let on_paste = self.on_paste;
        let menu_paste = on_paste.clone();
        let disabled = self.disabled;
        let typing = self.typing.clone();
        let typing_keys = self.typing;
        let paint_focus = focus.clone();
        div()
            .id("code-input")
            .debug_selector(|| "code-input".into())
            .relative()
            .flex()
            .capture_key_down(move |ev, window, cx| {
                let keys = &ev.keystroke;
                let mods = keys.modifiers;
                let skip = (!mods.control && !mods.alt && !mods.platform)
                    .then(|| keys.key.chars().next().and_then(digit))
                    .flatten()
                    .filter(|_| keys.key.chars().count() == 1);
                typing_keys.update(cx, |t, _| t.skip = skip);
                let paste = keys.key == "v" && mods.secondary(cx);
                if !paste || disabled {
                    return;
                }
                let Some(code) = clipboard_code(cx) else {
                    return;
                };
                window.prevent_default();
                cx.stop_propagation();
                on_paste(code, window, cx);
            })
            .child(
                OtpInput::new(&self.state)
                    .disabled(self.disabled)
                    .child(row),
            )
            // The input handler for characters (IME, Unicode input), registered while the cells
            // have focus.
            .when(!disabled, |d| {
                d.child(
                    canvas(
                        |_, _, _| {},
                        move |bounds, _, window, cx| {
                            if paint_focus.is_focused(window) {
                                window.handle_input(
                                    &paint_focus,
                                    ElementInputHandler::new(bounds, typing.clone()),
                                    cx,
                                );
                            }
                        },
                    )
                    .absolute()
                    .size_full(),
                )
            })
            .test_support()
            .context_menu(paste_menu(menu_paste, disabled))
    }
}

/// The code on the clipboard, if its text holds one.
fn clipboard_code(cx: &App) -> Option<String> {
    cx.read_from_clipboard()
        .and_then(|item| item.text())
        .as_deref()
        .and_then(code_from)
}

/// Right-click on the cells: Paste does what Ctrl+V does.
fn paste_menu(
    on_paste: Callback<String>,
    disabled: bool,
) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
    move |menu, window, _| {
        let on_paste = on_paste.clone();
        menu_frame(menu, window).item(menu_item("menu-paste", copy::PASTE).on_click(
            move |_, window, cx| {
                if let Some(code) = clipboard_code(cx).filter(|_| !disabled) {
                    on_paste(code, window, cx);
                }
            },
        ))
    }
}
