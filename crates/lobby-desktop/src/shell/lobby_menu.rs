//! A Lobby's own menu in the switcher: Change icon, Remove icon and Leave, for any Lobby on this
//! device. The open Lobby's go to the route's Session. Another Lobby's go to an errand: a Session
//! of its own for that one action, with no Lobby view, no Voice and no OS toasts or sounds, shut
//! down once the action is done or refused. The window and Voice stay where they are, and what the
//! errand's Session refuses comes to the window's notifications.
//!
//! There is one Session per Lobby: switching to the Lobby an errand is on makes the errand's
//! Session the route's ([`Shell::errand_session_for`]).
//!
//! Leave asks once. The owner's asks who owns the Lobby next, then hands it over and leaves in one
//! Intent. A Lobby that let the Member go leaves the switcher; the open one moves the window on.

use std::time::Duration;

use gpui_kit::{AppContext as _, Context, Entity, ParentElement as _, Subscription, Task, Window};
use lobby_client::{AccountOp, Intent, Issue, IssueKind, LobbyOrigin, Phase, UiEffect, copy};
use tracing::info;

use super::{LobbyRoute, QuitState, Route, Shell};
use crate::components::{LobbyAction, confirm_alert};
use crate::hand_over::HandOverView;
use crate::kit::WindowExt as _;
use crate::scale::device;
use crate::session_entity::{SessionEntity, SessionUiEvent};

/// How long an errand waits for its Lobby to answer before saying it could not reach it.
const ERRAND_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const HAND_OVER_WIDTH: f32 = 340.;

/// Another Lobby's action, on a Session of its own.
pub(super) struct Errand {
    origin: LobbyOrigin,
    session: Entity<SessionEntity>,
    stage: Stage,
    _events: Subscription,
    _changes: Subscription,
    _timeout: Task<()>,
}

enum Stage {
    /// Until the Lobby's READY, then `Job`.
    Connecting(Job),
    /// The Leave dialog asks.
    Asking,
    /// The Session took the Intent numbered `n`; done once nothing of `op` runs.
    Running { n: u64, op: AccountOp },
}

enum Job {
    Run { intent: Intent, op: AccountOp },
    AskToLeave,
}

impl Errand {
    /// What a failure to start says it is about.
    fn kind(&self) -> IssueKind {
        match &self.stage {
            Stage::Connecting(Job::Run {
                op: AccountOp::Icon,
                ..
            })
            | Stage::Running {
                op: AccountOp::Icon,
                ..
            } => IssueKind::PictureFailed,
            _ => IssueKind::LeaveFailed,
        }
    }
}

impl Shell {
    pub(super) fn lobby_action(
        &mut self,
        origin: LobbyOrigin,
        action: LobbyAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.quit != QuitState::Running {
            return;
        }
        info!(lobby = %origin, ?action, "Lobby menu");
        match action {
            LobbyAction::ChangeIcon => self.pick_icon(origin, window, cx),
            LobbyAction::RemoveIcon => {
                self.act_on(origin, Intent::RemoveIcon, AccountOp::Icon, window, cx)
            }
            LobbyAction::Leave => self.ask_to_leave(origin, window, cx),
        }
    }

    /// The OS file picker, then the Lobby makes a square of the picture.
    fn pick_icon(&mut self, origin: LobbyOrigin, window: &mut Window, cx: &mut Context<Self>) {
        if self.icon_pick.is_some() {
            return;
        }
        let picked = crate::files::pick_one(cx);
        self.icon_pick = Some(cx.spawn_in(window, async move |this, cx| {
            let path = picked.await;
            let _ = this.update_in(cx, |shell, window, cx| {
                shell.icon_pick = None;
                if let Some(path) = path {
                    shell.act_on(origin, Intent::SetIcon(path), AccountOp::Icon, window, cx);
                }
            });
        }));
    }

    /// The route's Session, when `origin` is the open Lobby.
    fn open_session(&self, origin: &LobbyOrigin) -> Option<Entity<SessionEntity>> {
        match &self.route {
            Route::Lobby(LobbyRoute {
                origin: open,
                session,
                ..
            }) if open == origin => Some(session.clone()),
            _ => None,
        }
    }

    fn act_on(
        &mut self,
        origin: LobbyOrigin,
        intent: Intent,
        op: AccountOp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.open_session(&origin) {
            Some(session) => {
                session.update(cx, |s, cx| s.dispatch(intent, cx));
            }
            None => self.start_errand(origin, Job::Run { intent, op }, window, cx),
        }
    }

    fn ask_to_leave(&mut self, origin: LobbyOrigin, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx) {
            return;
        }
        match self.open_session(&origin) {
            Some(session) => self.open_leave_dialog(origin, session, window, cx),
            None => self.start_errand(origin, Job::AskToLeave, window, cx),
        }
    }

    /// One errand at a time: a new one replaces one that has not started its action, and waits
    /// for nothing.
    fn start_errand(
        &mut self,
        origin: LobbyOrigin,
        job: Job,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(running) = &self.errand
            && matches!(running.stage, Stage::Running { .. })
        {
            info!(lobby = %running.origin, "an errand is still running: not starting another");
            return;
        }
        self.end_errand(cx);
        info!(lobby = %origin, "errand: connecting");
        let (handle, updates) = self.client.connect(&origin);
        let session = cx.new(|cx| SessionEntity::new(handle, updates, cx));
        let events = cx.subscribe_in(
            &session,
            window,
            |shell, _, ev: &SessionUiEvent, window, cx| shell.errand_effects(ev, window, cx),
        );
        let changes = cx.observe_in(&session, window, |shell, _, window, cx| {
            shell.errand_changed(window, cx)
        });
        let timeout = cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(ERRAND_CONNECT_TIMEOUT).await;
            let _ = this.update_in(cx, |shell, window, cx| shell.errand_timed_out(window, cx));
        });
        self.errand = Some(Errand {
            origin,
            session,
            stage: Stage::Connecting(job),
            _events: events,
            _changes: changes,
            _timeout: timeout,
        });
        cx.notify();
    }

    /// Ready: the errand's job starts. Running: done once nothing of it runs.
    fn errand_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(errand) = self.errand.as_mut() else {
            return;
        };
        let session = errand.session.read(cx);
        let Some(state) = session.state() else {
            return;
        };
        match &errand.stage {
            Stage::Connecting(_) if state.phase == Phase::Connected && state.lobby.is_some() => {
                let Stage::Connecting(job) = std::mem::replace(&mut errand.stage, Stage::Asking)
                else {
                    return;
                };
                match job {
                    Job::Run { intent, op } => {
                        let n = errand.session.update(cx, |s, cx| s.dispatch(intent, cx));
                        errand.stage = Stage::Running { n, op };
                    }
                    Job::AskToLeave => {
                        let (origin, session) = (errand.origin.clone(), errand.session.clone());
                        self.open_leave_dialog(origin, session, window, cx);
                    }
                }
            }
            Stage::Running { n, op } if session.taken(*n) && !state.account.running(*op) => {
                self.end_errand(cx);
            }
            _ => {}
        }
    }

    /// No Lobby view hears this Session: its Issues come to the window's notifications, and the
    /// saved Lobby (with the switcher's mark) follows its new icon.
    fn errand_effects(&mut self, ev: &SessionUiEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(origin) = self.errand.as_ref().map(|e| e.origin.clone()) else {
            return;
        };
        for effect in &ev.0.0 {
            match effect {
                UiEffect::Issue(issue) => {
                    self.raise_issue(issue.clone().in_lobby(&origin), window, cx)
                }
                UiEffect::PersistLobby(info) => {
                    self.lobbies
                        .update(cx, |lobbies, cx| lobbies.upsert(&origin, info, cx));
                }
                UiEffect::Left => return self.errand_left(cx),
                UiEffect::NeedsAuth(why) => {
                    info!(lobby = %origin, ?why, "errand: the Lobby asks for a new Sign In");
                    self.lobbies
                        .update(cx, |l, cx| l.set_signed_in(&origin, false, cx));
                    let message =
                        copy::sign_in_to(&self.lobbies.read(cx).display_name(&origin, None));
                    self.errand_failed(message, window, cx);
                    return;
                }
                _ => {}
            }
        }
    }

    fn errand_timed_out(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(errand) = &self.errand else {
            return;
        };
        if !matches!(errand.stage, Stage::Connecting(_)) {
            return;
        }
        info!(lobby = %errand.origin, "errand: the Lobby did not answer");
        let name = self.lobbies.read(cx).display_name(&errand.origin, None);
        self.errand_failed(copy::lobby_unreachable(&name), window, cx);
    }

    fn errand_failed(&mut self, message: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(errand) = &self.errand else {
            return;
        };
        let issue = Issue::new(errand.kind(), message).in_lobby(&errand.origin);
        if matches!(errand.stage, Stage::Asking) {
            window.close_dialog(cx);
        }
        self.end_errand(cx);
        self.raise_issue(issue, window, cx);
    }

    /// Another Lobby let the Member go: its tokens are forgotten as its Session closes, and it
    /// leaves Lobbies and the switcher. The window stays where it is.
    fn errand_left(&mut self, cx: &mut Context<Self>) {
        let Some(errand) = self.errand.take() else {
            return;
        };
        let origin = errand.origin;
        info!(lobby = %origin, "left another Lobby");
        let leaving = errand.session.update(cx, |s, cx| s.leave_lobby(cx));
        self.after_leaving(leaving, cx);
        self.issues.update(cx, |c, cx| c.clear_lobby(&origin, cx));
        self.lobbies.update(cx, |l, cx| l.remove(&origin, cx));
        cx.notify();
    }

    pub(super) fn end_errand(&mut self, cx: &mut Context<Self>) {
        let Some(errand) = self.errand.take() else {
            return;
        };
        info!(lobby = %errand.origin, "errand ended");
        let shutdown = errand.session.update(cx, |s, cx| s.shutdown(cx));
        self.after_leaving(shutdown, cx);
        cx.notify();
    }

    /// An errand runs a Session on `origin`, which the route takes over instead of starting one.
    pub(super) fn has_errand_on(&self, origin: &LobbyOrigin) -> bool {
        self.errand.as_ref().is_some_and(|e| e.origin == *origin)
    }

    /// The Session for the route that is about to show `origin`: the errand's, when one is on
    /// that Lobby, so the two never share its tokens. An action it has not started yet starts
    /// there; a Leave it was about to ask is dropped, since the Member went into the Lobby.
    pub(super) fn errand_session_for(
        &mut self,
        origin: &LobbyOrigin,
        cx: &mut Context<Self>,
    ) -> Option<Entity<SessionEntity>> {
        if self.errand.as_ref()?.origin != *origin {
            return None;
        }
        let errand = self.errand.take()?;
        info!(lobby = %origin, "the errand's Session becomes the route's");
        if let Stage::Connecting(Job::Run { intent, .. }) = errand.stage {
            errand.session.update(cx, |s, cx| s.dispatch(intent, cx));
        }
        Some(errand.session)
    }

    /// The Member's Leave asks once; the owner's asks who owns the Lobby next.
    fn open_leave_dialog(
        &mut self,
        origin: LobbyOrigin,
        session: Entity<SessionEntity>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = session.read(cx).state() else {
            return;
        };
        let live = state.lobby.as_ref().map(|l| l.name.as_str());
        let title = copy::leave_title(&self.lobbies.read(cx).display_name(&origin, live));
        let owner = state.is_owner();
        let heirs = owner.then(|| HandOverView::new(state));
        let heirs = heirs.map(|view| cx.new(|_| view));
        info!(lobby = %origin, owner, "Leave asks");
        let shell = cx.weak_entity();
        let (ok, close) = (shell.clone(), shell.clone());
        let closed = {
            let origin = origin.clone();
            move |cx: &mut gpui_kit::App| {
                let _ = close.update(cx, |s, cx| s.leave_dialog_closed(&origin, cx));
            }
        };
        let Some(heirs) = heirs else {
            window.open_alert_dialog(cx, move |alert, _, _| {
                let (ok, origin, closed) = (ok.clone(), origin.clone(), closed.clone());
                let alert = alert
                    .title(title.clone())
                    .description(copy::LEAVE_CONFIRM)
                    .on_close(move |_, _, cx| closed(cx));
                confirm_alert(
                    alert,
                    "Cancel",
                    copy::LEAVE_LOBBY,
                    |_, _| {},
                    move |_, cx| {
                        let _ =
                            ok.update(cx, |s, cx| s.confirm_leave(&origin, Intent::LeaveLobby, cx));
                    },
                )
            });
            return;
        };
        window.open_dialog(cx, move |dialog, window, cx| {
            let (ok, origin, closed) = (ok.clone(), origin.clone(), closed.clone());
            let footer = HandOverView::footer(
                &heirs,
                |window, cx| window.close_dialog(cx),
                move |heir, window, cx| {
                    let intent = Intent::HandOverAndLeave(heir);
                    let _ = ok.update(cx, |s, cx| s.confirm_leave(&origin, intent, cx));
                    window.close_dialog(cx);
                },
                cx,
            );
            dialog
                .title(title.clone())
                .w(device(HAND_OVER_WIDTH, window))
                .close_button(false)
                .child(heirs.clone())
                .footer(footer)
                .on_close(move |_, _, cx| closed(cx))
        });
    }

    /// The Leave goes to the open Lobby's Session, or to the errand that asked.
    fn confirm_leave(&mut self, origin: &LobbyOrigin, intent: Intent, cx: &mut Context<Self>) {
        info!(lobby = %origin, "Leave confirmed");
        if let Some(session) = self.open_session(origin) {
            session.update(cx, |s, cx| s.dispatch(intent, cx));
            return;
        }
        if let Some(errand) = self.errand.as_mut()
            && errand.origin == *origin
            && matches!(errand.stage, Stage::Asking)
        {
            let n = errand.session.update(cx, |s, cx| s.dispatch(intent, cx));
            errand.stage = Stage::Running {
                n,
                op: AccountOp::Leave,
            };
        }
    }

    /// A Leave dialog closed without leaving ends the errand that asked it.
    fn leave_dialog_closed(&mut self, origin: &LobbyOrigin, cx: &mut Context<Self>) {
        if self
            .errand
            .as_ref()
            .is_some_and(|e| e.origin == *origin && matches!(e.stage, Stage::Asking))
        {
            self.end_errand(cx);
        }
    }

    /// The Lobby an errand runs on, for the automation model.
    pub fn errand_on(&self) -> Option<&LobbyOrigin> {
        self.errand.as_ref().map(|e| &e.origin)
    }
}
