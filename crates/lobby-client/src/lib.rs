//! The Lobby client without pixels. GPUI-free.
//!
//! The public API is runtime-agnostic by construction: outside [`testing`] nothing here
//! is a `pub async fn` or a reactor-bound future. Every entry point that does IO spawns on the
//! runtime handle given to [`Client::new`] and returns a [`ClientTask`] or a channel, which a
//! GPUI executor may await without a Tokio reactor.

mod api;
mod atomic;
mod client;
pub mod compose;
mod connection;
pub mod copy;
pub mod drafts;
mod entry;
pub mod files;
pub mod ink;
pub mod issues;
pub mod links;
pub mod media;
pub mod media_cache;
pub mod message_text;
mod netwatch;
mod origin;
pub mod os_toast;
mod paths;
mod persist;
mod session;
mod settings;
pub mod state;
mod task;
mod time;
mod tokens;
pub mod update;
mod volume;

#[cfg(feature = "test-support")]
pub mod testing;

pub use api::{ApiError, ApiKind, LobbyInfoError, Page};
pub use client::{Client, StoredSession};
pub use connection::{ConnError, ConnEvent, Connection, Phase, ReconnectPolicy};
pub use drafts::{
    AttachError, DraftAttachment, DraftId, DraftSource, DraftState, Drafts, PastedFormat,
    SendBlock, UploadError, UploadKey, paste_png,
};
pub use entry::{
    Chip, ChipAt, Closed, EntryHandle, EntryIntent, EntryStep, EntryUpdates, EntryView, Fill,
    JOIN_CHECK_EVERY, NamePath, RESEND_AFTER, Resend, Said, Stage,
};
pub use issues::{Issue, IssueAction, IssueKind, Issues, Severity, Source};
#[cfg(all(windows, feature = "test-support"))]
pub use media::CaptureFaults;
#[cfg(feature = "test-support")]
pub use media::WatchLoad;
#[cfg(windows)]
pub use media::hardware_encoder;
pub use media::sounds::{Sound, SoundPlayer};
pub use media::{
    AudioSink, AudioSource, DEVICE_POLL, DEVICE_START_TIMEOUT, DeviceChoice, DeviceKind,
    DeviceList, DeviceSetup, DeviceSource, DeviceWatch, DspConfig, DspControl, DspLevels,
    DspReading, Endpoints, FakeDevices, MediaConfig, MediaError, MediaPath, MicLevel, MicTest,
    MonitorControl, NamedDevice, PaintSize, PathState, RouteChange, RouteProbe, ScreenSource,
    ShareOpId, SourceId, SourceKind, StunTimings, SystemDevices, TestMotion, Tone, VideoFeed,
    VideoFrame, VideoStats, VoiceCallId, VoiceEvent, VoiceStats, list_sources, share_blocked,
};
pub use media::{GainUnavailable, MicGain, MicMeter, MicQuality, MicReading, MicVolume, Zone};
pub use media::{Thumbnail, Thumbnails, request_thumbnails, synthetic_thumbnail};
pub use media_cache::{
    DECODED_BUDGET_BYTES, DecodedImage, Lru, MediaCache, MediaCacheError, Variant, picture,
};
pub use origin::{HttpPolicy, LobbyOrigin, OriginError};
pub use paths::Paths;
pub use persist::{Loaded, Lobby, PersistError, Persisted, SavedWindow, StateFile, Upserted};
pub use session::{
    COMMAND_DEADLINE, Clock, Completion, Intent, OFFER_TIMEOUT, PictureError, SessionEvent,
    SessionHandle, SessionUpdate, SignedOutWhy, Snapshot, UiEffect, UiEffects, Updates,
};
pub use settings::{
    Compressor, CompressorAmount, NoiseEngine, NoiseSuppression, Settings, UI_SCALE_DEFAULT,
    UI_SCALE_MAX, UI_SCALE_MIN, step_ui_scale,
};
pub use state::{
    AccountOp, ChangeSync, Changes, History, LobbyState, Older, OwnerAction, PendingMessage,
    Shortfall, StrainCause, StrainRole, StrainWarning, VoicePhase, WatchUi,
};
pub use task::{ClientTask, TaskGone};
pub use time::{
    day_label, full_time, local_date, local_date_time, local_day, local_time, time_ago, today,
    until_tomorrow,
};
pub use tokens::{KeyringTokenStore, MemoryTokenStore, TokenStore, TokenStoreError};
pub use volume::Volume;
