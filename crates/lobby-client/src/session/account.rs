//! The signed-in Member's own account, over the Session's own REST client, so its token refresh
//! stays single-flight.
//!
//! Leave Lobby closes this device's connection with 4006. The close may come before the answer,
//! so the Session notes it asked (`leaving`) when it asks, and the close is read against that
//! once, whichever of the two comes first. The close of a Leave waits for the Leave's answer: a
//! refusal means the owner removed the Member first.
//!
//! The Lobby refuses the owner's Leave, so the owner's is two calls: Make owner, then Leave. Leave
//! runs (as an account op) from the first call, so the owner is leaving until either refuses.

use std::path::PathBuf;

use lobby_protocol::{LobbyInfo, User, UserId};
use tracing::{info, warn};

use super::{Session, SignedOutWhy, UiEffect, io::Op};
use crate::{
    ApiError, copy,
    drafts::UploadError,
    issues::IssueKind,
    state::{AccountOp, OwnerAction},
};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PictureError {
    #[error("{}", copy::PAYLOAD_TOO_LARGE)]
    TooLarge,
    #[error("{}", copy::FILE_UNREADABLE)]
    Unreadable,
    #[error("{0}")]
    Api(ApiError),
}

impl From<UploadError> for PictureError {
    fn from(e: UploadError) -> Self {
        match e {
            UploadError::TooLarge => Self::TooLarge,
            UploadError::Io(_) => Self::Unreadable,
            UploadError::Api(e) => Self::Api(e),
        }
    }
}

impl Session {
    /// Starts `op` as the account's `kind`, unless one of that kind is already on its way.
    fn account_op(&mut self, kind: AccountOp, op: Op) {
        if !self.state.account.start(kind) {
            return;
        }
        info!(?kind, "account");
        self.io.run(op);
    }

    pub(super) fn upload_max(&self) -> u64 {
        self.state
            .lobby
            .as_ref()
            .map_or(u64::MAX, |l| l.upload_max_bytes)
    }

    /// The Lobby tells everyone the new name as `MEMBER_UPSERT`.
    pub(super) fn rename(&mut self, username: lobby_protocol::Username) {
        self.account_op(AccountOp::Rename, Op::Rename(username));
    }

    pub(super) fn set_avatar(&mut self, path: PathBuf) {
        let max = self.upload_max();
        self.account_op(AccountOp::Avatar, Op::SetAvatar { path, max });
    }

    pub(super) fn remove_avatar(&mut self) {
        self.account_op(AccountOp::Avatar, Op::RemoveAvatar);
    }

    /// Any Member may set the Lobby's icon; everyone hears of it as `LOBBY_UPDATE`.
    pub(super) fn set_icon(&mut self, path: PathBuf) {
        let max = self.upload_max();
        self.account_op(AccountOp::Icon, Op::SetIcon { path, max });
    }

    pub(super) fn remove_icon(&mut self) {
        self.account_op(AccountOp::Icon, Op::RemoveIcon);
    }

    /// The Lobby removes the Member and closes this connection with 4006, as for a Member the
    /// owner removed: that close is read as Left, not as removed.
    pub(super) fn leave_lobby_request(&mut self) {
        if self.state.account.running(AccountOp::Leave) {
            return;
        }
        self.leaving = true;
        self.account_op(AccountOp::Leave, Op::Leave);
    }

    pub(super) fn hand_over_and_leave(&mut self, heir: UserId) {
        if !self.state.account.start(AccountOp::Leave) {
            return;
        }
        info!(%heir, "making the heir the owner, then leaving");
        self.heir = Some(heir.clone());
        self.io.run(Op::Owner {
            user: heir,
            action: OwnerAction::MakeOwner,
        });
    }

    /// Once the Lobby took the hand-over, the Leave goes out; a refusal ends the Leave too.
    pub(super) fn handed_over(&mut self, heir: &UserId, result: Result<(), ApiError>) {
        match result {
            Ok(()) => {
                info!(%heir, "handed over: leaving");
                self.leaving = true;
                self.io.run(Op::Leave);
            }
            Err(e) => {
                warn!(%heir, error = %e, "hand-over refused");
                self.state.account.finish(AccountOp::Leave);
                let name = self.state.members.get(heir).map(|m| m.username.to_string());
                let message = copy::owner_action_failed(name.as_deref(), &e);
                self.raise(IssueKind::TransferFailed, message);
            }
        }
    }

    /// A taken name is said under the field, where the Member is looking; any other refusal is
    /// an Issue.
    pub(super) fn renamed(&mut self, result: Result<User, ApiError>) {
        self.state.account.finish(AccountOp::Rename);
        match result {
            Ok(User { username, .. }) => self.queued.push(UiEffect::Renamed(username)),
            Err(e) if copy::name_is_taken(&e) => self.queued.push(UiEffect::NameTaken),
            Err(e) => {
                warn!(error = %e, "rename refused");
                self.raise(IssueKind::RenameFailed, copy::account_failed(&e));
            }
        }
    }

    pub(super) fn picture_done(&mut self, op: AccountOp, result: Result<(), PictureError>) {
        self.state.account.finish(op);
        if let Err(e) = result {
            warn!(error = %e, ?op, "picture refused");
            self.raise(IssueKind::PictureFailed, copy::picture_failed(&e));
        }
    }

    /// The saved Lobby follows the Lobby's answer at once: a Session that ends right after it
    /// may never hear the `LOBBY_UPDATE`.
    pub(super) fn icon_done(&mut self, result: Result<LobbyInfo, PictureError>) {
        let result = result.map(|lobby| {
            self.queued.push(UiEffect::PersistLobby(Box::new(lobby)));
        });
        self.picture_done(AccountOp::Icon, result);
    }

    /// The caller follows `Left`: it shuts the Session down with
    /// [`Session::leave_lobby`] and forgets the Lobby. A refusal changes nothing, unless the
    /// connection already gave up on a 4006: then the owner removed the Member first.
    pub(super) fn left(&mut self, result: Result<(), ApiError>) {
        self.state.account.finish(AccountOp::Leave);
        match result {
            Ok(()) => self.queued.push(UiEffect::Left),
            Err(e) => {
                warn!(error = %e, "leaving refused");
                self.leaving = false;
                if std::mem::take(&mut self.leave_closed) {
                    self.queued.push(UiEffect::NeedsAuth(SignedOutWhy::Removed));
                } else {
                    self.raise(IssueKind::LeaveFailed, copy::account_failed(&e));
                }
            }
        }
    }
}
