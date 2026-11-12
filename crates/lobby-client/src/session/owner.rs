//! The owner's actions on Members: one at a time per Member, pending in the state until the Lobby
//! answers.

use lobby_protocol::UserId;
use tracing::{info, warn};

use super::{Session, UiEffect, UiEffects, account::Leave, io::Op};
use crate::{
    ApiError, copy,
    issues::{Issue, IssueKind},
    state::OwnerAction,
};

impl Session {
    /// The owner removes `user` from the Lobby or makes them the owner. What changed arrives as
    /// `MEMBER_REMOVED` or `LOBBY_UPDATE`; a refusal becomes an Issue.
    pub(super) fn owner_action(&mut self, user: &UserId, action: OwnerAction) {
        if self.state.owner_acting.contains_key(user) {
            return;
        }
        info!(%user, ?action, "owner action");
        self.state.owner_acting.insert(user.clone(), action);
        self.request(Op::Owner {
            user: user.clone(),
            action,
        });
    }

    pub(super) fn owner_acted(
        &mut self,
        user: &UserId,
        action: OwnerAction,
        result: Result<(), ApiError>,
    ) -> UiEffects {
        if matches!(action, OwnerAction::MakeOwner)
            && matches!(&self.leave, Leave::HandingOver(heir) if heir == user)
        {
            self.leave = Leave::No;
            self.handed_over(user, result);
            return UiEffects::default();
        }
        self.state.owner_acting.remove(user);
        let effect = match result {
            Ok(()) => {
                info!(%user, ?action, "owner action done");
                UiEffect::OwnerActed { user: user.clone() }
            }
            Err(e) => {
                warn!(%user, ?action, error = %e, "owner action refused");
                let name = self.state.members.get(user).map(|m| m.username.to_string());
                let kind = match action {
                    OwnerAction::Remove => IssueKind::RemoveFailed,
                    OwnerAction::MakeOwner => IssueKind::TransferFailed,
                };
                let message = copy::owner_action_failed(name.as_deref(), &e);
                UiEffect::Issue(Issue::new(kind, message))
            }
        };
        UiEffects(vec![effect])
    }
}
