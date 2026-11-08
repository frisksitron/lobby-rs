//! Who is a Member. Any Member Vouches for or Declines an Ask to Join; only the owner removes a
//! Member or hands ownership on; every Member but the owner may leave. Each acting Member is
//! checked again inside the transaction: a removal may have committed while they waited.

use lobby_protocol::{AccessRequestId, UnixMs, UserId};
use tracing::info;

use super::{Domain, WriteError};
use crate::{
    db::{self, admission, owner, sessions, users},
    lobby::Lobby,
};

impl Domain {
    /// Creates the User from the request (or brings a removed Member with the request's email
    /// back, under their old id). Everyone gets `ACCESS_REQUEST_REMOVED`, then
    /// the new Member's `MEMBER_UPSERT`.
    pub async fn vouch(&self, me: UserId, id: AccessRequestId) -> Result<(), WriteError> {
        self.run(move |w| {
            Box::pin(async move {
                let now = UnixMs::now();
                let mut tx = db::begin_immediate(&mut w.conn).await?;
                if users::active_profile(&mut tx, &me).await?.is_none() {
                    return Err(WriteError::Removed);
                }
                let Some(request) = admission::pending_request(&mut tx, &id).await? else {
                    return Err(WriteError::RequestNotFound);
                };
                if let Some(taken) =
                    users::taken(&mut tx, &request.username, &request.email, Some(&id)).await?
                {
                    return Err(taken.into());
                }
                let returning = users::removed_by_email(&mut tx, &request.email).await?;
                let profile = match &returning {
                    Some(user) => users::reactivate(&mut tx, user, &request.username, now).await?,
                    None => users::insert(&mut tx, &request.username, &request.email, now).await?,
                };
                admission::mark_vouched(&mut tx, &id, &me, &profile.id, now).await?;
                tx.commit().await?;
                info!(request = id.as_str(), by = %me, user = %profile.id,
                    returning = returning.is_some(), "vouched in");
                w.lobby.vouched_in(&id, &me, now, profile);
                Ok(())
            })
        })
        .await
    }

    pub async fn decline(&self, me: UserId, id: AccessRequestId) -> Result<(), WriteError> {
        self.run(move |w| {
            Box::pin(async move {
                let now = UnixMs::now();
                let mut tx = db::begin_immediate(&mut w.conn).await?;
                if users::active_profile(&mut tx, &me).await?.is_none() {
                    return Err(WriteError::Removed);
                }
                if !admission::mark_declined(&mut tx, &id, &me, now).await? {
                    return Err(WriteError::RequestNotFound);
                }
                tx.commit().await?;
                info!(request = id.as_str(), by = %me, "declined");
                w.lobby.access_request_declined(&id, &me, now);
                Ok(())
            })
        })
        .await
    }

    /// Their sessions end, their connection closes with 4006, everyone else gets
    /// `MEMBER_REMOVED`, and Sign In fails as for an unknown account. They may Ask to Join again
    /// with the same email.
    pub async fn remove_member(&self, me: UserId, user: UserId) -> Result<(), WriteError> {
        self.run(move |w| {
            Box::pin(async move {
                let mut tx = db::begin_immediate(&mut w.conn).await?;
                if owner::owner(&mut tx).await?.as_ref() != Some(&me) {
                    return Err(WriteError::NotOwner);
                }
                if user == me {
                    return Err(WriteError::CannotRemoveOwner);
                }
                let (ended, disconnected) = end_membership(&w.lobby, tx, &user).await?;
                info!(%user, by = %me, sessions = ended, disconnected, "member removed");
                Ok(())
            })
        })
        .await
    }

    /// Leave Lobby: the same removal as the owner's, of oneself. Their past Messages stay under
    /// their name. The owner cannot leave: they hand ownership on first.
    pub async fn leave(&self, me: UserId) -> Result<(), WriteError> {
        self.run(move |w| {
            Box::pin(async move {
                let mut tx = db::begin_immediate(&mut w.conn).await?;
                if owner::owner(&mut tx).await?.as_ref() == Some(&me) {
                    return Err(WriteError::OwnerCannotLeave);
                }
                let (ended, disconnected) = end_membership(&w.lobby, tx, &me).await?;
                info!(user = %me, sessions = ended, disconnected, "member left");
                Ok(())
            })
        })
        .await
    }

    /// Another active Member becomes the owner; everyone gets `LOBBY_UPDATE`. Naming the owner
    /// themself changes nothing.
    pub async fn transfer_owner(&self, me: UserId, to: UserId) -> Result<(), WriteError> {
        self.run(move |w| {
            Box::pin(async move {
                let now = UnixMs::now();
                let mut tx = db::begin_immediate(&mut w.conn).await?;
                if owner::owner(&mut tx).await?.as_ref() != Some(&me) {
                    return Err(WriteError::NotOwner);
                }
                if to == me {
                    return Ok(());
                }
                if users::active_profile(&mut tx, &to).await?.is_none() {
                    return Err(WriteError::NotAMember);
                }
                owner::transfer(&mut tx, &to, now).await?;
                tx.commit().await?;
                w.lobby.owner_changed(&to);
                info!(from = %me, to = %to, "ownership transferred");
                Ok(())
            })
        })
        .await
    }
}

/// Deactivates `user`, ends their sessions and waits, commits `tx` and tells the live Lobby: the
/// number of sessions ended, and whether a live connection closed.
async fn end_membership(
    lobby: &Lobby,
    mut tx: sqlx::Transaction<'_, sqlx::Sqlite>,
    user: &UserId,
) -> Result<(u64, bool), WriteError> {
    let now = UnixMs::now();
    if !users::deactivate(&mut tx, user, now).await? {
        return Err(WriteError::NotAMember);
    }
    let ended = sessions::delete_all(&mut tx, user).await?;
    admission::delete_waits_for(&mut tx, user).await?;
    tx.commit().await?;
    Ok((ended, lobby.member_removed(user)))
}
