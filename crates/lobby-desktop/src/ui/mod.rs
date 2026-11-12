//! Lobby's general controls, drawn on gpui-kit's unstyled `base` layer: the behaviour (press,
//! keys, focus, accessibility) is base's, the look is Lobby's and comes from the theme. A control
//! lives here once two components use it. `ui` imports nothing from `components`, `lobby_client`
//! or `lobby_protocol`.

mod button;
mod empty;
mod field;
mod focus;
mod icon;
mod key;
pub mod motion;
mod separator;
mod size;
mod skeleton;
mod slider;
mod spinner;
mod surface;
mod switch;
mod tooltip;

use gpui_kit::App;

pub use button::{Button, ClickHandler, Custom, PRESSABLE, PressLink, Variant};
pub use empty::{Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle};
pub use field::Field;
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
pub use surface::popover_surface;
pub use switch::Switch;
pub use tooltip::tooltip;

/// Sets up what the controls need in every window. Call after `gpui_kit::init` and before the
/// first window opens.
pub fn init(cx: &mut App) {
    button::init(cx);
    tooltip::init(cx);
}
