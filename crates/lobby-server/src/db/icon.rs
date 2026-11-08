//! `lobby_settings.icon_blob_id`: the Lobby's one icon. Write statements take the caller's
//! `BEGIN IMMEDIATE` transaction.

use std::str::FromStr;

use lobby_protocol::{BlobId, UnixMs};
use sqlx::SqliteConnection;

use super::DbError;

pub async fn get(conn: &mut SqliteConnection) -> Result<Option<BlobId>, DbError> {
    let id: Option<String> =
        sqlx::query_scalar("SELECT icon_blob_id FROM lobby_settings WHERE id = 1")
            .fetch_one(conn)
            .await?;
    id.map(|id| BlobId::from_str(&id).map_err(|e| DbError::Corrupt(e.to_string())))
        .transpose()
}

pub async fn set(
    conn: &mut SqliteConnection,
    icon: Option<&BlobId>,
    now: UnixMs,
) -> Result<(), DbError> {
    sqlx::query("UPDATE lobby_settings SET icon_blob_id = ?1, updated_at = ?2 WHERE id = 1")
        .bind(icon.map(BlobId::as_str))
        .bind(now.0)
        .execute(conn)
        .await?;
    Ok(())
}
