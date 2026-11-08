//! `sessions`: opaque token pairs stored as sha256 digests.

use std::str::FromStr;

use lobby_protocol::{UnixMs, UserId};
use sqlx::{FromRow, SqliteConnection};

use super::{Db, DbError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionId(pub i64);

/// The digests and expiries of a freshly minted pair; the tokens themselves never reach the DB.
#[derive(Debug, Clone, Copy)]
pub struct NewPair<'a> {
    pub access_hash: &'a [u8],
    pub access_expires_at: UnixMs,
    pub refresh_hash: &'a [u8],
    pub refresh_expires_at: UnixMs,
}

fn user_id(raw: &str) -> Result<UserId, DbError> {
    UserId::from_str(raw).map_err(|e| DbError::Corrupt(e.to_string()))
}

pub async fn create(
    conn: &mut SqliteConnection,
    user: &UserId,
    pair: &NewPair<'_>,
    now: UnixMs,
) -> Result<SessionId, DbError> {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO sessions (user_id, access_hash, access_expires_at, refresh_hash,
                               refresh_expires_at, created_at, last_seen_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6) RETURNING id",
    )
    .bind(user.as_str())
    .bind(pair.access_hash)
    .bind(pair.access_expires_at.0)
    .bind(pair.refresh_hash)
    .bind(pair.refresh_expires_at.0)
    .bind(now.0)
    .fetch_one(conn)
    .await?;
    Ok(SessionId(id))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthRow {
    pub session: SessionId,
    pub user: UserId,
    pub access_expires_at: UnixMs,
    pub used: bool,
}

/// Looks a session up on a reader: every REST request and IDENTIFY.
pub async fn by_access_hash(db: &Db, hash: &[u8]) -> Result<Option<AuthRow>, DbError> {
    #[derive(FromRow)]
    struct Row {
        id: i64,
        user_id: String,
        access_expires_at: i64,
        current_used_at: Option<i64>,
    }
    let row: Option<Row> = sqlx::query_as(
        "SELECT s.id, s.user_id, s.access_expires_at, s.current_used_at
         FROM sessions s JOIN users u ON u.id = s.user_id
         WHERE s.access_hash = ?1 AND u.deactivated_at IS NULL",
    )
    .bind(hash)
    .fetch_optional(db.readers())
    .await?;
    row.map(|r| {
        Ok(AuthRow {
            session: SessionId(r.id),
            user: user_id(&r.user_id)?,
            access_expires_at: UnixMs(r.access_expires_at),
            used: r.current_used_at.is_some(),
        })
    })
    .transpose()
}

/// The first use of the current pair (at most once per rotation). From then on, presenting the
/// previous refresh token counts as reuse.
pub async fn mark_used(
    conn: &mut SqliteConnection,
    session: SessionId,
    now: UnixMs,
) -> Result<(), DbError> {
    sqlx::query(
        "UPDATE sessions SET current_used_at = ?2, last_seen_at = ?2
         WHERE id = ?1 AND current_used_at IS NULL",
    )
    .bind(session.0)
    .bind(now.0)
    .execute(conn)
    .await?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refreshed {
    /// The current refresh token: the pair rotated.
    Rotated {
        session: SessionId,
        user: UserId,
    },
    /// The previous token while the current pair was never used: a lost response, reissued.
    Reissued {
        session: SessionId,
        user: UserId,
    },
    /// The previous token after the current pair was used: the session is deleted.
    ReuseDetected {
        session: SessionId,
        user: UserId,
    },
    /// The session's refresh expiry passed: the row is deleted.
    Expired,
    Unknown,
}

/// In one `BEGIN IMMEDIATE` transaction. On `Rotated` and `Reissued` the session now holds `pair`.
pub async fn refresh(
    writer: &mut SqliteConnection,
    presented: &[u8],
    pair: &NewPair<'_>,
    now: UnixMs,
) -> Result<Refreshed, DbError> {
    #[derive(FromRow)]
    struct Row {
        id: i64,
        user_id: String,
        refresh_expires_at: i64,
        current_used_at: Option<i64>,
    }
    let mut tx = super::begin_immediate(writer).await?;
    let current: Option<Row> = sqlx::query_as(
        "SELECT id, user_id, refresh_expires_at, current_used_at FROM sessions
         WHERE refresh_hash = ?1",
    )
    .bind(presented)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(row) = current {
        if row.refresh_expires_at <= now.0 {
            delete_one(&mut tx, SessionId(row.id)).await?;
            tx.commit().await?;
            return Ok(Refreshed::Expired);
        }
        sqlx::query(
            "UPDATE sessions SET prev_refresh_hash = ?2, rotated_at = ?3,
                    access_hash = ?4, access_expires_at = ?5,
                    refresh_hash = ?6, refresh_expires_at = ?7,
                    current_used_at = NULL, last_seen_at = ?3
             WHERE id = ?1",
        )
        .bind(row.id)
        .bind(presented)
        .bind(now.0)
        .bind(pair.access_hash)
        .bind(pair.access_expires_at.0)
        .bind(pair.refresh_hash)
        .bind(pair.refresh_expires_at.0)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        return Ok(Refreshed::Rotated {
            session: SessionId(row.id),
            user: user_id(&row.user_id)?,
        });
    }
    let previous: Option<Row> = sqlx::query_as(
        "SELECT id, user_id, refresh_expires_at, current_used_at FROM sessions
         WHERE prev_refresh_hash = ?1",
    )
    .bind(presented)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = previous else {
        tx.rollback().await?;
        return Ok(Refreshed::Unknown);
    };
    let session = SessionId(row.id);
    let user = user_id(&row.user_id)?;
    if row.refresh_expires_at <= now.0 {
        delete_one(&mut tx, SessionId(row.id)).await?;
        tx.commit().await?;
        return Ok(Refreshed::Expired);
    }
    if row.current_used_at.is_some() {
        delete_one(&mut tx, SessionId(row.id)).await?;
        tx.commit().await?;
        return Ok(Refreshed::ReuseDetected { session, user });
    }
    // No time bound: nobody holds a used successor, so this is no weaker than not having rotated.
    // `prev_refresh_hash` and `rotated_at` stay as they are.
    sqlx::query(
        "UPDATE sessions SET access_hash = ?2, access_expires_at = ?3,
                refresh_hash = ?4, refresh_expires_at = ?5,
                current_used_at = NULL, last_seen_at = ?6
         WHERE id = ?1",
    )
    .bind(row.id)
    .bind(pair.access_hash)
    .bind(pair.access_expires_at.0)
    .bind(pair.refresh_hash)
    .bind(pair.refresh_expires_at.0)
    .bind(now.0)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Refreshed::Reissued { session, user })
}

pub async fn delete_one(conn: &mut SqliteConnection, session: SessionId) -> Result<(), DbError> {
    sqlx::query("DELETE FROM sessions WHERE id = ?1")
        .bind(session.0)
        .execute(conn)
        .await?;
    Ok(())
}

pub async fn delete_all(conn: &mut SqliteConnection, user: &UserId) -> Result<u64, DbError> {
    Ok(sqlx::query("DELETE FROM sessions WHERE user_id = ?1")
        .bind(user.as_str())
        .execute(conn)
        .await?
        .rows_affected())
}

pub async fn delete_expired(conn: &mut SqliteConnection, now: UnixMs) -> Result<u64, DbError> {
    Ok(sqlx::query(
        "DELETE FROM sessions WHERE id IN
           (SELECT id FROM sessions WHERE refresh_expires_at < ?1 LIMIT ?2)",
    )
    .bind(now.0)
    .bind(super::admission::CLEANUP_BATCH)
    .execute(conn)
    .await?
    .rows_affected())
}
