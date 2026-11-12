//! The Notification Center's Decisions on access requests: Vouch, Decline and its Undo. Each row
//! keeps its place until it folds away, so the next one never slides under the pointer.

use gpui_kit::{Context, Task, Window};
use lobby_client::Intent;
use lobby_protocol::{AccessRequest, AccessRequestId, UnixMs};

use super::{
    LobbyView,
    toasts::{DecisionToast, decision_toast},
};
use crate::components::{
    Decide, DecisionRow, FOLD_FOR, RowStage, SETTLE_FOR, Settled, decision_rows,
};
use crate::kit::WindowExt as _;

/// A request this Member acted on, until its row folds away.
pub(super) struct Decision {
    /// The request as it stood when the Member acted.
    row: DecisionRow,
    stage: Stage,
}

enum Stage {
    /// The Vouch went out: the row waits for the Lobby to say the person is in.
    Vouching,
    /// The row says what happened for [`SETTLE_FOR`]. A Decline goes out when that is over, unless
    /// Undo took it back.
    Settled {
        how: Settled,
        _timer: Task<()>,
    },
    Folding {
        how: Settled,
        _timer: Task<()>,
    },
}

impl Decision {
    /// The row the Center shows for it. While the Vouch waits the Lobby's own row stands in.
    fn settled_row(&self) -> Option<DecisionRow> {
        let stage = match &self.stage {
            Stage::Vouching => return None,
            Stage::Settled { how, .. } => RowStage::Settled(*how),
            Stage::Folding { how, .. } => RowStage::Folding(*how),
        };
        Some(DecisionRow {
            stage,
            ..self.row.clone()
        })
    }
}

impl LobbyView {
    /// A Decision from the Notification Center. Vouch goes out at once and its row waits for the
    /// Lobby to say the person is in. Decline shows "Declined dave · Undo" at once and goes out
    /// only when [`SETTLE_FOR`] is over, unless Undo took it back. Either way the row keeps its
    /// place until it folds away, so the next one never slides under the pointer. A request whose
    /// row already says what happened takes neither; a Vouch still waiting may be pressed again.
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
        let acted = self.decisions.get(&id).map(|d| &d.stage);
        let vouch_again = what == Decide::Vouch && matches!(acted, Some(Stage::Vouching));
        let row = row.filter(|_| acted.is_none() || vouch_again);
        match what {
            Decide::Vouch => {
                let Some(row) = row else { return };
                self.decisions.insert(
                    id.clone(),
                    Decision {
                        row,
                        stage: Stage::Vouching,
                    },
                );
                self.session
                    .update(cx, |s, cx| s.dispatch(Intent::Vouch(id.clone()), cx));
            }
            Decide::Decline => {
                let Some(row) = row else { return };
                tracing::info!(request = id.as_str(), "decline: waiting out the undo");
                self.settle(id, row, Settled::Declining, cx);
            }
            Decide::Undo => {
                if matches!(
                    self.decisions.get(&id),
                    Some(Decision {
                        stage: Stage::Settled {
                            how: Settled::Declining,
                            ..
                        },
                        ..
                    })
                ) {
                    tracing::info!(request = id.as_str(), "decline undone");
                    self.decisions.remove(&id);
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
        row: DecisionRow,
        how: Settled,
        cx: &mut Context<Self>,
    ) {
        let key = id.clone();
        let timer = cx.background_executor().timer(SETTLE_FOR);
        let task = cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |v, cx| v.fold(key, cx));
        });
        let stage = Stage::Settled { how, _timer: task };
        self.decisions.insert(id, Decision { row, stage });
        cx.notify();
    }

    fn fold(&mut self, id: AccessRequestId, cx: &mut Context<Self>) {
        let Some(Decision {
            stage: Stage::Settled { how, .. },
            ..
        }) = self.decisions.get(&id)
        else {
            return;
        };
        let how = *how;
        if how == Settled::Declining {
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
        let Some(decision) = self.decisions.get_mut(&id) else {
            return;
        };
        let key = id.clone();
        let timer = cx.background_executor().timer(FOLD_FOR);
        let task = cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |v, cx| {
                v.decisions.remove(&key);
                cx.notify();
            });
        });
        decision.stage = Stage::Folding { how, _timer: task };
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
        let vouching: Vec<AccessRequestId> = self
            .decisions
            .iter()
            .filter(|(_, d)| matches!(d.stage, Stage::Vouching))
            .map(|(id, _)| id.clone())
            .collect();
        let handled: Vec<&AccessRequestId> = vouching.iter().filter(|id| !pending(id)).collect();
        // A refused Vouch (still pending, no longer deciding) stops waiting.
        let refused: Vec<&AccessRequestId> = vouching
            .iter()
            .filter(|id| pending(id) && !state.deciding.contains(*id))
            .collect();
        for id in refused {
            self.decisions.remove(id);
        }
        for id in handled {
            if let Some(decision) = self.decisions.remove(id) {
                self.settle(id.clone(), decision.row, Settled::VouchedIn, cx);
            }
        }
    }

    pub fn decisions(&self, cx: &gpui_kit::App) -> Vec<DecisionRow> {
        let Some(state) = self.session.read(cx).state() else {
            return Vec::new();
        };
        let settled: Vec<DecisionRow> = self
            .decisions
            .values()
            .filter_map(Decision::settled_row)
            .collect();
        let mut rows: Vec<DecisionRow> = decision_rows(state, UnixMs::now())
            .into_iter()
            .filter(|r| !settled.iter().any(|s| s.id == r.id))
            .collect();
        rows.extend(settled);
        // A row that left the Lobby's list keeps its place in it.
        rows.sort_by(|a, b| {
            AccessRequest::list_order(a.created_at, &a.id)
                .cmp(&AccessRequest::list_order(b.created_at, &b.id))
        });
        rows
    }
}
