//! The title bar's controls: `ui::Size::Bar` buttons in a wrapper that occludes, because the
//! title bar drags the window everywhere else.

use gpui_kit::{
    App, Div, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, Styled as _, Window,
    assets::IconName, div,
};

use crate::scale::u;
use crate::ui;

const BADGE: f32 = 6.;

pub struct BarButton;

impl BarButton {
    /// The cog that opens Settings, and closes them again while they are open.
    pub fn settings(open: bool, on_click: impl Fn(&mut Window, &mut App) + 'static) -> Div {
        let button = ui::Button::new("settings-toggle")
            .size(ui::Size::Bar)
            .icon(IconName::Settings)
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
            Some(color) => div()
                .relative()
                .child(control)
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .right_0()
                        .size(u(BADGE))
                        .rounded_full()
                        .bg(color),
                )
                .into_any_element(),
        };
        div().occlude().child(control)
    }
}
