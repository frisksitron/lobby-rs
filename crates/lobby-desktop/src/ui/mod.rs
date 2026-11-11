//! Lobby's general controls, drawn on gpui-kit's unstyled `base` layer: the behaviour (press,
//! keys, focus, accessibility) is base's, the look is Lobby's and comes from the theme. A control
//! lives here once two components use it. `ui` imports nothing from `components`, `lobby_client`
//! or `lobby_protocol`.

mod button;
mod empty;
mod focus;
mod icon;
mod separator;
mod size;
mod skeleton;
mod slider;
mod spinner;
mod switch;
mod tooltip;

use gpui_kit::App;

pub use button::{Button, Custom, Variant};
pub use empty::{Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle};
pub use focus::ring as focus_ring;
pub use icon::Icon;
pub use separator::Separator;
pub use size::{
    BAR_CONTROL_H, FIELD_H, ICON_MD, ICON_SM, ICON_XS, INLINE_H, PAGE_CONTROL_H,
    PAGE_CONTROL_RADIUS, ROW_HEIGHT, Size,
};
pub use skeleton::Skeleton;
pub use slider::Slider;
pub use spinner::Spinner;
pub use switch::Switch;

/// Sets up what the controls need in every window. Call after `gpui_kit::init` and before the
/// first window opens.
pub fn init(cx: &mut App) {
    tooltip::init(cx);
}
