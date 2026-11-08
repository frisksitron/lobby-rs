//! The blob store. Files only: rows are `db::blobs`. Order is always file, then row (in the
//! caller's transaction), then commit; a failed row deletes the file, and the orphan sweep catches
//! a crash in between.

pub mod image;
pub mod name;
pub mod sniff;

use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

use axum::{BoxError, body::Bytes};
use futures_util::{Stream, StreamExt as _};
use lobby_protocol::{BlobId, Preview};
use tokio::{io::AsyncWriteExt as _, sync::Semaphore};
use tokio_util::task::TaskTracker;
use tracing::{debug, warn};

use self::sniff::SNIFF_LEN;
pub use self::sniff::Sniffed;
use crate::{
    config::StorageConfig,
    cpu::{CpuBusy, on_cpu},
};

const TEMP_PREFIX: &str = ".tmp-";
const PREVIEW_DIR: &str = "chat_attachment_preview";
/// Every directory the store writes under the root, with what follows the blob id in its file
/// names. The orphan sweep looks nowhere else.
const STORE_DIRS: [(&str, &str); 4] = [
    (BlobKind::Avatar.as_str(), ""),
    (BlobKind::LobbyIcon.as_str(), ""),
    (BlobKind::ChatAttachment.as_str(), ""),
    (PREVIEW_DIR, ".jpg"),
];

/// Stored-file kinds (the `blobs.kind` CHECK).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlobKind {
    Avatar,
    LobbyIcon,
    ChatAttachment,
}

impl BlobKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Avatar => "avatar",
            Self::LobbyIcon => "lobby_icon",
            Self::ChatAttachment => "chat_attachment",
        }
    }
}

/// A path under the root: relative, forward slashes, never `..` or absolute. Only `BlobStore`
/// builds one from parts; `parse` checks one read back from the database.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StoragePath(String);

impl StoragePath {
    fn shard(id: &BlobId) -> &str {
        let start = BlobId::PREFIX.len() + 1;
        &id.as_str()[start..start + 2]
    }

    fn blob(kind: BlobKind, id: &BlobId) -> Self {
        Self(format!("{}/{}/{id}", kind.as_str(), Self::shard(id)))
    }

    fn preview(id: &BlobId) -> Self {
        Self(format!("{PREVIEW_DIR}/{}/{id}.jpg", Self::shard(id)))
    }

    pub fn parse(s: &str) -> Result<Self, BlobError> {
        let ok = !s.is_empty()
            && s.split('/')
                .all(|c| !c.is_empty() && c != "." && c != ".." && !c.contains(['\\', ':', '\0']));
        if ok {
            Ok(Self(s.to_owned()))
        } else {
            Err(BlobError::BadPath(s.to_owned()))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug)]
pub struct StoredBlob {
    pub id: BlobId,
    pub kind: BlobKind,
    pub path: StoragePath,
    pub mime: Sniffed,
    pub size: u64,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct StoredPreview {
    pub path: StoragePath,
    pub size: Preview,
}

impl StoredBlob {
    pub fn files(&self, preview: Option<&StoredPreview>) -> BlobFiles {
        BlobFiles {
            blob: self.path.clone(),
            preview: preview.map(|p| p.path.clone()),
        }
    }
}

/// Files to delete once their rows are gone (expired, failed insert, replaced avatar).
#[derive(Debug, Clone)]
pub struct BlobFiles {
    pub blob: StoragePath,
    pub preview: Option<StoragePath>,
}

#[derive(Debug, thiserror::Error)]
pub enum BlobError {
    #[error("File exceeds the upload size limit")]
    TooLarge,
    #[error("Request body is required")]
    Empty,
    #[error("Executable files are not allowed")]
    Executable,
    #[error("Unsupported file type")]
    Unsupported,
    #[error("Invalid image file")]
    InvalidImage,
    #[error(transparent)]
    Busy(#[from] CpuBusy),
    /// The body stream failed before its end: most often the client cancelled the upload.
    #[error("upload body ended early: {0}")]
    Aborted(BoxError),
    #[error("blob path is not under the root: {0}")]
    BadPath(String),
    #[error("blob io: {0}")]
    Io(#[from] io::Error),
}

#[derive(Clone)]
pub struct BlobStore {
    root: Arc<PathBuf>,
    max_bytes: u64,
    /// The services tier, where [`Self::discard`] deletes.
    cleanup: TaskTracker,
    /// The deletes not done yet. Closed from the start, so waiting on it waits for them alone.
    discarding: TaskTracker,
}

impl BlobStore {
    pub fn new(cfg: &StorageConfig, cleanup: TaskTracker) -> io::Result<Self> {
        std::fs::create_dir_all(&cfg.blob_root)?;
        let discarding = TaskTracker::new();
        discarding.close();
        Ok(Self {
            root: Arc::new(cfg.blob_root.clone()),
            max_bytes: cfg.upload_max_bytes,
            cleanup,
            discarding,
        })
    }

    pub fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    pub fn resolve(&self, path: &StoragePath) -> PathBuf {
        path.as_str()
            .split('/')
            .fold(self.root.as_ref().clone(), |p, c| p.join(c))
    }

    /// Streams `body` into a `.tmp-*` file in the final shard directory (the same filesystem, so
    /// the last step is a rename), counting bytes: one past `max_bytes` fails at once and the
    /// temp file is deleted on drop. The first 512 bytes are sniffed as soon as they are in, so
    /// an executable is refused before the rest of the body is read.
    pub async fn save<S, E>(
        &self,
        kind: BlobKind,
        raw_name: Option<&str>,
        mut body: S,
    ) -> Result<StoredBlob, BlobError>
    where
        S: Stream<Item = Result<Bytes, E>> + Unpin,
        E: Into<BoxError>,
    {
        let name = name::sanitize(raw_name.unwrap_or_default());
        let id = BlobId::random();
        let path = StoragePath::blob(kind, &id);
        let abs = self.resolve(&path);
        let dir = abs.parent().expect("a blob path has a shard directory");
        tokio::fs::create_dir_all(dir).await?;
        let (file, temp) = tempfile::Builder::new()
            .prefix(TEMP_PREFIX)
            .tempfile_in(dir)?
            .into_parts();
        let mut file = tokio::fs::File::from_std(file);

        let mut head = Vec::with_capacity(SNIFF_LEN);
        let mut mime = None;
        let mut written = 0u64;
        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(|e| stream_error(e.into()))?;
            written += chunk.len() as u64;
            if written > self.max_bytes {
                return Err(BlobError::TooLarge);
            }
            if mime.is_none() {
                let room = SNIFF_LEN - head.len();
                head.extend_from_slice(&chunk[..chunk.len().min(room)]);
                if head.len() == SNIFF_LEN {
                    mime = Some(sniff::classify(&head, kind)?);
                }
            }
            file.write_all(&chunk).await?;
        }
        if written == 0 {
            return Err(BlobError::Empty);
        }
        let mime = match mime {
            Some(m) => m,
            None => sniff::classify(&head, kind)?,
        };
        file.flush().await?;
        file.sync_all().await?;
        drop(file.into_std().await);
        temp.persist_noclobber(&abs).map_err(|e| e.error)?;
        Ok(StoredBlob {
            id,
            kind,
            path,
            mime,
            size: written,
            name,
        })
    }

    /// An avatar or Lobby icon: the square (`image::square`) of an upload stored (and sniffed)
    /// like any other, under a new id. The upload's own file stays for the caller to delete.
    pub async fn square(
        &self,
        cpu: &Arc<Semaphore>,
        upload: &StoredBlob,
    ) -> Result<StoredBlob, BlobError> {
        let kind = upload.kind;
        let abs = self.resolve(&upload.path);
        // Read under the permit, so waiting uploads never hold their files in memory.
        let read = move || image::square(io::BufReader::new(std::fs::File::open(abs)?));
        let (bytes, mime) = match on_cpu(cpu, read).await? {
            Ok(made) => made,
            Err(image::PreviewError::Read(e)) => return Err(e.into()),
            Err(e) => {
                debug!(error = %e, "not an image the decoder reads");
                return Err(BlobError::InvalidImage);
            }
        };
        let id = BlobId::random();
        let path = StoragePath::blob(kind, &id);
        let size = bytes.len() as u64;
        self.write_file(&path, bytes).await?;
        Ok(StoredBlob {
            id,
            kind,
            path,
            mime,
            size,
            name: upload.name.clone(),
        })
    }

    /// Chat previews, for `image/*` only. Every failure (decode, limits, `CpuBusy`, io) is
    /// `None`: the upload still succeeds, without a preview.
    pub async fn make_preview(
        &self,
        cpu: &Arc<Semaphore>,
        blob: &StoredBlob,
    ) -> Option<StoredPreview> {
        if !blob.mime.is_image() {
            return None;
        }
        let abs = self.resolve(&blob.path);
        // Read under the permit, so waiting uploads never hold their files in memory.
        let read = move || image::chat_preview(io::BufReader::new(std::fs::File::open(abs)?));
        let (jpeg, size) = match on_cpu(cpu, read).await {
            Ok(Ok(made)) => made,
            Ok(Err(e)) => {
                debug!(error = %e, "no preview: the image was not read or did not decode");
                return None;
            }
            Err(e) => {
                warn!(error = %e, "no preview: the CPU is busy");
                return None;
            }
        };
        let path = StoragePath::preview(&blob.id);
        match self.write_file(&path, jpeg).await {
            Ok(()) => Some(StoredPreview { path, size }),
            Err(e) => {
                warn!(error = %e, "no preview: it could not be written");
                None
            }
        }
    }

    async fn write_file(&self, path: &StoragePath, bytes: Vec<u8>) -> io::Result<()> {
        let abs = self.resolve(path);
        tokio::task::spawn_blocking(move || {
            use std::io::Write as _;
            let dir = abs.parent().expect("a blob path has a shard directory");
            std::fs::create_dir_all(dir)?;
            let mut file = tempfile::Builder::new()
                .prefix(TEMP_PREFIX)
                .tempfile_in(dir)?;
            file.write_all(&bytes)?;
            file.as_file().sync_all()?;
            file.persist_noclobber(&abs).map_err(|e| e.error)?;
            Ok(())
        })
        .await
        .map_err(io::Error::other)?
    }

    /// Best effort and idempotent: a missing file is fine, a failure is logged, and the orphan
    /// sweep tries again.
    pub async fn remove(&self, files: &BlobFiles) {
        for path in std::iter::once(&files.blob).chain(files.preview.as_ref()) {
            remove_file(&self.resolve(path)).await;
        }
    }

    /// [`Self::remove`] later, on the services tier: a `Drop` or the writer task does not wait
    /// for the disk.
    pub fn discard(&self, files: BlobFiles) {
        let store = self.clone();
        let removal = async move { store.remove(&files).await };
        self.cleanup.spawn(self.discarding.track_future(removal));
    }

    /// Waits until every [`Self::discard`] so far is done.
    #[cfg(feature = "test-support")]
    pub async fn discarded(&self) {
        self.discarding.wait().await;
    }

    pub async fn remove_rel(&self, rel: &str) {
        match StoragePath::parse(rel) {
            Ok(path) => remove_file(&self.resolve(&path)).await,
            Err(e) => warn!(error = %e, "orphan candidate skipped"),
        }
    }

    /// Every file the store could have written (a blob, a preview or a `.tmp-*`) last modified
    /// before `cutoff`, as root-relative forward-slash strings. The layout is three fixed levels:
    /// one of [`STORE_DIRS`], shard, file; anything else under the root is not ours to delete.
    pub async fn files_older_than(&self, cutoff: SystemTime) -> io::Result<Vec<String>> {
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || {
            let mut found = Vec::new();
            for (dir, suffix) in STORE_DIRS {
                for shard in subdirs(&root.join(dir))? {
                    let Some(shard_name) = shard.file_name().and_then(|n| n.to_str()) else {
                        continue;
                    };
                    for entry in std::fs::read_dir(&shard)? {
                        let entry = entry?;
                        let name = entry.file_name();
                        let Some(name) = name.to_str().filter(|n| is_store_file(n, suffix)) else {
                            continue;
                        };
                        let meta = entry.metadata()?;
                        if meta.is_file() && meta.modified()? < cutoff {
                            found.push(format!("{dir}/{shard_name}/{name}"));
                        }
                    }
                }
            }
            Ok(found)
        })
        .await
        .map_err(io::Error::other)?
    }
}

/// The route's `RequestBodyLimitLayer` ends a chunked body that is too large with an error
/// instead of a chunk; its type belongs to a crate we do not depend on, so it is recognised by
/// its message (the streamed-413 test fails if that text ever changes). Any other body error is
/// the client going away mid-upload.
fn stream_error(e: BoxError) -> BlobError {
    let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(&*e);
    while let Some(c) = cause {
        if c.to_string() == "length limit exceeded" {
            return BlobError::TooLarge;
        }
        cause = c.source();
    }
    BlobError::Aborted(e)
}

fn is_store_file(name: &str, suffix: &str) -> bool {
    name.starts_with(TEMP_PREFIX)
        || name
            .strip_suffix(suffix)
            .is_some_and(|id| id.parse::<BlobId>().is_ok())
}

fn subdirs(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut dirs = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(dirs),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            dirs.push(entry.path());
        }
    }
    Ok(dirs)
}

async fn remove_file(path: &Path) {
    match tokio::fs::remove_file(path).await {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => warn!(path = %path.display(), error = %e, "blob file not deleted"),
    }
}

#[cfg(test)]
mod tests {

    use futures_util::stream;

    use super::*;

    fn store(max: u64) -> (tempfile::TempDir, BlobStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(
            &StorageConfig {
                blob_root: dir.path().join("blobs"),
                upload_max_bytes: max,
            },
            TaskTracker::new(),
        )
        .unwrap();
        (dir, store)
    }

    fn files_under(root: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for e in std::fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    out.push(p);
                }
            }
        }
        out
    }

    #[test]
    fn storage_paths_reject_anything_outside_the_root() {
        for good in [
            "chat_attachment/ab/blb_abcdefabcdefabcdefabcdef",
            "chat_attachment_preview/ab/blb_x.jpg",
        ] {
            assert_eq!(StoragePath::parse(good).unwrap().as_str(), good);
        }
        for bad in [
            "",
            "/etc/passwd",
            "\\windows",
            "../x",
            "a/../../x",
            "a/./b",
            "a//b",
            "C:/x",
            "a\\b",
            "a/b/",
            "x\0y",
        ] {
            assert!(
                matches!(StoragePath::parse(bad), Err(BlobError::BadPath(_))),
                "{bad:?}"
            );
        }
        let id: BlobId = "blb_2582c529769d2a353bd49743".parse().unwrap();
        assert_eq!(
            StoragePath::blob(BlobKind::ChatAttachment, &id).as_str(),
            "chat_attachment/25/blb_2582c529769d2a353bd49743"
        );
        assert_eq!(
            StoragePath::preview(&id).as_str(),
            "chat_attachment_preview/25/blb_2582c529769d2a353bd49743.jpg"
        );
    }

    #[tokio::test]
    async fn an_executable_is_refused_before_the_rest_of_the_body_is_read() {
        let (_dir, store) = store(1 << 20);
        let mut chunks = vec![b"MZ".to_vec(), vec![0; 600]];
        chunks.push(vec![1; 10]);
        let mut pulled = 0;
        let counting = stream::iter(
            chunks
                .into_iter()
                .map(|c| Ok::<_, io::Error>(Bytes::from(c))),
        )
        .inspect(|_| pulled += 1);
        let refused = store
            .save(BlobKind::ChatAttachment, Some("a.png"), Box::pin(counting))
            .await;
        assert!(matches!(refused, Err(BlobError::Executable)));
        assert_eq!(
            pulled, 2,
            "stopped at the chunk that completed the sniff window"
        );
        assert!(files_under(&store.root).is_empty());
    }
}
