//! Linux: Hide and Show. Mutter will not un-minimize or raise a window on a request that carries
//! no input token, which is all a tray click or a second launch can send. So Hide only minimizes
//! the window, and the Shell always has one: every Session event finds its window, and nothing a
//! person made (the route, a draft, Voice) is lost. Show replaces a window that Hide minimized
//! with a new one for the same Shell, which the desktop maps; any other window is asked to come
//! forward and is replaced only if the desktop does not bring it. A desktop that cannot minimize
//! (sway) has nothing to hide with, and a Shell with no window loses the Session effects that run
//! in one: there the close button quits, as it does with no tray icon.
//!
//! The Shell's tasks hold no window (`crate::in_window`), so the replacement is the one moment a
//! window goes, and it is done in one step: open the new window, then remove the old one.

use std::time::Duration;

use gpui_kit::{AsyncApp, Context, DisplayId, FocusHandle, Task, WeakEntity, Window, WindowBounds};
use tokio::sync::oneshot;
use tracing::{info, warn};

use super::{QuitState, Shell};
use crate::host;
use crate::kit::WindowExt as _;

/// How long a window gets to become the active one after it is asked to: if the desktop did not
/// bring it forward by then it never will, and a new window replaces it.
const ACTIVATE_WITHIN: Duration = Duration::from_millis(400);
/// How often the window is looked at while it is waited for.
const ACTIVATE_POLL: Duration = Duration::from_millis(25);
/// How long the tray icon has to register. A bus that does not answer in that time means no tray,
/// as when no watcher is there.
const TRAY_REGISTER_WITHIN: Duration = Duration::from_secs(3);

#[derive(Default)]
pub(super) struct Reopen {
    /// Hide minimized the window and no replacement has come since. Only a replacement clears it,
    /// so a Show that is itself replaced by a newer Show still finds it set.
    hidden: bool,
    /// A link that came with a Show: opened in the window that results, by the task that ends it.
    link: Option<String>,
    /// Hide's minimize, or Show's replacement, that has not run yet. A newer Hide or Show drops
    /// it, which cancels it.
    action: Option<Task<()>>,
    /// A Show's wait for the desktop to bring the window forward. A newer Show or a Hide drops it.
    check: Option<Task<()>>,
    _tray_wait: Option<Task<()>>,
    minimize: Option<fn(&mut Window)>,
    /// What the desktop has said it can do with the window: sway offers no minimize, so Hide would
    /// leave the window in plain sight, and the Shell cannot be without one (its Session effects
    /// run in a window). There the close button quits, as it does with no tray icon.
    cannot_minimize: bool,
    minimizes: Option<fn(&Window) -> bool>,
    /// Hide has minimized the window, and neither a replacement nor the Member has brought it back
    /// since.
    minimized_by_hide: bool,
}

/// What a window that goes leaves for the one that replaces it.
struct Left {
    bounds: WindowBounds,
    display: Option<DisplayId>,
    focus: Option<FocusHandle>,
}

impl Shell {
    /// Hide: minimizes the window and remembers that this was done here.
    pub(super) fn hide_window(&mut self, cx: &mut Context<Self>) {
        self.reopen.hidden = true;
        self.reopen.check = None;
        self.reopen.link = None;
        // Minimizing sends window messages, and Hide may come from inside the window's own close
        // hook: not inside that update.
        let minimize = self.reopen.minimize;
        self.reopen.action = Some(cx.spawn(async move |this, cx| {
            let Ok(window) = this.read_with(cx, |shell, _| shell.window) else {
                return;
            };
            let _ = window.update(cx, |_, window, cx| {
                let _ = this.update(cx, |shell, _| shell.reopen.minimized_by_hide = true);
                match minimize {
                    Some(minimize) => minimize(window),
                    None => window.minimize_window(),
                }
            });
        }));
    }

    /// The window is active: if Hide had minimized it, the Member has brought it back themselves
    /// (the dock, Alt+Tab). Lobby no longer counts it as hidden, so the close button hides it
    /// once more and a Show from the tray leaves it be. Before Hide has minimized it an
    /// activation is no such thing.
    pub(super) fn window_is_up(&mut self, cx: &mut Context<Self>) {
        if !std::mem::take(&mut self.reopen.minimized_by_hide) {
            return;
        }
        info!("the window was brought back without Show");
        // A Hide that is still to run would minimize the window the Member just brought back. A
        // Show's task is kept: it finds the window not hidden and only opens its link.
        if self.hidden_to_tray {
            self.reopen.action = None;
        }
        self.reopen.hidden = false;
        self.hidden_to_tray = false;
        cx.notify();
    }

    /// Quit: no window may open or take focus while the app shuts down.
    pub(super) fn cancel_reopen(&mut self) {
        self.reopen.check = None;
        self.reopen.action = None;
        self.reopen.link = None;
    }

    /// What Hide does to the window instead of minimizing it: the test platform has no window
    /// that can be.
    pub fn set_minimizer(&mut self, minimize: fn(&mut Window)) {
        self.reopen.minimize = Some(minimize);
    }

    /// Whether the desktop can minimize the window, in place of what the window itself says.
    pub fn set_desktop_minimizes(&mut self, minimizes: fn(&Window) -> bool) {
        self.reopen.minimizes = Some(minimizes);
    }

    /// Reads whether the desktop can minimize the window. The compositor says so with its first
    /// configure, after the window is made, so this is read when the Member acts on the window
    /// (it is activated, its close is asked for), not when it is opened.
    pub(super) fn learn_desktop(&mut self, window: &Window) {
        let minimizes = match self.reopen.minimizes {
            Some(minimizes) => minimizes(window),
            None => window.window_controls().minimize,
        };
        if minimizes == self.reopen.cannot_minimize {
            self.reopen.cannot_minimize = !minimizes;
            if !minimizes {
                info!("the desktop cannot minimize a window: the close button quits");
            }
        }
    }

    /// Whether Hide can take the window out of the Member's sight.
    pub(super) fn desktop_hides(&self) -> bool {
        !self.reopen.cannot_minimize
    }

    /// Show: a window that Hide minimized is replaced at once, and any other is asked to come
    /// forward and replaced if it does not.
    pub(super) fn bring_window_back(&mut self, cx: &mut Context<Self>) {
        self.reopen.check = None;
        if self.reopen.hidden {
            self.reopen.action = Some(cx.spawn(async move |this, cx| {
                // The Member may have brought the window back before this ran.
                if this
                    .read_with(cx, |shell, _| shell.reopen.hidden)
                    .unwrap_or(false)
                {
                    replace_window(&this, cx);
                }
                finish_show(&this, cx);
            }));
            return;
        }
        let asks_desktop = self.show_takes_focus;
        self.reopen.check = Some(cx.spawn(async move |this, cx| {
            if asks_desktop && !window_comes_forward(&this, cx).await {
                info!("the desktop did not bring the window forward: opening a new one");
                replace_window(&this, cx);
            }
            finish_show(&this, cx);
        }));
    }

    /// A link that came from outside: the window comes forward first, and the link opens in the
    /// window that results, in the same task.
    pub(super) fn open_link_after_show(&mut self, link: String, cx: &mut Context<Self>) {
        self.reopen.link = Some(link);
        self.show_window(cx);
    }

    /// Registers the tray icon's answer, which comes from a thread of its own, within
    /// [`TRAY_REGISTER_WITHIN`]. Until it does the close button quits.
    pub fn keep_in_tray_when(
        &mut self,
        pending: oneshot::Receiver<Option<host::Tray>>,
        cx: &mut Context<Self>,
    ) {
        let limit = cx.background_executor().timer(TRAY_REGISTER_WITHIN);
        self.reopen._tray_wait = Some(cx.spawn(async move |this, cx| {
            // An icon that answers after the limit finds the receiver gone and is dropped.
            let tray = futures_lite::future::or(async { pending.await.ok().flatten() }, async {
                limit.await;
                None
            })
            .await;
            let _ = this.update(cx, |shell, _| match tray {
                Some(tray) if shell.quit == QuitState::Running => {
                    shell.keep_in_tray(Some(tray));
                }
                Some(_) => {}
                None => warn!("no tray icon: the close button quits"),
            });
        }));
    }

    /// A dialog lives in its window, and so do the tasks bound to it: what the Shell and its views
    /// keep about a dialog that is open goes with it, so nothing says "open" for a dialog the next
    /// window does not have.
    fn dismiss_dialogs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.drop_switch(window, cx);
        self.close_add_lobby(window, cx);
        if let Some(view) = self.lobby_view().cloned() {
            view.update(cx, |view, cx| view.dismiss_dialogs(window, cx));
        }
        self.end_errand(cx);
        // The leave confirmation keeps no state of its own.
        window.close_all_dialogs(cx);
    }

    fn leave_window(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Left {
        self.dismiss_dialogs(window, cx);
        // The Settings page covers the route's view, and GPUI gives a view the new window only
        // when that draws it: a view left out would find no window for its events (a Member being
        // admitted behind the page would never be let in).
        self.close_settings(window, cx);
        if self.setup.remember {
            self.remember_window(window, cx);
        }
        self.window_subscriptions.clear();
        Left {
            bounds: window.window_bounds(),
            display: window.display(cx).map(|display| display.id()),
            focus: window.focused(cx),
        }
    }

    fn window_is_back(
        &mut self,
        window: &mut Window,
        focus: Option<FocusHandle>,
        cx: &mut Context<Self>,
    ) {
        self.bind_window(window, cx);
        // The route's view keeps its own watches on the window, which stayed on the old one.
        if let Some(view) = self.lobby_view().cloned() {
            view.update(cx, |view, cx| {
                view.bind_window(window, cx);
                view.window_activated(window, cx);
            });
        }
        if let Some(view) = self.auth_view().cloned() {
            view.update(cx, |view, cx| {
                view.bind_window(window, cx);
                view.window_activated(window, cx);
            });
        }
        if self.show_takes_focus
            && let Some(focus) = focus
        {
            window.focus(&focus, cx);
        }
    }
}

/// Asks the desktop to bring the window forward and gives it [`ACTIVATE_WITHIN`], looking each
/// [`ACTIVATE_POLL`]. A window that is already active, or one that is gone, is as asked for.
async fn window_comes_forward(this: &WeakEntity<Shell>, cx: &mut AsyncApp) -> bool {
    let Ok(window) = this.read_with(cx, |shell, _| shell.window) else {
        return true;
    };
    let active = |cx: &mut AsyncApp| {
        window
            .update(cx, |_, window, _| window.is_window_active())
            .unwrap_or(true)
    };
    let _ = window.update(cx, |_, window, _| window.activate_window());
    let mut waited = Duration::ZERO;
    while !active(cx) {
        if waited >= ACTIVATE_WITHIN {
            return false;
        }
        cx.background_executor().timer(ACTIVATE_POLL).await;
        waited += ACTIVATE_POLL;
    }
    true
}

/// Opens a new window for the Shell, then removes the old one. No await in between: nothing else
/// runs while the Shell has two windows.
fn replace_window(this: &WeakEntity<Shell>, cx: &mut AsyncApp) {
    let Ok(old) = this.read_with(cx, |shell, _| {
        (shell.quit == QuitState::Running).then_some(shell.window)
    }) else {
        return;
    };
    let Some(old) = old else {
        return;
    };
    let left = old.update(cx, |_, window, cx| {
        this.update(cx, |shell, cx| shell.leave_window(window, cx))
    });
    let Ok(Ok(left)) = left else {
        return;
    };
    let Some(shell) = this.upgrade() else {
        return;
    };
    let mut setup = shell.read_with(cx, |shell, _| shell.setup);
    setup.display = left.display.or(setup.display);
    let bounds = left.bounds;
    let focus = left.focus;
    let opened = cx.update(|cx| {
        let options = Shell::window_options(&setup, Some(bounds), cx);
        gpui_kit::open_window(options, cx, {
            let shell = shell.clone();
            move |window, cx| {
                shell.update(cx, |shell, cx| shell.window_is_back(window, focus, cx));
                shell
            }
        })
    });
    match opened {
        Ok(_) => {
            info!("a new window for the same Shell");
            let _ = old.update(cx, |_, window, _| window.remove_window());
            let _ = this.update(cx, |shell, _| {
                shell.reopen.hidden = false;
                shell.reopen.minimized_by_hide = false;
            });
        }
        Err(error) => {
            warn!(%error, "cannot open a new window: the old one stays");
            let _ = old.update(cx, |_, window, cx| {
                this.update(cx, |shell, cx| shell.bind_window(window, cx))
            });
        }
    }
}

/// The end of every Show: a link that came with it opens in the window the Shell has now, which
/// is the new one when the window was replaced.
fn finish_show(this: &WeakEntity<Shell>, cx: &mut AsyncApp) {
    let Ok((window, link)) = this.update(cx, |shell, _| (shell.window, shell.reopen.link.take()))
    else {
        return;
    };
    if let Some(link) = link {
        let _: Result<_, _> = window.update(cx, |_, window, cx| {
            this.update(cx, |shell, cx| shell.show_link(&link, window, cx))
        });
    }
}
