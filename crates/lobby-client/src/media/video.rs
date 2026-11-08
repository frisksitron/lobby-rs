//! Screen Share pictures and their conversions: the Viewer converts the decoded NV12 to BGRA,
//! the format GPUI's `RenderImage` holds.

use std::{
    fmt,
    num::NonZeroUsize,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

#[cfg(any(test, feature = "test-support"))]
use fast_image_resize::{
    FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer,
    images::{Image, ImageRef},
};
use lobby_protocol::UserId;
#[cfg(any(test, feature = "test-support"))]
use lobby_protocol::encoder::fit;
use tokio::sync::mpsc;
#[cfg(any(test, feature = "test-support"))]
use yuvutils_rs::{BufferStoreMut, YuvPlanarImageMut};
use yuvutils_rs::{
    YuvBiPlanarImage, YuvConversionMode, YuvPlanarImage, YuvRange, YuvStandardMatrix,
};

use super::MediaError;

/// The UI keeps only the newest decoded frame it finds, so two are enough.
pub const RENDER_QUEUE: usize = 2;

const RANGE: YuvRange = YuvRange::Limited;
const MATRIX: YuvStandardMatrix = YuvStandardMatrix::Bt709;

/// One decoded Screen Share picture for the UI. A lobby-client type, so the GPUI-free crate
/// never builds a `RenderImage`.
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, B G R A, rows without padding.
    pub bgra: Vec<u8>,
    /// The share's own size, which the picture may be scaled down from: its shape, and the
    /// size the Voice stats name.
    pub source: (u32, u32),
    pub decoded: u64,
    /// Whose share it is a picture of; `None` on this Member's own.
    pub sharer: Option<UserId>,
}

impl fmt::Debug for VideoFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VideoFrame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("source", &self.source)
            .field("decoded", &self.decoded)
            .field("sharer", &self.sharer)
            .finish_non_exhaustive()
    }
}

/// The pictures of a share, and the size the view paints them at.
#[derive(Debug)]
pub struct VideoFeed {
    pub frames: mpsc::Receiver<VideoFrame>,
    pub paint: PaintSize,
}

impl VideoFeed {
    pub(crate) fn channel() -> (mpsc::Sender<VideoFrame>, PaintSize, VideoFeed) {
        let (tx, frames) = mpsc::channel(RENDER_QUEUE);
        let paint = PaintSize::default();
        let feed = VideoFeed {
            frames,
            paint: paint.clone(),
        };
        (tx, paint, feed)
    }
}

/// The size a view paints a share's pictures at, in device pixels, already fitted to their
/// shape. A Watch's decoder scales its pictures down to exactly that on the GPU, so only the
/// pixels on screen are read back and uploaded again. Unset, at least the share's own size, or
/// under `SCALED_MIN` on a side, pictures come at their own size. The Sharer's own view gets
/// pictures already small (`OWN_SHARE_MAX`), whatever it says.
#[derive(Debug, Clone, Default)]
pub struct PaintSize(Arc<AtomicU64>);

impl PaintSize {
    pub fn set(&self, (width, height): (u32, u32)) {
        let packed = (u64::from(width) << 32) | u64::from(height);
        self.0.store(packed, Ordering::Relaxed);
    }

    pub(crate) fn get(&self) -> Option<(u32, u32)> {
        let packed = self.0.load(Ordering::Relaxed);
        let size = ((packed >> 32) as u32, packed as u32);
        (size.0 > 0 && size.1 > 0).then_some(size)
    }
}

/// A tile this small lasts a layout pass at most; its pictures come whole rather than at a size
/// a video processor might refuse.
const SCALED_MIN: u32 = 16;

/// The size a picture of `source` is drawn at for a view that paints it at `paint`: exactly that,
/// to the pixel, so the view draws it unscaled, but never bigger than the picture. `None` when it
/// is drawn at its own size.
pub(crate) fn scaled_for(source: (u32, u32), paint: Option<(u32, u32)>) -> Option<(u32, u32)> {
    let (pw, ph) = paint?;
    let smaller = pw < source.0 || ph < source.1;
    (smaller && pw.min(ph) >= SCALED_MIN).then(|| (pw.min(source.0), ph.min(source.1)))
}

pub(crate) struct I420 {
    pub width: u32,
    pub height: u32,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
}

impl I420 {
    pub fn empty() -> I420 {
        I420 {
            width: 0,
            height: 0,
            y: Vec::new(),
            u: Vec::new(),
            v: Vec::new(),
        }
    }

    pub fn chroma_width(&self) -> u32 {
        self.width.div_ceil(2)
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        let chroma = (width.div_ceil(2) * height.div_ceil(2)) as usize;
        self.y.resize((width * height) as usize, 0);
        self.u.resize(chroma, 0);
        self.v.resize(chroma, 0);
    }
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) struct Converter {
    resizer: Resizer,
    scaled: Option<Image<'static>>,
}

#[cfg(any(test, feature = "test-support"))]
impl Converter {
    pub fn new() -> Converter {
        Converter {
            resizer: Resizer::new(),
            scaled: None,
        }
    }

    pub fn convert(
        &mut self,
        picture: &[u8],
        width: u32,
        height: u32,
        stride: u32,
        max: (u32, u32),
        out: &mut I420,
    ) -> Result<(), MediaError> {
        let (w, h) = fit((width, height), max);
        out.resize(w, h);
        let (pixels, pixels_stride): (&[u8], u32) = if (w, h) == (width, height) {
            (picture, stride)
        } else {
            // The row padding is cropped away: the source is `stride / 4` pixels wide.
            let src = ImageRef::new(stride / 4, height, picture, PixelType::U8x4)
                .map_err(|e| MediaError::Video(format!("source picture: {e}")))?;
            if self
                .scaled
                .as_ref()
                .is_none_or(|s| (s.width(), s.height()) != (w, h))
            {
                self.scaled = Some(Image::new(w, h, PixelType::U8x4));
            }
            let scaled = self.scaled.as_mut().expect("sized above");
            // The fourth byte is often padding (BGRx), and zero: weighted as alpha, the picture
            // would scale to black.
            let options = ResizeOptions::new()
                .use_alpha(false)
                .resize_alg(ResizeAlg::Convolution(FilterType::Bilinear))
                .crop(0.0, 0.0, f64::from(width), f64::from(height));
            self.resizer
                .resize(&src, scaled, &options)
                .map_err(|e| MediaError::Video(format!("scaling: {e}")))?;
            (scaled.buffer(), w * 4)
        };
        let chroma_stride = out.chroma_width();
        let mut planar = YuvPlanarImageMut {
            y_plane: BufferStoreMut::Borrowed(&mut out.y),
            y_stride: w,
            u_plane: BufferStoreMut::Borrowed(&mut out.u),
            u_stride: chroma_stride,
            v_plane: BufferStoreMut::Borrowed(&mut out.v),
            v_stride: chroma_stride,
            width: w,
            height: h,
        };
        yuvutils_rs::bgra_to_yuv420(
            &mut planar,
            pixels,
            pixels_stride,
            RANGE,
            MATRIX,
            YuvConversionMode::Balanced,
        )
        .map_err(|e| MediaError::Video(format!("colour conversion: {e}")))
    }
}

pub(crate) struct Planes<'a> {
    pub width: u32,
    pub height: u32,
    pub y: (&'a [u8], u32),
    pub u: (&'a [u8], u32),
    pub v: (&'a [u8], u32),
}

impl I420 {
    pub fn planes(&self) -> Planes<'_> {
        let chroma = self.chroma_width();
        Planes {
            width: self.width,
            height: self.height,
            y: (&self.y, self.width),
            u: (&self.u, chroma),
            v: (&self.v, chroma),
        }
    }
}

/// An NV12 picture: luma with its row stride, then interleaved chroma in `width`-byte rows.
pub(crate) struct Nv12Planes<'a> {
    pub width: u32,
    pub height: u32,
    pub y: (&'a [u8], u32),
    pub uv: &'a [u8],
}

fn colour(e: yuvutils_rs::YuvError) -> MediaError {
    MediaError::Video(format!("colour conversion: {e}"))
}

pub(crate) fn i420_to_bgra(p: &Planes<'_>, bands: NonZeroUsize) -> Result<Vec<u8>, MediaError> {
    in_bands((p.width, p.height), bands, |first_row, rows, out| {
        let chroma_row = first_row / 2;
        let image = YuvPlanarImage {
            y_plane: &p.y.0[first_row * p.y.1 as usize..],
            y_stride: p.y.1,
            u_plane: &p.u.0[chroma_row * p.u.1 as usize..],
            u_stride: p.u.1,
            v_plane: &p.v.0[chroma_row * p.v.1 as usize..],
            v_stride: p.v.1,
            width: p.width,
            height: rows,
        };
        yuvutils_rs::yuv420_to_bgra(&image, out, p.width * 4, RANGE, MATRIX).map_err(colour)
    })
}

pub(crate) fn nv12_to_bgra(p: &Nv12Planes<'_>, bands: NonZeroUsize) -> Result<Vec<u8>, MediaError> {
    let row = p.width as usize;
    in_bands((p.width, p.height), bands, |first_row, rows, out| {
        let image = YuvBiPlanarImage {
            y_plane: &p.y.0[first_row * p.y.1 as usize..],
            y_stride: p.y.1,
            // Exactly this band's rows: yuvutils wants interleaved chroma to the byte.
            uv_plane: &p.uv[first_row / 2 * row..][..rows.div_ceil(2) as usize * row],
            uv_stride: p.width,
            width: p.width,
            height: rows,
        };
        let mode = YuvConversionMode::Balanced;
        yuvutils_rs::yuv_nv12_to_bgra(&image, out, p.width * 4, RANGE, MATRIX, mode).map_err(colour)
    })
}

/// A BGRA picture of `width` x `height`, filled by `convert` in `bands` row bands at once: the
/// first on this thread, the others on scoped threads beside it. `convert` gets a band's first
/// row, its row count and its bytes.
fn in_bands(
    (width, height): (u32, u32),
    bands: NonZeroUsize,
    convert: impl Fn(usize, u32, &mut [u8]) -> Result<(), MediaError> + Sync,
) -> Result<Vec<u8>, MediaError> {
    let row_bytes = width as usize * 4;
    let mut bgra = vec![0u8; row_bytes * height as usize];
    // Even, so every band starts on a chroma row.
    let band_rows = (height.div_ceil(bands.get() as u32).next_multiple_of(2)).max(2) as usize;
    let convert =
        |first_row: usize, out: &mut [u8]| convert(first_row, (out.len() / row_bytes) as u32, out);
    std::thread::scope(|scope| {
        let mut bands = bgra.chunks_mut(band_rows * row_bytes).enumerate();
        let first = bands.next();
        let others: Vec<_> = bands
            .map(|(i, out)| scope.spawn(move || convert(i * band_rows, out)))
            .collect();
        let first = first.map_or(Ok(()), |(_, out)| convert(0, out));
        others.into_iter().fold(first, |done, band| {
            let band = band
                .join()
                .unwrap_or_else(|_| Err(MediaError::Video("a conversion band panicked".into())));
            done.and(band)
        })
    })?;
    Ok(bgra)
}
