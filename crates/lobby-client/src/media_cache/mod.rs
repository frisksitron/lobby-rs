//! Blob bytes for one Lobby origin: on disk, content-addressed and never
//! revalidated (blobs are immutable); one fetch per (blob, variant) in flight, at most four at
//! once; a 429 waits its Retry-After and tries that blob once more. Decoding hands back owned
//! pixels: the in-memory LRU of what is on screen lives with the GPU images in the desktop
//! (built on this module's [`Lru`]). `media` (without `_cache`) is Voice.

pub mod lru;

use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

use futures_util::{
    FutureExt as _, StreamExt as _,
    future::{BoxFuture, Shared},
};
use image::ImageDecoder as _;
use lobby_protocol::{Attachment, BlobId, routes};
pub use lru::Lru;
use reqwest::StatusCode;
use tokio::{
    io::AsyncWriteExt as _,
    runtime::Handle,
    sync::{Semaphore, oneshot},
    task::JoinSet,
};
use tracing::{debug, info, warn};

use crate::{
    ApiError, ClientTask, LobbyOrigin, Paths, api::read_body, atomic, copy, drafts, files,
};

pub const FETCHES_CONCURRENT: usize = 4;
/// Decodes at once (each holds a full-size RGBA buffer).
pub const DECODES_CONCURRENT: usize = 2;
pub const DISK_BUDGET_BYTES: u64 = 512 * 1024 * 1024;
pub const DECODED_BUDGET_BYTES: usize = 128 * 1024 * 1024;
pub const DISK_EVICT_EVERY: Duration = Duration::from_secs(3600);
/// A full image is shown in the preview dialog, never larger than this on its long edge.
pub const FULL_MAX_EDGE: u32 = 2048;
/// The most a preview, an avatar or a Lobby icon may weigh. The server makes them at most 480 px
/// and 256 px wide, a few hundred KiB.
const SMALL_IMAGE_MAX_BYTES: u64 = 2 * 1024 * 1024;
/// The most the server lets one upload weigh (it refuses a `storage.upload_max_bytes` above
/// this), so no honest blob is larger, whatever its `Attachment::size` claims.
const UPLOAD_MAX_BYTES: u64 = 1024 * 1024 * 1024;
pub const MEDIA_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
/// A Retry-After beyond this is not waited out: the blob fails, and the next render asks again.
pub const RETRY_AFTER_MAX: Duration = Duration::from_secs(60);
/// A `.tmp-` file this old belongs to no running fetch and is swept.
const TMP_MAX_AGE: Duration = Duration::from_secs(3600);

/// An attachment is shown as a picture with a reserved box only when it is an image the server
/// could preview. Its size is known before any byte arrives, so no layout can shift; an image
/// without a preview is a file card.
pub fn picture(attachment: &Attachment) -> Option<lobby_protocol::Preview> {
    attachment
        .mime_type
        .starts_with("image/")
        .then_some(attachment.preview)
        .flatten()
}

/// What is fetched of a blob, which also decides the most bytes the fetch accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Variant {
    /// The server's small picture of an attachment.
    Preview,
    /// A Member's avatar or the Lobby's icon, stored at its small size.
    Square,
    /// The blob itself, of the size its `Attachment` says.
    Full { size: u64 },
}

impl Variant {
    /// A body past this is refused. A Full blob is held to the size it was announced with, and
    /// to what the server lets any upload weigh.
    fn max_bytes(self) -> u64 {
        match self {
            Variant::Preview | Variant::Square => SMALL_IMAGE_MAX_BYTES,
            Variant::Full { size } => size.min(UPLOAD_MAX_BYTES),
        }
    }
}

/// BGRA8, the byte order GPUI's own image loader produces (RGBA with the first and third
/// channel swapped). Owned, so the desktop moves it into a `RenderImage` frame without a copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

impl DecodedImage {
    pub fn bytes(&self) -> usize {
        self.bgra.len()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MediaCacheError {
    #[error("{}", copy::MEDIA_NOT_FOUND)]
    NotFound,
    #[error("{}", copy::MEDIA_RATE_LIMITED)]
    RateLimited,
    /// More bytes came than the caller said the blob weighs.
    #[error("{}", copy::MEDIA_TOO_LARGE)]
    TooLarge,
    #[error("{0}")]
    Api(ApiError),
    #[error("{}", copy::IMAGE_UNREADABLE)]
    Decode,
    /// Open refused by the allowlist: the check lives here, not only behind a button.
    #[error("{}", copy::OPEN_NOT_ALLOWED)]
    NotAllowed,
    #[error("{0}")]
    Io(String),
}

impl From<io::Error> for MediaCacheError {
    fn from(e: io::Error) -> Self {
        MediaCacheError::Io(e.to_string())
    }
}

type FetchResult = Result<PathBuf, MediaCacheError>;
type SharedFetch = Shared<BoxFuture<'static, FetchResult>>;

/// One per origin, from `Client::media`. Cheap to clone; dropping the last clone aborts its
/// fetches and the eviction timer (they are tasks owned here).
#[derive(Clone)]
pub struct MediaCache {
    inner: Arc<Inner>,
}

struct Inner {
    rt: Handle,
    ctx: Arc<Ctx>,
    decode_permits: Arc<Semaphore>,
    /// The fetch tasks and the hourly disk eviction. A caller dropping its `ClientTask` never
    /// cancels a fetch another caller shares; dropping the cache aborts everything.
    tasks: Mutex<JoinSet<()>>,
}

/// What a fetch task holds. It carries no task handles, so a running task never keeps the
/// cache (and so itself) alive.
struct Ctx {
    http: reqwest::Client,
    origin: LobbyOrigin,
    dir: PathBuf,
    open_dir: PathBuf,
    fetch_permits: Semaphore,
    inflight: Mutex<HashMap<(BlobId, Variant), SharedFetch>>,
    #[cfg(feature = "test-support")]
    fetched: Mutex<HashMap<(BlobId, Variant), usize>>,
}

impl Ctx {
    fn path(&self, blob: &BlobId, variant: Variant) -> PathBuf {
        match variant {
            Variant::Full { .. } | Variant::Square => self.dir.join(blob.as_str()),
            Variant::Preview => self.dir.join(format!("{}.preview", blob.as_str())),
        }
    }

    #[cfg(feature = "test-support")]
    fn count_fetch(&self, blob: &BlobId, variant: Variant) {
        *self
            .fetched
            .lock()
            .expect("fetch counts")
            .entry((blob.clone(), variant))
            .or_default() += 1;
    }

    #[cfg(not(feature = "test-support"))]
    fn count_fetch(&self, _blob: &BlobId, _variant: Variant) {}

    fn url(&self, blob: &BlobId, variant: Variant) -> String {
        match variant {
            Variant::Full { .. } | Variant::Square => self.origin.http_url(&routes::media(blob)),
            Variant::Preview => self.origin.http_url(&routes::media_preview(blob)),
        }
    }
}

impl MediaCache {
    pub(crate) fn new(
        rt: Handle,
        http: reqwest::Client,
        origin: LobbyOrigin,
        paths: &Paths,
    ) -> MediaCache {
        let ctx = Arc::new(Ctx {
            http,
            dir: paths.media_cache_dir(&origin),
            open_dir: paths.open_dir(),
            origin,
            fetch_permits: Semaphore::new(FETCHES_CONCURRENT),
            inflight: Mutex::new(HashMap::new()),
            #[cfg(feature = "test-support")]
            fetched: Mutex::new(HashMap::new()),
        });
        let mut tasks = JoinSet::new();
        let dir = ctx.dir.clone();
        tasks.spawn_on(
            async move {
                loop {
                    let d = dir.clone();
                    match tokio::task::spawn_blocking(move || evict_disk(&d, DISK_BUDGET_BYTES))
                        .await
                    {
                        Ok(Ok(0)) => {}
                        Ok(Ok(removed)) => info!(removed, "media cache evicted"),
                        Ok(Err(e)) => debug!(error = %e, "media cache eviction skipped"),
                        Err(_) => {}
                    }
                    tokio::time::sleep(DISK_EVICT_EVERY).await;
                }
            },
            &rt,
        );
        MediaCache {
            inner: Arc::new(Inner {
                rt,
                ctx,
                decode_permits: Arc::new(Semaphore::new(DECODES_CONCURRENT)),
                tasks: Mutex::new(tasks),
            }),
        }
    }

    /// The blob's bytes on disk. A disk hit makes no request (and counts as a use for the disk
    /// LRU); a miss joins the fetch already running for it or starts the one fetch, which stops
    /// with `TooLarge` at [`Variant::max_bytes`].
    pub fn file(&self, blob: &BlobId, variant: Variant) -> ClientTask<FetchResult> {
        let inner = self.inner.clone();
        let blob = blob.clone();
        ClientTask::spawn(
            &self.inner.rt,
            async move { inner.file(blob, variant).await },
        )
    }

    /// The blob decoded, fetched as [`MediaCache::file`] does.
    pub fn image(
        &self,
        blob: &BlobId,
        variant: Variant,
    ) -> ClientTask<Result<DecodedImage, MediaCacheError>> {
        let inner = self.inner.clone();
        let blob = blob.clone();
        ClientTask::spawn(&self.inner.rt, async move {
            let path = inner.file(blob, variant).await?;
            let permit = inner.decode_permits.clone().acquire_owned().await;
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                decode(&path, variant)
            })
            .await
            .map_err(|_| MediaCacheError::Decode)?
        })
    }

    pub fn save_as(&self, attachment: &Attachment, dest: PathBuf) -> ClientTask<FetchResult> {
        let inner = self.inner.clone();
        let blob = attachment.id.clone();
        let full = Variant::Full {
            size: attachment.size,
        };
        ClientTask::spawn(&self.inner.rt, async move {
            let src = inner.file(blob, full).await?;
            copy_into(&src, &dest).await?;
            Ok(dest)
        })
    }

    /// Open: only when [`files::open_allowed`] says so (the policy is checked here, not only in
    /// the dialog). The blob is copied to `<open_dir>/<blob>/<sanitized name>` with the
    /// Mark-of-the-Web, and that path is what the OS shell opens. A copy already there with the
    /// blob's size is opened again as it is: a viewer may still hold it, and Windows refuses to
    /// replace an open file.
    pub fn open_copy(&self, attachment: &Attachment) -> ClientTask<FetchResult> {
        if !files::open_allowed(&attachment.name, &attachment.mime_type) {
            return ClientTask::ready(Err(MediaCacheError::NotAllowed));
        }
        let dest = self
            .inner
            .ctx
            .open_dir
            .join(attachment.id.as_str())
            .join(files::sanitize_file_name(&attachment.name));
        let inner = self.inner.clone();
        let blob = attachment.id.clone();
        let size = attachment.size;
        ClientTask::spawn(&self.inner.rt, async move {
            if tokio::fs::metadata(&dest)
                .await
                .is_ok_and(|m| m.len() == size)
            {
                return Ok(dest);
            }
            let src = inner.file(blob, Variant::Full { size }).await?;
            copy_into(&src, &dest).await?;
            Ok(dest)
        })
    }

    #[cfg(feature = "test-support")]
    pub fn fetch_counts(&self) -> HashMap<(BlobId, Variant), usize> {
        self.inner.ctx.fetched.lock().expect("fetch counts").clone()
    }
}

impl Inner {
    async fn file(&self, blob: BlobId, variant: Variant) -> FetchResult {
        let path = self.ctx.path(&blob, variant);
        if tokio::fs::metadata(&path).await.is_ok() {
            touch(path.clone()).await;
            return Ok(path);
        }
        self.join_or_start(blob, variant, path).await
    }

    fn join_or_start(&self, blob: BlobId, variant: Variant, path: PathBuf) -> SharedFetch {
        let key = (blob, variant);
        let mut inflight = self.ctx.inflight.lock().expect("inflight");
        if let Some(shared) = inflight.get(&key) {
            return shared.clone();
        }
        let (tx, rx) = oneshot::channel();
        let ctx = self.ctx.clone();
        let task_key = key.clone();
        {
            let mut tasks = self.tasks.lock().expect("tasks");
            while tasks.try_join_next().is_some() {}
            tasks.spawn_on(
                async move {
                    let result = fetch(&ctx, &task_key.0, task_key.1, &path).await;
                    ctx.inflight.lock().expect("inflight").remove(&task_key);
                    let _ = tx.send(result);
                },
                &self.rt,
            );
        }
        let shared = rx
            .map(|r| {
                r.unwrap_or_else(|_| Err(MediaCacheError::Io("the fetch was cancelled".into())))
            })
            .boxed()
            .shared();
        inflight.insert(key, shared.clone());
        shared
    }
}

/// `GET {origin}/media/{id}[/preview]` under a fetch permit, streamed into a temp file in the
/// cache directory and renamed into place. A 429 releases the permit, waits its Retry-After
/// (from `retryAfterMs`, else the header, else 1 s) and tries once more.
async fn fetch(ctx: &Ctx, blob: &BlobId, variant: Variant, path: &Path) -> FetchResult {
    let url = ctx.url(blob, variant);
    let mut waited = false;
    loop {
        let permit = ctx.fetch_permits.acquire().await;
        ctx.count_fetch(blob, variant);
        debug!(%blob, ?variant, "fetching");
        let res = ctx
            .http
            .get(&url)
            .timeout(MEDIA_REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|e| MediaCacheError::Api(ApiError::from_reqwest(&e)))?;
        let status = res.status();
        match status {
            StatusCode::OK => {
                write_through_tmp(res, variant.max_bytes(), path).await?;
                return Ok(path.to_owned());
            }
            StatusCode::NOT_FOUND => return Err(MediaCacheError::NotFound),
            StatusCode::TOO_MANY_REQUESTS => {
                let wait = retry_after(res).await;
                if waited || wait > RETRY_AFTER_MAX {
                    warn!(%blob, ?wait, "media rate limited");
                    return Err(MediaCacheError::RateLimited);
                }
                drop(permit);
                debug!(%blob, ?wait, "media rate limited: waiting once");
                waited = true;
                tokio::time::sleep(wait).await;
            }
            other => {
                let body = read_body(res).await.map_err(MediaCacheError::Api)?;
                return Err(MediaCacheError::Api(ApiError::from_error_response(
                    other, &body,
                )));
            }
        }
    }
}

async fn retry_after(res: reqwest::Response) -> Duration {
    let header = res
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(Duration::from_secs);
    let body = read_body(res).await.unwrap_or_default();
    ApiError::from_error_response(StatusCode::TOO_MANY_REQUESTS, &body)
        .retry_after
        .or(header)
        .unwrap_or(Duration::from_secs(1))
}

/// The response body to `<path>.tmp-<random>`, then renamed onto `path`, so a crash leaves a
/// temp file (swept later) and never a truncated blob. More than `max_bytes` of body is
/// `TooLarge`, whatever the response's `Content-Length`, and removes the temp file.
async fn write_through_tmp(
    res: reqwest::Response,
    max_bytes: u64,
    path: &Path,
) -> Result<(), MediaCacheError> {
    atomic::replace(path, async |tmp| {
        let mut file = tokio::fs::File::create(tmp).await?;
        let mut body = res.bytes_stream();
        let mut written = 0u64;
        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(|e| MediaCacheError::Api(ApiError::from_reqwest(&e)))?;
            written += chunk.len() as u64;
            if written > max_bytes {
                warn!(max_bytes, "media body over the size limit");
                return Err(MediaCacheError::TooLarge);
            }
            file.write_all(&chunk).await?;
        }
        file.flush().await?;
        Ok(())
    })
    .await
}

/// A copy of a cached blob at `dest` (Save…, Open): through a temp file beside `dest`, renamed
/// into place, then marked as downloaded. A file system without streams keeps the file and
/// loses the mark, which is logged.
async fn copy_into(src: &Path, dest: &Path) -> Result<(), MediaCacheError> {
    atomic::replace(dest, async |tmp| {
        tokio::fs::copy(src, tmp).await?;
        Ok::<(), MediaCacheError>(())
    })
    .await?;
    let marked = dest.to_owned();
    let marked = tokio::task::spawn_blocking(move || files::mark_downloaded(&marked)).await;
    match marked {
        Ok(Ok(())) => info!(path = %dest.display(), "saved with Mark-of-the-Web"),
        Ok(Err(e)) => warn!(path = %dest.display(), error = %e, "saved without Mark-of-the-Web"),
        Err(_) => warn!(path = %dest.display(), "saved; the Mark-of-the-Web task panicked"),
    }
    Ok(())
}

/// A disk hit is a use: the mtime moves, so the LRU keeps it.
async fn touch(path: PathBuf) {
    let touched = tokio::task::spawn_blocking(move || {
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)?
            .set_modified(SystemTime::now())
    })
    .await;
    if let Ok(Err(e)) = touched {
        debug!(error = %e, "could not touch a cached blob");
    }
}

/// Decodes a cached file with the server's limits, applies EXIF orientation, fits anything but a
/// preview into [`FULL_MAX_EDGE`] and converts to BGRA. Blocking: run it off the runtime.
fn decode(path: &Path, variant: Variant) -> Result<DecodedImage, MediaCacheError> {
    let file = io::BufReader::new(std::fs::File::open(path)?);
    let mut reader = image::ImageReader::new(file)
        .with_guessed_format()
        .map_err(|_| MediaCacheError::Decode)?;
    let mut limits = drafts::image_limits();
    reader.limits(limits.clone());
    let mut decoder = reader.into_decoder().map_err(|_| MediaCacheError::Decode)?;
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    // Not every decoder (JPEG among them) holds itself to `max_alloc`.
    limits
        .reserve(decoder.total_bytes())
        .map_err(|_| MediaCacheError::Decode)?;
    let mut img =
        image::DynamicImage::from_decoder(decoder).map_err(|_| MediaCacheError::Decode)?;
    // Scaled before it is turned, so a quarter turn copies the fitted image, not the decode.
    if variant != Variant::Preview && img.width().max(img.height()) > FULL_MAX_EDGE {
        img = img.resize(
            FULL_MAX_EDGE,
            FULL_MAX_EDGE,
            image::imageops::FilterType::CatmullRom,
        );
    }
    img.apply_orientation(orientation);
    let (width, height) = (img.width(), img.height());
    let mut bgra = img.into_rgba8().into_raw();
    for px in bgra.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    Ok(DecodedImage {
        width,
        height,
        bgra,
    })
}

/// LRU by mtime: the oldest files go until `budget` holds; a `.tmp-` file older than
/// [`TMP_MAX_AGE`] goes regardless (no fetch runs that long). Returns how many were removed.
pub(crate) fn evict_disk(dir: &Path, budget: u64) -> io::Result<usize> {
    let now = SystemTime::now();
    let mut files = Vec::new();
    let mut removed = 0;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        if !meta.is_file() {
            continue;
        }
        let modified = meta.modified().unwrap_or(now);
        let is_tmp = entry.file_name().to_string_lossy().contains(".tmp-");
        if is_tmp {
            let stale = now
                .duration_since(modified)
                .is_ok_and(|age| age > TMP_MAX_AGE);
            if stale && std::fs::remove_file(entry.path()).is_ok() {
                removed += 1;
            }
            continue;
        }
        files.push((modified, meta.len(), entry.path()));
    }
    files.sort_by_key(|(modified, _, _)| *modified);
    let mut total: u64 = files.iter().map(|(_, len, _)| len).sum();
    for (_, len, path) in files {
        if total <= budget {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total -= len;
            removed += 1;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Client, HttpPolicy, MemoryTokenStore,
        testing::{FakeBehavior, FakeLobby},
    };

    fn cache_for(fake: &FakeLobby, dir: &Path) -> MediaCache {
        let client = Client::new(
            Handle::current(),
            Arc::new(MemoryTokenStore::new()),
            HttpPolicy::LoopbackOnly,
        )
        .unwrap();
        client.media(
            &fake.origin(),
            &Paths {
                data_dir: dir.to_owned(),
            },
        )
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn save_and_open_copy_write_through_a_temp_file_with_the_mark_of_the_web() {
        let fake = FakeLobby::start(FakeBehavior::default()).await;
        let dir = tempfile::tempdir().unwrap();
        let cache = cache_for(&fake, dir.path());
        let blob = BlobId::random();
        let dest = dir.path().join("Downloads").join("x.png");
        let attachment = |name: &str, mime: &str| Attachment {
            id: blob.clone(),
            name: name.into(),
            mime_type: mime.into(),
            size: FakeLobby::media_png().len() as u64,
            preview: None,
        };
        let saved = cache
            .save_as(&attachment("x.png", "image/png"), dest.clone())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(saved, dest);
        assert_eq!(std::fs::read(&dest).unwrap(), FakeLobby::media_png());
        let downloads: Vec<String> = std::fs::read_dir(dest.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(downloads, ["x.png"], "no temp file beside it");
        #[cfg(windows)]
        {
            let mut stream = dest.as_os_str().to_owned();
            stream.push(":Zone.Identifier");
            assert_eq!(
                std::fs::read(stream).unwrap(),
                b"[ZoneTransfer]\r\nZoneId=3\r\n"
            );
        }

        let opened = cache
            .open_copy(&attachment("..\\..\\cat.png", "image/png"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            opened,
            dir.path()
                .join("cache")
                .join("open")
                .join(blob.as_str())
                .join("cat.png")
        );
        assert_eq!(std::fs::read(&opened).unwrap(), FakeLobby::media_png());
        // A viewer holding the copy open (sharing read only, as many do) does not stop a
        // second Open.
        let mut viewer = std::fs::OpenOptions::new();
        viewer.read(true);
        #[cfg(windows)]
        std::os::windows::fs::OpenOptionsExt::share_mode(&mut viewer, 1);
        let held = viewer.open(&opened).unwrap();
        let again = cache
            .open_copy(&attachment(r"..\..\cat.png", "image/png"))
            .await
            .unwrap();
        assert_eq!(again, Ok(opened.clone()));
        drop(held);
        assert_eq!(
            cache
                .open_copy(&attachment("x.bat", "text/plain"))
                .await
                .unwrap(),
            Err(MediaCacheError::NotAllowed)
        );
        assert_eq!(
            cache
                .open_copy(&attachment("doc.pdf", "application/octet-stream"))
                .await
                .unwrap(),
            Err(MediaCacheError::NotAllowed),
            "the extension and the sniffed type must agree"
        );
        assert_eq!(
            cache.fetch_counts().values().sum::<usize>(),
            1,
            "the blob was fetched once, for the save; the copies came from disk"
        );
    }
}
