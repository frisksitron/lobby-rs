//! The dimming behind a dialog, drawn inside the window's rounded frame. The kit dims its whole
//! surface, and on Linux that includes the margin the window's shadow is drawn in, so the dim
//! showed as a dark box with square corners around the window. `appearance::apply` clears the
//! kit's colour and keeps it here; the dialogs still catch the click on their backdrop.

use gpui_kit::{
    AnyElement, App, Global, Hsla, IntoElement as _, ParentElement as _, Styled as _, Window, div,
};

use crate::kit::WindowExt as _;
use crate::scale::round_to_window;

/// The theme's overlay colour, which the theme itself no longer carries: Lobby's own dims read
/// it through `appearance::overlay`.
pub struct DialogScrim(pub Hsla);

impl Global for DialogScrim {}

/// `content`, dimmed while a dialog is open. The dialog is above it, in the kit's layer.
pub fn dim_for_dialogs(content: AnyElement, window: &mut Window, cx: &mut App) -> AnyElement {
    let scrim = cx.try_global::<DialogScrim>().map(|scrim| scrim.0);
    let dim = scrim.filter(|_| window.has_active_dialog(cx));
    let dim = dim.map(|color| round_to_window(div().absolute().inset_0().bg(color), window, cx));
    div()
        .relative()
        .size_full()
        .child(content)
        .children(dim)
        .into_any_element()
}
