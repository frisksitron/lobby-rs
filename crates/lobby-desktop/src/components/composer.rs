//! The composer's formatting bar: one small ghost button per Markdown construct, each with its
//! shortcut in its tooltip. The constructs around the caret show as on. The Markdown itself comes
//! from `lobby_client::compose`.

use gpui_kit::{
    Anchor, App, IntoElement, KeyDownEvent, ParentElement as _, RenderOnce, Styled as _, Window,
    assets::IconName,
    component::{
        ActiveTheme as _, Icon, Selectable as _, Sizable as _,
        button::{Button, ButtonVariants as _},
        menu::{DropdownMenu as _, PopupMenuItem},
        separator::Separator,
        toolbar::{Toolbar, ToolbarGroup},
    },
    div,
    prelude::FluentBuilder as _,
};
use lobby_client::compose::Format;

use super::{Callback, ICON_SM, SURFACE_INNER_RADIUS};
use crate::scale::{device, device_in, u};

pub const FORMAT_BAR_HEIGHT: f32 = 28.;

pub fn format_icon(format: Format) -> IconName {
    match format {
        Format::Bold => IconName::Bold,
        Format::Italic => IconName::Italic,
        Format::Strike => IconName::Strikethrough,
        Format::Code => IconName::Code,
        Format::CodeBlock => IconName::SquareCode,
        Format::BulletList => IconName::List,
        Format::NumberedList => IconName::ListOrdered,
        Format::Quote => IconName::TextQuote,
        Format::Link => IconName::Link,
    }
}

pub fn format_id(format: Format) -> &'static str {
    match format {
        Format::Bold => "format-bold",
        Format::Italic => "format-italic",
        Format::Strike => "format-strike",
        Format::Code => "format-code",
        Format::CodeBlock => "format-code-block",
        Format::BulletList => "format-bullet-list",
        Format::NumberedList => "format-numbered-list",
        Format::Quote => "format-quote",
        Format::Link => "format-link",
    }
}

pub fn format_for(ev: &KeyDownEvent) -> Option<Format> {
    let k = &ev.keystroke;
    let m = &k.modifiers;
    if !m.control || m.alt || m.platform {
        return None;
    }
    let pressed = format!(
        "ctrl-{}{}",
        if m.shift { "shift-" } else { "" },
        k.key.to_ascii_lowercase()
    );
    Format::ALL.into_iter().find(|f| f.keystroke() == pressed)
}

#[derive(IntoElement)]
pub struct FormatBar {
    on_format: Callback<Format>,
    compact: bool,
    active: Vec<Format>,
}

impl FormatBar {
    pub fn new(on_format: Callback<Format>) -> Self {
        Self {
            on_format,
            compact: false,
            active: Vec::new(),
        }
    }

    pub fn active(mut self, active: Vec<Format>) -> Self {
        self.active = active;
        self
    }

    pub fn compact(mut self, compact: bool) -> Self {
        self.compact = compact;
        self
    }
}

impl RenderOnce for FormatBar {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let fg = cx.theme().foreground;
        let glyph = fg.opacity(0.68);
        if self.compact {
            let has_active = !self.active.is_empty();
            let on = self.on_format;
            let active = self.active;
            return div()
                .child(
                    Button::new("format-menu")
                        .accessibility_label("Formatting")
                        .ghost()
                        .small()
                        // Folded, it stands beside Send and attach in the composer's one row.
                        .rounded(device_in(SURFACE_INNER_RADIUS, cx))
                        .text_color(glyph)
                        .icon(Icon::new(IconName::Type))
                        .tooltip("Formatting")
                        .selected(has_active)
                        .dropdown_menu_with_anchor(Anchor::BottomRight, move |menu, window, cx| {
                            let (muted, primary) =
                                (cx.theme().muted_foreground, cx.theme().primary);
                            Format::ALL.into_iter().fold(
                                menu.min_w(device(220., window)),
                                |menu, f| {
                                    let on = on.clone();
                                    let lit = active.contains(&f);
                                    menu.item(
                                        PopupMenuItem::element(move |_, _| {
                                            div()
                                                .w_full()
                                                .flex()
                                                .items_center()
                                                .gap_2()
                                                .child(
                                                    Icon::new(format_icon(f))
                                                        .size(u(ICON_SM))
                                                        .when(lit, |i| i.text_color(primary)),
                                                )
                                                .child(
                                                    div()
                                                        .flex_1()
                                                        .text_sm()
                                                        .when(lit, |d| {
                                                            d.font_weight(
                                                                gpui_kit::FontWeight::SEMIBOLD,
                                                            )
                                                        })
                                                        .child(f.label()),
                                                )
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .text_color(muted)
                                                        .child(f.shortcut()),
                                                )
                                        })
                                        .on_click(move |_, window, cx| on(f, window, cx)),
                                    )
                                },
                            )
                        }),
                )
                .into_any_element();
        }
        let button = |format: Format| {
            let on = self.on_format.clone();
            Button::new(format_id(format))
                .accessibility_label(format.label())
                .ghost()
                .small()
                .text_color(glyph)
                .selected(self.active.contains(&format))
                .icon(Icon::new(format_icon(format)))
                .tooltip(format!("{} ({})", format.label(), format.shortcut()))
                .on_click(move |_, window, cx| on(format, window, cx))
        };
        let inline = [
            Format::Bold,
            Format::Italic,
            Format::Strike,
            Format::Code,
            Format::Link,
        ];
        let blocks = [
            Format::BulletList,
            Format::NumberedList,
            Format::Quote,
            Format::CodeBlock,
        ];
        let group = |id: &'static str, label: &'static str, formats: &[Format]| {
            ToolbarGroup::new(id)
                .label(label)
                .flex()
                .items_center()
                .gap(u(1.))
                .children(formats.iter().copied().map(button))
        };
        Toolbar::new("format-bar")
            .h(u(FORMAT_BAR_HEIGHT))
            .min_h(u(FORMAT_BAR_HEIGHT))
            .p_0()
            .gap(u(1.))
            .content(group("format-inline", "Text", &inline))
            .content(Separator::vertical().mx_1().h(u(14.)))
            .content(group("format-blocks", "Blocks", &blocks))
            .into_any_element()
    }
}
