//! The jobs task, in the services tier: two loops polled side by side. The Voice watchdog sleeps
//! until the earliest join deadline, or until a Join moves it, so a session is ended on time rather
//! than on a polling grid. Cleanup runs every `jobs.cleanup_interval` (the first one interval after
//! boot); side by side, a long cleanup never delays a deadline.

use std::{
    sync::Arc,
    time::{Duration, SystemTime},
};

use lobby_protocol::UnixMs;
use tokio::time::{Instant, MissedTickBehavior, sleep_until};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::{
    blobs::BlobStore,
    db::{Db, blobs},
    domain::{Batch, Domain, WriteError},
    limits::{CodeSends, RouteLimits},
    lobby::Lobby,
};

pub const DECLINED_RETENTION: Duration = Duration::from_secs(30 * 24 * 3600);

/// A file must be older than this to be an orphan: well past the 120 s upload timeout, so one
/// persisted just before its row commits is never swept.
pub const ORPHAN_MIN_AGE: Duration = Duration::from_secs(3600);

#[derive(Clone)]
pub(crate) struct Jobs {
    pub lobby: Lobby,
    pub db: Db,
    pub domain: Domain,
    pub blobs: BlobStore,
    pub limits: Arc<RouteLimits>,
    pub code_sends: Arc<CodeSends>,
    pub cleanup_every: Duration,
}

pub(crate) async fn run(jobs: Jobs, stop: CancellationToken) {
    tokio::join!(watchdog(&jobs.lobby, &stop), cleanup(&jobs, &stop));
    debug!("jobs stopped");
}

async fn watchdog(lobby: &Lobby, stop: &CancellationToken) {
    let wakeup = lobby.voice_wakeup();
    loop {
        let next = lobby.tick_voice(Instant::now());
        let deadline = async {
            match next {
                Some(at) => sleep_until(at).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            biased;
            () = stop.cancelled() => break,
            () = deadline => {}
            () = wakeup.notified() => {}
        }
    }
}

async fn cleanup(jobs: &Jobs, stop: &CancellationToken) {
    // Not at boot: a test that holds the writes to order a race would find a cleanup write in it.
    let mut every =
        tokio::time::interval_at(Instant::now() + jobs.cleanup_every, jobs.cleanup_every);
    every.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            biased;
            () = stop.cancelled() => break,
            _ = every.tick() => {}
        }
        match run_cleanup(jobs, UnixMs::now(), Instant::now(), stop).await {
            Ok(r) => info!(?r, "cleanup"),
            Err(WriteError::Unavailable) => debug!("cleanup stopped: the server is shutting down"),
            Err(e) => warn!(error = %e, "cleanup failed; next run retries"),
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CleanupReport {
    pub magic_codes: u64,
    pub join_tickets: u64,
    pub join_waits: u64,
    pub sessions: u64,
    pub declined_requests: u64,
    pub expired_blobs: u64,
    pub orphans: u64,
    pub limit_windows: usize,
    pub code_sends: usize,
}

/// One batch at a time (each its own short write, so chat sends interleave), looped until the
/// backlog is drained; `stop` is checked between batches.
async fn drain(
    domain: &Domain,
    stop: &CancellationToken,
    batch: Batch,
    now: UnixMs,
) -> Result<u64, WriteError> {
    let mut total = 0;
    while !stop.is_cancelled() {
        let n = domain.clean(batch, now).await?;
        total += n;
        if n == 0 {
            break;
        }
    }
    Ok(total)
}

/// Expired unclaimed uploads, a batch of rows at a time, their files after each batch has
/// committed.
async fn drain_expired_blobs(
    jobs: &Jobs,
    stop: &CancellationToken,
    now: UnixMs,
) -> Result<u64, WriteError> {
    let mut total = 0;
    while !stop.is_cancelled() {
        let files = jobs.domain.take_expired_uploads(now).await?;
        if files.is_empty() {
            break;
        }
        total += files.len() as u64;
        for f in &files {
            jobs.blobs.remove(f).await;
        }
    }
    Ok(total)
}

/// Files under the blob root with no row that are older than [`ORPHAN_MIN_AGE`] as of the cleanup's
/// own `now`. Temp files from a crashed upload are swept the same way. Best effort: a failed walk
/// is logged, not retried.
async fn sweep_orphans(
    jobs: &Jobs,
    stop: &CancellationToken,
    now: UnixMs,
) -> Result<u64, WriteError> {
    let age = ORPHAN_MIN_AGE.as_millis() as i64;
    let cutoff_ms = u64::try_from(now.0.saturating_sub(age)).unwrap_or(0);
    let cutoff = SystemTime::UNIX_EPOCH + Duration::from_millis(cutoff_ms);
    let candidates = match jobs.blobs.files_older_than(cutoff).await {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "orphan sweep could not walk the blob root");
            return Ok(0);
        }
    };
    let known = blobs::known_paths(&jobs.db, &candidates).await?;
    let mut swept = 0;
    for rel in candidates.iter().filter(|c| !known.contains(*c)) {
        if stop.is_cancelled() {
            break;
        }
        jobs.blobs.remove_rel(rel).await;
        swept += 1;
    }
    Ok(swept)
}

pub(crate) async fn run_cleanup(
    jobs: &Jobs,
    now: UnixMs,
    instant: Instant,
    stop: &CancellationToken,
) -> Result<CleanupReport, WriteError> {
    let domain = &jobs.domain;
    let declined_before = UnixMs(now.0 - DECLINED_RETENTION.as_millis() as i64);
    let report = CleanupReport {
        magic_codes: drain(domain, stop, Batch::MagicCodes, now).await?,
        join_tickets: drain(domain, stop, Batch::JoinTickets, now).await?,
        join_waits: drain(domain, stop, Batch::JoinWaits, now).await?,
        sessions: drain(domain, stop, Batch::Sessions, now).await?,
        declined_requests: drain(domain, stop, Batch::Declined(declined_before), now).await?,
        expired_blobs: drain_expired_blobs(jobs, stop, now).await?,
        orphans: sweep_orphans(jobs, stop, now).await?,
        limit_windows: jobs.limits.prune(instant),
        code_sends: jobs.code_sends.prune(instant),
    };
    domain.optimize().await?;
    Ok(report)
}

#[cfg(test)]
mod tests {

    use lobby_protocol::{UserId, Username};
    use sqlx::SqliteConnection;
    use tokio_util::task::TaskTracker;

    use super::*;
    use crate::{
        blobs::BlobKind,
        config::{Config, StorageConfig},
        db::{DbLock, blobs::NewBlob},
        lobby::test_lobby,
    };

    /// Jobs over a database a test filled through `writer` first; the writer task ends with
    /// `stop`.
    fn jobs_for(
        db: Db,
        (conn, lock): (SqliteConnection, DbLock),
        blobs: BlobStore,
        stop: &TaskTracker,
    ) -> Jobs {
        let (lobby, _sfu) = test_lobby(&[]);
        let mut cfg = Config::default();
        cfg.smtp.from = "Lobby <noreply@lobby.test>".into();
        let mailer = crate::mail::Mailer::new(
            &cfg.smtp,
            "Lobby",
            cfg.auth.magic_code_ttl,
            TaskTracker::new(),
            CancellationToken::new(),
        )
        .unwrap();
        let code_sends = Arc::new(CodeSends::new(3, 10, 50));
        let writer = crate::domain::Writer {
            conn,
            lock,
            lobby: lobby.clone(),
            blobs: blobs.clone(),
            cfg: Arc::new(cfg),
            code_sends: code_sends.clone(),
            mailer,
        };
        let domain = Domain::start(writer, stop);
        Jobs {
            lobby,
            db,
            domain,
            blobs,
            limits: Arc::new(RouteLimits::new(true)),
            code_sends,
            cleanup_every: Duration::from_secs(3600),
        }
    }

    async fn stored_blob(
        blobs: &BlobStore,
        writer: &mut SqliteConnection,
        owner: &UserId,
        expires_at: UnixMs,
    ) -> crate::blobs::StoredBlob {
        use futures_util::stream;
        let body = stream::iter([Ok::<_, std::io::Error>(axum::body::Bytes::from_static(
            b"hello",
        ))]);
        let stored = blobs
            .save(BlobKind::ChatAttachment, Some("a.txt"), body)
            .await
            .unwrap();
        blobs::insert(
            writer,
            NewBlob {
                stored: &stored,
                preview: None,
                uploaded_by: owner,
                now: UnixMs(1),
                expires_at: Some(expires_at),
            },
        )
        .await
        .unwrap();
        stored
    }

    async fn an_owner(writer: &mut SqliteConnection) -> UserId {
        use lobby_protocol::Email;
        let alice = crate::db::users::insert(
            writer,
            &Username::parse("alice").unwrap(),
            &Email::parse("alice@example.com").unwrap(),
            UnixMs(1),
        )
        .await
        .unwrap();
        alice.id
    }

    #[tokio::test]
    async fn the_orphan_sweep_spares_fresh_files_and_rows() {
        use futures_util::stream;
        let dir = tempfile::tempdir().unwrap();
        let lock = DbLock::acquire(&dir.path().join("lobby.db")).unwrap();
        let (db, mut writer) = Db::open(&dir.path().join("lobby.db")).await.unwrap();
        let blobs = BlobStore::new(
            &StorageConfig {
                blob_root: dir.path().join("blobs"),
                upload_max_bytes: 1 << 20,
            },
            TaskTracker::new(),
        )
        .unwrap();
        let owner = an_owner(&mut writer).await;
        let body = stream::iter([Ok::<_, std::io::Error>(axum::body::Bytes::from_static(
            b"orphan",
        ))]);
        let orphan = blobs
            .save(BlobKind::ChatAttachment, Some("o.txt"), body)
            .await
            .unwrap();
        let temp = blobs.resolve(&orphan.path).with_file_name(".tmp-crashed");
        std::fs::write(&temp, b"partial").unwrap();
        // Files the store never writes, however old: a stranger in a shard directory, a
        // preview without its `.jpg`, and a blob-named file outside the store's directories.
        let blob_file = blobs.resolve(&orphan.path);
        let root = blob_file.ancestors().nth(3).unwrap();
        let strangers = [
            blob_file.with_file_name("notes.txt"),
            root.join("chat_attachment_preview/ab")
                .join(orphan.id.as_str()),
            root.join("backups/ab").join(orphan.id.as_str()),
        ];
        for s in &strangers {
            std::fs::create_dir_all(s.parent().unwrap()).unwrap();
            std::fs::write(s, b"not ours").unwrap();
        }
        let expires = UnixMs::now().saturating_add(Duration::from_secs(24 * 3600));
        let held = stored_blob(&blobs, &mut writer, &owner, expires).await;
        let writes = TaskTracker::new();
        let jobs = jobs_for(db, (writer, lock), blobs, &writes);
        let stop = CancellationToken::new();

        let now = UnixMs::now();
        let fresh = run_cleanup(&jobs, now, Instant::now(), &stop)
            .await
            .unwrap();
        assert_eq!(fresh.orphans, 0);
        assert!(jobs.blobs.resolve(&orphan.path).exists());

        let later = UnixMs(now.0 + 2 * 3_600_000);
        let swept = run_cleanup(&jobs, later, Instant::now(), &stop)
            .await
            .unwrap();
        assert_eq!(swept.orphans, 2);
        assert!(!jobs.blobs.resolve(&orphan.path).exists());
        assert!(!temp.exists());
        assert!(jobs.blobs.resolve(&held.path).exists());
        for s in &strangers {
            assert!(s.exists(), "{} is not the store's", s.display());
        }
        jobs.domain.stop_admitting();
        writes.close();
        writes.wait().await;
        jobs.db.close().await;
    }
}
