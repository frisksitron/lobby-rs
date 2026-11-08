//! `magic_codes`, `join_tickets` and `access_requests`: the statements of Magic Codes, join tickets
//! and Ask to Join. Write statements take the open `BEGIN IMMEDIATE` transaction of their caller,
//! so a check and its write commit together.

use std::str::FromStr;

use lobby_protocol::{AccessRequest, AccessRequestId, Email, UnixMs, UserId, Username};
use sqlx::{FromRow, SqliteConnection};

use super::{Db, DbError};

fn corrupt(e: impl ToString) -> DbError {
    DbError::Corrupt(e.to_string())
}

fn email(raw: &str) -> Result<Email, DbError> {
    Email::parse(raw).map_err(corrupt)
}

pub async fn insert_code(
    conn: &mut SqliteConnection,
    challenge_hash: &[u8],
    email: &Email,
    code_hash: &[u8],
    expires_at: UnixMs,
    now: UnixMs,
) -> Result<(), DbError> {
    sqlx::query(
        "INSERT INTO magic_codes (challenge_hash, email, code_hash, expires_at, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )
    .bind(challenge_hash)
    .bind(email.as_str())
    .bind(code_hash)
    .bind(expires_at.0)
    .bind(now.0)
    .execute(conn)
    .await?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeRow {
    pub id: i64,
    pub email: Email,
    pub code_hash: Vec<u8>,
    pub expires_at: UnixMs,
}

/// The unused code of a challenge.
pub async fn challenged_code(
    conn: &mut SqliteConnection,
    challenge_hash: &[u8],
) -> Result<Option<CodeRow>, DbError> {
    #[derive(FromRow)]
    struct Row {
        id: i64,
        email: String,
        code_hash: Vec<u8>,
        expires_at: i64,
    }
    let row: Option<Row> = sqlx::query_as(
        "SELECT id, email, code_hash, expires_at FROM magic_codes
         WHERE challenge_hash = ?1 AND used_at IS NULL",
    )
    .bind(challenge_hash)
    .fetch_optional(conn)
    .await?;
    row.map(|r| {
        Ok(CodeRow {
            id: r.id,
            email: email(&r.email)?,
            code_hash: r.code_hash,
            expires_at: UnixMs(r.expires_at),
        })
    })
    .transpose()
}

/// Counts an attempt before the code is compared.
pub async fn count_attempt(conn: &mut SqliteConnection, id: i64) -> Result<bool, DbError> {
    let done = sqlx::query(
        "UPDATE magic_codes SET attempts = attempts + 1 WHERE id = ?1 AND attempts < ?2",
    )
    .bind(id)
    .bind(i64::from(lobby_protocol::limits::MAGIC_CODE_ATTEMPTS))
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

pub async fn use_code(conn: &mut SqliteConnection, id: i64, now: UnixMs) -> Result<(), DbError> {
    sqlx::query("UPDATE magic_codes SET used_at = ?2 WHERE id = ?1 AND used_at IS NULL")
        .bind(id)
        .bind(now.0)
        .execute(conn)
        .await?;
    Ok(())
}

pub async fn insert_ticket(
    conn: &mut SqliteConnection,
    email: &Email,
    token_hash: &[u8],
    expires_at: UnixMs,
    now: UnixMs,
) -> Result<(), DbError> {
    sqlx::query(
        "INSERT INTO join_tickets (email, token_hash, expires_at, created_at)
         VALUES (?1, ?2, ?3, ?4)",
    )
    .bind(email.as_str())
    .bind(token_hash)
    .bind(expires_at.0)
    .bind(now.0)
    .execute(conn)
    .await?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TicketRefused {
    Invalid,
    Expired,
}

/// Spends a ticket inside the caller's transaction (a rollback keeps it usable).
pub async fn consume_ticket(
    conn: &mut SqliteConnection,
    token_hash: &[u8],
    now: UnixMs,
) -> Result<Result<Email, TicketRefused>, DbError> {
    let used: Option<String> = sqlx::query_scalar(
        "UPDATE join_tickets SET used_at = ?2
         WHERE token_hash = ?1 AND used_at IS NULL AND expires_at > ?2
         RETURNING email",
    )
    .bind(token_hash)
    .bind(now.0)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(raw) = used {
        return Ok(Ok(email(&raw)?));
    }
    let expired: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM join_tickets WHERE token_hash = ?1 AND used_at IS NULL)",
    )
    .bind(token_hash)
    .fetch_one(conn)
    .await?;
    Ok(Err(if expired {
        TicketRefused::Expired
    } else {
        TicketRefused::Invalid
    }))
}

#[derive(FromRow)]
struct RequestRow {
    id: String,
    username: String,
    email: String,
    created_at: i64,
}

impl TryFrom<RequestRow> for AccessRequest {
    type Error = DbError;
    fn try_from(r: RequestRow) -> Result<AccessRequest, DbError> {
        Ok(AccessRequest {
            id: AccessRequestId::from_str(&r.id).map_err(corrupt)?,
            username: Username::parse(&r.username).map_err(corrupt)?,
            requester_label: r.email,
            created_at: UnixMs(r.created_at),
        })
    }
}

pub async fn pending(db: &Db) -> Result<Vec<AccessRequest>, DbError> {
    let rows: Vec<RequestRow> = sqlx::query_as(
        "SELECT id, username, email, created_at FROM access_requests
         WHERE status = 'pending' ORDER BY created_at DESC, id DESC",
    )
    .fetch_all(db.readers())
    .await?;
    rows.into_iter().map(AccessRequest::try_from).collect()
}

pub async fn pending_for_email(
    conn: &mut SqliteConnection,
    email: &Email,
) -> Result<Option<AccessRequestId>, DbError> {
    request_for_email(conn, email, "pending").await
}

/// The request of `email` that a Member declined. It lasts until the cleanup job drops it, so a
/// Decline holds for `jobs::DECLINED_RETENTION`.
pub async fn declined_for_email(
    conn: &mut SqliteConnection,
    email: &Email,
) -> Result<Option<AccessRequestId>, DbError> {
    request_for_email(conn, email, "declined").await
}

async fn request_for_email(
    conn: &mut SqliteConnection,
    email: &Email,
    status: &str,
) -> Result<Option<AccessRequestId>, DbError> {
    let id: Option<String> = sqlx::query_scalar(
        "SELECT id FROM access_requests WHERE email = ?1 AND status = ?2
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(email.as_str())
    .bind(status)
    .fetch_optional(conn)
    .await?;
    id.map(|id| AccessRequestId::from_str(&id).map_err(corrupt))
        .transpose()
}

/// How many requests wait for a Vouch: in all, from one client site, and for one mailbox.
#[derive(Debug, FromRow)]
pub struct PendingCounts {
    pub total: i64,
    pub from_site: i64,
    pub for_mailbox: i64,
}

pub async fn pending_counts(
    conn: &mut SqliteConnection,
    site: &str,
    email: &Email,
) -> Result<PendingCounts, DbError> {
    Ok(sqlx::query_as(
        "SELECT COUNT(*) AS total,
                COUNT(*) FILTER (WHERE site = ?1) AS from_site,
                COUNT(*) FILTER (WHERE mailbox = ?2) AS for_mailbox
         FROM access_requests WHERE status = 'pending'",
    )
    .bind(site)
    .bind(email.without_tag().as_str())
    .fetch_one(conn)
    .await?)
}

pub async fn insert_wait(
    conn: &mut SqliteConnection,
    request: &AccessRequestId,
    token_hash: &[u8],
    expires_at: UnixMs,
    now: UnixMs,
) -> Result<(), DbError> {
    sqlx::query(
        "INSERT INTO join_waits (request_id, token_hash, expires_at, created_at)
         VALUES (?1, ?2, ?3, ?4)",
    )
    .bind(request.as_str())
    .bind(token_hash)
    .bind(expires_at.0)
    .bind(now.0)
    .execute(conn)
    .await?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitState {
    Pending,
    VouchedIn { wait: i64, user: UserId },
    Declined,
}

pub async fn wait_state(
    conn: &mut SqliteConnection,
    token_hash: &[u8],
    now: UnixMs,
) -> Result<Option<WaitState>, DbError> {
    #[derive(FromRow)]
    struct Row {
        id: i64,
        status: String,
        approved_user_id: Option<String>,
    }
    let row: Option<Row> = sqlx::query_as(
        "SELECT w.id, r.status, r.approved_user_id FROM join_waits w
         JOIN access_requests r ON r.id = w.request_id
         WHERE w.token_hash = ?1 AND w.used_at IS NULL AND w.expires_at > ?2",
    )
    .bind(token_hash)
    .bind(now.0)
    .fetch_optional(conn)
    .await?;
    row.map(|r| match (r.status.as_str(), r.approved_user_id) {
        ("pending", _) => Ok(WaitState::Pending),
        ("vouched_in", Some(user)) => Ok(WaitState::VouchedIn {
            wait: r.id,
            user: UserId::from_str(&user).map_err(corrupt)?,
        }),
        ("declined", _) => Ok(WaitState::Declined),
        (other, _) => Err(corrupt(format!("access request status {other:?}"))),
    })
    .transpose()
}

pub async fn use_wait(conn: &mut SqliteConnection, wait: i64, now: UnixMs) -> Result<(), DbError> {
    sqlx::query("UPDATE join_waits SET used_at = ?2 WHERE id = ?1 AND used_at IS NULL")
        .bind(wait)
        .bind(now.0)
        .execute(conn)
        .await?;
    Ok(())
}

pub async fn delete_waits_for(conn: &mut SqliteConnection, user: &UserId) -> Result<u64, DbError> {
    Ok(sqlx::query(
        "DELETE FROM join_waits WHERE request_id IN
           (SELECT id FROM access_requests WHERE approved_user_id = ?1)",
    )
    .bind(user.as_str())
    .execute(conn)
    .await?
    .rows_affected())
}

pub async fn insert_request(
    conn: &mut SqliteConnection,
    username: &Username,
    email: &Email,
    site: &str,
    now: UnixMs,
) -> Result<AccessRequest, DbError> {
    let id = AccessRequestId::random();
    sqlx::query(
        "INSERT INTO access_requests
           (id, username, email, site, mailbox, status, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6, ?6)",
    )
    .bind(id.as_str())
    .bind(username.as_str())
    .bind(email.as_str())
    .bind(site)
    .bind(email.without_tag().as_str())
    .bind(now.0)
    .execute(conn)
    .await?;
    Ok(AccessRequest {
        id,
        username: username.clone(),
        requester_label: email.as_str().to_owned(),
        created_at: now,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingRequest {
    pub username: Username,
    pub email: Email,
}

pub async fn pending_request(
    conn: &mut SqliteConnection,
    id: &AccessRequestId,
) -> Result<Option<PendingRequest>, DbError> {
    #[derive(FromRow)]
    struct Row {
        username: String,
        email: String,
    }
    let row: Option<Row> = sqlx::query_as(
        "SELECT username, email FROM access_requests WHERE id = ?1 AND status = 'pending'",
    )
    .bind(id.as_str())
    .fetch_optional(conn)
    .await?;
    row.map(|r| {
        Ok(PendingRequest {
            username: Username::parse(&r.username).map_err(corrupt)?,
            email: email(&r.email)?,
        })
    })
    .transpose()
}

pub async fn mark_vouched(
    conn: &mut SqliteConnection,
    id: &AccessRequestId,
    by: &UserId,
    user: &UserId,
    now: UnixMs,
) -> Result<(), DbError> {
    sqlx::query(
        "UPDATE access_requests
         SET status = 'vouched_in', handled_at = ?4,
             handled_by_user_id = ?2, approved_user_id = ?3, updated_at = ?4
         WHERE id = ?1 AND status = 'pending'",
    )
    .bind(id.as_str())
    .bind(by.as_str())
    .bind(user.as_str())
    .bind(now.0)
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn mark_declined(
    conn: &mut SqliteConnection,
    id: &AccessRequestId,
    by: &UserId,
    now: UnixMs,
) -> Result<bool, DbError> {
    let done = sqlx::query(
        "UPDATE access_requests
         SET status = 'declined', handled_at = ?3,
             handled_by_user_id = ?2, updated_at = ?3
         WHERE id = ?1 AND status = 'pending'",
    )
    .bind(id.as_str())
    .bind(by.as_str())
    .bind(now.0)
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

// Cleanup: each call is one short batch; the caller loops until it returns 0.

pub const CLEANUP_BATCH: i64 = 500;

pub async fn delete_expired_codes(
    conn: &mut SqliteConnection,
    now: UnixMs,
) -> Result<u64, DbError> {
    Ok(sqlx::query(
        "DELETE FROM magic_codes WHERE id IN
           (SELECT id FROM magic_codes WHERE expires_at < ?1 LIMIT ?2)",
    )
    .bind(now.0)
    .bind(CLEANUP_BATCH)
    .execute(conn)
    .await?
    .rows_affected())
}

pub async fn delete_expired_tickets(
    conn: &mut SqliteConnection,
    now: UnixMs,
) -> Result<u64, DbError> {
    Ok(sqlx::query(
        "DELETE FROM join_tickets WHERE id IN
           (SELECT id FROM join_tickets WHERE expires_at < ?1 LIMIT ?2)",
    )
    .bind(now.0)
    .bind(CLEANUP_BATCH)
    .execute(conn)
    .await?
    .rows_affected())
}

pub async fn delete_expired_waits(
    conn: &mut SqliteConnection,
    now: UnixMs,
) -> Result<u64, DbError> {
    Ok(sqlx::query(
        "DELETE FROM join_waits WHERE id IN
           (SELECT id FROM join_waits WHERE expires_at < ?1 LIMIT ?2)",
    )
    .bind(now.0)
    .bind(CLEANUP_BATCH)
    .execute(conn)
    .await?
    .rows_affected())
}

/// Deletes up to one batch of requests that have waited since before `before`, and answers their
/// ids. Their wait tokens go with them.
pub async fn delete_stale_pending(
    conn: &mut SqliteConnection,
    before: UnixMs,
) -> Result<Vec<AccessRequestId>, DbError> {
    let ids: Vec<String> = sqlx::query_scalar(
        "DELETE FROM access_requests WHERE id IN
           (SELECT id FROM access_requests
            WHERE status = 'pending' AND created_at < ?1 LIMIT ?2)
         RETURNING id",
    )
    .bind(before.0)
    .bind(CLEANUP_BATCH)
    .fetch_all(conn)
    .await?;
    ids.iter()
        .map(|id| AccessRequestId::from_str(id).map_err(corrupt))
        .collect()
}

pub async fn delete_old_declined(
    conn: &mut SqliteConnection,
    before: UnixMs,
) -> Result<u64, DbError> {
    Ok(sqlx::query(
        "DELETE FROM access_requests WHERE id IN
           (SELECT id FROM access_requests
            WHERE status = 'declined' AND handled_at < ?1 LIMIT ?2)",
    )
    .bind(before.0)
    .bind(CLEANUP_BATCH)
    .execute(conn)
    .await?
    .rows_affected())
}
