//! The Viewer's receive path: the transport hands encoded frames of the watched share, each
//! named by its Sharer, to `lobby-video-rx` through a bounded queue; it decodes them with
//! Media Foundation ([`super::mf_decode`]) and copies each picture out for `lobby-video-cv`,
//! which converts it to BGRA and offers it, named the same, to the UI. The two threads run side
//! by side, so a picture takes the longer of the two, not both. A frame of another Sharer flushes
//! the decoder, and the first picture of each is reported to the Session. A decode error (or the
//! transport's report of a gap) means nothing decodes until the next keyframe: delta frames are
//! skipped and the transport asks the SFU for a keyframe.
//!
//! Frames queue in order, encoded and then decoded; the UI keeps the newest it finds.

use std::{
    collections::VecDeque,
    num::NonZeroUsize,
    sync::{
        Arc, Condvar, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc as std_mpsc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use lobby_protocol::{UserId, encoder::bps_for};
use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};

#[cfg(windows)]
use super::mf_decode::{Decoder, Picture};
use super::{
    MediaError, ShareOpId, VoiceEvent, VoiceEvents,
    video::{PaintSize, VideoFrame},
};

pub const DECODE_QUEUE: usize = 8;
const POLL: Duration = Duration::from_millis(100);
pub const KEYFRAME_REQUEST_EVERY: Duration = Duration::from_millis(500);
/// Row bands a picture is converted in at once.
pub const CONVERT_BANDS: NonZeroUsize = NonZeroUsize::new(4).unwrap();
/// Decoded pictures that wait for the conversion thread. The GPU decodes a burst of frames, as
/// the network delivers them, faster than they convert; past this many the decode thread waits
/// too, and the burst waits still encoded rather than being dropped.
const WAITING: usize = 2;
/// Decoded pictures between the two threads: the one being filled, those waiting, and the one
/// being converted. They go round, so no picture allocates once the size settles.
const PICTURE_POOL: usize = WAITING + 2;
/// How often the Session hears how busy the Watch's threads were.
pub const LOAD_EVERY: Duration = Duration::from_secs(1);

/// What the Watch's threads did over one window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchLoad {
    pub window: Duration,
    /// Decoding pictures and copying them out.
    pub decode_busy: Duration,
    /// Converting pictures for the UI.
    pub convert_busy: Duration,
    /// Frames the decode thread took, and the pictures it decoded from them.
    pub frames: u32,
    pub pictures: u32,
    /// Frames the decoder never got because it fell behind: those the decode queue had no room
    /// for, and the delta frames discarded after them until the next keyframe.
    pub lost: u64,
    /// Pictures converted that the UI's queue had no room for, or that were never converted
    /// because it had none.
    pub ui_dropped: u64,
}

impl WatchLoad {
    pub fn decode_percent(&self) -> u32 {
        percent(self.decode_busy, self.window)
    }

    pub fn convert_percent(&self) -> u32 {
        percent(self.convert_busy, self.window)
    }
}

fn percent(part: Duration, whole: Duration) -> u32 {
    (part.as_micros() * 100 / whole.as_micros().max(1)) as u32
}

/// The transport's side of a watch: which received frames may reach the decoder, and when to
/// ask for a keyframe. A gap NACK could not fill (`MediaData.contiguous == false`), a queue
/// overflow or a decoder that lost its references makes delta frames useless until the next
/// keyframe: they are discarded, never decoded into garbage.
#[derive(Debug)]
pub(crate) struct KeyframeGate {
    waiting: Option<Wait>,
    last_request: Option<Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wait {
    Start,
    For(Lost),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Lost {
    Network,
    Decoder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Gate {
    pub forward: bool,
    pub request_keyframe: bool,
    pub lost: Option<Lost>,
}

impl KeyframeGate {
    pub fn new() -> KeyframeGate {
        KeyframeGate {
            waiting: Some(Wait::Start),
            last_request: None,
        }
    }

    pub fn reset(&mut self) {
        self.waiting = Some(Wait::Start);
    }

    pub fn on_frame(
        &mut self,
        contiguous: bool,
        keyframe: bool,
        decoder_lost: bool,
        now: Instant,
    ) -> Gate {
        if !keyframe && (!contiguous || decoder_lost) && self.waiting.is_none() {
            let lost = if contiguous {
                Lost::Decoder
            } else {
                Lost::Network
            };
            self.waiting = Some(Wait::For(lost));
            return Gate {
                forward: false,
                request_keyframe: self.may_request(now),
                lost: Some(lost),
            };
        }
        if keyframe {
            self.waiting = None;
        }
        // Still waiting: ask again (at most every 500 ms), since a request can be lost or
        // coalesced away by the SFU, and a still screen sends no keyframe by itself.
        let waiting = self.waiting.is_some();
        Gate {
            forward: !waiting,
            request_keyframe: waiting && self.may_request(now),
            lost: match self.waiting {
                Some(Wait::For(lost)) => Some(lost),
                _ => (!contiguous).then_some(Lost::Network),
            },
        }
    }

    /// The frame forwarded found the decoder's queue full.
    pub fn overflowed(&mut self, now: Instant) -> bool {
        self.waiting = Some(Wait::For(Lost::Decoder));
        self.may_request(now)
    }

    fn may_request(&mut self, now: Instant) -> bool {
        let allowed = self
            .last_request
            .is_none_or(|t| now.duration_since(t) >= KEYFRAME_REQUEST_EVERY);
        if allowed {
            self.last_request = Some(now);
        }
        allowed
    }
}

/// An encoded frame of `from`'s share. The first of a Sharer the decoder did not decode last
/// is a keyframe: the transport switches at one.
pub(crate) struct VideoIn {
    pub from: UserId,
    pub data: Arc<[u8]>,
    pub keyframe: bool,
}

#[derive(Clone)]
pub(crate) struct WatchInput {
    pub frames: std_mpsc::SyncSender<VideoIn>,
    /// The decoder lost the picture of the Sharer it decodes: their frames are useless until a
    /// keyframe.
    pub needs_keyframe: Arc<AtomicBool>,
    pub shown: Arc<Shown>,
    /// The Sharer the Session asks for, the newest only: no move to them can be lost.
    pub wanted: watch::Receiver<UserId>,
    pub lost_total: Arc<AtomicU64>,
}

/// The size of the latest decoded picture, and whether the GPU decoded it, for the Voice stats.
#[derive(Debug, Default)]
pub(crate) struct Shown {
    size: AtomicU64,
    hardware: AtomicBool,
}

impl Shown {
    fn set(&self, width: u32, height: u32, hardware: bool) {
        self.hardware.store(hardware, Ordering::Relaxed);
        let packed = (u64::from(width) << 32) | u64::from(height);
        self.size.store(packed, Ordering::Relaxed);
    }

    pub fn get(&self) -> Option<((u32, u32), bool)> {
        let packed = self.size.load(Ordering::Relaxed);
        let hardware = self.hardware.load(Ordering::Relaxed);
        (packed != 0).then_some((((packed >> 32) as u32, packed as u32), hardware))
    }
}

pub(crate) struct WatchHandle {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    wanted: watch::Sender<UserId>,
}

/// Where the decoder's pictures and reports go.
pub(crate) struct WatchOut {
    pub pictures: mpsc::Sender<VideoFrame>,
    /// What the view says its pictures are painted at, read for each picture.
    pub paint: PaintSize,
    pub events: VoiceEvents,
    pub watch: ShareOpId,
    /// The decoder breaks after this many pictures, as one that fails does.
    #[cfg(feature = "test-support")]
    pub fails_after: Option<u64>,
}

impl WatchHandle {
    /// The decoder, and the transport's side of the Watch, which wants `sharer` first.
    pub fn start(out: WatchOut, sharer: UserId) -> Result<(WatchHandle, WatchInput), MediaError> {
        let (tx, rx) = std_mpsc::sync_channel(DECODE_QUEUE);
        let (wanted, wanted_rx) = watch::channel(sharer);
        let stop = Arc::new(AtomicBool::new(false));
        let input = WatchInput {
            frames: tx,
            needs_keyframe: Arc::default(),
            shown: Arc::default(),
            wanted: wanted_rx,
            lost_total: Arc::default(),
        };
        let handoff = Arc::new(Handoff::new());
        let conversion = Conversion {
            handoff: handoff.clone(),
            pictures: out.pictures,
            events: out.events.clone(),
            watch: out.watch,
            shown: input.shown.clone(),
        };
        let converter = std::thread::Builder::new()
            .name("lobby-video-cv".into())
            .spawn(move || conversion.run())
            .map_err(|e| MediaError::Video(e.to_string()))?;
        let decoding = Decoding {
            rx,
            handoff: handoff.clone(),
            needs_keyframe: input.needs_keyframe.clone(),
            lost_total: input.lost_total.clone(),
            stop: stop.clone(),
            paint: out.paint,
            events: out.events,
            watch: out.watch,
            #[cfg(feature = "test-support")]
            fails_after: out.fails_after,
        };
        let thread = std::thread::Builder::new()
            .name("lobby-video-rx".into())
            .spawn(move || {
                let decoded = Decoder::new().and_then(|decoder| decoding.run(decoder));
                decoding.handoff.end();
                let converted = converter.join().unwrap_or_else(|_| {
                    Err(MediaError::Video("the conversion thread panicked".into()))
                });
                if let Err(error) = decoded.and(converted) {
                    warn!(%error, "the screen share decoder failed");
                    decoding.events.blocking_send(VoiceEvent::WatchFailed {
                        watch: decoding.watch,
                        error,
                    });
                }
            })
            .map_err(|e| {
                handoff.end();
                MediaError::Video(e.to_string())
            })?;
        Ok((
            WatchHandle {
                stop,
                thread: Some(thread),
                wanted,
            },
            input,
        ))
    }

    /// The transport moves to `sharer` at their next keyframe.
    pub fn want(&self, sharer: UserId) {
        self.wanted.send_replace(sharer);
    }

    pub fn stop(mut self) -> JoinHandle<()> {
        self.stop.store(true, Ordering::SeqCst);
        self.thread.take().expect("the Watch thread is running")
    }
}

impl Drop for WatchHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// A decoded picture on its way to the UI.
struct Decoded {
    sharer: UserId,
    picture: Picture,
    /// The decoder's count, for the UI's stats.
    number: u64,
}

struct Handoff {
    slot: Mutex<Slot>,
    ready: Condvar,
}

#[derive(Default)]
struct Slot {
    waiting: VecDeque<Decoded>,
    pool: Vec<Picture>,
    /// The Watch stopped, or the UI's feed closed: no more pictures either way.
    ended: bool,
    /// Since the decode thread last took them for its report.
    convert_busy: Duration,
    ui_dropped: u64,
}

impl Handoff {
    fn new() -> Handoff {
        let slot = Slot {
            pool: (0..PICTURE_POOL).map(|_| Picture::empty()).collect(),
            ..Slot::default()
        };
        Handoff {
            slot: Mutex::new(slot),
            ready: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Slot> {
        self.slot.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn buffer(&self) -> Picture {
        self.lock().pool.pop().unwrap_or_else(Picture::empty)
    }

    /// Queues `decoded` once fewer than [`WAITING`] wait. `false` once the Watch has ended or
    /// `stop` is set.
    fn offer(&self, decoded: Decoded, stop: &AtomicBool) -> bool {
        let mut slot = self.lock();
        while !slot.ended && slot.waiting.len() >= WAITING {
            if stop.load(Ordering::SeqCst) {
                return false;
            }
            slot = self
                .ready
                .wait_timeout(slot, POLL)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        if slot.ended {
            return false;
        }
        slot.waiting.push_back(decoded);
        self.ready.notify_all();
        true
    }

    /// The oldest waiting picture, once there is one; `None` once the Watch has ended.
    fn take(&self) -> Option<Decoded> {
        let mut slot = self.lock();
        loop {
            if slot.ended {
                return None;
            }
            if let Some(decoded) = slot.waiting.pop_front() {
                self.ready.notify_all();
                return Some(decoded);
            }
            slot = self
                .ready
                .wait(slot)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// The conversion thread is done with `picture`, which kept it busy for `busy`.
    fn done(&self, picture: Picture, busy: Duration, ui_dropped: bool) {
        let mut slot = self.lock();
        slot.pool.push(picture);
        slot.convert_busy += busy;
        slot.ui_dropped += u64::from(ui_dropped);
    }

    fn end(&self) {
        self.lock().ended = true;
        self.ready.notify_all();
    }

    /// The conversion thread's busy time and the UI's drops since the last call.
    fn counts(&self) -> (Duration, u64) {
        let mut slot = self.lock();
        let busy = std::mem::take(&mut slot.convert_busy);
        (busy, std::mem::take(&mut slot.ui_dropped))
    }
}

struct Decoding {
    rx: std_mpsc::Receiver<VideoIn>,
    handoff: Arc<Handoff>,
    needs_keyframe: Arc<AtomicBool>,
    lost_total: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    paint: PaintSize,
    events: VoiceEvents,
    watch: ShareOpId,
    #[cfg(feature = "test-support")]
    fails_after: Option<u64>,
}

/// The newest picture decoded, which the decoder holds to copy out again at another size, and
/// the size it was copied out at.
struct Last {
    sharer: UserId,
    number: u64,
    paint: Option<(u32, u32)>,
}

/// What the decode thread counts toward its next [`WatchLoad`].
struct Window {
    since: Instant,
    busy: Duration,
    frames: u32,
    pictures: u32,
    lost: u64,
}

impl Window {
    fn new(since: Instant, lost: u64) -> Window {
        Window {
            since,
            busy: Duration::ZERO,
            frames: 0,
            pictures: 0,
            lost,
        }
    }
}

impl Decoding {
    /// Decodes until stopped. `Err` is a failure the Watch cannot recover from.
    fn run(&self, mut decoder: Decoder) -> Result<(), MediaError> {
        let mut waiting_for_keyframe = true;
        let mut from: Option<UserId> = None;
        let (mut decoded, mut skipped, mut errors) = (0u64, 0u64, 0u64);
        let mut window = Window::new(Instant::now(), self.lost());
        let mut last: Option<Last> = None;
        while !self.stop.load(Ordering::SeqCst) {
            if from.is_some() {
                self.report(&mut window);
            }
            let input = match self.rx.recv_timeout(POLL) {
                Ok(input) => input,
                Err(std_mpsc::RecvTimeoutError::Timeout) => {
                    let ended = match &mut last {
                        Some(last) => !self.repaint(&mut decoder, last)?,
                        None => false,
                    };
                    if ended {
                        break;
                    }
                    continue;
                }
                Err(std_mpsc::RecvTimeoutError::Disconnected) => break,
            };
            let VideoIn {
                from: sharer,
                data,
                keyframe,
            } = input;
            if from.as_ref() != Some(&sharer) {
                last = None;
                if from.is_some() {
                    decoder.flush()?;
                }
                debug!(%sharer, "the decoder takes another share");
                from = Some(sharer.clone());
                waiting_for_keyframe = true;
            }
            if waiting_for_keyframe && !keyframe {
                skipped += 1;
                continue;
            }
            let started = Instant::now();
            window.frames += 1;
            match decoder.decode(&data) {
                Ok(true) => {}
                Ok(false) => {
                    window.busy += started.elapsed();
                    continue;
                }
                Err(e) => {
                    window.busy += started.elapsed();
                    errors += 1;
                    debug!(error = %e, keyframe, bytes = data.len(), nals = ?nal_types(&data), "decode failed: waiting for a keyframe");
                    waiting_for_keyframe = true;
                    self.needs_keyframe.store(true, Ordering::SeqCst);
                    last = None;
                    decoder.flush()?;
                    continue;
                }
            }
            waiting_for_keyframe = false;
            decoded += 1;
            let mut picture = self.handoff.buffer();
            let paint = self.paint.get();
            decoder.copy(&mut picture, paint)?;
            last = Some(Last {
                sharer: sharer.clone(),
                number: decoded,
                paint,
            });
            window.busy += started.elapsed();
            window.pictures += 1;
            #[cfg(feature = "test-support")]
            if self.fails_after.is_some_and(|n| decoded > n) {
                return Err(MediaError::Video("the decoder broke".into()));
            }
            let picture = Decoded {
                sharer,
                picture,
                number: decoded,
            };
            if !self.handoff.offer(picture, &self.stop) {
                break;
            }
        }
        info!(decoded, skipped, errors, "screen share decoder stopped");
        Ok(())
    }

    /// Offers `last` again once the view paints at another size: a still screen's next picture
    /// comes only a second later, and until then a grown view would show the small one stretched.
    /// `false` once the Watch has ended.
    fn repaint(&self, decoder: &mut Decoder, last: &mut Last) -> Result<bool, MediaError> {
        let paint = self.paint.get();
        if paint == last.paint {
            return Ok(true);
        }
        last.paint = paint;
        let mut picture = self.handoff.buffer();
        // The software decoder's picture is gone after a decode that made none.
        if !decoder.copy(&mut picture, paint)? {
            self.handoff.done(picture, Duration::ZERO, false);
            return Ok(true);
        }
        let picture = Decoded {
            sharer: last.sharer.clone(),
            picture,
            number: last.number,
        };
        Ok(self.handoff.offer(picture, &self.stop))
    }

    fn lost(&self) -> u64 {
        self.lost_total.load(Ordering::Relaxed)
    }

    /// Once a window is over, the Session hears what it held. A report that finds the Session's
    /// queue full is dropped: the next one comes a second later.
    fn report(&self, window: &mut Window) {
        let now = Instant::now();
        if now.saturating_duration_since(window.since) < LOAD_EVERY {
            return;
        }
        let lost = self.lost();
        let (convert_busy, ui_dropped) = self.handoff.counts();
        let load = WatchLoad {
            window: now.saturating_duration_since(window.since),
            decode_busy: window.busy,
            convert_busy,
            frames: window.frames,
            pictures: window.pictures,
            lost: lost.saturating_sub(window.lost),
            ui_dropped,
        };
        *window = Window::new(now, lost);
        debug!(
            decode = load.decode_percent(),
            convert = load.convert_percent(),
            frames = load.frames,
            pictures = load.pictures,
            lost = load.lost,
            ui_dropped = load.ui_dropped,
            "watch load"
        );
        self.events.try_send(VoiceEvent::WatchLoad {
            watch: self.watch,
            load,
        });
    }
}

struct Conversion {
    handoff: Arc<Handoff>,
    pictures: mpsc::Sender<VideoFrame>,
    events: VoiceEvents,
    watch: ShareOpId,
    shown: Arc<Shown>,
}

impl Conversion {
    fn run(self) -> Result<(), MediaError> {
        let mut shown: Option<UserId> = None;
        let (mut converted, mut ui_dropped) = (0u64, 0u64);
        while let Some(Decoded {
            sharer,
            picture,
            number,
        }) = self.handoff.take()
        {
            let (width, height, hardware) = (picture.width, picture.height, picture.hardware());
            // The UI's queue is full, so this picture would be dropped: no conversion (up to
            // 33 MB at 2160p). A Sharer's first picture is converted anyway, since it is reported.
            if self.pictures.capacity() == 0 && shown.as_ref() == Some(&sharer) {
                self.handoff.done(picture, Duration::ZERO, true);
                self.shown.set(width, height, hardware);
                ui_dropped += 1;
                continue;
            }
            let started = Instant::now();
            let bgra = picture.to_bgra(CONVERT_BANDS);
            let busy = started.elapsed();
            let bgra = match bgra {
                Ok(bgra) => bgra,
                Err(e) => {
                    self.handoff.done(picture, busy, false);
                    self.handoff.end();
                    return Err(e);
                }
            };
            converted += 1;
            let (painted_w, painted_h) = picture.size();
            let frame = VideoFrame {
                width: painted_w,
                height: painted_h,
                bgra,
                source: (width, height),
                decoded: number,
                sharer: Some(sharer.clone()),
            };
            let sent = self.pictures.try_send(frame);
            let full = matches!(sent, Err(mpsc::error::TrySendError::Full(_)));
            self.handoff.done(picture, busy, full);
            self.shown.set(width, height, hardware);
            match sent {
                Ok(()) if shown.as_ref() != Some(&sharer) => {
                    let kbps = bps_for((width, height)) / 1000;
                    info!(%sharer, "share {width}x{height} @ {kbps} kbps: first picture decoded");
                    shown = Some(sharer.clone());
                    self.events.blocking_send(VoiceEvent::WatchShows {
                        watch: self.watch,
                        sharer,
                    });
                }
                Ok(()) => {}
                // A first picture that found no room is not reported: the next one is.
                Err(mpsc::error::TrySendError::Full(_)) => ui_dropped += 1,
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    self.handoff.end();
                    break;
                }
            }
        }
        info!(converted, ui_dropped, "screen share conversion stopped");
        Ok(())
    }
}

/// The `profile-level-id` (profile_idc, constraint flags, level_idc) of the first SPS in an
/// Annex B access unit.
#[cfg(any(test, feature = "test-support"))]
pub(crate) fn sps_profile_level(au: &[u8]) -> Option<u32> {
    const SPS: u8 = 7;
    let start = au
        .windows(4)
        .position(|w| w[..3] == [0, 0, 1] && w[3] & 0x1f == SPS)?;
    let id = au.get(start + 4..start + 7)?;
    Some(u32::from_be_bytes([0, id[0], id[1], id[2]]))
}

pub(super) fn nal_types(au: &[u8]) -> Vec<u8> {
    au.windows(4)
        .filter(|w| w[..3] == [0, 0, 1])
        .map(|w| w[3] & 0x1f)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    use crate::{SessionEvent, media::VoiceCallId};

    #[cfg(windows)]
    fn keyframe(width: u32) -> Arc<[u8]> {
        match width {
            64 => include_bytes!("testdata/flat-64x64.h264").as_slice(),
            96 => include_bytes!("testdata/flat-96x64.h264"),
            128 => include_bytes!("testdata/flat-128x64.h264"),
            _ => unreachable!("no keyframe {width} wide"),
        }
        .into()
    }

    #[cfg(windows)]
    fn until(what: &str, done: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[cfg(windows)]
    #[test]
    fn a_sharer_shows_once_their_first_picture_is_in_the_feed() {
        let alice: UserId = "usr_000000000000000000000002".parse().unwrap();
        let (pictures, mut feed) = mpsc::channel(1);
        let (events, mut reports) = mpsc::channel::<SessionEvent>(8);
        let behind = VideoFrame {
            width: 1,
            height: 1,
            bgra: vec![0; 4],
            source: (1, 1),
            decoded: 0,
            sharer: None,
        };
        pictures.try_send(behind).unwrap();
        let out = WatchOut {
            pictures,
            paint: PaintSize::default(),
            events: VoiceEvents::new(VoiceCallId(1), events),
            watch: ShareOpId(1),
            #[cfg(feature = "test-support")]
            fails_after: None,
        };
        let (handle, input) = WatchHandle::start(out, alice.clone()).unwrap();
        let send = |width| {
            let frame = VideoIn {
                from: alice.clone(),
                data: keyframe(width),
                keyframe: true,
            };
            input.frames.send(frame).unwrap();
        };
        send(64);
        send(96);
        until("the second picture decoded", || {
            input.shown.get().is_some_and(|(size, _)| size == (96, 64))
        });
        // Loads come once a second whatever is shown.
        let next_report = |reports: &mut mpsc::Receiver<SessionEvent>, wait: bool| loop {
            let report = if wait {
                reports.blocking_recv()
            } else {
                reports.try_recv().ok()
            };
            match report {
                Some(SessionEvent::Voice {
                    event: VoiceEvent::WatchLoad { .. },
                    ..
                }) => continue,
                Some(SessionEvent::Voice { event, .. }) => break Some(event),
                Some(other) => panic!("not a Voice event: {other:?}"),
                None => break None,
            }
        };
        assert_eq!(
            next_report(&mut reports, false),
            None,
            "not shown while the feed is full"
        );

        assert_eq!(feed.try_recv().unwrap().sharer, None);
        send(128);
        let shown = feed.blocking_recv().expect("a picture of alice's");
        assert_eq!((shown.sharer.as_ref(), shown.width), (Some(&alice), 128));
        assert_eq!(
            next_report(&mut reports, true),
            Some(VoiceEvent::WatchShows {
                watch: ShareOpId(1),
                sharer: alice
            })
        );
        drop(input);
        handle.stop().join().unwrap();
    }

    #[test]
    fn a_gap_discards_delta_frames_until_the_keyframe_and_asks_for_one() {
        let t0 = Instant::now();
        let mut g = KeyframeGate::new();
        let early = g.on_frame(true, false, false, t0);
        assert!(!early.forward && early.request_keyframe);
        assert_eq!(
            early.lost, None,
            "nothing is lost before the first keyframe"
        );
        assert!(g.on_frame(true, true, false, t0).forward);
        assert!(g.on_frame(true, false, false, t0).forward);
        let t0 = t0 + Duration::from_secs(1);
        let gap = g.on_frame(false, false, false, t0 + Duration::from_millis(10));
        assert_eq!(
            gap,
            Gate {
                forward: false,
                request_keyframe: true,
                lost: Some(Lost::Network),
            }
        );
        for i in 0..5 {
            let at = t0 + Duration::from_millis(20 + i);
            assert_eq!(
                g.on_frame(true, false, false, at),
                Gate {
                    forward: false,
                    request_keyframe: false,
                    lost: Some(Lost::Network),
                }
            );
        }
        assert!(
            !g.on_frame(false, false, false, t0 + Duration::from_millis(300))
                .request_keyframe
        );
        assert!(
            g.on_frame(false, false, false, t0 + Duration::from_millis(600))
                .request_keyframe
        );
        assert!(
            g.on_frame(true, true, false, t0 + Duration::from_millis(700))
                .forward
        );
        assert!(
            g.on_frame(true, false, false, t0 + Duration::from_millis(733))
                .forward
        );
    }
}
