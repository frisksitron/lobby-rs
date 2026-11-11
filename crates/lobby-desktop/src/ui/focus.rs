//! The keyboard focus ring every control draws: the theme's ring colour on the edge and a band
//! outside it that follows the corners. It sits outside the control, so a clipping parent needs
//! room for it.

use gpui_kit::{
    AbsoluteLength, AnyElement, App, Corners, IntoElement as _, Pixels, StyleRefinement,
    Styled as _, Window, div, px,
};

use crate::kit::ActiveTheme as _;

pub(super) const RING_WIDTH: f32 = 3.;
pub(super) const RING_OPACITY: f32 = 0.5;

/// The ring around a control whose box is `style`, for its caller to add as a child. The caller
/// also gives the control's own edge the ring colour, where it has one.
pub fn ring(style: &StyleRefinement, window: &Window, cx: &App) -> AnyElement {
    let rem = window.rem_size();
    let length = |l: Option<AbsoluteLength>| l.map(|l| l.to_pixels(rem)).unwrap_or_default();
    let radii = &style.corner_radii;
    let borders = &style.border_widths;
    let width = px(RING_WIDTH); // px: the ring is an edge, as wide at every UI scale
    let corner = |r: Option<AbsoluteLength>| length(r) + width;
    let radius = Corners {
        top_left: corner(radii.top_left),
        top_right: corner(radii.top_right),
        bottom_left: corner(radii.bottom_left),
        bottom_right: corner(radii.bottom_right),
    };
    let edge = |border: Option<AbsoluteLength>| -> Pixels { -(width + length(border)) };
    div()
        .flex_none()
        .absolute()
        .top(edge(borders.top))
        .left(edge(borders.left))
        .right(edge(borders.right))
        .bottom(edge(borders.bottom))
        .border(width)
        .border_color(cx.theme().ring.alpha(RING_OPACITY))
        .rounded_tl(radius.top_left)
        .rounded_tr(radius.top_right)
        .rounded_bl(radius.bottom_left)
        .rounded_br(radius.bottom_right)
        .into_any_element()
}
