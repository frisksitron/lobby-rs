//! `Db::open`'s state table, and refresh rotation at its boundary.

use std::path::Path;

use lobby_protocol::{Email, UnixMs, Username};
use sqlx::{Connection as _, SqliteConnection, sqlite::SqliteConnectOptions};

use super::{
    APPLICATION_ID, Db, DbOpenError,
    sessions::{self, NewPair, Refreshed},
    users,
};

const DAY_MS: i64 = 24 * 3600 * 1000;

async fn raw(path: &Path) -> SqliteConnection {
    SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true),
    )
    .await
    .unwrap()
}

async fn app_id(path: &Path) -> i64 {
    let mut c = raw(path).await;
    sqlx::query_scalar("PRAGMA application_id")
        .fetch_one(&mut c)
        .await
        .unwrap()
}

async fn close((db, writer): (Db, SqliteConnection)) {
    db.close().await;
    writer.close().await.unwrap();
}

#[tokio::test]
async fn open_state_table() {
    let dir = tempfile::tempdir().unwrap();

    let fresh = dir.path().join("new/lobby.db");
    let opened = Db::open(&fresh).await.unwrap();
    let id = opened.0.instance_id().await.unwrap();
    close(opened).await;
    assert_eq!(app_id(&fresh).await, APPLICATION_ID);
    let opened = Db::open(&fresh).await.unwrap();
    assert_eq!(opened.0.instance_id().await.unwrap(), id);
    assert!(opened.0.ping().await);
    close(opened).await;

    let empty = dir.path().join("empty.db");
    std::fs::write(&empty, b"").unwrap();
    close(Db::open(&empty).await.unwrap()).await;

    // A database that is not ours and a file that is not SQLite at all are refused, untouched.
    let foreign = dir.path().join("foreign.db");
    {
        let mut c = raw(&foreign).await;
        sqlx::query("CREATE TABLE other_app_version (id INTEGER PRIMARY KEY, version_id INTEGER)")
            .execute(&mut c)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE users (id TEXT)")
            .execute(&mut c)
            .await
            .unwrap();
    }
    let before = std::fs::read(&foreign).unwrap();
    let err = Db::open(&foreign).await.err().expect("refused");
    assert!(matches!(err, DbOpenError::Foreign { .. }), "{err}");
    assert!(
        err.to_string().contains("was not created by lobby-server"),
        "{err}"
    );
    assert_eq!(std::fs::read(&foreign).unwrap(), before, "not modified");
    let junk = dir.path().join("junk.db");
    std::fs::write(
        &junk,
        b"definitely not a database, just some bytes in a file",
    )
    .unwrap();
    assert!(matches!(
        Db::open(&junk).await,
        Err(DbOpenError::Foreign { .. })
    ));
}

fn pair<'a>(access: &'a [u8; 32], refresh: &'a [u8; 32], now: UnixMs) -> NewPair<'a> {
    NewPair {
        access_hash: access,
        access_expires_at: UnixMs(now.0 + 15 * 60_000),
        refresh_hash: refresh,
        refresh_expires_at: UnixMs(now.0 + 30 * DAY_MS),
    }
}

/// Two refreshes with the same token, one after the other as the writer runs them: the first
/// rotates, and the second, whose answer was never used, is reissued. A request that carried the
/// old access token and reaches the writer between them does not count as a use of the new pair.
#[tokio::test]
async fn two_refreshes_with_one_token_rotate_once_and_reissue_once() {
    let dir = tempfile::tempdir().unwrap();
    let (db, mut writer) = Db::open(&dir.path().join("lobby.db")).await.unwrap();
    let t0 = UnixMs(DAY_MS);
    let alice = users::insert(
        &mut writer,
        &Username::parse("alice").unwrap(),
        &Email::parse("alice@example.com").unwrap(),
        t0,
    )
    .await
    .unwrap();
    let (a0, r0) = ([1u8; 32], [2u8; 32]);
    sessions::create(&mut writer, &alice.id, &pair(&a0, &r0, t0), t0)
        .await
        .unwrap();
    let (x, y) = (([3u8; 32], [4u8; 32]), ([5u8; 32], [6u8; 32]));
    let first = sessions::refresh(&mut writer, &r0, &pair(&x.0, &x.1, t0), t0).await;
    sessions::mark_used(&mut writer, &a0, t0).await.unwrap();
    let second = sessions::refresh(&mut writer, &r0, &pair(&y.0, &y.1, t0), t0).await;
    assert!(matches!(first.unwrap(), Refreshed::Rotated { .. }));
    assert!(matches!(second.unwrap(), Refreshed::Reissued { .. }));
    close((db, writer)).await;
}
