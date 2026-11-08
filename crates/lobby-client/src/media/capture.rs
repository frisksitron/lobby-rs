use std::{
    ffi::c_void,
    sync::{Arc, Condvar, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use lobby_protocol::encoder::FRAME_INTERVAL;
use tracing::{debug, info, warn};
use windows_capture::{
    capture::{CaptureControl, Context, GraphicsCaptureApiHandler},
    frame::Frame,
    graphics_capture_api::InternalCaptureControl,
    settings::{CursorCaptureSettings, MinimumUpdateIntervalSettings},
    window::Window,
};

use super::{
    MediaError,
    gpu::{self, Blitter, GpuPicture, Lease, POOL_TEXTURES},
    pattern::{Pattern, Stamp, unix_micros},
    screen::{ScreenSource, SourceId, TestMotion, monitor, start_capture},
    win,
};

/// WGC delivers at most this often (it sends only on change).
const MIN_UPDATE_INTERVAL: Duration = Duration::from_millis(16);
const CAPTURE_STATS_EVERY: Duration = Duration::from_secs(5);
const THIRTY_FPS: Duration = Duration::from_micros(33_334);
const STILL_STOP_POLL: Duration = Duration::from_millis(20);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CaptureEnd {
    Closed,
    Failed(MediaError),
}

/// How a share's capture runs, fixed when the share starts.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct CaptureSetup {
    #[cfg(feature = "test-support")]
    pub faults: CaptureFaults,
}

/// What a test breaks in a share's capture. Pictures count from 0.
#[cfg(feature = "test-support")]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CaptureFaults {
    /// Every picture from this one on fails, as one that cannot be read does.
    pub fail_from: Option<u32>,
}

struct SlotState {
    frame: Option<GpuPicture>,
    ended: Option<CaptureEnd>,
    stop: bool,
    max: (u32, u32),
    woken: bool,
    offered: u64,
    source: Option<(u32, u32)>,
}

pub(crate) struct FrameSlot {
    state: Mutex<SlotState>,
    changed: Condvar,
}

pub(crate) enum Waited {
    Frame(GpuPicture),
    Ended(CaptureEnd),
    Stopped,
    /// [`FrameSlot::wake`] was called.
    Woken,
    Timeout,
}

impl FrameSlot {
    pub fn new(max: (u32, u32)) -> FrameSlot {
        FrameSlot {
            state: Mutex::new(SlotState {
                frame: None,
                ended: None,
                stop: false,
                max,
                woken: false,
                offered: 0,
                source: None,
            }),
            changed: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, SlotState> {
        // A capture thread that panicked while holding the lock left valid data behind.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn deliver(
        &self,
        source: (u32, u32),
        blit: impl FnOnce((u32, u32)) -> Result<Option<Lease>, MediaError>,
    ) -> Result<bool, MediaError> {
        let max = {
            let mut s = self.lock();
            s.offered += 1;
            s.max
        };
        let Some(lease) = blit(max)? else {
            return Ok(false);
        };
        let picture = GpuPicture {
            lease,
            captured: Instant::now(),
        };
        let mut s = self.lock();
        s.source = Some(source);
        s.frame = Some(picture);
        drop(s);
        self.changed.notify_all();
        Ok(true)
    }

    pub fn offered(&self) -> u64 {
        self.lock().offered
    }

    /// The size of what is captured, once a picture came.
    pub fn source(&self) -> Option<(u32, u32)> {
        self.lock().source
    }

    fn end(&self, why: CaptureEnd) {
        self.lock().ended.get_or_insert(why);
        self.changed.notify_all();
    }

    pub fn stop(&self) {
        self.lock().stop = true;
        self.changed.notify_all();
    }

    fn stopped(&self) -> bool {
        self.lock().stop
    }

    pub fn wake(&self) {
        self.lock().woken = true;
        self.changed.notify_all();
    }

    pub fn wait(&self, deadline: Instant) -> Waited {
        let mut s = self.lock();
        loop {
            if s.stop {
                return Waited::Stopped;
            }
            if let Some(frame) = s.frame.take() {
                return Waited::Frame(frame);
            }
            if let Some(why) = s.ended.take() {
                return Waited::Ended(why);
            }
            if std::mem::take(&mut s.woken) {
                return Waited::Woken;
            }
            let now = Instant::now();
            if now >= deadline {
                return Waited::Timeout;
            }
            s = self
                .changed
                .wait_timeout(s, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}

pub(crate) enum Capture {
    Wgc(WgcCapture),
    TestPattern(std::thread::JoinHandle<()>),
}

impl Capture {
    /// Starts capturing `source` into `slot`. Blocks while Windows sets the capture up (tens
    /// of milliseconds), so it runs on the encoder's thread, never the UI's. A source that blocks
    /// capture is refused, and so is a minimized window: Windows would start the capture and
    /// never deliver a picture.
    pub fn start(
        source: &ScreenSource,
        slot: Arc<FrameSlot>,
        setup: CaptureSetup,
    ) -> Result<Capture, MediaError> {
        if source.id.blocks_capture() {
            return Err(MediaError::SourceBlocked(source.name.clone()));
        }
        if let SourceId::Window { hwnd } = source.id
            && win::is_minimized(hwnd as *mut c_void)
        {
            return Err(MediaError::SourceMinimized(source.name.clone()));
        }
        match &source.id {
            SourceId::TestPattern { motion, size, tint } => {
                let (motion, size, tint) = (*motion, *size, *tint);
                let thread = std::thread::Builder::new()
                    .name("lobby-test-pattern".into())
                    .spawn(move || test_pattern(&slot, motion, size, tint, setup))
                    .map_err(|e| MediaError::Capture(e.to_string()))?;
                Ok(Capture::TestPattern(thread))
            }
            id @ (SourceId::Monitor { .. } | SourceId::Window { .. }) => {
                WgcCapture::start(id, slot, setup).map(Capture::Wgc)
            }
            SourceId::TestBlocked => Err(MediaError::SourceBlocked(source.name.clone())),
        }
    }

    pub fn stop(self) {
        match self {
            Capture::Wgc(c) => c.stop(),
            Capture::TestPattern(thread) => {
                if thread.join().is_err() {
                    warn!("test pattern thread panicked");
                }
            }
        }
    }
}

struct CaptureStats {
    /// Since the capture started.
    pictures: u32,
    since: Instant,
    drawn: u32,
    dropped: u32,
    blit: Duration,
}

impl CaptureStats {
    fn new() -> CaptureStats {
        CaptureStats {
            pictures: 0,
            since: Instant::now(),
            drawn: 0,
            dropped: 0,
            blit: Duration::ZERO,
        }
    }

    fn next(&mut self, setup: &CaptureSetup) -> Result<(), MediaError> {
        #[cfg(feature = "test-support")]
        if setup
            .faults
            .fail_from
            .is_some_and(|from| self.pictures >= from)
        {
            return Err(MediaError::Capture(
                "a picture could not be read (a test fault)".into(),
            ));
        }
        #[cfg(not(feature = "test-support"))]
        let _ = setup;
        self.pictures += 1;
        Ok(())
    }

    fn drew(&mut self, blit: Duration, drawn: bool) {
        self.drawn += u32::from(drawn);
        self.dropped += u32::from(!drawn);
        self.blit += blit;
    }

    fn log_if_due(&mut self, (width, height): (u32, u32)) {
        let elapsed = self.since.elapsed();
        if elapsed < CAPTURE_STATS_EVERY {
            return;
        }
        let blit_ms = self.blit.as_secs_f64() * 1000.0 / f64::from(self.drawn.max(1));
        info!(
            width,
            height,
            fps = format_args!("{:.1}", f64::from(self.drawn) / elapsed.as_secs_f64()),
            dropped = self.dropped,
            blit_ms = format_args!("{blit_ms:.2}"),
            "screen capture"
        );
        *self = CaptureStats {
            pictures: self.pictures,
            ..CaptureStats::new()
        };
    }
}

fn test_pattern(
    slot: &FrameSlot,
    motion: TestMotion,
    size: (u32, u32),
    tint: Option<[u8; 3]>,
    setup: CaptureSetup,
) {
    let every = match motion {
        TestMotion::Still => None,
        TestMotion::Thirty => Some(THIRTY_FPS),
        TestMotion::Sixty
        | TestMotion::Scroll
        | TestMotion::ScrollTexture
        | TestMotion::ScrollNoise => Some(FRAME_INTERVAL),
    };
    let gpu = gpu::Upload::new(None, size).and_then(|upload| {
        let blitter = Blitter::new(upload.device(), POOL_TEXTURES)?;
        Ok((upload, blitter))
    });
    let (upload, mut blitter) = match gpu {
        Ok(gpu) => gpu,
        Err(e) => return slot.end(CaptureEnd::Failed(e)),
    };
    let mut pattern = Pattern::new(motion, size, tint);
    let mut stats = CaptureStats::new();
    let mut frame_no = 0u32;
    let mut next = Instant::now();
    info!(?motion, ?size, "test pattern started");
    loop {
        if slot.stopped() {
            break;
        }
        let painted = pattern.paint(frame_no);
        let changed = pattern.stamp(
            painted,
            Stamp {
                frame: frame_no,
                captured_micros: unix_micros(),
            },
        );
        let delivered = stats.next(&setup).and_then(|()| {
            upload.put(pattern.picture(), changed);
            // The upload stands in for the capture: a captured picture arrives on the GPU.
            let arrived = Instant::now();
            let drawn = slot.deliver(size, |max| blitter.blit(upload.texture(), max))?;
            stats.drew(arrived.elapsed(), drawn);
            Ok(())
        });
        if let Err(e) = delivered {
            slot.end(CaptureEnd::Failed(e));
            break;
        }
        stats.log_if_due(size);
        let Some(every) = every else {
            // A still screen: Windows Graphics Capture sends nothing until something changes.
            while !slot.stopped() {
                std::thread::sleep(STILL_STOP_POLL);
            }
            break;
        };
        frame_no += 1;
        next += every;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else {
            next = now;
        }
    }
    debug!(frames = frame_no, "test pattern stopped");
}

#[derive(Clone)]
pub struct Flags {
    slot: Arc<FrameSlot>,
    setup: CaptureSetup,
}

struct Handler {
    slot: Arc<FrameSlot>,
    setup: CaptureSetup,
    blitter: Blitter,
    stats: CaptureStats,
}

impl GraphicsCaptureApiHandler for Handler {
    type Flags = Flags;
    type Error = MediaError;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        let Flags { slot, setup } = ctx.flags;
        Ok(Handler {
            slot,
            setup,
            blitter: Blitter::new(&ctx.device, POOL_TEXTURES)?,
            stats: CaptureStats::new(),
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        if self.slot.stopped() {
            control.stop();
            return Ok(());
        }
        // windows-capture keeps an error it is handed to itself and stops sending pictures,
        // so the share would freeze on its last one: the encoder hears of it instead.
        if let Err(e) = self.picture(frame) {
            warn!(error = %e, "screen capture failed");
            self.slot.end(CaptureEnd::Failed(e));
            control.stop();
        }
        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        info!("the shared window closed");
        self.slot.end(CaptureEnd::Closed);
        Ok(())
    }
}

impl Handler {
    fn picture(&mut self, frame: &mut Frame) -> Result<(), MediaError> {
        let t0 = Instant::now();
        let size = (frame.width(), frame.height());
        self.stats.next(&self.setup)?;
        let texture = frame.as_raw_texture();
        let blitter = &mut self.blitter;
        let drawn = self.slot.deliver(size, |max| blitter.blit(texture, max))?;
        self.stats.drew(t0.elapsed(), drawn);
        self.stats.log_if_due(size);
        Ok(())
    }
}

pub(crate) struct WgcCapture {
    control: CaptureControl<Handler, MediaError>,
}

impl WgcCapture {
    fn start(id: &SourceId, slot: Arc<FrameSlot>, setup: CaptureSetup) -> Result<Self, MediaError> {
        let flags = Flags { slot, setup };
        let (cursor, interval) = (
            CursorCaptureSettings::WithCursor,
            MinimumUpdateIntervalSettings::Custom(MIN_UPDATE_INTERVAL),
        );
        let control = match id {
            SourceId::Monitor { hmonitor } => {
                start_capture::<Handler, _>(monitor(*hmonitor)?, cursor, interval, flags)
            }
            SourceId::Window { hwnd } => {
                let window = Window::from_raw_hwnd(*hwnd as *mut c_void);
                if !window.is_valid() {
                    return Err(MediaError::SourceGone);
                }
                start_capture::<Handler, _>(window, cursor, interval, flags)
            }
            SourceId::TestPattern { .. } | SourceId::TestBlocked => {
                unreachable!("handled by Capture::start")
            }
        }
        .map_err(MediaError::Capture)?;
        Ok(WgcCapture { control })
    }

    fn stop(self) {
        if let Err(e) = self.control.stop() {
            warn!(error = %e, "screen capture did not stop cleanly");
        }
    }
}
