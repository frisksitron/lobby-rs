//! The pictures a Member sets: their own avatar (`PUT` and `DELETE /api/v1/users/me/avatar`) and
//! the Lobby's icon (`PUT` and `DELETE /api/v1/lobby/icon`, open to every Member). The upload is
//! staged as its square before the write is accepted; `domain::files` swaps the rows.

use axum::{
    Json,
    body::Body,
    extract::{Query, State},
    http::HeaderMap,
};
use lobby_protocol::{LobbyInfo, User, routes::UploadQuery};

use crate::{
    auth::AuthUser, blobs::BlobKind, domain::Staged, error::ApiError, server::App,
    uploads::refuse_declared_too_large,
};

/// Extractor order as for `POST /uploads`: state, auth, then the query, headers and body.
pub async fn set_avatar(
    State(app): State<App>,
    AuthUser(me): AuthUser,
    Query(q): Query<UploadQuery>,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<User>, ApiError> {
    refuse_declared_too_large(&app, &headers)?;
    let staged = Staged::upload(
        &app.blobs,
        &app.cpu,
        BlobKind::Avatar,
        q.name.as_deref(),
        body.into_data_stream(),
    )
    .await?;
    Ok(Json(app.domain.set_avatar(me.user, Some(staged)).await?))
}

pub async fn remove_avatar(
    State(app): State<App>,
    AuthUser(me): AuthUser,
) -> Result<Json<User>, ApiError> {
    Ok(Json(app.domain.set_avatar(me.user, None).await?))
}

pub async fn set_icon(
    State(app): State<App>,
    AuthUser(me): AuthUser,
    Query(q): Query<UploadQuery>,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<LobbyInfo>, ApiError> {
    refuse_declared_too_large(&app, &headers)?;
    let staged = Staged::upload(
        &app.blobs,
        &app.cpu,
        BlobKind::LobbyIcon,
        q.name.as_deref(),
        body.into_data_stream(),
    )
    .await?;
    Ok(Json(app.domain.set_icon(me.user, Some(staged)).await?))
}

pub async fn remove_icon(
    State(app): State<App>,
    AuthUser(me): AuthUser,
) -> Result<Json<LobbyInfo>, ApiError> {
    Ok(Json(app.domain.set_icon(me.user, None).await?))
}
