//! What only the Lobby's owner may do: remove a Member, and hand ownership to another Member; and
//! what every Member but the owner may do: leave.

use axum::{extract::State, http::StatusCode};
use lobby_protocol::{OwnerTransfer, UserId};

use crate::{
    auth::AuthUser,
    error::{ApiError, AppJson, AppPath},
    server::App,
};

pub async fn remove(
    State(app): State<App>,
    AuthUser(me): AuthUser,
    AppPath(user): AppPath<UserId>,
) -> Result<StatusCode, ApiError> {
    app.domain.remove_member(me.user, user).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /api/v1/users/me` (Leave Lobby).
pub async fn leave(State(app): State<App>, AuthUser(me): AuthUser) -> Result<StatusCode, ApiError> {
    app.domain.leave(me.user).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn transfer(
    State(app): State<App>,
    AuthUser(me): AuthUser,
    AppJson(req): AppJson<OwnerTransfer>,
) -> Result<StatusCode, ApiError> {
    app.domain.transfer_owner(me.user, req.user_id).await?;
    Ok(StatusCode::NO_CONTENT)
}
