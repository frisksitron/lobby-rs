use std::str::FromStr;

use lobby_protocol::{
    AccessRequestId, BlobId, Email, Member, Presence, UnixMs, User, UserId, Username, VoiceState,
};
use sqlx::{FromRow, SqliteConnection};

use super::{Db, DbError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub id: UserId,
    pub username: Username,
    /// Where their Magic Codes go. Only their own READY carries it.
    pub email: Email,
    pub avatar: Option<BlobId>,
    pub created_at: UnixMs,
}

impl Profile {
    pub fn user(&self) -> User {
        User {
            id: self.id.clone(),
            username: self.username.clone(),
            avatar: self.avatar.clone(),
            created_at: self.created_at,
        }
    }

    pub fn member(&self, presence: Presence, voice: Option<VoiceState>) -> Member {
        Member {
            id: self.id.clone(),
            username: self.username.clone(),
            avatar: self.avatar.clone(),
            created_at: self.created_at,
            presence,
            voice,
        }
    }
}

#[derive(FromRow)]
struct ProfileRow {
    id: String,
    username: String,
    email: String,
    avatar_blob_id: Option<String>,
    created_at: i64,
}

impl TryFrom<ProfileRow> for Profile {
    type Error = DbError;
    fn try_from(r: ProfileRow) -> Result<Profile, DbError> {
        let corrupt = |e: &dyn ToString| DbError::Corrupt(e.to_string());
        Ok(Profile {
            id: UserId::from_str(&r.id).map_err(|e| corrupt(&e))?,
            username: Username::parse(&r.username).map_err(|e| corrupt(&e))?,
            email: Email::parse(&r.email).map_err(|e| corrupt(&e))?,
            avatar: r
                .avatar_blob_id
                .map(|id| BlobId::from_str(&id).map_err(|e| corrupt(&e)))
                .transpose()?,
            created_at: UnixMs(r.created_at),
        })
    }
}

pub async fn active_profiles(db: &Db) -> Result<Vec<Profile>, DbError> {
    let rows: Vec<ProfileRow> = sqlx::query_as(
        "SELECT id, username, email, avatar_blob_id, created_at
         FROM users WHERE deactivated_at IS NULL ORDER BY username",
    )
    .fetch_all(db.readers())
    .await?;
    rows.into_iter().map(Profile::try_from).collect()
}

pub async fn active_profile(
    conn: &mut SqliteConnection,
    id: &UserId,
) -> Result<Option<Profile>, DbError> {
    let row: Option<ProfileRow> = sqlx::query_as(
        "SELECT id, username, email, avatar_blob_id, created_at
         FROM users WHERE id = ?1 AND deactivated_at IS NULL",
    )
    .bind(id.as_str())
    .fetch_optional(conn)
    .await?;
    row.map(Profile::try_from).transpose()
}

pub async fn active_by_email(
    conn: &mut SqliteConnection,
    email: &Email,
) -> Result<Option<Profile>, DbError> {
    let row: Option<ProfileRow> = sqlx::query_as(
        "SELECT id, username, email, avatar_blob_id, created_at
         FROM users WHERE email = ?1 AND deactivated_at IS NULL",
    )
    .bind(email.as_str())
    .fetch_optional(conn)
    .await?;
    row.map(Profile::try_from).transpose()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Taken {
    Username,
    Email,
}

/// Usernames are unique case-insensitively across every User and pending Access Requests: a
/// removed Member keeps theirs, except for their own return (the same email). Emails are unique
/// across active Users and pending requests, so a removed Member may ask again. `except` leaves
/// one pending request out (the one being Vouched in).
pub async fn taken(
    conn: &mut SqliteConnection,
    username: &Username,
    email: &Email,
    except: Option<&AccessRequestId>,
) -> Result<Option<Taken>, DbError> {
    let except = except.map(AccessRequestId::as_str).unwrap_or_default();
    let name: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM users WHERE username = ?1
                       AND NOT (deactivated_at IS NOT NULL AND email = ?3))
             OR EXISTS(SELECT 1 FROM access_requests
                       WHERE username = ?1 AND status = 'pending' AND id <> ?2)",
    )
    .bind(username.as_str())
    .bind(except)
    .bind(email.as_str())
    .fetch_one(&mut *conn)
    .await?;
    if name {
        return Ok(Some(Taken::Username));
    }
    let mail: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM users WHERE email = ?1 AND deactivated_at IS NULL)
             OR EXISTS(SELECT 1 FROM access_requests
                       WHERE email = ?1 AND status = 'pending' AND id <> ?2)",
    )
    .bind(email.as_str())
    .bind(except)
    .fetch_one(conn)
    .await?;
    Ok(mail.then_some(Taken::Email))
}

/// Whether `me` may not take `username`: another User (a removed one too) or a pending Access
/// Request has it, case-insensitively. Their own name in another case is theirs to take.
pub async fn username_taken(
    conn: &mut SqliteConnection,
    username: &Username,
    me: &UserId,
) -> Result<bool, DbError> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM users WHERE username = ?1 AND id <> ?2)
             OR EXISTS(SELECT 1 FROM access_requests WHERE username = ?1 AND status = 'pending')",
    )
    .bind(username.as_str())
    .bind(me.as_str())
    .fetch_one(conn)
    .await?)
}

pub async fn rename(
    conn: &mut SqliteConnection,
    user: &UserId,
    username: &Username,
    now: UnixMs,
) -> Result<(), DbError> {
    sqlx::query("UPDATE users SET username = ?2, updated_at = ?3 WHERE id = ?1")
        .bind(user.as_str())
        .bind(username.as_str())
        .bind(now.0)
        .execute(conn)
        .await?;
    Ok(())
}

pub async fn set_avatar(
    conn: &mut SqliteConnection,
    user: &UserId,
    avatar: Option<&BlobId>,
    now: UnixMs,
) -> Result<(), DbError> {
    sqlx::query("UPDATE users SET avatar_blob_id = ?2, updated_at = ?3 WHERE id = ?1")
        .bind(user.as_str())
        .bind(avatar.map(BlobId::as_str))
        .bind(now.0)
        .execute(conn)
        .await?;
    Ok(())
}

/// Whether the `users` table has ever had a row (deactivated Users count; Users are never deleted).
pub async fn any_user(conn: &mut SqliteConnection) -> Result<bool, DbError> {
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users)")
        .fetch_one(conn)
        .await?)
}

/// A new active User, in the caller's transaction.
pub async fn insert(
    conn: &mut SqliteConnection,
    username: &Username,
    email: &Email,
    now: UnixMs,
) -> Result<Profile, DbError> {
    let id = UserId::random();
    sqlx::query("INSERT INTO users (id, username, email, created_at) VALUES (?1, ?2, ?3, ?4)")
        .bind(id.as_str())
        .bind(username.as_str())
        .bind(email.as_str())
        .bind(now.0)
        .execute(conn)
        .await?;
    Ok(Profile {
        id,
        username: username.clone(),
        email: email.clone(),
        avatar: None,
        created_at: now,
    })
}

pub async fn deactivate(
    conn: &mut SqliteConnection,
    user: &UserId,
    now: UnixMs,
) -> Result<bool, DbError> {
    let done = sqlx::query(
        "UPDATE users SET deactivated_at = ?2, updated_at = ?2
         WHERE id = ?1 AND deactivated_at IS NULL",
    )
    .bind(user.as_str())
    .bind(now.0)
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

pub async fn removed_by_email(
    conn: &mut SqliteConnection,
    email: &Email,
) -> Result<Option<UserId>, DbError> {
    let id: Option<String> =
        sqlx::query_scalar("SELECT id FROM users WHERE email = ?1 AND deactivated_at IS NOT NULL")
            .bind(email.as_str())
            .fetch_optional(conn)
            .await?;
    id.map(|id| UserId::from_str(&id).map_err(|e| DbError::Corrupt(e.to_string())))
        .transpose()
}

/// A removed User back as a Member with the same id (their Messages stay theirs), under the
/// username of the request they were Vouched in with.
pub async fn reactivate(
    conn: &mut SqliteConnection,
    user: &UserId,
    username: &Username,
    now: UnixMs,
) -> Result<Profile, DbError> {
    let row: ProfileRow = sqlx::query_as(
        "UPDATE users SET deactivated_at = NULL, username = ?2, updated_at = ?3
         WHERE id = ?1 AND deactivated_at IS NOT NULL
         RETURNING id, username, email, avatar_blob_id, created_at",
    )
    .bind(user.as_str())
    .bind(username.as_str())
    .bind(now.0)
    .fetch_one(&mut *conn)
    .await?;
    // Removal deleted them, but a Sign In racing it may have stored one since: a returning Member
    // starts with no sessions.
    super::sessions::delete_all(conn, user).await?;
    Profile::try_from(row)
}
