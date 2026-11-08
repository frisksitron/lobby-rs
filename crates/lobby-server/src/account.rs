//! The signed-in Member's own account: their name.

use axum::{Json, extract::State};
use lobby_protocol::{User, UsernameChange};

use crate::{
    auth::AuthUser,
    error::{ApiError, AppJson},
    server::App,
};

pub async fn rename(
    State(app): State<App>,
    AuthUser(me): AuthUser,
    AppJson(req): AppJson<UsernameChange>,
) -> Result<Json<User>, ApiError> {
    Ok(Json(app.domain.rename(me.user, req.username).await?))
}
