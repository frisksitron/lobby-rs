//! Right-click menus. They are the kit's `PopupMenu`, drawn like the Lobby switcher's, so they stay
//! inside the window (the OS's own menus do not follow the theme). Every item has an id for the
//! scenarios to click by.

use gpui_kit::{
    InteractiveElement as _, ParentElement as _, TestSupportExt as _, Window,
    component::menu::{PopupMenu, PopupMenuItem},
    div,
};

use crate::scale::device;

/// The narrowest a right-click menu is, so a short word does not make a sliver.
const MENU_MIN_W: f32 = 140.;

/// A menu item. Its id is the element the scenarios click.
pub fn menu_item(id: &'static str, label: &'static str) -> PopupMenuItem {
    PopupMenuItem::element(move |_, _| {
        div()
            .id(id)
            .debug_selector(|| id.into())
            .child(label)
            .test_support()
    })
}

pub fn menu_frame(menu: PopupMenu, window: &Window) -> PopupMenu {
    menu.min_w(device(MENU_MIN_W, window))
}
