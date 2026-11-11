//! The floating sheet: the one surface lifted off the window, by its edge alone, with no shadow.
//! The member list lives on it.

use gpui_kit::{App, Div, Styled as _, div};

use super::tokens::SURFACE_RADIUS;
use crate::kit::ActiveTheme as _;
use crate::scale::u;

pub fn floating_sheet(cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .bg(theme.sidebar)
        .border_1()
        .border_color(theme.border)
        .rounded(u(SURFACE_RADIUS))
}
