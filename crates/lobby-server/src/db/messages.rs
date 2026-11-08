//! `messages`: the Lobby's single Chat. `seq` is the `MessageId`. A deleted Message keeps its
//! row, emptied, and nothing but a resend of its nonce and the changes feed ever reads it.

use std::str::FromStr;

use lobby_protocol::{
    AuthorRef, BlobId, Message, MessageChanges, MessageId, UnixMs, UserId, limits,
};
use sqlx::{FromRow, SqliteConnection};

use super::{Db, DbError, blobs, users};
use crate::blobs::BlobFiles;

#[derive(FromRow)]
struct Row {
    seq: i64,
    content: String,
    nonce: String,
    created_at: i64,
    edited_at: Option<i64>,
    rev: Option<i64>,
    author_id: String,
    username: String,
    avatar_blob_id: Option<String>,
    deactivated: bool,
}

impl TryFrom<Row> for Message {
    type Error = DbError;
    fn try_from(r: Row) -> Result<Message, DbError> {
        let corrupt = |e: lobby_protocol::IdError| DbError::Corrupt(e.to_string());
        Ok(Message {
            id: MessageId::new(r.seq).map_err(corrupt)?,
            author: AuthorRef {
                id: UserId::from_str(&r.author_id).map_err(corrupt)?,
                username: r.username,
                avatar: r
                    .avatar_blob_id
                    .map(|id| BlobId::from_str(&id).map_err(corrupt))
                    .transpose()?,
                deactivated: r.deactivated,
            },
            content: r.content,
            attachments: Vec::new(),
            nonce: r.nonce,
            created_at: UnixMs(r.created_at),
            edited_at: r.edited_at.map(UnixMs),
            rev: r.rev.map(rev).transpose()?,
        })
    }
}

fn rev(r: i64) -> Result<u64, DbError> {
    u64::try_from(r).map_err(|e| DbError::Corrupt(e.to_string()))
}

const SELECT: &str = "SELECT m.seq, m.content, m.nonce, m.created_at, m.edited_at, m.rev,
        u.id AS author_id, u.username, u.avatar_blob_id, (u.deactivated_at IS NOT NULL) AS deactivated
 FROM messages m JOIN users u ON u.id = m.author_id";

pub async fn last_seq(db: &Db) -> Result<u64, DbError> {
    let seq: Option<i64> = sqlx::query_scalar("SELECT max(seq) FROM messages")
        .fetch_one(db.readers())
        .await?;
    seq.map_or(Ok(0), rev)
}

pub async fn last_rev(db: &Db) -> Result<u64, DbError> {
    let mut conn = db.readers().acquire().await?;
    newest_rev(&mut conn).await
}

async fn newest_rev(conn: &mut SqliteConnection) -> Result<u64, DbError> {
    let last: Option<i64> = sqlx::query_scalar("SELECT max(rev) FROM messages")
        .fetch_one(&mut *conn)
        .await?;
    last.map_or(Ok(0), rev)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inserted {
    New(Message),
    /// `(author, nonce)` was stored before: the stored Message, for the sender only.
    Duplicate(Message),
    /// `(author, nonce)` was stored and deleted since. Nothing stored.
    Deleted,
    /// The claim matched fewer rows than ids (another Member's, expired or already claimed):
    /// rolled back, nothing stored.
    AttachmentsUnavailable,
    /// The author is no longer an active Member: a send that waited for the writer while a ban
    /// committed. Nothing stored.
    AuthorRemoved,
}

/// On the writer the caller holds. The caller broadcasts a `New` Message before it releases
/// `writer`, so commit order is broadcast order. The duplicate check comes before the claim, so a
/// resend of a stored Message replays it although its blobs are claimed by now. The author is
/// checked inside the transaction, so a ban that committed first wins.
pub async fn insert(
    writer: &mut SqliteConnection,
    author: &UserId,
    content: &str,
    nonce: &str,
    attachments: &[BlobId],
    now: UnixMs,
) -> Result<Inserted, DbError> {
    let mut tx = super::begin_immediate(writer).await?;
    if users::active_profile(&mut tx, author).await?.is_none() {
        tx.rollback().await?;
        return Ok(Inserted::AuthorRemoved);
    }
    let seq: Option<i64> = sqlx::query_scalar(
        "INSERT INTO messages (author_id, content, nonce, created_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (author_id, nonce) DO NOTHING RETURNING seq",
    )
    .bind(author.as_str())
    .bind(content)
    .bind(nonce)
    .bind(now.0)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(seq) = seq else {
        tx.rollback().await?;
        return Ok(match by_nonce(writer, author, nonce).await? {
            Some(stored) => Inserted::Duplicate(stored),
            None => Inserted::Deleted,
        });
    };
    if !attachments.is_empty() && !blobs::claim(&mut tx, seq, attachments, author, now).await? {
        tx.rollback().await?;
        return Ok(Inserted::AttachmentsUnavailable);
    }
    // The AuthorRef is read fresh, inside the same transaction.
    let message = by_seq(&mut tx, seq).await?;
    tx.commit().await?;
    Ok(Inserted::New(message))
}

/// Why an edit or a delete changed nothing. Nothing was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotChanged {
    /// No such Message, or it was deleted.
    Missing,
    NotAuthor,
    /// The author is no longer an active Member, as in [`Inserted::AuthorRemoved`].
    AuthorRemoved,
}

/// Inside `tx`: whether `author` may change Message `id`.
async fn check_author(
    tx: &mut SqliteConnection,
    author: &UserId,
    id: MessageId,
) -> Result<Result<(), NotChanged>, DbError> {
    if users::active_profile(tx, author).await?.is_none() {
        return Ok(Err(NotChanged::AuthorRemoved));
    }
    let by: Option<String> =
        sqlx::query_scalar("SELECT author_id FROM messages WHERE seq = ?1 AND deleted_at IS NULL")
            .bind(id.get())
            .fetch_optional(&mut *tx)
            .await?;
    Ok(match by {
        None => Err(NotChanged::Missing),
        Some(by) if by != author.as_str() => Err(NotChanged::NotAuthor),
        Some(_) => Ok(()),
    })
}

/// On the writer the caller holds, which broadcasts the edited Message before it releases it.
/// The edit takes the next change revision; the writer is the only one, so none repeats.
pub async fn edit(
    writer: &mut SqliteConnection,
    author: &UserId,
    id: MessageId,
    content: &str,
    now: UnixMs,
) -> Result<Result<Message, NotChanged>, DbError> {
    let mut tx = super::begin_immediate(writer).await?;
    if let Err(no) = check_author(&mut tx, author, id).await? {
        tx.rollback().await?;
        return Ok(Err(no));
    }
    sqlx::query(
        "UPDATE messages SET content = ?2, edited_at = ?3,
             rev = (SELECT coalesce(max(rev), 0) + 1 FROM messages)
         WHERE seq = ?1",
    )
    .bind(id.get())
    .bind(content)
    .bind(now.0)
    .execute(&mut *tx)
    .await?;
    let message = by_seq(&mut tx, id.get()).await?;
    tx.commit().await?;
    Ok(Ok(message))
}

#[derive(Debug)]
pub struct Deleted {
    /// For the caller to delete after the commit.
    pub files: Vec<BlobFiles>,
    pub rev: u64,
}

/// On the writer the caller holds. The row stays, emptied, under the next change revision; its
/// attachments' rows go. History pages by `seq` and skips it, so its gap pages like any other.
pub async fn delete(
    writer: &mut SqliteConnection,
    author: &UserId,
    id: MessageId,
    now: UnixMs,
) -> Result<Result<Deleted, NotChanged>, DbError> {
    let mut tx = super::begin_immediate(writer).await?;
    if let Err(no) = check_author(&mut tx, author, id).await? {
        tx.rollback().await?;
        return Ok(Err(no));
    }
    let files = blobs::take_of_message(&mut tx, id.get()).await?;
    let changed: i64 = sqlx::query_scalar(
        "UPDATE messages SET content = '', deleted_at = ?2,
             rev = (SELECT coalesce(max(rev), 0) + 1 FROM messages)
         WHERE seq = ?1 RETURNING rev",
    )
    .bind(id.get())
    .bind(now.0)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Ok(Deleted {
        files,
        rev: rev(changed)?,
    }))
}

/// The Messages edited or deleted after change revision `after`, each once as it is now, the
/// oldest change first.
pub async fn changes(db: &Db, after: u64) -> Result<MessageChanges, DbError> {
    let after = i64::try_from(after).unwrap_or(i64::MAX);
    let take = i64::try_from(limits::CHANGES_MAX).unwrap_or(i64::MAX);
    let mut conn = db.readers().acquire().await?;
    let rows: Vec<Row> = sqlx::query_as(const_format(
        SELECT,
        " WHERE m.rev > ?1 AND m.deleted_at IS NULL ORDER BY m.rev LIMIT ?2",
    ))
    .bind(after)
    .bind(take)
    .fetch_all(&mut *conn)
    .await?;
    let mut updated = rows
        .into_iter()
        .map(Message::try_from)
        .collect::<Result<Vec<_>, _>>()?;
    with_attachments(&mut conn, &mut updated).await?;
    let removed: Vec<i64> = sqlx::query_scalar(
        "SELECT seq FROM messages WHERE rev > ?1 AND deleted_at IS NOT NULL ORDER BY rev LIMIT ?2",
    )
    .bind(after)
    .bind(take)
    .fetch_all(&mut *conn)
    .await?;
    let removed = removed
        .into_iter()
        .map(|seq| MessageId::new(seq).map_err(|e| DbError::Corrupt(e.to_string())))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(MessageChanges { updated, removed })
}

async fn with_attachments(
    conn: &mut SqliteConnection,
    messages: &mut [Message],
) -> Result<(), DbError> {
    let seqs: Vec<i64> = messages.iter().map(|m| m.id.get()).collect();
    let mut attachments = blobs::of_messages(conn, &seqs).await?;
    for m in messages {
        m.attachments = attachments.remove(&m.id.get()).unwrap_or_default();
    }
    Ok(())
}

async fn by_seq(conn: &mut SqliteConnection, seq: i64) -> Result<Message, DbError> {
    let row: Row = sqlx::query_as(const_format(SELECT, " WHERE m.seq = ?1"))
        .bind(seq)
        .fetch_one(&mut *conn)
        .await?;
    let mut message = Message::try_from(row)?;
    with_attachments(conn, std::slice::from_mut(&mut message)).await?;
    Ok(message)
}

async fn by_nonce(
    conn: &mut SqliteConnection,
    author: &UserId,
    nonce: &str,
) -> Result<Option<Message>, DbError> {
    let row: Option<Row> = sqlx::query_as(const_format(
        SELECT,
        " WHERE m.author_id = ?1 AND m.nonce = ?2 AND m.deleted_at IS NULL",
    ))
    .bind(author.as_str())
    .bind(nonce)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let mut message = Message::try_from(row)?;
    with_attachments(conn, std::slice::from_mut(&mut message)).await?;
    Ok(Some(message))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Latest,
    Before(MessageId),
    After(MessageId),
}

/// One page, ascending by id, and whether more exist beyond it in the requested direction.
pub async fn page(db: &Db, page: Page, limit: u8) -> Result<(Vec<Message>, bool), DbError> {
    let mut conn = db.readers().acquire().await?;
    read_page(&mut conn, page, limit).await
}

/// [`page`] and the newest change revision, read in one snapshot: the page shows every edit and
/// deletion up to that revision, and none after it.
pub async fn page_at_rev(
    db: &Db,
    page: Page,
    limit: u8,
) -> Result<(Vec<Message>, bool, u64), DbError> {
    let mut tx = db.readers().begin().await?;
    let (messages, has_more) = read_page(&mut tx, page, limit).await?;
    let rev = newest_rev(&mut tx).await?;
    tx.commit().await?;
    Ok((messages, has_more, rev))
}

/// Fetches `limit + 1` rows, so no count query is needed.
async fn read_page(
    conn: &mut SqliteConnection,
    page: Page,
    limit: u8,
) -> Result<(Vec<Message>, bool), DbError> {
    let take = i64::from(limit) + 1;
    let rows: Vec<Row> = match page {
        Page::Latest => {
            sqlx::query_as(const_format(
                SELECT,
                " WHERE m.deleted_at IS NULL ORDER BY m.seq DESC LIMIT ?1",
            ))
            .bind(take)
            .fetch_all(&mut *conn)
            .await?
        }
        Page::Before(id) => {
            sqlx::query_as(const_format(
                SELECT,
                " WHERE m.seq < ?1 AND m.deleted_at IS NULL ORDER BY m.seq DESC LIMIT ?2",
            ))
            .bind(id.get())
            .bind(take)
            .fetch_all(&mut *conn)
            .await?
        }
        Page::After(id) => {
            sqlx::query_as(const_format(
                SELECT,
                " WHERE m.seq > ?1 AND m.deleted_at IS NULL ORDER BY m.seq ASC LIMIT ?2",
            ))
            .bind(id.get())
            .bind(take)
            .fetch_all(&mut *conn)
            .await?
        }
    };
    let has_more = rows.len() > usize::from(limit);
    let mut messages = rows
        .into_iter()
        .take(usize::from(limit))
        .map(Message::try_from)
        .collect::<Result<Vec<_>, _>>()?;
    with_attachments(conn, &mut messages).await?;
    if !matches!(page, Page::After(_)) {
        messages.reverse();
    }
    Ok((messages, has_more))
}

/// The shared `SELECT` plus a fixed suffix: both are our own literals, so the statement is safe.
fn const_format(head: &'static str, tail: &'static str) -> sqlx::AssertSqlSafe<String> {
    sqlx::AssertSqlSafe(format!("{head}{tail}"))
}
