//! The one door to gpui-kit's styled components (`gpui_kit::component`). Lobby draws its own
//! controls on `gpui_kit::base` (see `crate::ui`). From the styled layer it keeps only what base
//! has no primitive for. Every other file imports those from here, and
//! `cargo xtask check-kit` fails on `component::` anywhere else in the crate, so a new use of the
//! styled layer is a line added to this file in review.

pub use gpui_kit::component::{
    ActiveTheme, Colorize, Theme, ThemeColor, ThemeConfig, ThemeMode, ThemeRegistry, TitleBar,
    WindowExt, oklch, window_paddings,
};

#[cfg(target_os = "linux")]
pub use gpui_kit::component::window_corner_radii;

pub mod dialog {
    use gpui_kit::{
        App, Window,
        component::{
            WindowExt as _,
            dialog::{Dialog, DialogEntrance},
        },
    };

    pub use gpui_kit::component::dialog::{AlertDialog, DialogFooter};

    /// Opens a dialog that fades in as it settles a short way into place, on the theme's motion
    /// (`appearance::apply`), and leaves the same way.
    pub fn open(
        window: &mut Window,
        cx: &mut App,
        build: impl Fn(Dialog, &mut Window, &mut App) -> Dialog + 'static,
    ) {
        window.open_dialog(cx, move |dialog, window, cx| {
            build(dialog.entrance(DialogEntrance::FadeSlide), window, cx)
        });
    }

    /// [`open`] for an alert.
    pub fn open_alert(
        window: &mut Window,
        cx: &mut App,
        build: impl Fn(AlertDialog, &mut Window, &mut App) -> AlertDialog + 'static,
    ) {
        window.open_alert_dialog(cx, move |alert, window, cx| {
            build(alert.entrance(DialogEntrance::FadeSlide), window, cx)
        });
    }
}

pub mod menu {
    pub use gpui_kit::component::menu::{ContextMenuExt, DropdownMenu, PopupMenu, PopupMenuItem};
}

pub mod message_scroller {
    pub use gpui_kit::component::message_scroller::{MessageScroller, MessageScrollerState};
}

pub mod native_menu {
    pub use gpui_kit::component::native_menu::NativeMenu;
}

pub mod notification {
    pub use gpui_kit::component::notification::Notification;
}

pub mod searchable_list {
    pub use gpui_kit::component::searchable_list::{
        SearchableListDelegate, SearchableListItem, SearchableVec,
    };
}

pub mod select {
    pub use gpui_kit::component::select::{Select, SelectEvent, SelectState};
}
