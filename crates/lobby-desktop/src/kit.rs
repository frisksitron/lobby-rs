//! The one door to gpui-kit's styled components (`gpui_kit::component`). Lobby draws its own
//! controls on `gpui_kit::base` (see `crate::ui`). From the styled layer it keeps only what base
//! has no primitive for. Every other file imports those from here, and
//! `cargo xtask check-kit` fails on `component::` anywhere else in the crate, so a new use of the
//! styled layer is a line added to this file in review.

pub use gpui_kit::component::{
    ActiveTheme, Colorize, Theme, ThemeColor, ThemeConfig, ThemeMode, ThemeRegistry, TitleBar,
    WindowExt, oklch,
};

pub mod dialog {
    pub use gpui_kit::component::dialog::{AlertDialog, DialogFooter};
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
