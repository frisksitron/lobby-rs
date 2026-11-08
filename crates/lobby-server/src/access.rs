//! Vouch and Decline: any Member handles a pending Ask to Join.

use axum::{extract::State, http::StatusCode};
use lobby_protocol::AccessRequestId;

use crate::{
    auth::AuthUser,
    error::{ApiError, AppPath},
    server::App,
};

pub async fn vouch(
    State(app): State<App>,
    AuthUser(me): AuthUser,
    AppPath(id): AppPath<AccessRequestId>,
) -> Result<StatusCode, ApiError> {
    app.domain.vouch(me.user, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn decline(
    State(app): State<App>,
    AuthUser(me): AuthUser,
    AppPath(id): AppPath<AccessRequestId>,
) -> Result<StatusCode, ApiError> {
    app.domain.decline(me.user, id).await?;
    Ok(StatusCode::NO_CONTENT)
}
