//! UI scale. Layout sizes are written as the pixels they have at 100% and drawn in rems, so they
//! follow the window's rem size, which Lobby sets from the person's scale (see
//! `appearance::apply`). Only what must not scale stays in `px(..)`: hairlines and borders, the
//! OS window's size, and values that go to the OS.

use crate::kit::ActiveTheme as _;
use gpui_kit::{App, Pixels, Rems, Window, px, rems};

/// The rem size at 100%.
pub const BASE_REM: f32 = 16.;
/// The kit's monospace size at 100%.
pub const BASE_MONO: f32 = 13.;
/// The kit's radii at 100%.
pub const BASE_RADIUS: f32 = 6.;
pub const BASE_RADIUS_LG: f32 = 8.;

/// A size in design pixels (what it measures at 100%) as a length that scales. Exact at 100%:
/// the division and the rem size are powers of two.
pub fn u(design_px: f32) -> Rems {
    rems(design_px / BASE_REM)
}

/// How much bigger than 100% the window draws, from its rem size.
pub fn window_scale(window: &Window) -> f32 {
    f32::from(window.rem_size()) / BASE_REM
}

/// The same from the theme's font size, which the window's rem size follows: for code that has
/// no window to ask.
pub fn app_scale(cx: &App) -> f32 {
    f32::from(cx.theme().font_size) / BASE_REM
}

/// The window's content size in design pixels: what a layout rule written for 100% is compared
/// with, so a bigger scale folds the sidebar sooner and never gives a layout less room than it
/// was designed for.
pub fn design_viewport(window: &Window) -> (f32, f32) {
    let scale = window_scale(window);
    let size = window.viewport_size();
    (
        f32::from(size.width) / scale,
        f32::from(size.height) / scale,
    )
}

/// A design size as real pixels, for the few APIs that take only `Pixels`.
pub fn device(design_px: f32, window: &Window) -> Pixels {
    px(design_px * window_scale(window))
}

/// [`device`] for code that has no window to ask.
pub fn device_in(design_px: f32, cx: &App) -> Pixels {
    px(design_px * app_scale(cx))
}
