//! The small heading above a group of rows: sentence case, muted, 12 px semibold. The caller
//! sets the spacing around it.

use gpui_kit::{App, Div, FontWeight, ParentElement as _, SharedString, Styled as _, div};

use crate::kit::ActiveTheme as _;

pub fn section_label(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}
