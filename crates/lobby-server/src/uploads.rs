//! `POST /api/v1/uploads`: a raw body becomes an unclaimed chat attachment.

use axum::{
    Json,
    body::Body,
    extract::{Query, State},
    http::{HeaderMap, StatusCode, header},
};
use lobby_protocol::{Attachment, routes::UploadQuery};
use tracing::debug;

use crate::{
    auth::AuthUser,
    blobs::{BlobError, BlobKind},
    domain::Staged,
    error::ApiError,
    server::App,
};

/// A body that says up front that it is too big is refused before a byte of it is read.
pub fn refuse_declared_too_large(app: &App, headers: &HeaderMap) -> Result<(), BlobError> {
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if declared.is_some_and(|n| n > app.blobs.max_bytes()) {
        return Err(BlobError::TooLarge);
    }
    Ok(())
}

/// Extractor order: state, auth, then the query, headers and the body. Decoding and encoding the
/// preview finish before the write is accepted.
pub async fn create(
    State(app): State<App>,
    AuthUser(who): AuthUser,
    Query(q): Query<UploadQuery>,
    headers: HeaderMap,
    body: Body,
) -> Result<(StatusCode, Json<Attachment>), ApiError> {
    refuse_declared_too_large(&app, &headers)?;
    let staged = Staged::upload(
        &app.blobs,
        &app.cpu,
        BlobKind::ChatAttachment,
        q.name.as_deref(),
        body.into_data_stream(),
    )
    .await?;
    let attachment = app.domain.store_upload(who.user, staged).await?;
    debug!(blob = %attachment.id, size = attachment.size, "upload stored");
    Ok((StatusCode::CREATED, Json(attachment)))
}
