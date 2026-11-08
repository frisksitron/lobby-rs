//! Draft attachments: the composer's files on their way to becoming `Attachment`s.
//! They live in `LobbyState`, so Send's rules, the upload cap, failed-send restore and nonce
//! reuse are decided in one pure place and run headless. No IO and no clock, apart from
//! [`DraftSource::from_path`]'s one `stat`.

use std::{
    fmt,
    io::Cursor,
    path::{Path, PathBuf},
    sync::Arc,
};

use lobby_protocol::{
    Attachment, BlobId,
    limits::{self, IMAGE_MAX_ALLOC, IMAGE_MAX_EDGE},
};

use crate::{ApiError, copy};

const UPLOADS_CONCURRENT: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DraftId(u64);

impl fmt::Display for DraftId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UploadKey {
    pub draft: DraftId,
    pub attempt: u32,
}

/// Where the bytes come from. Kept for the draft's life, so Retry (and the re-upload after
/// `ATTACHMENT_INVALID`) never asks the person again.
#[derive(Clone, PartialEq, Eq)]
pub enum DraftSource {
    /// A picked or dropped file. `size` is read once, when attached (the pre-check).
    File {
        path: PathBuf,
        size: u64,
    },
    Bytes {
        name: String,
        bytes: Arc<[u8]>,
    },
}

impl fmt::Debug for DraftSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File { path, size } => write!(f, "File({path:?}, {size} bytes)"),
            Self::Bytes { name, bytes } => write!(f, "Bytes({name:?}, {} bytes)", bytes.len()),
        }
    }
}

impl DraftSource {
    /// One synchronous `stat`: a directory or an unreadable path is refused here, before any
    /// entry exists.
    pub fn from_path(path: &Path) -> Result<DraftSource, AttachError> {
        let meta = std::fs::metadata(path).map_err(|_| AttachError::Unreadable)?;
        if !meta.is_file() {
            return Err(AttachError::NotAFile);
        }
        Ok(DraftSource::File {
            path: path.to_owned(),
            size: meta.len(),
        })
    }

    pub fn size(&self) -> u64 {
        match self {
            Self::File { size, .. } => *size,
            Self::Bytes { bytes, .. } => bytes.len() as u64,
        }
    }

    pub fn name(&self) -> String {
        match self {
            Self::File { path, .. } => path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "upload.bin".to_owned()),
            Self::Bytes { name, .. } => name.clone(),
        }
    }
}

/// The clipboard's image encodings (Windows hands a CF_DIB as BMP).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PastedFormat {
    Png,
    Bmp,
    Jpeg,
    Gif,
    Webp,
}

/// A pasted bitmap as a draft: anything but PNG is re-encoded to PNG in memory, and it is never
/// written to a temp file. CPU work; the desktop calls it off the UI thread.
pub fn paste_png(bytes: &[u8], format: PastedFormat) -> Result<DraftSource, AttachError> {
    let name = "pasted-image.png".to_owned();
    let format = match format {
        PastedFormat::Png => {
            return Ok(DraftSource::Bytes {
                name,
                bytes: Arc::from(bytes),
            });
        }
        PastedFormat::Bmp => image::ImageFormat::Bmp,
        PastedFormat::Jpeg => image::ImageFormat::Jpeg,
        PastedFormat::Gif => image::ImageFormat::Gif,
        PastedFormat::Webp => image::ImageFormat::WebP,
    };
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(image_limits());
    let decoded = reader.decode().map_err(|_| AttachError::Unreadable)?;
    let mut out = Cursor::new(Vec::new());
    decoded
        .write_to(&mut out, image::ImageFormat::Png)
        .map_err(|_| AttachError::Unreadable)?;
    Ok(DraftSource::Bytes {
        name,
        bytes: Arc::from(out.into_inner()),
    })
}

pub(crate) fn image_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(IMAGE_MAX_EDGE);
    limits.max_image_height = Some(IMAGE_MAX_EDGE);
    limits.max_alloc = Some(IMAGE_MAX_ALLOC);
    limits
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DraftState {
    Queued,
    Uploading { attempt: u32, sent: u64 },
    Ready(Attachment),
    Failed { reason: String, retryable: bool },
}

impl DraftState {
    pub fn is_ready(&self) -> bool {
        matches!(self, DraftState::Ready(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftAttachment {
    pub id: DraftId,
    pub source: DraftSource,
    pub name: String,
    pub size: u64,
    pub state: DraftState,
}

impl DraftAttachment {
    pub fn attachment(&self) -> Option<&Attachment> {
        match &self.state {
            DraftState::Ready(a) => Some(a),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendBlock {
    /// Blank text and no files.
    Empty,
    Uploading,
    Failed,
    TooMany,
}

/// The composer's attachments, in the order added.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Drafts {
    items: Vec<DraftAttachment>,
    next_id: u64,
    attempts: u32,
}

impl Drafts {
    pub fn items(&self) -> &[DraftAttachment] {
        &self.items
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Derived, never stored: anything Queued or Uploading blocks first, then anything Failed.
    /// Whether a Send is allowed is [`LobbyState::send_block`](crate::LobbyState::send_block).
    pub(crate) fn send_block(&self) -> Option<SendBlock> {
        if self
            .items
            .iter()
            .any(|d| matches!(d.state, DraftState::Queued | DraftState::Uploading { .. }))
        {
            Some(SendBlock::Uploading)
        } else if self
            .items
            .iter()
            .any(|d| matches!(d.state, DraftState::Failed { .. }))
        {
            Some(SendBlock::Failed)
        } else if self.items.len() > limits::ATTACHMENTS_MAX {
            Some(SendBlock::TooMany)
        } else {
            None
        }
    }

    pub fn ready_ids(&self) -> Vec<BlobId> {
        blob_ids(&self.items)
    }

    pub(crate) fn add(
        &mut self,
        source: DraftSource,
        max_bytes: u64,
    ) -> Result<DraftId, AttachError> {
        if source.size() > max_bytes {
            return Err(AttachError::TooLarge);
        }
        if self.items.len() >= limits::ATTACHMENTS_MAX {
            return Err(AttachError::TooMany);
        }
        self.next_id += 1;
        let id = DraftId(self.next_id);
        self.items.push(DraftAttachment {
            id,
            name: source.name(),
            size: source.size(),
            source,
            state: DraftState::Queued,
        });
        Ok(id)
    }

    pub(crate) fn start_next(&mut self) -> Vec<(UploadKey, DraftSource)> {
        let uploading = self
            .items
            .iter()
            .filter(|d| matches!(d.state, DraftState::Uploading { .. }))
            .count();
        let mut started = Vec::new();
        for d in self
            .items
            .iter_mut()
            .filter(|d| d.state == DraftState::Queued)
            .take(UPLOADS_CONCURRENT.saturating_sub(uploading))
        {
            self.attempts += 1;
            let key = UploadKey {
                draft: d.id,
                attempt: self.attempts,
            };
            d.state = DraftState::Uploading {
                attempt: key.attempt,
                sent: 0,
            };
            started.push((key, d.source.clone()));
        }
        started
    }

    fn uploading(&mut self, key: UploadKey) -> Option<&mut DraftAttachment> {
        self.items.iter_mut().find(|d| {
            d.id == key.draft && matches!(d.state, DraftState::Uploading { attempt, .. } if attempt == key.attempt)
        })
    }

    pub(crate) fn progress(&mut self, key: UploadKey, sent: u64) -> bool {
        let Some(d) = self.uploading(key) else {
            return false;
        };
        d.state = DraftState::Uploading {
            attempt: key.attempt,
            sent,
        };
        true
    }

    pub(crate) fn finish(
        &mut self,
        key: UploadKey,
        result: Result<Attachment, UploadError>,
    ) -> bool {
        let Some(d) = self.uploading(key) else {
            return false;
        };
        d.state = match result {
            Ok(attachment) => DraftState::Ready(attachment),
            Err(e) => DraftState::Failed {
                reason: e.to_string(),
                retryable: e.is_retryable(),
            },
        };
        true
    }

    pub(crate) fn retry(&mut self, id: DraftId) -> bool {
        let Some(d) = self
            .items
            .iter_mut()
            .find(|d| d.id == id && matches!(d.state, DraftState::Failed { .. }))
        else {
            return false;
        };
        d.state = DraftState::Queued;
        true
    }

    pub(crate) fn remove(&mut self, id: DraftId) -> Option<UploadKey> {
        let i = self.items.iter().position(|d| d.id == id)?;
        let d = self.items.remove(i);
        match d.state {
            DraftState::Uploading { attempt, .. } => Some(UploadKey { draft: id, attempt }),
            _ => None,
        }
    }

    pub(crate) fn take_ready(&mut self) -> Vec<DraftAttachment> {
        let (ready, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.items)
            .into_iter()
            .partition(|d| d.state.is_ready());
        self.items = rest;
        ready
    }

    /// A failed send gives its attachments back, in front of anything added since. `invalid`:
    /// the Lobby said `ATTACHMENT_INVALID`, so they come back Failed and Retry re-uploads from
    /// the kept source.
    pub(crate) fn restore(&mut self, mut restored: Vec<DraftAttachment>, invalid: bool) {
        if invalid {
            for d in &mut restored {
                d.state = DraftState::Failed {
                    reason: copy::ATTACHMENT_GONE.to_owned(),
                    retryable: true,
                };
            }
        }
        restored.append(&mut self.items);
        self.items = restored;
    }

    pub(crate) fn withdraw(&mut self, stored: &[Attachment]) {
        self.items.retain(|d| {
            !d.attachment()
                .is_some_and(|a| stored.iter().any(|s| s.id == a.id))
        });
    }
}

pub(crate) fn blob_ids(drafts: &[DraftAttachment]) -> Vec<BlobId> {
    drafts
        .iter()
        .filter_map(|d| d.attachment().map(|a| a.id.clone()))
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AttachError {
    #[error("{}", copy::PAYLOAD_TOO_LARGE)]
    TooLarge,
    #[error("{}", copy::TOO_MANY_ATTACHMENTS)]
    TooMany,
    #[error("{}", copy::NOT_A_FILE)]
    NotAFile,
    #[error("{}", copy::FILE_UNREADABLE)]
    Unreadable,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UploadError {
    #[error("{}", copy::PAYLOAD_TOO_LARGE)]
    TooLarge,
    #[error("{0}")]
    Api(ApiError),
    #[error("{}", copy::FILE_UNREADABLE)]
    Io(String),
}

impl UploadError {
    /// Retry is offered only when the same bytes could go through next time.
    pub fn is_retryable(&self) -> bool {
        match self {
            UploadError::Api(e) => e.is_retryable(),
            UploadError::TooLarge | UploadError::Io(_) => false,
        }
    }
}
