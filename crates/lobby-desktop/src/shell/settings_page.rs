//! The Settings page over the current route: opening and closing it and following the route, and
//! moving on once the Lobby the window is in let the Member leave.

use gpui_kit::{AppContext as _, Context, Window};
use tracing::info;

use super::{LobbyRoute, QuitState, Route, Shell, SwitchTarget};
use crate::kit::WindowExt as _;
use crate::settings_view::{SettingsEvent, SettingsView};

impl Shell {
    /// The title bar's cog: opens the page, or closes it while it is open.
    pub(super) fn toggle_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.is_some() {
            self.close_settings(window, cx);
        } else {
            self.open_settings(window, cx);
        }
    }

    /// Ctrl+, and the title bar's cog: the Settings page, over this route and back to it on Esc or
    /// Back. Not over a dialog that asks something.
    pub fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.quit != QuitState::Running || window.has_active_dialog(cx) {
            return;
        }
        if let Some(open) = &self.settings {
            let focus = open.read(cx).focus_handle();
            window.focus(&focus, cx);
            return;
        }
        info!("opening Settings");
        let lobbies = self.lobbies.clone();
        let tray = self.has_tray;
        let (devices, session) = (self.devices, self.session().cloned());
        let page = cx.new(|cx| SettingsView::new(lobbies, tray, devices, session, cx));
        self.settings_events = Some(cx.subscribe_in(
            &page,
            window,
            |shell, _, ev: &SettingsEvent, window, cx| match ev {
                SettingsEvent::Close => shell.close_settings(window, cx),
            },
        ));
        let focus = page.read(cx).focus_handle();
        window.focus(&focus, cx);
        self.settings = Some(page);
        self.give_page(cx);
        cx.notify();
    }

    /// The route changed under an open Settings page: its Voice meter follows the route.
    pub(super) fn follow_route(&mut self, cx: &mut Context<Self>) {
        let Some(page) = self.settings.clone() else {
            return;
        };
        let session = self.session().cloned();
        page.update(cx, |p, cx| p.set_session(session, cx));
    }

    /// The Lobby let the Member leave: the Session is shut down and its tokens forgotten before
    /// anything is shown (so no Sign In says they were removed), the Lobby leaves `state.json`
    /// and the switcher, and the window goes on to the next saved Lobby, or to the URL step when
    /// none is left.
    pub(super) fn left_lobby(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.quit != QuitState::Running || self.signing_out.is_some() {
            return;
        }
        let Route::Lobby(LobbyRoute {
            origin, session, ..
        }) = &self.route
        else {
            return;
        };
        let origin = origin.clone();
        info!(lobby = %origin, "left the Lobby");
        let task = session.update(cx, |s, cx| s.leave_lobby(cx));
        self.close_settings(window, cx);
        self.signing_out = Some(cx.spawn_in(window, async move |this, cx| {
            let _ = task.await;
            let _ = this.update_in(cx, |shell, window, cx| {
                shell.signing_out = None;
                shell.lobbies.update(cx, |l, cx| l.remove(&origin, cx));
                if shell.back_to.as_ref() == Some(&origin) {
                    shell.back_to = None;
                }
                if shell.quit != QuitState::Running || !shell.still_at(&origin) {
                    return;
                }
                let next = shell
                    .lobbies
                    .read(cx)
                    .persisted()
                    .restore_target()
                    .map(|l| l.origin.clone());
                shell.back_to = None;
                match next {
                    Some(next) => {
                        shell.show_url_step(None, window, cx);
                        shell.switch_now(SwitchTarget::Saved(next), window, cx);
                    }
                    None => shell.show_url_step(Some(""), window, cx),
                }
            });
        }));
        cx.notify();
    }

    pub fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.take().is_none() {
            return;
        }
        info!("closing Settings");
        self.settings_events = None;
        self.give_page(cx);
        match &self.route {
            Route::Lobby(lobby) => lobby.view.update(cx, |v, cx| v.focus_composer(window, cx)),
            Route::LobbyUrl(_) => self.url.update(cx, |u, cx| u.focus(window, cx)),
            Route::SignIn(sign_in) => sign_in.auth.update(cx, |a, cx| a.refocus(window, cx)),
        }
        cx.notify();
    }

    /// The Lobby view draws the page under its own title bar; the other routes are drawn by
    /// the Shell.
    pub(super) fn give_page(&mut self, cx: &mut Context<Self>) {
        if let Route::Lobby(lobby) = &self.route {
            let page = self.settings.clone().map(Into::into);
            lobby.view.update(cx, |v, cx| v.set_page(page, cx));
        }
    }
}
