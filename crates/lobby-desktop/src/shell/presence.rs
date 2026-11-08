//! How the app is present to the OS and to the Lobby: waking from sleep, the window's place and
//! the tray, what the OS asks of it from outside, and idle presence.

use std::time::Duration;

use gpui_kit::{Context, Window};
use lobby_client::{Intent, SoundPlayer};
use tokio::sync::mpsc::UnboundedReceiver;
use tracing::info;

use super::{IdleSource, QuitState, Shell};
use crate::host::{self, HostEvent, single_instance};
use crate::window_bounds;

/// No keyboard or mouse input this long means idle presence.
pub const IDLE_AFTER: Duration = Duration::from_secs(10 * 60);
pub const IDLE_CHECK_EVERY: Duration = Duration::from_secs(30);

impl Shell {
    pub fn on_system_wake(&mut self, cx: &mut Context<Self>) {
        info!("the OS resumed");
        if let Some(session) = self.session().cloned() {
            session.update(cx, |s, cx| s.dispatch(Intent::ProbeNow, cx));
        }
        self.check_idle(cx);
    }

    pub(super) fn remember_window(&mut self, window: &Window, cx: &mut Context<Self>) {
        let display = window.display(cx).and_then(|d| d.uuid().ok());
        let saved = window_bounds::to_saved(window.window_bounds(), display.map(|u| u.to_string()));
        self.lobbies
            .update(cx, |lobbies, cx| lobbies.set_window(saved, cx));
    }

    /// There is an icon that brings the window back, so the close button may hide it (when
    /// Settings say so); `tray` is that icon (`None` in tests, which have no notification area),
    /// removed when the Quit flow is done.
    pub fn keep_in_tray(&mut self, tray: Option<host::Tray>) {
        self.has_tray = true;
        self.tray = tray;
    }

    /// Where the UI sounds go. A test hands in `SoundPlayer::channel()`'s, the app's own opens the
    /// system default output.
    pub fn set_sound_player(&mut self, player: SoundPlayer) {
        self.sounds = Some(player);
    }

    pub fn hold_instance(&mut self, guard: single_instance::Guard) {
        self.instance = Some(guard);
    }

    /// Takes what the OS asks of the app from outside its window: the tray's menu and click.
    pub fn listen(&mut self, mut events: UnboundedReceiver<HostEvent>, cx: &mut Context<Self>) {
        self.host_events = Some(cx.spawn(async move |this, cx| {
            while let Some(event) = events.recv().await {
                let handled = this.update(cx, |shell, cx| match event {
                    HostEvent::Show => shell.show_window(cx),
                    HostEvent::Quit => shell.quit(cx),
                    HostEvent::QuitForSetup => {
                        crate::updates::AppUpdates::keep_update_staged(cx);
                        shell.quit(cx);
                    }
                    HostEvent::Relaunched => {
                        info!("shown by a second launch");
                        shell.show_window(cx);
                    }
                });
                if handled.is_err() {
                    break;
                }
            }
        }));
    }

    /// The window's close button. With the tray icon there and "Close to tray" on, it never quits:
    /// Quit is Ctrl+Q and the icon's menu. Without an icon the app would be unreachable, so it
    /// quits; with the setting off, the person chose that.
    pub fn on_close_requested(&mut self, cx: &mut Context<Self>) {
        if self.quit != QuitState::Running {
            return;
        }
        if self.has_tray && self.lobbies.read(cx).settings().close_to_tray {
            self.hide_to_tray(cx);
        } else {
            self.quit(cx);
        }
    }

    pub fn hide_to_tray(&mut self, cx: &mut Context<Self>) {
        if self.quit != QuitState::Running || self.hidden_to_tray {
            return;
        }
        info!("window hidden to the tray");
        self.hidden_to_tray = true;
        (self.window_host)(self.native, host::WindowCommand::Hide, cx);
        cx.notify();
    }

    /// Brings the window back and to the front, from the tray or a second launch. Also for a window
    /// that is merely behind others.
    pub fn show_window(&mut self, cx: &mut Context<Self>) {
        if self.quit != QuitState::Running {
            return;
        }
        info!("window shown");
        self.hidden_to_tray = false;
        let command = if self.show_takes_focus {
            host::WindowCommand::Show
        } else {
            host::WindowCommand::ShowQuietly
        };
        (self.window_host)(self.native, command, cx);
        cx.notify();
    }

    pub fn has_tray(&self) -> bool {
        self.has_tray
    }

    /// Hidden to the notification-area icon. Desktop notifications read this: a Message a person
    /// cannot see is worth a notification even while the app is active.
    pub fn hidden_to_tray(&self) -> bool {
        self.hidden_to_tray
    }

    pub fn on_input(&mut self, cx: &mut Context<Self>) {
        if self.session().is_some_and(|s| s.read(cx).idle()) {
            self.apply_idle(Duration::ZERO, cx);
        }
    }

    pub fn check_idle(&mut self, cx: &mut Context<Self>) {
        if let Some(idle) = (self.idle_source)() {
            self.apply_idle(idle, cx);
        }
    }

    pub fn apply_idle(&mut self, idle_for: Duration, cx: &mut Context<Self>) {
        let idle = idle_for >= IDLE_AFTER;
        if let Some(session) = self.session().cloned() {
            session.update(cx, |s, cx| s.dispatch(Intent::SetIdle(idle), cx));
        }
    }

    pub fn set_idle_source(&mut self, source: IdleSource) {
        self.idle_source = source;
    }
}
