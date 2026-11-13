//! Thumbnails for the Screen Share picker. The picker asks for the sources it lists; each answers
//! once, with a small BGRA picture or with none, on a channel as it is ready. A worker pool of
//! plain threads takes the pictures (never the UI's thread), a few at a time, and the handle
//! cancels the rest when it is dropped.

#[cfg(windows)]
use std::sync::mpsc as std_mpsc;
use std::{
    collections::VecDeque,
    hash::{DefaultHasher, Hash as _, Hasher as _},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[cfg(windows)]
use fast_image_resize::{
    FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer,
    images::{Image, ImageRef},
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

#[cfg(windows)]
use super::MediaError;
use super::screen::{ScreenSource, SourceId};

/// A thumbnail fits in this box, aspect kept, never enlarged.
const THUMB_MAX: (u32, u32) = (320, 200);
/// How many sources are captured at once.
const THUMB_WORKERS: usize = 3;
/// How long one source may take before it answers "none". A picture normally arrives in well under
/// a tenth of this.
const THUMB_TIMEOUT: Duration = Duration::from_millis(1500);
/// How often a waiting worker looks at the cancellation.
#[cfg(windows)]
const POLL: Duration = Duration::from_millis(20);

/// A small picture of a screen or window: `width * height * 4` bytes, B G R A, rows without
/// padding, alpha always opaque.
#[derive(Clone, PartialEq, Eq)]
pub struct Thumbnail {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

impl std::fmt::Debug for Thumbnail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Thumbnail")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

/// One source's answer. `None` when it cannot give a picture (a minimized window, a capture that
/// failed or took too long): the picker keeps its icon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThumbUpdate {
    pub id: SourceId,
    pub thumbnail: Option<Thumbnail>,
}

#[cfg_attr(
    not(windows),
    expect(dead_code, reason = "only Windows waits on its budget")
)]
pub(crate) struct Budget {
    deadline: Instant,
    cancel: CancellationToken,
}

#[cfg(windows)]
impl Budget {
    /// The next value on `rx`, or `None` at the deadline, when the sender is gone or when the
    /// request was cancelled.
    pub fn wait<T>(&self, rx: &std_mpsc::Receiver<T>) -> Option<T> {
        loop {
            if self.cancel.is_cancelled() {
                return None;
            }
            let left = self.deadline.checked_duration_since(Instant::now())?;
            match rx.recv_timeout(left.min(POLL)) {
                Ok(value) => return Some(value),
                Err(std_mpsc::RecvTimeoutError::Timeout) => {}
                Err(std_mpsc::RecvTimeoutError::Disconnected) => return None,
            }
        }
    }
}

type Grab = dyn Fn(&SourceId, &Budget) -> Option<Thumbnail> + Send + Sync;

/// The thumbnails of one request, arriving on `updates` in the order they are ready. Dropping this
/// cancels the rest. `updates` closes once every source has answered.
pub struct Thumbnails {
    pub updates: mpsc::Receiver<ThumbUpdate>,
    cancel: CancellationToken,
}

impl Drop for Thumbnails {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// Asks for a thumbnail of each of `sources`, in that order, but those that block capture: what
/// such a window shows is not Lobby's to picture. With `test_patterns` (fake media) every source
/// answers with a synthetic picture and nothing on screen is captured.
pub fn request_thumbnails(sources: &[ScreenSource], test_patterns: bool) -> Thumbnails {
    let ids = sources
        .iter()
        .filter(|s| !s.blocked)
        .map(|s| s.id.clone())
        .collect();
    let grab: Arc<Grab> = if test_patterns {
        Arc::new(|id, _| Some(synthetic(id)))
    } else {
        Arc::new(system::grab)
    };
    Thumbnails::spawn(ids, grab)
}

impl Thumbnails {
    pub(crate) fn spawn(ids: Vec<SourceId>, grab: Arc<Grab>) -> Thumbnails {
        let (tx, updates) = mpsc::channel(ids.len().max(1));
        let cancel = CancellationToken::new();
        let workers = THUMB_WORKERS.min(ids.len());
        let queue = Arc::new(Mutex::new(VecDeque::from(ids)));
        let mut started = 0;
        for n in 0..workers {
            let (queue, tx, grab, cancel) =
                (queue.clone(), tx.clone(), grab.clone(), cancel.clone());
            let spawned = std::thread::Builder::new()
                .name(format!("lobby-thumb-{n}"))
                .spawn(move || work(&queue, &tx, &*grab, THUMB_TIMEOUT, &cancel));
            match spawned {
                Ok(_) => started += 1,
                Err(e) => warn!(error = %e, "a thumbnail worker did not start"),
            }
        }
        if started == 0 {
            while let Some(id) = take(&queue) {
                let _ = tx.try_send(ThumbUpdate {
                    id,
                    thumbnail: None,
                });
            }
        }
        Thumbnails { updates, cancel }
    }
}

fn take(queue: &Mutex<VecDeque<SourceId>>) -> Option<SourceId> {
    queue.lock().unwrap_or_else(|e| e.into_inner()).pop_front()
}

fn work(
    queue: &Mutex<VecDeque<SourceId>>,
    tx: &mpsc::Sender<ThumbUpdate>,
    grab: &Grab,
    timeout: Duration,
    cancel: &CancellationToken,
) {
    while !cancel.is_cancelled() {
        let Some(id) = take(queue) else { return };
        let budget = Budget {
            deadline: Instant::now() + timeout,
            cancel: cancel.clone(),
        };
        let started = Instant::now();
        let thumbnail = grab(&id, &budget);
        if cancel.is_cancelled() {
            return;
        }
        debug!(
            ?id,
            got = thumbnail.is_some(),
            ms = started.elapsed().as_millis() as u64,
            "thumbnail"
        );
        // The channel holds one answer per source, so this never finds it full.
        let _ = tx.try_send(ThumbUpdate { id, thumbnail });
    }
}

/// The size a `width` × `height` picture shrinks to: inside [`THUMB_MAX`], aspect kept.
#[cfg(windows)]
fn thumb_size(width: u32, height: u32) -> (u32, u32) {
    let scale = (f64::from(THUMB_MAX.0) / f64::from(width.max(1)))
        .min(f64::from(THUMB_MAX.1) / f64::from(height.max(1)))
        .min(1.0);
    let side = |v: u32| ((f64::from(v) * scale).round() as u32).max(1);
    (side(width), side(height))
}

/// Scales a captured BGRA picture whose rows are `stride` bytes apart down to a thumbnail.
#[cfg(windows)]
pub(crate) fn shrink(
    resizer: &mut Resizer,
    bgra: &[u8],
    width: u32,
    height: u32,
    stride: u32,
) -> Result<Thumbnail, MediaError> {
    let (w, h) = thumb_size(width, height);
    let (row, rows) = ((width * 4) as usize, height as usize);
    // Cropping a padded source would still let the scaler read the padding at the right edge, so
    // padded rows are packed first.
    let packed: Vec<u8>;
    let pixels = if stride as usize == row {
        bgra.get(..row * rows)
    } else {
        packed = bgra
            .chunks(stride as usize)
            .take(rows)
            .flat_map(|r| r.get(..row))
            .flatten()
            .copied()
            .collect();
        Some(&packed[..])
    };
    let src = pixels
        .and_then(|p| ImageRef::new(width, height, p, PixelType::U8x4).ok())
        .ok_or_else(|| {
            MediaError::Video(format!("a {width}x{height} picture of the wrong size"))
        })?;
    let mut out = Image::new(w, h, PixelType::U8x4);
    // The fourth byte is often padding, and zero: weighted as alpha, the picture would go black.
    let options = ResizeOptions::new()
        .use_alpha(false)
        .resize_alg(ResizeAlg::Convolution(FilterType::Bilinear));
    resizer
        .resize(&src, &mut out, &options)
        .map_err(|e| MediaError::Video(format!("scaling: {e}")))?;
    let mut bgra = out.into_vec();
    for px in bgra.chunks_exact_mut(4) {
        px[3] = 255;
    }
    Ok(Thumbnail {
        width: w,
        height: h,
        bgra,
    })
}

/// A made-up picture for `id`: a gradient whose hue follows the source, with a lighter window
/// shape on it. For fake media and the gallery, so nothing real is ever captured.
pub fn synthetic(id: &SourceId) -> Thumbnail {
    let mut hasher = DefaultHasher::new();
    id.hash(&mut hasher);
    let hue = (hasher.finish() % 360) as f32;
    let (w, h) = (THUMB_MAX.0, THUMB_MAX.0 * 9 / 16);
    let channel = |offset: f32, t: f32| -> u8 {
        let phase = ((hue + offset) % 360.0) / 60.0;
        let wave = (phase - 3.0).abs() - 1.0;
        let base = wave.clamp(0.0, 1.0);
        (30.0 + base * 90.0 * (0.6 + 0.4 * t)) as u8
    };
    let mut bgra = vec![255u8; (w * h * 4) as usize];
    for (i, px) in bgra.chunks_exact_mut(4).enumerate() {
        let (x, y) = (i as u32 % w, i as u32 / w);
        let t = (x + y) as f32 / (w + h) as f32;
        let inside = x > w / 8 && x < w * 7 / 8 && y > h / 6 && y < h * 5 / 6;
        let title = inside && y < h / 6 + h / 10;
        let lift = if title {
            70
        } else if inside {
            40
        } else {
            0
        };
        px[0] = channel(240.0, t).saturating_add(lift);
        px[1] = channel(120.0, t).saturating_add(lift);
        px[2] = channel(0.0, t).saturating_add(lift);
    }
    Thumbnail {
        width: w,
        height: h,
        bgra,
    }
}

#[cfg(windows)]
mod system {
    use std::{ffi::c_void, sync::mpsc};

    use fast_image_resize::Resizer;
    use tracing::debug;
    use windows_capture::{
        capture::{Context, GraphicsCaptureApiHandler},
        frame::Frame,
        graphics_capture_api::InternalCaptureControl,
        settings::{CursorCaptureSettings, GraphicsCaptureItemType, MinimumUpdateIntervalSettings},
        window::Window,
    };

    use super::{Budget, Thumbnail, shrink, synthetic};
    use crate::media::{
        MediaError,
        screen::{SourceId, monitor, start_capture},
        win,
    };

    /// Nothing but black: what `PrintWindow` leaves of a window that draws itself on the GPU in a
    /// way it cannot ask.
    fn is_blank(t: &Thumbnail) -> bool {
        t.bgra.chunks_exact(4).all(|px| px[..3] == [0, 0, 0])
    }

    pub fn grab(id: &SourceId, budget: &Budget) -> Option<Thumbnail> {
        match id {
            SourceId::Monitor { hmonitor } => wgc(monitor(*hmonitor).ok()?, budget),
            SourceId::Window { hwnd } => {
                let raw = *hwnd as *mut c_void;
                if win::is_minimized(raw) {
                    return None;
                }
                print_window(raw)
                    .filter(|t| !is_blank(t))
                    .or_else(|| wgc_window(raw, budget))
            }
            SourceId::TestPattern { .. } => Some(synthetic(id)),
            SourceId::TestBlocked => None,
        }
    }

    fn print_window(hwnd: *mut c_void) -> Option<Thumbnail> {
        let (w, h, bgra) = win::print_window(hwnd)?;
        shrink(&mut Resizer::new(), &bgra, w, h, w * 4).ok()
    }

    struct OneFrame {
        sent: Option<mpsc::SyncSender<Thumbnail>>,
        resizer: Resizer,
    }

    impl GraphicsCaptureApiHandler for OneFrame {
        type Flags = mpsc::SyncSender<Thumbnail>;
        type Error = MediaError;

        fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
            Ok(OneFrame {
                sent: Some(ctx.flags),
                resizer: Resizer::new(),
            })
        }

        fn on_frame_arrived(
            &mut self,
            frame: &mut Frame,
            control: InternalCaptureControl,
        ) -> Result<(), Self::Error> {
            if let Some(sent) = self.sent.take() {
                let (width, height) = (frame.width(), frame.height());
                let mut buffer = frame
                    .buffer()
                    .map_err(|e| MediaError::Capture(format!("frame readback: {e}")))?;
                let stride = buffer.row_pitch();
                let thumbnail = shrink(
                    &mut self.resizer,
                    buffer.as_raw_buffer(),
                    width,
                    height,
                    stride,
                )?;
                let _ = sent.send(thumbnail);
            }
            control.stop();
            Ok(())
        }
    }

    fn wgc_window(hwnd: *mut c_void, budget: &Budget) -> Option<Thumbnail> {
        let window = Window::from_raw_hwnd(hwnd);
        window.is_valid().then(|| wgc(window, budget)).flatten()
    }

    fn wgc<T>(item: T, budget: &Budget) -> Option<Thumbnail>
    where
        T: TryInto<GraphicsCaptureItemType> + Send + Clone + 'static,
    {
        let (tx, rx) = mpsc::sync_channel(1);
        let control = start_capture::<OneFrame, T>(
            item,
            CursorCaptureSettings::WithoutCursor,
            MinimumUpdateIntervalSettings::Default,
            tx,
        )
        .ok()?;
        let got = budget.wait(&rx);
        if let Err(e) = control.stop() {
            debug!(error = %e, "thumbnail capture did not stop cleanly");
        }
        got
    }
}

#[cfg(not(windows))]
mod system {
    use super::{Budget, Thumbnail};
    use crate::media::screen::SourceId;

    /// The desktop's own dialog picks what is shared, and there is nothing to list.
    pub fn grab(_: &SourceId, _: &Budget) -> Option<Thumbnail> {
        None
    }
}

#[cfg(all(test, windows))]
mod probe {
    use std::time::Instant;

    use super::*;
    use crate::media::screen::list_sources;

    fn luma(t: &Thumbnail) -> (f64, f64) {
        let n = (t.width * t.height) as f64;
        let (mut sum, mut lit) = (0.0, 0.0);
        for px in t.bgra.chunks_exact(4) {
            let y =
                0.0722 * f64::from(px[0]) + 0.7152 * f64::from(px[1]) + 0.2126 * f64::from(px[2]);
            sum += y;
            if y > 8.0 {
                lit += 1.0;
            }
        }
        (sum / n, lit / n)
    }

    /// Captures this machine's real monitors and windows once through the pool and prints when
    /// each answered. Nothing is written to disk.
    #[test]
    #[ignore = "captures this machine's screens and windows"]
    fn real_capture_report() {
        let sources = list_sources(false).unwrap();
        let by_id: std::collections::HashMap<_, _> = sources
            .iter()
            .map(|s| (s.id.clone(), s.name.clone()))
            .collect();
        let started = Instant::now();
        let mut thumbs = request_thumbnails(&sources, false);
        let (mut answered, mut got) = (0, 0);
        while let Some(update) = thumbs.updates.blocking_recv() {
            answered += 1;
            got += usize::from(update.thumbnail.is_some());
            let (name, how) = (&by_id[&update.id], started.elapsed().as_millis());
            match &update.thumbnail {
                Some(t) => {
                    let (mean, lit) = luma(t);
                    println!(
                        "{how:>5} ms  {:36.36} {}x{} mean {mean:.0} lit {lit:.2}",
                        name, t.width, t.height
                    );
                }
                None => println!("{how:>5} ms  {name:36.36} none"),
            }
        }
        println!(
            "{answered} answered, {got} with a picture, {} ms for {} sources with {THUMB_WORKERS} workers",
            started.elapsed().as_millis(),
            sources.len()
        );
    }
}
