//! SQLite access: one writer connection, which only the writer task in `crate::domain` holds, and
//! four read-only readers for lookups and history. Statements are runtime-checked `sqlx::query*`
//! calls in this module's files.

pub mod admission;
pub mod blobs;
mod error;
pub mod icon;
mod lock;
pub mod messages;
pub mod owner;
pub mod sessions;
#[cfg(test)]
mod tests;
pub mod users;

use std::{
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};

pub use error::DbError;
use lobby_protocol::LobbyInstanceId;
pub use lock::{DbLock, LockError};
use sqlx::{
    Connection as _, SqliteConnection, SqlitePool,
    migrate::{MigrateError, Migrator},
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};
use tracing::info;

/// `PRAGMA application_id` of every lobby-server database: "LOBY".
pub const APPLICATION_ID: i64 = 0x4C4F_4259;
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
pub const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);
const PING_TIMEOUT: Duration = Duration::from_secs(1);
const READERS: u32 = 4;

pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// Every write transaction takes SQLite's write lock up front, so a read inside it can never be
/// outdated by the time the transaction writes.
pub async fn begin_immediate(
    writer: &mut SqliteConnection,
) -> Result<sqlx::Transaction<'_, sqlx::Sqlite>, DbError> {
    Ok(writer.begin_with("BEGIN IMMEDIATE").await?)
}

#[derive(Debug, thiserror::Error)]
pub enum DbOpenError {
    #[error("database at {} was not created by lobby-server", .path.display())]
    Foreign { path: PathBuf },
    #[error("database at {} failed its integrity check: {finding}", .path.display())]
    Integrity { path: PathBuf, finding: String },
    #[error("cannot create the database directory {}: {source}", .path.display())]
    Dir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot open the database at {}: {source}", .path.display())]
    Open {
        path: PathBuf,
        #[source]
        source: sqlx::Error,
    },
    #[error("database migration failed: {0}")]
    Migrate(#[source] MigrateError),
}

/// The readers. The writer is a connection of its own, which [`Db::open`] hands out once.
#[derive(Clone)]
pub struct Db {
    readers: SqlitePool,
}

impl Db {
    /// The foreign-DB guard, migrations, `application_id` and the integrity checks, in that order.
    /// Answers the readers and the one writer connection, which runs `PRAGMA optimize` when it
    /// closes. The caller holds the [`DbLock`].
    pub async fn open(path: &Path) -> Result<(Db, SqliteConnection), DbOpenError> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|source| DbOpenError::Dir {
                path: dir.to_owned(),
                source,
            })?;
        }
        let open_err = |source| DbOpenError::Open {
            path: path.to_owned(),
            source,
        };
        let fresh = guard_foreign(path).await?;
        let base = SqliteConnectOptions::new()
            .filename(path)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true)
            .busy_timeout(BUSY_TIMEOUT);
        let mut writer = SqliteConnection::connect_with(
            &base
                .clone()
                .create_if_missing(true)
                .optimize_on_close(true, None),
        )
        .await
        .map_err(open_err)?;
        MIGRATOR
            .run(&mut writer)
            .await
            .map_err(DbOpenError::Migrate)?;
        let app_id: i64 = sqlx::query_scalar("PRAGMA application_id")
            .fetch_one(&mut writer)
            .await
            .map_err(open_err)?;
        if app_id == 0 {
            // PRAGMA takes no bound parameters; the value is our own constant.
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "PRAGMA application_id = {APPLICATION_ID}"
            )))
            .execute(&mut writer)
            .await
            .map_err(open_err)?;
        }
        check_integrity(&mut writer, path).await?;
        // The readers stay open: sqlx closes an idle one after 10 minutes and every one after
        // 30, and the next read after a quiet spell would open the file again.
        let readers = SqlitePoolOptions::new()
            .max_connections(READERS)
            .min_connections(READERS)
            .idle_timeout(None)
            .max_lifetime(None)
            .acquire_timeout(ACQUIRE_TIMEOUT)
            .connect_with(base.read_only(true))
            .await
            .map_err(open_err)?;
        info!(path = %path.display(), fresh, "database ready");
        Ok((Db { readers }, writer))
    }

    pub fn readers(&self) -> &SqlitePool {
        &self.readers
    }

    pub async fn ping(&self) -> bool {
        tokio::time::timeout(PING_TIMEOUT, sqlx::query("SELECT 1").execute(&self.readers))
            .await
            .is_ok_and(|r| r.is_ok())
    }

    pub async fn instance_id(&self) -> Result<LobbyInstanceId, DbError> {
        let id: String = sqlx::query_scalar("SELECT instance_id FROM lobby_settings WHERE id = 1")
            .fetch_one(&self.readers)
            .await?;
        LobbyInstanceId::from_str(&id).map_err(|e| DbError::Corrupt(e.to_string()))
    }

    pub async fn close(&self) {
        self.readers.close().await;
    }
}

/// `Ok(true)` for a new or empty file, `Ok(false)` for one of ours; anything else is refused before
/// a single byte of it is changed.
async fn guard_foreign(path: &Path) -> Result<bool, DbOpenError> {
    let empty = std::fs::metadata(path).map_or(true, |m| m.len() == 0);
    if empty {
        return Ok(true);
    }
    let open_err = |source| DbOpenError::Open {
        path: path.to_owned(),
        source,
    };
    let mut conn = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(path)
            .read_only(true)
            .busy_timeout(BUSY_TIMEOUT),
    )
    .await
    .map_err(open_err)?;
    let foreign = || DbOpenError::Foreign {
        path: path.to_owned(),
    };
    let Ok(tables) =
        sqlx::query_scalar::<_, String>("SELECT name FROM sqlite_master WHERE type = 'table'")
            .fetch_all(&mut conn)
            .await
    else {
        return Err(foreign());
    };
    let app_id: i64 = sqlx::query_scalar("PRAGMA application_id")
        .fetch_one(&mut conn)
        .await
        .map_err(open_err)?;
    let _ = conn.close().await;
    if tables.is_empty() {
        return Ok(true);
    }
    if app_id == APPLICATION_ID && tables.iter().any(|t| t == "_sqlx_migrations") {
        Ok(false)
    } else {
        Err(foreign())
    }
}

async fn check_integrity(conn: &mut SqliteConnection, path: &Path) -> Result<(), DbOpenError> {
    let integrity = |finding: String| DbOpenError::Integrity {
        path: path.to_owned(),
        finding,
    };
    let fk = sqlx::query("PRAGMA foreign_key_check")
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| integrity(e.to_string()))?;
    if !fk.is_empty() {
        return Err(integrity(format!("{} foreign key violation(s)", fk.len())));
    }
    let quick: Vec<String> = sqlx::query_scalar("PRAGMA quick_check")
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| integrity(e.to_string()))?;
    if quick != ["ok"] {
        return Err(integrity(quick.join("; ")));
    }
    Ok(())
}
