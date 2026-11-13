//! A task that finishes in the window its view is in when it finishes. `spawn_in` binds a task to
//! the window it started in, and on Linux that window can be replaced (`shell/reopen.rs`) while
//! the task waits: its `update_in` then fails and the bookkeeping it was to do is never done.

use gpui_kit::{AppContext as _, AsyncApp, Context, WeakEntity, Window};

/// Runs `f` on `this` in the window it is now in. `None` when the view is gone, in no window, or
/// the app is quitting (`AsyncApp::with_window` refuses then, as `update_in` did).
pub fn update<T: 'static, R>(
    this: &WeakEntity<T>,
    cx: &mut AsyncApp,
    f: impl FnOnce(&mut T, &mut Window, &mut Context<T>) -> R,
) -> Option<R> {
    cx.with_window(this.entity_id(), |window, cx| {
        this.update(cx, |view, cx| f(view, window, cx)).ok()
    })
    .flatten()
}
