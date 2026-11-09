//! Moving between Lobbies: the switcher's picks and Ctrl+1…9, the ask before leaving Voice, and
//! the Add Lobby dialog.

use gpui_kit::{
    App, AppContext as _, Context, Entity, KeyDownEvent, ParentElement as _, Window,
    component::WindowExt as _,
};
use lobby_client::{LobbyOrigin, StoredSession, TokenStoreError, VoicePhase, copy};
use lobby_protocol::LobbyInfo;
use tracing::info;

use super::{QuitState, Route, Shell, StartedSession};
use crate::add_lobby::{AddLobbyEvent, AddLobbyView};
use crate::components::{Avatar, AvatarSize, SwitcherPick, confirm_alert};
use crate::entry_entity::EntryEntity;
use crate::scale::device;

const ADD_LOBBY_WIDTH: f32 = 400.;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwitchTarget {
    Saved(LobbyOrigin),
    /// Add Lobby reached it: its Entry goes on to Sign In, or is signed in already.
    Reached {
        entry: Entity<EntryEntity>,
        origin: LobbyOrigin,
        info: Box<LobbyInfo>,
        signed_in: bool,
    },
}

impl SwitchTarget {
    fn origin(&self) -> &LobbyOrigin {
        match self {
            SwitchTarget::Saved(origin) | SwitchTarget::Reached { origin, .. } => origin,
        }
    }
}

impl Shell {
    /// Ctrl+1…9: straight to the nth Lobby in the switcher, from anywhere in the window (not
    /// while a dialog asks something).
    pub(super) fn on_key(
        &mut self,
        ev: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let k = &ev.keystroke;
        let m = &k.modifiers;
        if !m.control || m.alt || m.shift || m.platform || self.quit != QuitState::Running {
            return;
        }
        let Some(n) = k.key.parse::<usize>().ok().filter(|n| (1..=9).contains(n)) else {
            return;
        };
        if window.has_active_dialog(cx) {
            return;
        }
        let origin = self
            .lobbies
            .read(cx)
            .list()
            .get(n - 1)
            .map(|l| l.origin.clone());
        if let Some(origin) = origin {
            cx.stop_propagation();
            info!(lobby = %origin, n, "switch by shortcut");
            self.switch_to(origin, window, cx);
        }
    }

    pub fn pick(&mut self, pick: SwitcherPick, window: &mut Window, cx: &mut Context<Self>) {
        match pick {
            SwitcherPick::Lobby(origin) => self.switch_to(origin, window, cx),
            SwitcherPick::Add => self.add_lobby(window, cx),
            SwitcherPick::Act(origin, action) => self.lobby_action(origin, action, window, cx),
        }
    }

    pub fn in_voice(&self, cx: &App) -> bool {
        self.session()
            .and_then(|s| s.read(cx).state())
            .is_some_and(|s| s.voice.phase != VoicePhase::Idle)
    }

    pub fn switch_to(&mut self, origin: LobbyOrigin, window: &mut Window, cx: &mut Context<Self>) {
        if self.quit != QuitState::Running || self.current_origin() == Some(&origin) {
            return;
        }
        self.request_switch(SwitchTarget::Saved(origin), window, cx);
    }

    fn request_switch(
        &mut self,
        target: SwitchTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.in_voice(cx) {
            self.confirm_switch_dialog(target, window, cx);
        } else {
            self.switch_now(target, window, cx);
        }
    }

    /// Asks to Leave Voice and Switch, or Stay (the kit's alert dialog).
    fn confirm_switch_dialog(
        &mut self,
        target: SwitchTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let lobbies = self.lobbies.read(cx);
        let name_of = |origin: &LobbyOrigin| lobbies.display_name(origin, None);
        let to = match &target {
            SwitchTarget::Reached { info, .. } => info.name.clone(),
            SwitchTarget::Saved(origin) => name_of(origin),
        };
        let color = lobbies.color_of(target.origin());
        let state = self.session().and_then(|s| s.read(cx).state());
        let from = state
            .and_then(|s| s.lobby.as_ref().map(|l| l.name.clone()))
            .or_else(|| self.current_origin().map(name_of))
            .unwrap_or_else(|| "this Lobby".to_owned());
        let sharing = state.is_some_and(|s| s.share.sharing.is_some());
        info!(to = %target.origin(), sharing, "switch asks first: In Voice");
        self.pending_switch = Some(target);
        let (title, description) = (
            copy::switch_title(&to),
            copy::switch_leaves_voice(&from, sharing),
        );
        let shell = cx.weak_entity();
        window.open_alert_dialog(cx, move |alert, _, cx| {
            let (ok, cancel, close) = (shell.clone(), shell.clone(), shell.clone());
            let alert = alert
                .icon(Avatar::lobby(to.clone(), color).size(AvatarSize::Alert))
                .title(title.clone())
                .description(description.clone())
                .on_close(move |_, _, cx| {
                    let _ = close.update(cx, |s, cx| s.cancel_switch(cx));
                });
            confirm_alert(
                alert,
                copy::STAY,
                copy::SWITCH_AND_LEAVE,
                move |_, cx| {
                    let _ = cancel.update(cx, |s, cx| s.cancel_switch(cx));
                },
                move |window, cx| {
                    let _ = ok.update(cx, |s, cx| s.confirm_switch(window, cx));
                },
                cx,
            )
        });
        cx.notify();
    }

    pub fn confirm_switch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(target) = self.pending_switch.take() {
            info!(to = %target.origin(), "switch confirmed: leaving Voice");
            self.switch_now(target, window, cx);
        }
    }

    pub fn cancel_switch(&mut self, cx: &mut Context<Self>) {
        if self.pending_switch.take().is_some() {
            info!("switch cancelled: staying");
            cx.notify();
        }
    }

    pub(super) fn switch_now(
        &mut self,
        target: SwitchTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cancel_connect();
        if let Route::Lobby(lobby) = &self.route
            && lobby.origin != *target.origin()
        {
            self.back_to = Some(lobby.origin.clone());
        }
        match target {
            SwitchTarget::Saved(origin) => {
                info!(lobby = %origin, "switching Lobby");
                let check = self.client.stored_session(&origin);
                self.switching = Some(cx.spawn_in(window, async move |this, cx| {
                    let Ok(stored) = check.await else { return };
                    let _ = this.update_in(cx, |shell, window, cx| {
                        shell.apply_switch(origin, stored, window, cx);
                    });
                }));
            }
            SwitchTarget::Reached {
                entry,
                origin,
                info,
                signed_in,
            } => {
                info!(lobby = %origin, signed_in, "switching to the added Lobby");
                if signed_in {
                    self.enter_lobby(origin, Some(*info), None, window, cx);
                } else {
                    self.lobbies
                        .update(cx, |l, cx| l.set_last_active(&origin, cx));
                    self.sign_in_with(entry, None, window, cx);
                }
            }
        }
    }

    /// A switch to a saved Lobby, once the token store answered. Its Session starts from what the
    /// Lobby showed when it was left, if the Shell kept that. Without tokens it goes to Sign In
    /// through the URL step, whose failure says what was tried.
    fn apply_switch(
        &mut self,
        origin: LobbyOrigin,
        stored: Result<Option<StoredSession>, TokenStoreError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.switching = None;
        if self.quit != QuitState::Running {
            return;
        }
        if let Ok(found) = &stored {
            self.lobbies
                .update(cx, |l, cx| l.set_signed_in(&origin, found.is_some(), cx));
        }
        match stored {
            Ok(Some(_)) if self.has_errand_on(&origin) => {
                // The Lobby's menu already runs a Session there, and it becomes the route's.
                let info = self.kept.state(&origin).and_then(|s| s.lobby.clone());
                self.enter_lobby(origin, info, None, window, cx)
            }
            Ok(Some(stored)) if origin.is_allowed(self.client.policy()) => {
                let last = self.kept.state(&origin);
                let info = last.and_then(|s| s.lobby.clone());
                let (session, updates) = self.client.resume(stored, last);
                let started = StartedSession {
                    origin: origin.clone(),
                    session,
                    updates,
                };
                self.enter_lobby(origin, info, Some(started), window, cx)
            }
            Ok(_) | Err(_) => {
                self.show_url_step(Some(origin.as_str()), window, cx);
                self.connect(window, cx);
            }
        }
    }

    /// A `lobby://` link from outside the app: the Open in Lobby button of a Lobby's page. The
    /// window comes to the front first. A Lobby the person is in or has added is opened; any other
    /// is only put in the address form. It is never connected to here: pressing Connect with the
    /// address in view is the person's yes, or a link could walk them into typing their email and
    /// Magic Code on a lookalike server.
    pub fn open_link(&mut self, raw: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.quit != QuitState::Running {
            return;
        }
        self.show_window(cx);
        let Some(origin) = LobbyOrigin::from_deep_link(raw, self.client.policy()) else {
            info!(len = raw.len(), "a lobby:// link is not a Lobby address");
            return;
        };
        if self.current_origin() == Some(&origin) {
            info!(lobby = %origin, "link: already in that Lobby");
            return;
        }
        let saved = self
            .lobbies
            .read(cx)
            .list()
            .iter()
            .any(|l| l.origin == origin);
        // Another dialog is the person's to answer: a second one would stack on it.
        if window.has_active_dialog(cx) && (saved || self.add_lobby.is_none()) {
            info!(lobby = %origin, "link: a dialog is open, so the link is ignored");
            return;
        }
        if saved {
            info!(lobby = %origin, "link: switching to a Lobby of this device");
            self.switch_to(origin, window, cx);
        } else if let Some(view) = self.add_lobby.clone() {
            if view.read(cx).busy(cx) {
                info!(lobby = %origin, "link: Add Lobby is connecting, so the link is ignored");
                return;
            }
            info!(lobby = %origin, "link: address put in the open Add Lobby");
            view.update(cx, |v, cx| v.set_address(origin.as_str(), window, cx));
        } else if matches!(self.route, Route::Lobby(_)) {
            info!(lobby = %origin, "link: Add Lobby opened with its address");
            self.open_add_lobby(window, cx);
            if let Some(view) = self.add_lobby.clone() {
                view.update(cx, |v, cx| v.set_address(origin.as_str(), window, cx));
            }
        } else {
            info!(lobby = %origin, "link: address put in the Lobby address step");
            self.back_to = None;
            self.show_url_step(Some(origin.as_str()), window, cx);
        }
    }

    pub fn add_lobby(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.quit != QuitState::Running {
            return;
        }
        if matches!(self.route, Route::Lobby(_)) {
            self.open_add_lobby(window, cx);
        } else {
            self.back_to = None;
            self.show_url_step(Some(""), window, cx);
        }
    }

    fn open_add_lobby(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.add_lobby.is_some() {
            return;
        }
        let client = self.client.clone();
        let current = self.current_origin().cloned().map(|origin| {
            let live = self
                .session()
                .and_then(|s| s.read(cx).state())
                .and_then(|s| s.lobby.as_ref())
                .map(|l| l.name.as_str());
            let name = self.lobbies.read(cx).display_name(&origin, live);
            (origin, name)
        });
        let view = cx.new(|cx| AddLobbyView::new(client, current, window, cx));
        self.add_events = Some(cx.subscribe_in(
            &view,
            window,
            |shell, _, ev: &AddLobbyEvent, window, cx| shell.on_add_lobby(ev.clone(), window, cx),
        ));
        let body = view.clone();
        let shell = cx.weak_entity();
        window.open_dialog(cx, move |dialog, window, _| {
            let shell = shell.clone();
            let enter = body.clone();
            dialog
                .title(copy::ADD_LOBBY_TITLE)
                .w(device(ADD_LOBBY_WIDTH, window))
                .close_button(false)
                .child(body.clone())
                // Enter is the dialog's Confirm: it connects, and the dialog stays until the
                // Lobby answered (or said why not).
                .on_ok(move |_, window, cx| {
                    enter.update(cx, |v, cx| v.submit(window, cx));
                    false
                })
                .on_close(move |_, _, cx| {
                    let _ = shell.update(cx, |s, cx| {
                        s.add_lobby = None;
                        s.add_events = None;
                        cx.notify();
                    });
                })
        });
        view.update(cx, |v, cx| v.focus(window, cx));
        self.add_lobby = Some(view);
        cx.notify();
    }

    fn close_add_lobby(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.add_lobby.take().is_some() {
            self.add_events = None;
            window.close_dialog(cx);
            cx.notify();
        }
    }

    pub fn on_add_lobby(&mut self, ev: AddLobbyEvent, window: &mut Window, cx: &mut Context<Self>) {
        match ev {
            AddLobbyEvent::Cancel => self.close_add_lobby(window, cx),
            AddLobbyEvent::Reached {
                entry,
                origin,
                info,
                signed_in,
            } => {
                self.close_add_lobby(window, cx);
                self.lobbies.update(cx, |lobbies, cx| {
                    lobbies.upsert(&origin, &info, cx);
                    lobbies.set_signed_in(&origin, signed_in, cx);
                });
                if self.current_origin() == Some(&origin) {
                    info!(lobby = %origin, "Add Lobby: already in it");
                    return;
                }
                let target = SwitchTarget::Reached {
                    entry,
                    origin,
                    info,
                    signed_in,
                };
                self.request_switch(target, window, cx);
            }
        }
    }
}
