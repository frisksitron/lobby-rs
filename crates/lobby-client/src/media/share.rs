//! Keyframes are on demand only: a request from the SFU (through the transport) forces an IDR,
//! at most one per 500 ms; when no new picture comes within 100 ms (a still screen), the last
//! one is re-encoded as that IDR. With nothing new for a second the last picture is re-encoded
//! anyway, so a Viewer of a still screen keeps getting frames. An encoded frame is never dropped
//! silently: if the queue to the transport is full, everything up to the next IDR is dropped,
//! an IDR is forced and the event counted (a dropped delta frame leaves no RTP gap a Viewer
//! could notice).

use std::{
    collections::VecDeque,
    num::NonZeroU32,
    sync::Arc,
    thread::JoinHandle,
    time::{Duration, Instant},
};

use lobby_protocol::{
    Quality, SourceSize,
    encoder::{FRAME_INTERVAL, MIN_SPACING, bps_for},
};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::{
    MediaError, ShareOpId, VoiceEvent, VoiceEvents,
    capture::{Capture, CaptureEnd, CaptureSetup, FrameSlot, Waited},
    gpu::GpuPicture,
    mf::HardwareEncoder,
    own_share::{OwnShareFeed, OwnShareTap},
    rtc::{EncodedVideo, KeyframeRequests},
    screen::ScreenSource,
};

/// The one limit on how often a share makes a keyframe, a big frame for every Viewer. The SFU
/// passes on each request at once, and those that come sooner are served by the next keyframe.
pub const KEYFRAME_MIN_INTERVAL: Duration = Duration::from_millis(500);
pub const IDLE_REPEAT: Duration = Duration::from_secs(1);
pub const ENCODED_QUEUE: usize = 8;
const STATS_EVERY: Duration = Duration::from_secs(5);
const LOAD_EVERY: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShareLoad {
    pub window: Duration,
    /// Pictures the capture made, whether or not the encoder had room for them.
    pub offered: u32,
    /// New pictures out of the encoder; repeats of the last are not counted.
    pub encoded: u32,
}

impl ShareLoad {
    pub fn offered_fps(&self) -> u32 {
        per_second(self.offered, self.window)
    }

    pub fn encoded_fps(&self) -> u32 {
        per_second(self.encoded, self.window)
    }
}

fn per_second(count: u32, window: Duration) -> u32 {
    (f64::from(count) / window.as_secs_f64().max(f64::EPSILON)).round() as u32
}

struct LoadWindow {
    since: Instant,
    offered_then: u64,
    encoded: u32,
}
const POLL: Duration = Duration::from_millis(100);
const RTP_VIDEO_HZ: u64 = 90_000;

#[derive(Debug, Default)]
pub(crate) struct Policy {
    last_encode: Option<Instant>,
    last_captured: Option<Instant>,
    last_forced: Option<Instant>,
    drop_until_idr: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Encode {
    Fresh { idr: bool },
    Repeat { idr: bool },
    Nothing,
}

impl Policy {
    fn idr_allowed(&self, asked: Option<Instant>, now: Instant) -> bool {
        (asked.is_some() || self.drop_until_idr)
            && self
                .last_forced
                .is_none_or(|t| now.duration_since(t) >= KEYFRAME_MIN_INTERVAL)
    }

    pub fn decide(
        &self,
        fresh: Option<Instant>,
        have_last: bool,
        asked: Option<Instant>,
        now: Instant,
    ) -> Encode {
        let idr = self.idr_allowed(asked, now);
        if let Some(captured) = fresh {
            let spaced = self
                .last_captured
                .is_none_or(|t| captured.saturating_duration_since(t) >= MIN_SPACING);
            let slot_free = self
                .last_encode
                .is_none_or(|t| now.duration_since(t) >= FRAME_INTERVAL);
            if spaced || slot_free || idr {
                return Encode::Fresh { idr };
            }
        }
        if !have_last {
            return Encode::Nothing;
        }
        // No new picture to key: the last one is keyed again at once rather than waiting for the
        // next, which a still screen never makes.
        if idr && asked.is_some() {
            return Encode::Repeat { idr: true };
        }
        if self
            .last_encode
            .is_some_and(|t| now.duration_since(t) >= IDLE_REPEAT)
        {
            return Encode::Repeat { idr };
        }
        Encode::Nothing
    }

    pub fn next_deadline(&self, fresh: bool, asked: Option<Instant>, now: Instant) -> Instant {
        let mut at = now + POLL;
        if let Some(t) = self.last_encode {
            at = at.min(t + IDLE_REPEAT);
            if fresh {
                at = at.min(t + FRAME_INTERVAL);
            }
        }
        if asked.is_some() {
            at = at.min(self.last_forced.map_or(now, |t| t + KEYFRAME_MIN_INTERVAL));
        }
        at.max(now)
    }

    pub fn encoded(&mut self, idr: bool, now: Instant, captured: Option<Instant>) {
        self.last_encode = Some(now);
        if captured.is_some() {
            self.last_captured = captured;
        }
        if idr {
            self.last_forced = Some(now);
        }
    }
}

pub(crate) struct ShareHandle {
    slot: Arc<FrameSlot>,
    thread: Option<JoinHandle<()>>,
}

pub(crate) struct ShareLink {
    pub frames: mpsc::Sender<EncodedVideo>,
    pub keyframes: Arc<KeyframeRequests>,
    pub own: Arc<OwnShareFeed>,
    pub capture: CaptureSetup,
    pub quality: Quality,
}

impl ShareHandle {
    pub fn start(
        source: ScreenSource,
        link: ShareLink,
        events: VoiceEvents,
        share: ShareOpId,
    ) -> Result<ShareHandle, MediaError> {
        let slot = Arc::new(FrameSlot::new(link.quality.box_size()));
        let thread_slot = slot.clone();
        let thread = std::thread::Builder::new()
            .name("lobby-video-tx".into())
            .spawn(move || {
                if let Err(error) = run(&source, &thread_slot, &link, &events, share) {
                    warn!(%error, "screen share stopped");
                    events.blocking_send(VoiceEvent::ShareFailed { share, error });
                }
            })
            .map_err(|e| MediaError::Capture(e.to_string()))?;
        Ok(ShareHandle {
            slot,
            thread: Some(thread),
        })
    }

    pub fn stop(mut self) -> JoinHandle<()> {
        self.slot.stop();
        self.thread.take().expect("the Share thread is running")
    }
}

impl Drop for ShareHandle {
    fn drop(&mut self) {
        self.slot.stop();
    }
}

#[derive(Default)]
struct Stats {
    encoded: u64,
    keyframes: u64,
    repeats: u64,
    dropped: u64,
    bytes: u64,
    submit_ms: Vec<f32>,
    lag_ms: Vec<f32>,
    in_flight_max: usize,
    since: Option<Instant>,
}

fn quantile(values: &[f32], q: f32) -> Option<f32> {
    values
        .get((values.len().saturating_sub(1) as f32 * q) as usize)
        .copied()
}

impl Stats {
    fn log_if_due(&mut self, now: Instant, encoder: &Encoder) {
        let since = *self.since.get_or_insert(now);
        let elapsed = now.duration_since(since);
        if elapsed < STATS_EVERY {
            return;
        }
        self.submit_ms.sort_by(f32::total_cmp);
        self.lag_ms.sort_by(f32::total_cmp);
        let secs = elapsed.as_secs_f64();
        let (width, height) = encoder.hw.size();
        info!(
            encoder = %encoder.name,
            width,
            height,
            fps = format_args!("{:.1}", self.encoded as f64 / secs),
            kbps = (self.bytes as f64 * 8.0 / secs / 1000.0) as u64,
            target_kbps = bps_for(encoder.hw.size()) / 1000,
            keyframes = self.keyframes,
            repeats = self.repeats,
            dropped = self.dropped,
            submit_p50_ms = ?quantile(&self.submit_ms, 0.5),
            submit_p95_ms = ?quantile(&self.submit_ms, 0.95),
            lag_p50_ms = ?quantile(&self.lag_ms, 0.5),
            lag_p95_ms = ?quantile(&self.lag_ms, 0.95),
            in_flight_max = self.in_flight_max,
            "share encoder"
        );
        *self = Stats {
            since: Some(now),
            ..Stats::default()
        };
    }
}

/// A picture handed to the encoder, and what the loop needs back with its bytes.
#[derive(Debug, Clone, Copy)]
struct Job {
    /// When the picture was captured, or for a repeat when it was sent again.
    at: Instant,
    repeat: bool,
    idr: bool,
    /// How long the keyframe request it serves had waited.
    asked_ms: Option<u64>,
}

struct Encoder {
    hw: HardwareEncoder,
    name: Arc<str>,
    pending: VecDeque<(u64, Job)>,
}

fn run(
    source: &ScreenSource,
    slot: &Arc<FrameSlot>,
    link: &ShareLink,
    events: &VoiceEvents,
    id: ShareOpId,
) -> Result<(), MediaError> {
    let capture = Capture::start(source, slot.clone(), link.capture)?;
    info!(source = %source.name, quality = ?link.quality, "screen share capture started");
    let wake = slot.clone();
    link.keyframes.wake_with(move || wake.wake());
    let mut share = Share {
        slot,
        link,
        events,
        id,
        encoder: None,
        pacing: Policy::default(),
        fresh: None,
        last: None,
        last_rtp: None,
        source_size: None,
        own: OwnShareTap::new(link.own.clone()),
        started: Instant::now(),
        stats: Stats::default(),
        load: LoadWindow {
            since: Instant::now(),
            offered_then: 0,
            encoded: 0,
        },
    };
    let result = share.encode();
    slot.stop();
    capture.stop();
    info!("screen share capture stopped");
    result
}

struct Share<'a> {
    slot: &'a Arc<FrameSlot>,
    link: &'a ShareLink,
    events: &'a VoiceEvents,
    id: ShareOpId,
    encoder: Option<Encoder>,
    pacing: Policy,
    fresh: Option<GpuPicture>,
    /// The picture last encoded, which a repeat sends again.
    last: Option<GpuPicture>,
    last_rtp: Option<u64>,
    /// The size of what is captured, as the Session last heard it.
    source_size: Option<(u32, u32)>,
    own: OwnShareTap,
    started: Instant,
    stats: Stats,
    load: LoadWindow,
}

impl Share<'_> {
    fn encode(&mut self) -> Result<(), MediaError> {
        loop {
            let now = Instant::now();
            let asked = self.link.keyframes.pending();
            let deadline = self
                .pacing
                .next_deadline(self.unseen().is_some(), asked, now);
            match self.slot.wait(deadline) {
                Waited::Stopped => return Ok(()),
                Waited::Ended(CaptureEnd::Closed) => return Err(MediaError::SourceClosed),
                Waited::Ended(CaptureEnd::Failed(e)) => return Err(e),
                Waited::Frame(f) => {
                    self.fresh = Some(f);
                    self.report_source();
                }
                Waited::Woken | Waited::Timeout => {}
            }
            if !self.flush()? {
                return Ok(());
            }
            let now = Instant::now();
            self.turn(now)?;
            self.report_load(now);
            if let Some(newest) = self.fresh.as_ref().or(self.last.as_ref()) {
                self.own.offer(newest, now);
            }
            if !self.flush()? {
                return Ok(());
            }
        }
    }

    fn unseen(&self) -> Option<Instant> {
        self.fresh
            .as_ref()
            .map(|p| p.captured)
            .filter(|c| self.pacing.last_captured.is_none_or(|t| *c > t))
    }

    fn turn(&mut self, now: Instant) -> Result<(), MediaError> {
        let asked = self.link.keyframes.pending();
        let decided = self
            .pacing
            .decide(self.unseen(), self.last.is_some(), asked, now);
        let (idr, repeat) = match decided {
            Encode::Nothing => return Ok(()),
            Encode::Fresh { idr } => {
                self.last = self.fresh.take();
                (idr, false)
            }
            Encode::Repeat { idr } => (idr, true),
        };
        let picture = self.last.as_ref().expect("decided on the last picture");
        if idr {
            self.link.keyframes.served();
        }
        let size = picture.lease.size();
        if self.encoder.as_ref().is_none_or(|e| e.hw.size() != size) {
            // Few encoder sessions run at once on NVIDIA's GPUs: the old one ends first. A new
            // one starts with a keyframe.
            self.encoder = None;
            let bps = bps_for(size);
            let wake = self.slot.clone();
            let hw = HardwareEncoder::new(
                picture.lease.device(),
                size,
                bps,
                Arc::new(move || {
                    wake.wake();
                }),
            )?;
            let name = Arc::from(hw.name());
            info!(encoder = %name, width = size.0, height = size.1, kbps = bps / 1000, "share encoder started");
            self.encoder = Some(Encoder {
                hw,
                name,
                pending: VecDeque::new(),
            });
        }
        let encoder = self.encoder.as_mut().expect("started above");
        let job = Job {
            // A repeat is a new picture in time, so its timestamps move on.
            at: if repeat { now } else { picture.captured },
            repeat,
            idr,
            asked_ms: asked.map(|t| now.saturating_duration_since(t).as_millis() as u64),
        };
        let t0 = Instant::now();
        let frame = encoder.hw.submit(&picture.lease, idr)?;
        encoder.pending.push_back((frame, job));
        self.stats
            .submit_ms
            .push(t0.elapsed().as_secs_f32() * 1000.0);
        self.pacing.encoded(idr, now, (!repeat).then_some(job.at));
        Ok(())
    }

    fn report_load(&mut self, now: Instant) {
        let window = now.saturating_duration_since(self.load.since);
        if window < LOAD_EVERY {
            return;
        }
        let offered = self.slot.offered();
        let load = ShareLoad {
            window,
            offered: (offered - self.load.offered_then) as u32,
            encoded: self.load.encoded,
        };
        self.load = LoadWindow {
            since: now,
            offered_then: offered,
            encoded: 0,
        };
        self.events.try_send(VoiceEvent::ShareLoad {
            share: self.id,
            load,
        });
    }

    fn flush(&mut self) -> Result<bool, MediaError> {
        let Some(encoder) = &mut self.encoder else {
            return Ok(true);
        };
        while let Some(out) = encoder.hw.take()? {
            // Pictures before this one that are still pending got no output.
            while encoder.pending.front().is_some_and(|(n, _)| *n < out.frame) {
                encoder.pending.pop_front();
            }
            let Some((_, job)) = encoder.pending.pop_front_if(|(n, _)| *n == out.frame) else {
                continue;
            };
            let now = Instant::now();
            let stats = &mut self.stats;
            if !job.repeat {
                self.load.encoded += 1;
                let lag = out.at.saturating_duration_since(job.at);
                stats.lag_ms.push(lag.as_secs_f32() * 1000.0);
                stats.in_flight_max = stats.in_flight_max.max(1 + encoder.pending.len());
            }
            let (len, keyframe) = (out.data.len(), out.keyframe);
            if job.idr || job.repeat {
                debug!(
                    keyframe,
                    idr = job.idr,
                    repeat = job.repeat,
                    bytes = len,
                    asked_ms = job.asked_ms,
                    "share encode"
                );
            }
            if self.pacing.drop_until_idr && !keyframe {
                stats.dropped += 1;
            } else {
                let micros = job.at.duration_since(self.started).as_micros() as u64;
                let rtp =
                    (micros * RTP_VIDEO_HZ / 1_000_000).max(self.last_rtp.map_or(0, |t| t + 1));
                self.last_rtp = Some(rtp);
                let frame = EncodedVideo {
                    data: out.data,
                    keyframe,
                    at: job.at,
                    rtp_time: rtp,
                    size: encoder.hw.size(),
                    encoder: encoder.name.clone(),
                };
                match self.link.frames.try_send(frame) {
                    Ok(()) => {
                        self.pacing.drop_until_idr = false;
                        stats.encoded += 1;
                        stats.bytes += len as u64;
                        stats.keyframes += u64::from(keyframe);
                        stats.repeats += u64::from(job.repeat);
                    }
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        warn!("the share's transport queue is full: dropping to the next keyframe");
                        self.pacing.drop_until_idr = true;
                        stats.dropped += 1;
                    }
                    // The transport is gone: Voice is ending, and so is the share.
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        debug!("share transport closed");
                        return Ok(false);
                    }
                }
            }
            stats.log_if_due(now, encoder);
        }
        Ok(true)
    }

    /// Tells the Session the size of what is captured when it first knows it and each time it
    /// changes. A full queue leaves it for the next picture, not the encoder waiting.
    fn report_source(&mut self) {
        let Some(source) = self.slot.source().filter(|s| Some(*s) != self.source_size) else {
            return;
        };
        let (Some(width), Some(height)) = (NonZeroU32::new(source.0), NonZeroU32::new(source.1))
        else {
            return;
        };
        let size = SourceSize { width, height };
        if self.events.try_send(VoiceEvent::ShareSource {
            share: self.id,
            size,
        }) {
            info!(width = source.0, height = source.1, "share source size");
            self.source_size = Some(source);
        }
    }
}
