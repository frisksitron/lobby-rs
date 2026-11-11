//! The one door to gpui-kit's styled components (`gpui_kit::component`). Lobby draws its own
//! controls on `gpui_kit::base` (see `crate::ui`); from the styled layer it keeps the theme, the
//! window root and the widgets base has no primitive for (menus, the select, dialogs, toasts, the
//! title bar and the message scroller). Every other file imports those from here, and
//! `cargo xtask check-kit` fails on `component::` anywhere else in the crate, so a new use of the
//! styled layer is a line added to this file in review. An export goes once its last user has
//! moved to `crate::ui`.

pub use gpui_kit::component::{
    ActiveTheme, Colorize, InteractiveElementExt, Sizable, Theme, ThemeColor, ThemeConfig,
    ThemeMode, ThemeRegistry, ThemeStyled, TitleBar, WindowExt, oklch,
};

pub mod attachment {
    pub use gpui_kit::component::attachment::{
        Attachment, AttachmentActions, AttachmentContent, AttachmentDescription, AttachmentGroup,
        AttachmentMedia, AttachmentStatus, AttachmentTitle,
    };
}

pub mod dialog {
    pub use gpui_kit::component::dialog::{AlertDialog, DialogFooter};
}

pub mod input {
    pub use gpui_kit::component::input::{
        Copy, Cut, Input, InputEvent, InputState, MoveUp, OtpEvent, OtpState, Paste, SelectAll,
        Textarea, TextareaState,
    };
}

pub mod menu {
    pub use gpui_kit::component::menu::{ContextMenuExt, DropdownMenu, PopupMenu, PopupMenuItem};
}

pub mod message {
    pub use gpui_kit::component::message::{Message, MessageAvatar, MessageContent, MessageHeader};
}

pub mod message_scroller {
    pub use gpui_kit::component::message_scroller::{MessageScroller, MessageScrollerState};
}

pub mod notification {
    pub use gpui_kit::component::notification::Notification;
}

pub mod popover {
    pub use gpui_kit::component::popover::Popover;
}

pub mod resizable {
    pub use gpui_kit::component::resizable::{HandleEdge, resize_handle};
}

pub mod scroll {
    pub use gpui_kit::component::scroll::{Scrollbar, ScrollbarAxis};
}

pub mod searchable_list {
    pub use gpui_kit::component::searchable_list::{
        SearchableListDelegate, SearchableListItem, SearchableVec,
    };
}

pub mod select {
    pub use gpui_kit::component::select::{Select, SelectEvent, SelectState};
}

pub mod text {
    pub use gpui_kit::component::text::{
        InlineElement, InlineRenderContext, MarkdownNode, MarkdownParseContext, MarkdownPlugin,
        SelectionFormat, TextView, TextViewStyle,
    };

    pub mod markdown_ast {
        pub use gpui_kit::component::text::markdown_ast::Node;
    }
}

pub mod tooltip {
    pub use gpui_kit::component::tooltip::Tooltip;
}
