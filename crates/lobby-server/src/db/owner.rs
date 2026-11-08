//! `lobby_settings.owner_id`: the one Member who removes Members and hands ownership on. Write
//! statements take the caller's `BEGIN IMMEDIATE` transaction.

use std::str::FromStr;

use lobby_protocol::{UnixMs, UserId};
use sqlx::SqliteConnection;

use super::DbError;

pub async fn owner(conn: &mut SqliteConnection) -> Result<Option<UserId>, DbError> {
    let id: Option<String> = sqlx::query_scalar("SELECT owner_id FROM lobby_settings WHERE id = 1")
        .fetch_one(conn)
        .await?;
    id.map(|id| UserId::from_str(&id).map_err(|e| DbError::Corrupt(e.to_string())))
        .transpose()
}

/// The first Member owns the Lobby they set up; a no-op once it has an owner.
pub async fn set_first(
    conn: &mut SqliteConnection,
    user: &UserId,
    now: UnixMs,
) -> Result<(), DbError> {
    sqlx::query(
        "UPDATE lobby_settings SET owner_id = ?1, updated_at = ?2 WHERE id = 1 AND owner_id IS NULL",
    )
    .bind(user.as_str())
    .bind(now.0)
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn transfer(
    conn: &mut SqliteConnection,
    to: &UserId,
    now: UnixMs,
) -> Result<(), DbError> {
    sqlx::query("UPDATE lobby_settings SET owner_id = ?1, updated_at = ?2 WHERE id = 1")
        .bind(to.as_str())
        .bind(now.0)
        .execute(conn)
        .await?;
    Ok(())
}
