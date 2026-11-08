//! The media engine: pure-Rust transport (str0m) and DSP, libopus as the one
//! C codec. The Voice path:
//!
//! ```text
//! microphone (cpal) → mono 48 kHz ring → lobby-voice-tx: VAD → Opus 128 kbps → lobby-rtc → SFU
//! SFU → lobby-rtc: route by mid → slot ring → output callback: jitter → Opus → mix → speakers
//! ```

mod audio;
#[cfg(feature = "test-support")]
mod bench;
#[cfg(windows)]
mod capture;
mod devices;
mod dsp;
mod engine;
#[cfg(windows)]
mod gpu;
mod jitter;
mod meter;
#[cfg(windows)]
mod mf;
#[cfg(windows)]
mod mf_decode;
mod mic_volume;
mod mic_watch;
mod mixer;
mod monitor;
mod opus_codec;
mod own_share;
pub(crate) mod pattern;
mod playout;
mod rtc;
mod screen;
#[cfg(windows)]
mod share;
pub mod sounds;
mod thumbs;
mod vad;
pub(crate) mod video;
mod watch;
#[cfg(windows)]
mod win;

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use lobby_protocol::{RestartPolicy, Sdp, UserId, VoiceSessionId};
use tokio::sync::mpsc;

pub use audio::MicTest;
#[cfg(feature = "test-support")]
pub use bench::{CaptureChain, CodecLoop, Onset, PlayedBlock, PulseShape, Pulses};
#[cfg(all(windows, feature = "test-support"))]
pub use capture::CaptureFaults;
pub use devices::{
    DEVICE_POLL, DeviceChoice, DeviceKind, DeviceList, DeviceSource, DeviceWatch, Endpoints,
    FakeDevices, NamedDevice, SystemDevices,
};
pub use dsp::{DspConfig, DspControl};
pub(crate) use engine::VoiceHandle;
pub use meter::{DspLevels, DspReading, MicLevel, MicMeter, MicQuality, MicReading, Zone};
#[cfg(windows)]
pub use mf::hardware_encoder;
pub use mic_volume::{GainUnavailable, MicGain, MicVolume};
pub use monitor::MonitorControl;
pub(crate) use own_share::OwnShareFeed;
pub use own_share::ShareLoad;
pub use rtc::RouteProbe;
pub use screen::{ScreenSource, SourceId, SourceKind, TestMotion, list_sources};
#[cfg(all(windows, feature = "test-support"))]
pub use screen::{grab_monitor, grab_window};
pub use thumbs::{
    ThumbUpdate, Thumbnail, Thumbnails, request_thumbnails, synthetic as synthetic_thumbnail,
};
pub use video::{PaintSize, VideoFeed, VideoFrame};
pub use watch::WatchLoad;

use crate::{NoiseSuppression, SessionEvent};

static SHARE_BLOCKED: Mutex<Option<MediaError>> = Mutex::new(None);

/// Why this computer cannot share a screen, as the latest probe found ([`Client::probe_share`]);
/// `None` before one has ended. Screen Share needs Windows and a GPU with a hardware H.264
/// encoder.
///
/// [`Client::probe_share`]: crate::Client::probe_share
pub fn share_blocked() -> Option<MediaError> {
    SHARE_BLOCKED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

pub(crate) fn probe_share() -> Option<MediaError> {
    #[cfg(windows)]
    let blocked = hardware_encoder()
        .is_none()
        .then_some(MediaError::NoHardwareEncoder);
    #[cfg(not(windows))]
    let blocked = Some(MediaError::ShareUnsupported);
    tracing::info!(blocked = ?blocked, "screen share probed");
    *SHARE_BLOCKED.lock().unwrap_or_else(|e| e.into_inner()) = blocked.clone();
    blocked
}

/// How long opening the microphone and speakers may take. The devices open
/// on the `lobby-audio` thread, never the caller's; the Session gives up after this long.
pub const DEVICE_START_TIMEOUT: Duration = Duration::from_secs(3);

pub const SAMPLE_RATE: u32 = 48_000;
/// One 20 ms Opus frame at 48 kHz.
pub const FRAME: usize = 960;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tone {
    pub hz: f32,
    pub amplitude: f32,
}

impl Tone {
    pub fn hz(hz: f32) -> Tone {
        Tone {
            hz,
            amplitude: 0.25,
        }
    }

    /// The `n`th sample at 48 kHz.
    pub(crate) fn sample(self, n: u64) -> f32 {
        let step = std::f64::consts::TAU * f64::from(self.hz) / f64::from(SAMPLE_RATE);
        (f64::from(self.amplitude) * (step * n as f64).sin()) as f32
    }
}

/// A device to open, and where to ask which devices there are.
#[derive(Clone)]
pub struct DeviceSetup {
    pub choice: DeviceChoice,
    pub source: Arc<dyn DeviceSource>,
}

impl DeviceSetup {
    pub fn new(choice: DeviceChoice, source: Arc<dyn DeviceSource>) -> Self {
        Self { choice, source }
    }
}

impl std::fmt::Debug for DeviceSetup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DeviceSetup({:?})", self.choice)
    }
}

#[derive(Debug, Clone)]
pub enum AudioSource {
    /// The microphone, whose raw level is measured into `level`.
    Device {
        setup: DeviceSetup,
        level: MicLevel,
    },
    Synthetic(Tone),
    #[cfg(feature = "test-support")]
    Pulses(Pulses),
    /// A microphone that does not open: the devices report this error.
    #[cfg(feature = "test-support")]
    Broken(MediaError),
}

pub enum AudioSink {
    Device(DeviceSetup),
    Null,
    #[cfg(feature = "test-support")]
    Capture(rtrb::Producer<f32>),
    #[cfg(feature = "test-support")]
    Timed(rtrb::Producer<PlayedBlock>),
}

impl std::fmt::Debug for AudioSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Device(_) => "Device",
            Self::Null => "Null",
            #[cfg(feature = "test-support")]
            Self::Capture(_) => "Capture",
            #[cfg(feature = "test-support")]
            Self::Timed(_) => "Timed",
        })
    }
}

/// How fast ICE notices that the path to the SFU is dead. Consent checks run every `max_rto`; an
/// unanswered one is retransmitted after `initial_rto`, doubling up to `max_rto`, and after
/// `retransmits` unanswered ones the path is down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StunTimings {
    pub initial_rto: Duration,
    pub max_rto: Duration,
    pub retransmits: usize,
}

impl Default for StunTimings {
    fn default() -> Self {
        StunTimings {
            initial_rto: Duration::from_millis(100),
            max_rto: Duration::from_millis(500),
            retransmits: 5,
        }
    }
}

#[derive(Debug)]
pub struct MediaConfig {
    pub source: AudioSource,
    pub sink: AudioSink,
    pub stun: StunTimings,
    pub restart: RestartPolicy,
    /// Restart the media path when the OS reports a network change that moves our route to the
    /// SFU (Windows only). Tests leave the machine's network alone.
    pub watch_network: bool,
    pub route: RouteProbe,
    /// How the microphone is cleaned; the call follows changes to it as they are made.
    pub dsp: DspControl,
    /// What the call measures along the cleaning, for the Settings' rack.
    pub levels: DspLevels,
    /// Whether the person hears themselves; the call follows changes to it as they are made.
    pub monitor: MonitorControl,
    #[cfg(all(windows, feature = "test-support"))]
    pub capture_faults: CaptureFaults,
    /// Joining fails with this error before anything starts, as a media thread that cannot be
    /// spawned does.
    #[cfg(feature = "test-support")]
    pub start_fails: Option<MediaError>,
    /// A Watch's decoder breaks after this many pictures, as one that fails does.
    #[cfg(feature = "test-support")]
    pub decoder_fails_after: Option<u64>,
    /// The microphone counts as silent after this many 20 ms frames of zeros, not 5 s of them.
    #[cfg(feature = "test-support")]
    pub mic_silent_after: Option<u32>,
}

impl MediaConfig {
    /// The microphone and speakers `input` and `output` name. `level` hears the microphone, `dsp`
    /// says how it is cleaned, and `levels` hears it along the way.
    pub fn devices(
        input: DeviceChoice,
        output: DeviceChoice,
        devices: Arc<dyn DeviceSource>,
        level: MicLevel,
        dsp: DspControl,
        levels: DspLevels,
    ) -> MediaConfig {
        MediaConfig {
            source: AudioSource::Device {
                setup: DeviceSetup::new(input, devices.clone()),
                level,
            },
            sink: AudioSink::Device(DeviceSetup::new(output, devices)),
            stun: StunTimings::default(),
            restart: RestartPolicy::DEFAULT,
            watch_network: true,
            route: RouteProbe::default(),
            dsp,
            levels,
            monitor: MonitorControl::default(),
            #[cfg(all(windows, feature = "test-support"))]
            capture_faults: CaptureFaults::default(),
            #[cfg(feature = "test-support")]
            start_fails: None,
            #[cfg(feature = "test-support")]
            decoder_fails_after: None,
            #[cfg(feature = "test-support")]
            mic_silent_after: None,
        }
    }

    /// No devices and no OS hooks: for tests and automation. The tone is not speech, so no noise
    /// suppression: it would take it out.
    pub fn fake() -> MediaConfig {
        MediaConfig {
            source: AudioSource::Synthetic(Tone::hz(440.0)),
            sink: AudioSink::Null,
            stun: StunTimings::default(),
            restart: RestartPolicy::DEFAULT,
            watch_network: false,
            route: RouteProbe::default(),
            dsp: DspControl::new(DspConfig {
                noise: NoiseSuppression {
                    on: false,
                    ..NoiseSuppression::default()
                },
                ..DspConfig::default()
            }),
            levels: DspLevels::default(),
            monitor: MonitorControl::default(),
            #[cfg(all(windows, feature = "test-support"))]
            capture_faults: CaptureFaults::default(),
            #[cfg(feature = "test-support")]
            start_fails: None,
            #[cfg(feature = "test-support")]
            decoder_fails_after: None,
            #[cfg(feature = "test-support")]
            mic_silent_after: None,
        }
    }
}

/// Numbers this client's Voice sessions (one per Join), so an event or a command outcome that
/// belongs to an ended one is recognised and ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VoiceCallId(pub(crate) u64);

/// Numbers this client's Screen Shares and watches within the Session, so the outcome of an
/// earlier one (a late refusal, a capture thread that is ending) never ends a newer one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ShareOpId(pub(crate) u64);

#[derive(Debug, Clone)]
pub(crate) struct VoiceEvents {
    call: VoiceCallId,
    tx: mpsc::Sender<SessionEvent>,
}

impl VoiceEvents {
    pub fn new(call: VoiceCallId, tx: mpsc::Sender<SessionEvent>) -> Self {
        Self { call, tx }
    }

    fn wrap(&self, event: VoiceEvent) -> SessionEvent {
        SessionEvent::Voice {
            call: self.call,
            event,
        }
    }

    /// Never waits: `false` if the queue is full (or gone), so the caller can retry.
    pub fn try_send(&self, event: VoiceEvent) -> bool {
        self.tx.try_send(self.wrap(event)).is_ok()
    }

    pub async fn send(&self, event: VoiceEvent) {
        let _ = self.tx.send(self.wrap(event)).await;
    }

    /// From a plain thread (never inside a runtime): waits for room, so the event is not lost.
    pub fn blocking_send(&self, event: VoiceEvent) {
        let _ = self.tx.blocking_send(self.wrap(event));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceEvent {
    DevicesReady,
    /// The chosen microphone or speakers are gone, and the system default stands in.
    DeviceFellBack {
        kind: DeviceKind,
        wanted: String,
    },
    Answer {
        vsid: VoiceSessionId,
        rev: u32,
        sdp: Sdp,
    },
    /// This Member's own VAD verdict on what the microphone sends.
    Speaking(bool),
    /// The other Members speaking in what this client plays now, sorted, when that changes.
    Speakers(Vec<UserId>),
    /// The Member has talked for a second with the microphone muted. At most once per mute.
    TalkingMuted,
    /// The unmuted microphone has sent only digital zeros for seconds: a mute switch on a
    /// headset, or the OS's microphone privacy switch. Once per call until sound returns.
    MicSilent,
    /// ICE lost the path to the SFU, or has one again.
    Path(PathState),
    /// The OS reported an interface or address added or removed.
    NetworkChanged(RouteChange),
    /// Once a second, once ICE has a path.
    Stats(VoiceStats),
    Failed(MediaError),
    ShareFailed {
        share: ShareOpId,
        error: MediaError,
    },
    /// The share's first picture came, or what it captures changed size, as a shared window
    /// does when it is resized: the server is to hear of it.
    ShareSource {
        share: ShareOpId,
        size: lobby_protocol::SourceSize,
    },
    /// The Watch's feed holds the first picture of `sharer`'s share, and has only theirs from
    /// now on.
    WatchShows {
        watch: ShareOpId,
        sharer: UserId,
    },
    /// The Watch's decoder stopped for good.
    WatchFailed {
        watch: ShareOpId,
        error: MediaError,
    },
    /// Once a second while the Watch runs: how busy its threads were.
    WatchLoad {
        watch: ShareOpId,
        load: WatchLoad,
    },
    /// Once a second while the share runs: how many pictures the capture offered the encoder and
    /// how many it encoded.
    ShareLoad {
        share: ShareOpId,
        load: ShareLoad,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathState {
    Up,
    Down,
}

/// What a network change did to the route to the SFU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteChange {
    Same,
    /// The OS now reaches the SFU through another local address.
    Moved,
}

/// How the media reaches the SFU: UDP, or TCP where the network blocks UDP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaPath {
    Udp,
    Tcp,
}

/// The media connection as it was over the last second, for the Voice stats dialog. Audio is
/// always Opus and Screen Share always H.264: the only codecs this client negotiates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceStats {
    pub path: MediaPath,
    pub sfu: SocketAddr,
    pub rtt: Option<Duration>,
    /// Of the packets sent to this client, and of those it sent, in tenths of a percent; `None`
    /// before the first report.
    pub loss_in: Option<u16>,
    pub loss_out: Option<u16>,
    /// The highest jitter among the Members this client hears.
    pub jitter: Option<Duration>,
    /// Bits per second; `None` until a second report.
    pub audio_out: Option<u64>,
    pub audio_in: Option<u64>,
    /// What str0m's bandwidth estimate says this client can send, in bits per second; `None`
    /// until it has one.
    pub uplink_estimate: Option<u64>,
    /// While sharing.
    pub share_out: Option<VideoStats>,
    /// While watching.
    pub share_in: Option<VideoStats>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoStats {
    /// `None` until a second report.
    pub bits_per_second: Option<u64>,
    /// Width and height of the latest picture, once there is one.
    pub size: Option<(u32, u32)>,
    /// What encoded the latest picture of this client's own share: the GPU's encoder by its
    /// maker's name.
    pub encoder: Option<Arc<str>>,
    /// Whether the GPU decoded the latest picture of the watched share; `None` for the own share
    /// and before the first picture.
    pub hardware_decode: Option<bool>,
    /// Frames of the watched share lost to holes no retransmission filled over the last second:
    /// the hole, and the delta frames discarded after it until the next keyframe. Zero on the
    /// own share.
    pub lost: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MediaError {
    #[error("No microphone found. Connect one and try again.")]
    NoDevice,
    #[error("No speakers or headphones found. Connect some and try again.")]
    NoOutputDevice,
    #[error("The audio device is not available.")]
    DeviceNotFound,
    #[error("The audio device is in use by another application.")]
    DeviceBusy,
    #[error("The audio device did not respond. Check it and try again.")]
    DeviceTimeout,
    #[error("Microphone access was denied. Allow it in Windows privacy settings.")]
    PermissionDenied,
    #[error("Audio device error: {0}")]
    Device(String),
    #[error("No network route to the voice server: {0}")]
    NoRoute(String),
    #[error("Audio codec error: {0}")]
    Codec(String),
    #[error("Connection failed: {0}")]
    Transport(String),
    #[error("Screen capture failed: {0}")]
    Capture(String),
    #[error("The shared window was closed.")]
    SourceClosed,
    #[error("That screen or window is no longer available.")]
    SourceGone,
    /// The window keeps itself out of captures; it holds the window's name.
    #[error("{}", crate::copy::window_blocks_sharing(.0))]
    SourceBlocked(String),
    /// The window is minimized; it holds the window's name.
    #[error("{}", crate::copy::window_minimized(.0))]
    SourceMinimized(String),
    #[error("Screen Share works on Windows only.")]
    ShareUnsupported,
    #[error("This computer's GPU has no hardware H.264 encoder, which Screen Share needs.")]
    NoHardwareEncoder,
    #[error("This computer has no H.264 decoder. On Windows N, install the Media Feature Pack.")]
    NoDecoder,
    #[error("Video error: {0}")]
    Video(String),
}
