//! The surface Lobby's own popups stand on: the popover fill, its edge drawn as a ring of the
//! foreground and a short shadow. The ring is a shadow rather than a border so the shadow shows
//! through it, the way the kit's menus and selects draw theirs.

use gpui_kit::{App, BoxShadow, Hsla, Pixels, Styled, hsla};

use crate::kit::ActiveTheme as _;

const RING_INK: f32 = 0.1;
const SHADOW_INK: f32 = 0.1;

pub fn popover_surface<E: Styled>(element: E, cx: &App) -> E {
    let theme = cx.theme();
    let ink = hsla(0., 0., 0., SHADOW_INK);
    element
        .bg(theme.popover)
        .text_color(theme.popover_foreground)
        .shadow(vec![
            layer(0., 0., 1., theme.foreground.alpha(RING_INK)),
            layer(4., 3., -1., ink),
            layer(2., 2., -2., ink),
        ])
        .rounded(theme.radius)
}

fn layer(y: f32, blur: f32, spread: f32, color: Hsla) -> BoxShadow {
    BoxShadow::new(Pixels::ZERO, Pixels::from(y), color)
        .blur_radius(Pixels::from(blur))
        .spread_radius(Pixels::from(spread))
}
