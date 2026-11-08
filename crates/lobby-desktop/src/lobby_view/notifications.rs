//! What the Lobby view does to tell the person something: a toast for the connection, an Issue
//! into the Center with its toast, the Center opening and closing, and what the actions in them
//! do.

use gpui_kit::{Context, Window, component::WindowExt as _};
use lobby_client::{Intent, Issue, IssueAction, IssueKind};

use super::toasts::{ConnectionToast, DecisionToast, IssueToast, remove_connection_toast};
use super::{LobbyView, LobbyViewEvent, connection_toast, issue_toast};
use crate::components::ConnectionAction;

impl LobbyView {
    /// A toast for a connection state the person must act on, once per entering it. Do Not
    /// Disturb and an open Center hold it back like any toast; the Center's entry and the bar's
    /// tint stay.
    pub(super) fn toast_connection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let notice = self.connection_notice(cx).filter(|n| n.toast);
        let phase = notice
            .as_ref()
            .and_then(|_| self.session.read(cx).state())
            .map(|s| std::mem::discriminant(&s.phase));
        if phase == self.connection_toasted {
            return;
        }
        self.connection_toasted = phase;
        match notice {
            Some(_) if self.toasts_held(cx) => {
                tracing::debug!("connection toast held back");
            }
            Some(notice) => {
                window.push_notification(connection_toast(&notice, cx.weak_entity()), cx)
            }
            None => remove_connection_toast(window, cx),
        }
    }

    /// No toast: the person chose Do Not Disturb, or the Center is open and already shows it.
    pub(super) fn toasts_held(&self, cx: &gpui_kit::App) -> bool {
        self.center_open || self.session.read(cx).dnd()
    }

    /// Something the person should know: into the Center, plus a toast unless they chose Do Not
    /// Disturb or have the Center open. A repeat of a kind replaces its entry and toast. This is
    /// where an Issue gets the Lobby it is about, unless it already names another (an errand's).
    pub fn notify(&mut self, issue: Issue, window: &mut Window, cx: &mut Context<Self>) {
        let issue = match self.session.read(cx).origin() {
            Some(origin) if issue.lobby.is_none() => issue.in_lobby(origin),
            _ => issue,
        };
        let held = self.toasts_held(cx);
        let kind = issue.kind;
        let toast = (!held).then(|| issue_toast(&issue, cx.weak_entity()));
        self.issues.update(cx, |c, cx| c.raise(issue, cx));
        if let Some(toast) = toast {
            window.push_notification(toast, cx);
        } else {
            tracing::debug!(kind = kind.key(), "toast held back");
        }
    }

    /// The Issue that offers Rejoin, if one is in the Center.
    pub(super) fn rejoin_issue(&self, cx: &gpui_kit::App) -> Option<Issue> {
        self.issues
            .read(cx)
            .issues()
            .iter()
            .find(|i| i.kind.action() == Some(IssueAction::Rejoin))
            .cloned()
    }

    /// Joining again answers every Issue that offered it.
    pub(super) fn dismiss_rejoins(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let kinds: Vec<IssueKind> = self
            .issues
            .read(cx)
            .issues()
            .iter()
            .map(|i| i.kind)
            .filter(|kind| kind.action() == Some(IssueAction::Rejoin))
            .collect();
        for kind in kinds {
            self.dismiss(kind, window, cx);
        }
    }

    /// Opening the Center takes the toasts down: it shows all of them. A Message's OS toast
    /// stays, the Center does not show Messages.
    pub fn set_center_open(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.center_open = open;
        if open {
            self.issues.update(cx, |c, cx| c.mark_seen(cx));
            window.remove_notification::<IssueToast>(cx);
            window.remove_notification::<ConnectionToast>(cx);
            window.remove_notification::<DecisionToast>(cx);
            self.decision_toasts.clear();
        }
        cx.notify();
    }

    /// Runs the action `issue` offers, in the Lobby it was about: from another Lobby (a toast
    /// that outlived a switch) it does nothing.
    pub fn notification_action(
        &mut self,
        issue: &Issue,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let here = self.session.read(cx).origin();
        if issue
            .lobby
            .as_ref()
            .is_some_and(|lobby| Some(lobby) != here)
        {
            tracing::debug!(
                kind = issue.kind.key(),
                "action of another Lobby's Issue ignored"
            );
            return;
        }
        let Some(action) = issue.kind.action() else {
            return;
        };
        match action {
            IssueAction::Rejoin => self.join_voice(window, cx),
            IssueAction::SendAgain => {
                self.composer.update(cx, |c, cx| c.send_again(window, cx));
            }
            IssueAction::ShowInFolder => {
                let kept = self.lobbies.read(cx).kept_file().cloned();
                if let Some(kept) = kept {
                    tracing::info!(path = %kept.display(), "showing the kept state file");
                    crate::files::reveal_path(&kept, cx);
                }
            }
        }
        self.set_center_open(false, window, cx);
    }

    pub fn dismiss(&mut self, kind: IssueKind, window: &mut Window, cx: &mut Context<Self>) {
        self.issues.update(cx, |c, cx| c.dismiss(kind, cx));
        window.remove_notification1::<IssueToast>(kind.key(), cx);
    }

    pub fn connection_action(
        &mut self,
        action: ConnectionAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        tracing::info!(?action, "connection action");
        match action {
            ConnectionAction::TryNow | ConnectionAction::Reconnect => {
                self.session
                    .update(cx, |s, cx| s.dispatch(Intent::RetryNow, cx));
            }
            ConnectionAction::ChangeLobby => cx.emit(LobbyViewEvent::ChangeLobby),
        }
        remove_connection_toast(window, cx);
        cx.notify();
    }
}
