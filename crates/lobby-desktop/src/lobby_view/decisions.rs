//! The Notification Center's Decisions on access requests: Vouch, Decline and its Undo. Each row
//! keeps its place until it folds away, so the next one never slides under the pointer.

use gpui_kit::{Context, Window, component::WindowExt as _};
use lobby_client::Intent;
use lobby_protocol::{AccessRequest, AccessRequestId, UnixMs};

use super::{
    LobbyView,
    toasts::{DecisionToast, decision_toast},
};
use crate::components::{Decide, DecisionRow, FOLD_FOR, SETTLE_FOR, Settled, decision_rows};

impl LobbyView {
    /// A Decision from the Notification Center. Vouch goes out at once and its row waits for the
    /// Lobby to say the person is in. Decline shows "Declined dave · Undo" at once and goes out
    /// only when [`SETTLE_FOR`] is over, unless Undo took it back. Either way the row keeps its
    /// place until it folds away, so the next one never slides under the pointer.
    pub fn decide(
        &mut self,
        id: AccessRequestId,
        what: Decide,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if what != Decide::Undo {
            self.drop_decision_toast(&id, window, cx);
        }
        let now = UnixMs::now();
        let row = self.session.read(cx).state().and_then(|s| {
            s.access_requests
                .iter()
                .find(|r| r.id == id)
                .map(|r| DecisionRow::new(r, now))
        });
        match what {
            Decide::Vouch => {
                let Some(row) = row else { return };
                self.vouching.insert(id.clone(), row);
                self.session
                    .update(cx, |s, cx| s.dispatch(Intent::Vouch(id.clone()), cx));
            }
            Decide::Decline => {
                let Some(row) = row.filter(|_| !self.settled.contains_key(&id)) else {
                    return;
                };
                tracing::info!(request = id.as_str(), "decline: waiting out the undo");
                self.settle(id, row, Settled::Declining, cx);
            }
            Decide::Undo => {
                if self
                    .settled
                    .get(&id)
                    .is_some_and(|(r, _)| r.settled == Some(Settled::Declining) && !r.folding)
                {
                    tracing::info!(request = id.as_str(), "decline undone");
                    self.settled.remove(&id);
                    cx.notify();
                }
            }
        }
    }

    /// A new Ask to Join: a toast that stays with Vouch and Decline, unless Do Not Disturb or the
    /// open Center holds it back (the Center's row and the bell's dot stay).
    pub(super) fn toast_decision(
        &mut self,
        request: &AccessRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.toasts_held(cx) {
            tracing::debug!(request = request.id.as_str(), "decision toast held back");
            return;
        }
        self.decision_toasts.insert(request.id.clone());
        window.push_notification(decision_toast(request, cx.weak_entity()), cx);
    }

    pub(super) fn drop_decision_toast(
        &mut self,
        id: &AccessRequestId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.decision_toasts.remove(id) {
            window.remove_notification1::<DecisionToast>(id.as_str().to_owned(), cx);
        }
    }

    /// Shows what happened in the request's row; when [`SETTLE_FOR`] is over a Decline goes out
    /// (if the request still waits) and the row folds away.
    fn settle(
        &mut self,
        id: AccessRequestId,
        mut row: DecisionRow,
        how: Settled,
        cx: &mut Context<Self>,
    ) {
        row.settled = Some(how);
        row.deciding = false;
        let key = id.clone();
        let timer = cx.background_executor().timer(SETTLE_FOR);
        let task = cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |v, cx| v.fold(key, cx));
        });
        self.settled.insert(id, (row, task));
        cx.notify();
    }

    fn fold(&mut self, id: AccessRequestId, cx: &mut Context<Self>) {
        let Some((row, _)) = self.settled.get_mut(&id) else {
            return;
        };
        if row.settled == Some(Settled::Declining) {
            let still_waiting = self
                .session
                .read(cx)
                .state()
                .is_some_and(|s| s.access_requests.iter().any(|r| r.id == id));
            if still_waiting {
                self.session
                    .update(cx, |s, cx| s.dispatch(Intent::Decline(id.clone()), cx));
            }
        }
        let Some((row, task)) = self.settled.get_mut(&id) else {
            return;
        };
        row.folding = true;
        let key = id.clone();
        let timer = cx.background_executor().timer(FOLD_FOR);
        *task = cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |v, cx| {
                v.settled.remove(&key);
                cx.notify();
            });
        });
        cx.notify();
    }

    /// A Vouch the Lobby handled: the request left the list, so its row says "carol is in" at once.
    /// Her `MEMBER_UPSERT` follows the removal a moment later and the row must not vanish
    /// meanwhile.
    pub(super) fn settle_vouches(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.session.read(cx).state() else {
            return;
        };
        let pending = |id: &AccessRequestId| state.access_requests.iter().any(|r| &r.id == id);
        let handled: Vec<AccessRequestId> = self
            .vouching
            .keys()
            .filter(|id| !pending(id))
            .cloned()
            .collect();
        // A refused Vouch (still pending, no longer deciding) stops waiting.
        let refused: Vec<AccessRequestId> = self
            .vouching
            .keys()
            .filter(|id| pending(id) && !state.deciding.contains(*id))
            .cloned()
            .collect();
        for id in refused {
            self.vouching.remove(&id);
        }
        for id in handled {
            if let Some(row) = self.vouching.remove(&id) {
                self.settle(id, row, Settled::VouchedIn, cx);
            }
        }
    }

    pub fn decisions(&self, cx: &gpui_kit::App) -> Vec<DecisionRow> {
        let Some(state) = self.session.read(cx).state() else {
            return Vec::new();
        };
        let mut rows: Vec<DecisionRow> = decision_rows(state, UnixMs::now())
            .into_iter()
            .filter(|r| !self.settled.contains_key(&r.id))
            .collect();
        rows.extend(self.settled.values().map(|(row, _)| row.clone()));
        // A row that left the Lobby's list keeps its place in it.
        rows.sort_by(|a, b| {
            AccessRequest::list_order(a.created_at, &a.id)
                .cmp(&AccessRequest::list_order(b.created_at, &b.id))
        });
        rows
    }
}
