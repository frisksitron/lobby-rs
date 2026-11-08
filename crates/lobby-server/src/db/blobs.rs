//! `blobs` rows only: the files are `crate::blobs`. Id lists are bound as one JSON array and opened
//! with `json_each`, so every statement is a fixed literal.

use std::collections::{HashMap, HashSet};

use lobby_protocol::{Attachment, BlobId, Preview, UnixMs, UserId};
use sqlx::{FromRow, SqliteConnection};

use super::{Db, DbError};
use crate::blobs::{BlobFiles, StoragePath, StoredBlob, StoredPreview};

pub const BATCH: i64 = 500;
const PATH_CHUNK: usize = 500;

/// A new blob row: unclaimed with an expiry for chat attachments, neither for avatars and icons
/// (the table's CHECK enforces that).
pub struct NewBlob<'a> {
    pub stored: &'a StoredBlob,
    pub preview: Option<&'a StoredPreview>,
    pub uploaded_by: &'a UserId,
    pub now: UnixMs,
    pub expires_at: Option<UnixMs>,
}

/// One short statement on the writer or in its transaction. Returns the wire `Attachment`, so the
/// upload answer and history are built from the same fields.
pub async fn insert(conn: &mut SqliteConnection, b: NewBlob<'_>) -> Result<Attachment, DbError> {
    let size = i64::try_from(b.stored.size).map_err(|e| DbError::Corrupt(e.to_string()))?;
    sqlx::query(
        "INSERT INTO blobs (id, kind, storage_path, mime_type, size_bytes, original_name,
                            uploaded_by, preview_path, preview_width, preview_height,
                            expires_at, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
    )
    .bind(b.stored.id.as_str())
    .bind(b.stored.kind.as_str())
    .bind(b.stored.path.as_str())
    .bind(b.stored.mime.mime())
    .bind(size)
    .bind(&b.stored.name)
    .bind(b.uploaded_by.as_str())
    .bind(b.preview.map(|p| p.path.as_str()))
    .bind(b.preview.map(|p| i64::from(p.size.width)))
    .bind(b.preview.map(|p| i64::from(p.size.height)))
    .bind(b.expires_at.map(|t| t.0))
    .bind(b.now.0)
    .execute(conn)
    .await?;
    Ok(Attachment {
        id: b.stored.id.clone(),
        name: b.stored.name.clone(),
        mime_type: b.stored.mime.mime().to_owned(),
        size: b.stored.size,
        preview: b.preview.map(|p| p.size),
    })
}

/// A replaced avatar or icon: its row goes in the transaction that points elsewhere, and the
/// caller deletes the returned files after the commit.
pub async fn delete(
    conn: &mut SqliteConnection,
    id: &BlobId,
) -> Result<Option<BlobFiles>, DbError> {
    let row: Option<(String, Option<String>)> =
        sqlx::query_as("DELETE FROM blobs WHERE id = ?1 RETURNING storage_path, preview_path")
            .bind(id.as_str())
            .fetch_optional(conn)
            .await?;
    row.map(|(blob, preview)| {
        Ok(BlobFiles {
            blob: stored_path(&blob)?,
            preview: preview.as_deref().map(stored_path).transpose()?,
        })
    })
    .transpose()
}

fn json_list<T: serde::Serialize>(items: &[T]) -> String {
    serde_json::to_string(items).expect("a list of strings or integers serializes")
}

/// Inside `messages::insert`'s transaction: claims every id for message `seq`. `Ok(true)` iff each
/// one was claimable: this author's, unclaimed, not expired.
pub async fn claim(
    tx: &mut SqliteConnection,
    seq: i64,
    ids: &[BlobId],
    author: &UserId,
    now: UnixMs,
) -> Result<bool, DbError> {
    let claimed = sqlx::query(
        "UPDATE blobs SET message_seq = ?1, expires_at = NULL
         WHERE id IN (SELECT value FROM json_each(?2)) AND kind = 'chat_attachment'
           AND uploaded_by = ?3 AND message_seq IS NULL
           AND (expires_at IS NULL OR expires_at > ?4)",
    )
    .bind(seq)
    .bind(json_list(ids))
    .bind(author.as_str())
    .bind(now.0)
    .execute(tx)
    .await?
    .rows_affected();
    Ok(claimed == ids.len() as u64)
}

#[derive(FromRow)]
struct AttachmentRow {
    id: String,
    original_name: String,
    mime_type: String,
    size_bytes: i64,
    preview_width: Option<i64>,
    preview_height: Option<i64>,
    message_seq: i64,
}

impl TryFrom<AttachmentRow> for Attachment {
    type Error = DbError;
    fn try_from(r: AttachmentRow) -> Result<Attachment, DbError> {
        let corrupt = |e: &dyn ToString| DbError::Corrupt(e.to_string());
        let side = |v: i64| u32::try_from(v).map_err(|e| corrupt(&e));
        let preview = match (r.preview_width, r.preview_height) {
            (Some(width), Some(height)) => Some(Preview {
                width: side(width)?,
                height: side(height)?,
            }),
            _ => None,
        };
        Ok(Attachment {
            id: r.id.parse().map_err(|e| corrupt(&e))?,
            name: r.original_name,
            mime_type: r.mime_type,
            size: u64::try_from(r.size_bytes).map_err(|e| corrupt(&e))?,
            preview,
        })
    }
}

/// The attachments of these Messages in one query, in upload order (`rowid`): the live
/// `MESSAGE_CREATE` and every history page read them here, so they cannot disagree.
pub async fn of_messages(
    conn: &mut SqliteConnection,
    seqs: &[i64],
) -> Result<HashMap<i64, Vec<Attachment>>, DbError> {
    let rows: Vec<AttachmentRow> = sqlx::query_as(
        "SELECT id, original_name, mime_type, size_bytes, preview_width, preview_height,
                message_seq
         FROM blobs WHERE message_seq IN (SELECT value FROM json_each(?1)) ORDER BY rowid",
    )
    .bind(json_list(seqs))
    .fetch_all(conn)
    .await?;
    let mut by_seq: HashMap<i64, Vec<Attachment>> = HashMap::new();
    for row in rows {
        let seq = row.message_seq;
        by_seq.entry(seq).or_default().push(row.try_into()?);
    }
    Ok(by_seq)
}

/// What `/media` needs. Any existing row is served, claimed or not: the id is the capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaRow {
    pub path: StoragePath,
    pub preview: Option<StoragePath>,
    pub mime: String,
    pub name: String,
}

#[derive(FromRow)]
struct MediaRaw {
    storage_path: String,
    preview_path: Option<String>,
    mime_type: String,
    original_name: String,
}

fn stored_path(s: &str) -> Result<StoragePath, DbError> {
    StoragePath::parse(s).map_err(|e| DbError::Corrupt(e.to_string()))
}

pub async fn media(db: &Db, id: &BlobId) -> Result<Option<MediaRow>, DbError> {
    let raw: Option<MediaRaw> = sqlx::query_as(
        "SELECT storage_path, preview_path, mime_type, original_name FROM blobs WHERE id = ?1",
    )
    .bind(id.as_str())
    .fetch_optional(db.readers())
    .await?;
    raw.map(|r| {
        Ok(MediaRow {
            path: stored_path(&r.storage_path)?,
            preview: r.preview_path.as_deref().map(stored_path).transpose()?,
            mime: r.mime_type,
            name: r.original_name,
        })
    })
    .transpose()
}

/// One batch in one statement: deletes up to 500 expired unclaimed rows and returns their files,
/// which the caller deletes after the writer is released.
pub async fn take_expired(
    conn: &mut SqliteConnection,
    now: UnixMs,
) -> Result<Vec<BlobFiles>, DbError> {
    let rows: Vec<(String, Option<String>)> = sqlx::query_as(
        "DELETE FROM blobs WHERE id IN (
             SELECT id FROM blobs
             WHERE message_seq IS NULL AND expires_at IS NOT NULL AND expires_at < ?1 LIMIT ?2)
         RETURNING storage_path, preview_path",
    )
    .bind(now.0)
    .bind(BATCH)
    .fetch_all(conn)
    .await?;
    files_of(rows)
}

/// Inside the transaction that deletes Message `seq`: its attachments' rows go first, for the
/// foreign key, and the caller deletes the returned files after the commit.
pub async fn take_of_message(
    conn: &mut SqliteConnection,
    seq: i64,
) -> Result<Vec<BlobFiles>, DbError> {
    let rows: Vec<(String, Option<String>)> = sqlx::query_as(
        "DELETE FROM blobs WHERE message_seq = ?1 RETURNING storage_path, preview_path",
    )
    .bind(seq)
    .fetch_all(conn)
    .await?;
    files_of(rows)
}

fn files_of(rows: Vec<(String, Option<String>)>) -> Result<Vec<BlobFiles>, DbError> {
    rows.into_iter()
        .map(|(blob, preview)| {
            Ok(BlobFiles {
                blob: stored_path(&blob)?,
                preview: preview.as_deref().map(stored_path).transpose()?,
            })
        })
        .collect()
}

/// The orphan sweep's question: which of these root-relative paths does a row name, as its file
/// or its preview?
pub async fn known_paths(db: &Db, rels: &[String]) -> Result<HashSet<String>, DbError> {
    let mut known = HashSet::new();
    for chunk in rels.chunks(PATH_CHUNK) {
        let found: Vec<String> = sqlx::query_scalar(
            "SELECT storage_path FROM blobs WHERE storage_path IN (SELECT value FROM json_each(?1))
             UNION
             SELECT preview_path FROM blobs WHERE preview_path IN (SELECT value FROM json_each(?1))",
        )
        .bind(json_list(chunk))
        .fetch_all(db.readers())
        .await?;
        known.extend(found);
    }
    Ok(known)
}
