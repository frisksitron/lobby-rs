//! The app's own updates. A published build knows where its releases live (a [`ReleaseFeed`]); the
//! [`Updater`] reads the feed's manifest now and then, downloads a newer app next to the running
//! one while it reports progress, and checks the download's size and SHA-256 against the
//! manifest. Swapping it in is [`swap_in`], which the app does on its way out.
//!
//! A feed is laid out like a GitHub repository's Releases: the newest manifest at
//! `<base>/latest/download/latest.json`, each version's app at
//! `<base>/download/v<version>/LobbySetup.exe`.

use std::{
    fmt, fs, io,
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tokio::{
    io::AsyncWriteExt as _,
    runtime::Handle,
    sync::{Notify, watch},
};
use tokio_util::task::AbortOnDropHandle;
use tracing::{info, warn};
use url::Url;

/// The release asset: the app itself, which installs itself when started from anywhere else.
pub const ASSET: &str = "LobbySetup.exe";
/// Every file an install or an update makes in the app's folder, for uninstall.
pub const APP_FILES: [&str; 5] = ["Lobby.exe", OLD, STAGED, PARTIAL, SETUP_COPY];
pub const MANIFEST: &str = "latest.json";

const MANIFEST_TIMEOUT: Duration = Duration::from_secs(20);
/// A download that receives nothing for this long is abandoned until the next check.
const STALL_AFTER: Duration = Duration::from_secs(30);
/// Progress is reported in steps of this fraction of the download, so a fast link does not wake
/// the UI for every packet.
const PROGRESS_STEPS: u64 = 200;

/// The download in progress, next to the app, and the finished one waiting for a restart.
const PARTIAL: &str = "Lobby.download";
const STAGED: &str = "Lobby.update.exe";
/// The app that was running when an update was swapped in; the next start deletes it.
const OLD: &str = "Lobby.old.exe";
/// Setup's copy of itself, before it is swapped in.
const SETUP_COPY: &str = "Lobby.setup.exe";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not a version: {0:?}")]
pub struct BadVersion(String);

impl FromStr for Version {
    type Err = BadVersion;

    /// `1.2.3`, or `v1.2.3` as a tag says it.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = || BadVersion(s.to_owned());
        let mut parts = s.strip_prefix('v').unwrap_or(s).split('.');
        let mut next = || -> Result<u32, BadVersion> {
            parts.next().ok_or_else(bad)?.parse().map_err(|_| bad())
        };
        let version = Version {
            major: next()?,
            minor: next()?,
            patch: next()?,
        };
        match parts.next() {
            None => Ok(version),
            Some(_) => Err(bad()),
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl Serialize for Version {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Version {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// `latest.json`: the newest release and what its app must hash to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: Version,
    pub size: u64,
    /// Lowercase hex.
    pub sha256: String,
}

/// Where a build's releases live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseFeed {
    base: Url,
}

impl ReleaseFeed {
    /// `owner/name`'s GitHub Releases.
    pub fn github(repo: &str) -> Result<ReleaseFeed, url::ParseError> {
        Self::at(&format!("https://github.com/{repo}/releases"))
    }

    /// Any server laid out like GitHub Releases, such as a test's.
    pub fn at(base: &str) -> Result<ReleaseFeed, url::ParseError> {
        let base = Url::parse(base.trim_end_matches('/'))?;
        Ok(ReleaseFeed { base })
    }

    pub fn manifest_url(&self) -> String {
        format!("{}/latest/download/{MANIFEST}", self.base)
    }

    pub fn asset_url(&self, version: Version) -> String {
        format!("{}/download/v{version}/{ASSET}", self.base)
    }

    /// The page a person reads a release's notes on.
    pub fn notes_url(&self, version: Version) -> String {
        format!("{}/tag/v{version}", self.base)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum UpdateError {
    #[error("the release feed could not be reached")]
    Unreachable,
    #[error("the release feed's answer was not a manifest")]
    BadManifest,
    #[error("the download stopped")]
    Interrupted,
    #[error("the download does not match its manifest")]
    Corrupt,
    #[error("the update could not be saved next to the app")]
    Disk,
}

/// What the app shows about its updates. The title bar shows nothing while the app is current;
/// Settings says what the last check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateStatus {
    Current(Check),
    Downloading {
        version: Version,
        received: u64,
        total: u64,
    },
    /// Downloaded and checked: a restart runs it.
    Ready {
        version: Version,
        file: PathBuf,
    },
    Failed {
        version: Version,
        error: UpdateError,
    },
}

/// What the last look at the release feed found, while there is no update in hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    NotYet,
    Checking,
    UpToDate,
    /// Offline, or the feed answered with no manifest: the next check tries again.
    Unreachable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpdateTiming {
    /// The first check waits this long, so it stays out of the way of startup's connect.
    pub first_check: Duration,
    pub check_every: Duration,
}

impl Default for UpdateTiming {
    fn default() -> Self {
        Self {
            first_check: Duration::from_secs(5),
            check_every: Duration::from_secs(60 * 60),
        }
    }
}

/// Checks for updates and downloads them until dropped.
pub struct Updater {
    status: watch::Receiver<UpdateStatus>,
    check_now: Arc<Notify>,
    _task: AbortOnDropHandle<()>,
}

impl Updater {
    /// `dir` is the running app's folder: the update is downloaded there, so the swap is a
    /// rename on one volume.
    pub fn start(
        rt: &Handle,
        http: reqwest::Client,
        feed: ReleaseFeed,
        current: Version,
        dir: PathBuf,
        timing: UpdateTiming,
    ) -> Updater {
        let (tx, status) = watch::channel(UpdateStatus::Current(Check::NotYet));
        let check_now = Arc::new(Notify::new());
        let run = Run {
            http,
            feed,
            current,
            dir,
            tx,
        };
        let task = rt.spawn(run.run(timing, check_now.clone()));
        Updater {
            status,
            check_now,
            _task: AbortOnDropHandle::new(task),
        }
    }

    /// Changes as the checks and the download go. Runtime-agnostic: a GPUI task may await it.
    pub fn status(&self) -> watch::Receiver<UpdateStatus> {
        self.status.clone()
    }

    /// Checks at once instead of at the next interval, as after a failed download or a Lobby
    /// that runs a newer protocol.
    pub fn check_now(&self) {
        self.check_now.notify_one();
    }
}

struct Run {
    http: reqwest::Client,
    feed: ReleaseFeed,
    current: Version,
    dir: PathBuf,
    tx: watch::Sender<UpdateStatus>,
}

impl Run {
    async fn run(self, timing: UpdateTiming, check_now: Arc<Notify>) {
        tokio::select! {
            () = tokio::time::sleep(timing.first_check) => {}
            () = check_now.notified() => {}
        }
        loop {
            self.check().await;
            tokio::select! {
                () = tokio::time::sleep(timing.check_every) => {}
                () = check_now.notified() => {}
            }
        }
    }

    async fn check(&self) {
        // An update downloading or ready stays on show while the feed is read again.
        let quiet =
            |s: &UpdateStatus| matches!(s, UpdateStatus::Current(_) | UpdateStatus::Failed { .. });
        self.tx.send_if_modified(|s| {
            let announce = quiet(s);
            if announce {
                *s = UpdateStatus::Current(Check::Checking);
            }
            announce
        });
        let manifest = match self.manifest().await {
            Ok(m) => m,
            Err(e) => {
                info!(error = %e, "no update manifest");
                self.tx.send_if_modified(|s| {
                    let checking = *s == UpdateStatus::Current(Check::Checking);
                    if checking {
                        *s = UpdateStatus::Current(Check::Unreachable);
                    }
                    checking
                });
                return;
            }
        };
        if manifest.version <= self.current {
            self.tx.send_replace(UpdateStatus::Current(Check::UpToDate));
            return;
        }
        if let UpdateStatus::Ready { version, .. } = &*self.tx.borrow()
            && *version == manifest.version
        {
            return;
        }
        let staged = self.dir.join(STAGED);
        // Downloaded by an earlier run that was never restarted.
        if matches!(hash_file(&staged), Ok(found) if found == manifest.sha256) {
            info!(version = %manifest.version, "the update is already downloaded");
            self.ready(manifest.version, staged);
            return;
        }
        info!(version = %manifest.version, size = manifest.size, "downloading the update");
        match self.download(&manifest, &staged).await {
            Ok(()) => {
                info!(version = %manifest.version, "the update is ready");
                self.ready(manifest.version, staged);
            }
            Err(error) => {
                warn!(version = %manifest.version, %error, "the update failed");
                let _ = fs::remove_file(self.dir.join(PARTIAL));
                self.tx.send_replace(UpdateStatus::Failed {
                    version: manifest.version,
                    error,
                });
            }
        }
    }

    fn ready(&self, version: Version, file: PathBuf) {
        self.tx.send_replace(UpdateStatus::Ready { version, file });
    }

    async fn manifest(&self) -> Result<Manifest, UpdateError> {
        let response = self
            .http
            .get(self.feed.manifest_url())
            .timeout(MANIFEST_TIMEOUT)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|_| UpdateError::Unreachable)?;
        let body = response
            .bytes()
            .await
            .map_err(|_| UpdateError::Unreachable)?;
        serde_json::from_slice(&body).map_err(|_| UpdateError::BadManifest)
    }

    async fn download(&self, manifest: &Manifest, staged: &Path) -> Result<(), UpdateError> {
        let (version, total) = (manifest.version, manifest.size);
        let progress = |received| {
            self.tx.send_replace(UpdateStatus::Downloading {
                version,
                received,
                total,
            });
        };
        progress(0);
        let mut response = self
            .http
            .get(self.feed.asset_url(version))
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|_| UpdateError::Unreachable)?;
        let partial = self.dir.join(PARTIAL);
        let mut open = tokio::fs::OpenOptions::new();
        open.write(true).create(true).truncate(true);
        // No sharing: a second Lobby in this folder gets an error instead of writing into it.
        #[cfg(windows)]
        open.share_mode(0);
        let mut file = open.open(&partial).await.map_err(|_| UpdateError::Disk)?;
        let mut hash = Sha256::new();
        let (mut received, mut reported) = (0u64, 0u64);
        let step = (total / PROGRESS_STEPS).max(1);
        loop {
            let chunk = tokio::time::timeout(STALL_AFTER, response.chunk())
                .await
                .map_err(|_| UpdateError::Interrupted)?
                .map_err(|_| UpdateError::Interrupted)?;
            let Some(chunk) = chunk else { break };
            received += chunk.len() as u64;
            if received > total {
                return Err(UpdateError::Corrupt);
            }
            hash.update(&chunk);
            file.write_all(&chunk)
                .await
                .map_err(|_| UpdateError::Disk)?;
            if received - reported >= step {
                reported = received;
                progress(received);
            }
        }
        // On disk before it can become the app: a power cut after the swap must not leave a
        // Lobby.exe whose data was still only in the cache.
        file.sync_all().await.map_err(|_| UpdateError::Disk)?;
        drop(file);
        if received != total || hex(&hash.finalize()) != manifest.sha256 {
            return Err(UpdateError::Corrupt);
        }
        fs::rename(&partial, staged).map_err(|_| UpdateError::Disk)?;
        // What a restart would run is the file, so the file is what gets checked.
        let check = staged.to_owned();
        let on_disk = tokio::task::spawn_blocking(move || hash_file(&check))
            .await
            .map_err(|_| UpdateError::Disk)?
            .map_err(|_| UpdateError::Disk)?;
        if on_disk != manifest.sha256 {
            let _ = fs::remove_file(staged);
            return Err(UpdateError::Corrupt);
        }
        progress(total);
        Ok(())
    }
}

fn hash_file(path: &Path) -> io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    io::copy(&mut file, &mut hash)?;
    Ok(hex(&hash.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The SHA-256 a manifest names for `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// Puts `new` where `exe` is. A running exe cannot be overwritten or deleted on Windows, but it
/// can be renamed: it moves aside to `Lobby.old.exe`, which [`clean_up`] deletes once it no
/// longer runs. If the second rename fails the first is undone.
pub fn swap_in(exe: &Path, new: &Path) -> io::Result<()> {
    let old = exe.with_file_name(OLD);
    match fs::remove_file(&old) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let moved_aside = match fs::rename(exe, &old) {
        Ok(()) => true,
        Err(e) if e.kind() == io::ErrorKind::NotFound => false,
        Err(e) => return Err(e),
    };
    if let Err(e) = fs::rename(new, exe) {
        if moved_aside {
            let _ = fs::rename(&old, exe);
        }
        return Err(e);
    }
    Ok(())
}

/// Deletes what an earlier update or install left beside `exe`: the app it replaced, and setup's
/// copy. Whatever is still in use stays for the next start. A cut-off download is the updater's:
/// it starts over in the same file.
pub fn clean_up(exe: &Path) {
    for leftover in [OLD, SETUP_COPY] {
        let _ = fs::remove_file(exe.with_file_name(leftover));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_swap_moves_the_running_app_aside_and_the_next_start_clears_it() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("Lobby.exe");
        let new = dir.path().join(STAGED);
        fs::write(&exe, b"old").unwrap();
        fs::write(&new, b"new").unwrap();
        swap_in(&exe, &new).unwrap();
        assert_eq!(fs::read(&exe).unwrap(), b"new");
        assert_eq!(fs::read(dir.path().join(OLD)).unwrap(), b"old");
        assert!(!new.exists());
        clean_up(&exe);
        assert!(!dir.path().join(OLD).exists());
        assert!(exe.exists());
    }
}
