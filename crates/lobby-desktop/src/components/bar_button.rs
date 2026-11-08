//! The title bar's controls: kit ghost buttons in a wrapper that occludes, because the title bar
//! drags the window everywhere else.

use gpui_kit::{
    App, Div, ElementId, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    Styled as _, Window,
    assets::IconName,
    component::{
        Icon, Selectable as _, Sizable as _,
        badge::Badge,
        button::{Button, ButtonVariants as _},
    },
    div,
};

use super::tokens::BAR_CONTROL_H;
use crate::scale::u;

pub struct BarButton;

impl BarButton {
    pub fn button(id: impl Into<ElementId>) -> Button {
        Button::new(id).ghost().small().h(u(BAR_CONTROL_H))
    }

    pub fn icon(id: impl Into<ElementId>, icon: IconName) -> Button {
        Self::button(id).w(u(BAR_CONTROL_H)).icon(Icon::new(icon))
    }

    /// The cog that opens Settings, and closes them again while they are open.
    pub fn settings(open: bool, on_click: impl Fn(&mut Window, &mut App) + 'static) -> Div {
        let button = Self::icon("settings-toggle", IconName::Settings)
            .accessibility_label("Settings")
            .selected(open)
            .tooltip("Settings (Ctrl+,)")
            .on_click(move |_, window, cx| on_click(window, cx));
        Self::slot(button, None)
            .debug_selector(|| "settings-toggle".into())
            .flex_shrink_0()
    }

    pub fn slot(control: impl IntoElement, badge: Option<Hsla>) -> Div {
        let control = match badge {
            None => control.into_any_element(),
            Some(color) => Badge::new()
                .dot()
                .color(color)
                .child(control)
                .into_any_element(),
        };
        div().occlude().child(control)
    }
}
