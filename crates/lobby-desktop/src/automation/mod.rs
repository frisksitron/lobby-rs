//! The verification harness's app side, behind the `automation` feature: the scenario language,
//! the dump a predicate reads, synthetic input and element crops. The UI tests and the automation
//! channel share it, so a scenario proves the same thing in both.

// Synthetic input and test observation have no place in a shipped build.
#[cfg(not(debug_assertions))]
compile_error!(
    "the `automation` feature is for dev builds only: it drives the app without the person \
    . Build releases without it."
);

mod input;
mod model;
#[cfg(windows)]
pub mod pipe;
pub mod scenario;

use gpui_kit::{Bounds, Pixels};
use image::RgbaImage;

pub use input::{
    StepError, WindowStep, clickable, element_bounds, perform, resize_target, rest_pointer,
};
pub use model::{dump, elements};

/// GPUI's clock as the Sessions' clock, for the tests: `advance_clock` then moves a Session's
/// deadlines with the views' timers, and the instants a Session hands the views (a cooldown's
/// end, a stroke's fade) are on the clock the views compare them with.
pub struct GpuiClock(gpui_kit::BackgroundExecutor);

impl GpuiClock {
    pub fn of(executor: gpui_kit::BackgroundExecutor) -> std::sync::Arc<GpuiClock> {
        std::sync::Arc::new(GpuiClock(executor))
    }
}

impl lobby_client::Clock for GpuiClock {
    fn now(&self) -> std::time::Instant {
        self.0.now()
    }

    fn sleep_until(
        &self,
        at: std::time::Instant,
    ) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(self.0.timer(at.saturating_duration_since(self.0.now())))
    }
}

/// Every Join's media fails to start with this error, as a media thread that cannot be spawned
/// does. Set on the app by a test; read at every Join.
#[derive(Debug, Clone)]
pub struct StartFault(pub lobby_client::MediaError);

impl gpui_kit::Global for StartFault {}

pub fn stage_faults(media: &mut lobby_client::MediaConfig, cx: &gpui_kit::App) {
    if let Some(StartFault(error)) = cx.try_global::<StartFault>() {
        media.start_fails = Some(error.clone());
    }
}

pub fn script_host(
    picks: Vec<Vec<std::path::PathBuf>>,
    save_dir: Option<std::path::PathBuf>,
    cx: &mut gpui_kit::App,
) -> Result<(), String> {
    let recorder = cx
        .try_global::<crate::files::FileRecorder>()
        .ok_or("this app has no file recorder")?;
    let mut log = recorder.0.borrow_mut();
    log.picks = picks.into();
    log.save_dir = save_dir;
    Ok(())
}

pub fn clipboard_image(path: &std::path::Path) -> Result<gpui_kit::ClipboardItem, String> {
    use gpui_kit::{Image, ImageFormat};
    let format = match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => ImageFormat::Png,
        Some("jpg" | "jpeg") => ImageFormat::Jpeg,
        Some("webp") => ImageFormat::Webp,
        Some("gif") => ImageFormat::Gif,
        Some("bmp") => ImageFormat::Bmp,
        _ => return Err(format!("{} is not a picture file", path.display())),
    };
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Ok(gpui_kit::ClipboardItem::new_image(&Image::from_bytes(
        format, bytes,
    )))
}

pub fn clipboard_files(paths: &[std::path::PathBuf]) -> gpui_kit::ClipboardItem {
    let mut files = gpui_kit::ExternalPaths::default();
    files.0.extend(paths.iter().cloned());
    gpui_kit::ClipboardItem {
        entries: vec![gpui_kit::ClipboardEntry::ExternalPaths(files)],
    }
}

pub fn crop(image: &RgbaImage, bounds: Bounds<Pixels>, scale: f32) -> RgbaImage {
    let to_px = |v: Pixels| (f32::from(v) * scale).round().max(0.) as u32;
    let x = to_px(bounds.origin.x).min(image.width());
    let y = to_px(bounds.origin.y).min(image.height());
    let w = to_px(bounds.size.width).min(image.width() - x);
    let h = to_px(bounds.size.height).min(image.height() - y);
    let part = image::imageops::crop_imm(image, x, y, w, h).to_image();
    downscale(&part, scale)
}

pub fn downscale(image: &RgbaImage, scale: f32) -> RgbaImage {
    let k = scale.round() as u32;
    if k <= 1 || (scale - k as f32).abs() > 0.01 {
        return image.clone();
    }
    let (w, h) = (image.width() / k, image.height() / k);
    RgbaImage::from_fn(w, h, |x, y| {
        let mut sum = [0u32; 4];
        for dy in 0..k {
            for dx in 0..k {
                let p = image.get_pixel(x * k + dx, y * k + dy).0;
                for c in 0..4 {
                    sum[c] += u32::from(p[c]);
                }
            }
        }
        let n = k * k;
        image::Rgba(sum.map(|s| ((s + n / 2) / n) as u8))
    })
}
