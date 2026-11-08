//! The colours of the app: Living Room, the one theme, dark, from `themes/living-room.json`.
//! [`apply`] is the one place that changes the global `Theme`. It does the same thing every time
//! it is called with the same settings, so the Shell calls it at startup and on every change.

use crate::scale::{BASE_MONO, BASE_RADIUS, BASE_RADIUS_LG, BASE_REM};
use gpui_kit::{
    App,
    component::{Theme, ThemeConfig, ThemeMode, ThemeRegistry},
    px,
};
use lobby_client::Settings;
use std::rc::Rc;

const NAME: &str = "Living Room";
const FILE: &str = include_str!("../themes/living-room.json");

fn living_room(cx: &mut App) -> Rc<ThemeConfig> {
    if !ThemeRegistry::global(cx).themes().contains_key(NAME) {
        ThemeRegistry::global_mut(cx)
            .load_themes_from_str(FILE)
            .expect("the bundled theme is valid");
    }
    ThemeRegistry::global(cx).themes()[NAME].clone()
}

/// Shows `settings` on the global `Theme`: Living Room, and Lobby's own settings (toasts, font,
/// sizes), which loading a theme wipes.
pub fn apply(settings: &Settings, cx: &mut App) {
    let shown = living_room(cx);
    let radii = (shown.radius, shown.radius_lg);
    Theme::global_mut(cx).dark_theme = shown;
    Theme::change(ThemeMode::Dark, None, cx);
    let scale = f32::from(settings.ui_scale) / 100.;
    Theme::update(cx, |theme| {
        theme.font_family = crate::fonts::FAMILY.into();
        // The window's rem size follows the font size, so this one number scales every `u(..)`.
        theme.font_size = px(BASE_REM * scale); // px: theme unit
        theme.mono_font_size = px(BASE_MONO * scale); // px: theme unit
        let radius = |own: Option<usize>, base: f32| {
            px(own.map_or(base, |r| r as f32) * scale) // px: theme unit
        };
        theme.radius = radius(radii.0, BASE_RADIUS);
        theme.radius_lg = radius(radii.1, BASE_RADIUS_LG);
        // Toasts start under the title bar in the main column; the Lobby view keeps them beside
        // the member panel.
        let toasts = &mut theme.notification;
        toasts.width = px(360. * scale); // px: theme unit
        toasts.max_items = 3;
        let top = (crate::components::TITLE_BAR_HEIGHT + 12.) * scale;
        toasts.margins.top = px(top); // px: theme unit
        let docked = crate::lobby_view::SidebarLayout::Docked(crate::lobby_view::SIDEBAR_SLIM_W);
        toasts.margins.right = crate::lobby_view::toast_right_margin(docked, scale);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::{TestAppContext, component::ThemeColor};

    /// The kit fills a colour the file leaves out from Default Dark, whose cold greys and blues
    /// would then show among the warm ones.
    #[gpui_kit::test]
    fn every_colour_comes_from_living_room(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            apply(&Settings::default(), cx);
            // Each colour serialises as "#rrggbbaa"; a clear one looks the same in any theme.
            let shown = serde_json::to_value(Theme::global(cx).colors).unwrap();
            let kit = serde_json::to_value(*ThemeColor::dark()).unwrap();
            let left: Vec<&String> = shown
                .as_object()
                .unwrap()
                .iter()
                .filter(|(slot, hex)| {
                    kit[slot.as_str()] == **hex && !hex.as_str().unwrap().ends_with("00")
                })
                .map(|(slot, _)| slot)
                .collect();
            assert!(left.is_empty(), "still Default Dark: {left:?}");
        });
    }
}
