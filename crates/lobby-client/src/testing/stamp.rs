//! The test pattern's pictures after they crossed a share: the stamp each carries, the source
//! picture it came from, and how close the two are.

pub use crate::media::pattern::{Stamp, busy_region};
use crate::{
    VideoFrame,
    media::{
        pattern::{Pattern, STAMP_CELLS, cell_columns, stamp_rows},
        video::{Converter, I420},
    },
};

/// The stamp in `frame`'s top edge, whatever size the share's quality scaled the picture to;
/// `None` when it is damaged past its check.
pub fn read_stamp(frame: &VideoFrame) -> Option<Stamp> {
    let (width, rows) = (frame.width, stamp_rows(frame.width));
    if frame.height < rows || frame.bgra.len() < (width * frame.height * 4) as usize {
        return None;
    }
    let (rows_from, rows_to) = (rows / 4, (rows - rows / 4).max(rows / 4 + 1));
    let mut cells = [false; STAMP_CELLS];
    for (i, cell) in cells.iter_mut().enumerate() {
        let columns = cell_columns(width, i);
        let inset = (columns.len() / 4) as u32;
        let (from, to) = (
            columns.start + inset,
            (columns.end - inset).max(columns.start + 1),
        );
        let (mut sum, mut n) = (0u32, 0u32);
        for row in rows_from..rows_to {
            let at = (row * width + from) as usize * 4;
            for px in frame.bgra[at..at + (to - from) as usize * 4].chunks_exact(4) {
                sum += u32::from(px[0]) + u32::from(px[1]) + u32::from(px[2]);
                n += 3;
            }
        }
        *cell = sum / n.max(1) > 127;
    }
    Stamp::from_cells(&cells)
}

/// A scroll workload's source pictures, drawn as the share's capture draws them.
pub struct ScrollSource(Pattern);

impl ScrollSource {
    pub fn new(motion: crate::TestMotion, size: (u32, u32)) -> ScrollSource {
        ScrollSource(Pattern::new(motion, size, None))
    }

    /// The BGRA picture `stamp` names, stamped as it was.
    pub fn picture(&mut self, stamp: Stamp) -> &[u8] {
        let painted = self.0.paint(stamp.frame);
        self.0.stamp(painted, stamp);
        self.0.picture()
    }
}

/// PSNR in dB of the luma of two BGRA pictures of one size, converted as a share converts its
/// pictures (BT.709, limited range). Infinite for identical ones.
pub fn luma_psnr(source: &[u8], seen: &[u8], size: (u32, u32)) -> f64 {
    luma_psnr_in(source, seen, size, (0, 0, size.0, size.1))
}

/// [`luma_psnr`] over the rectangle `(left, top, width, height)` of the pictures alone.
pub fn luma_psnr_in(
    source: &[u8],
    seen: &[u8],
    (width, height): (u32, u32),
    (left, top, w, h): (u32, u32, u32, u32),
) -> f64 {
    let luma = |bgra: &[u8]| {
        let mut yuv = I420::empty();
        Converter::new()
            .convert(bgra, width, height, width * 4, (width, height), &mut yuv)
            .expect("a picture of this size converts");
        yuv.y
    };
    let (a, b) = (luma(source), luma(seen));
    let squared: u64 = (top..top + h)
        .map(|row| {
            let at = (row * width + left) as usize..(row * width + left + w) as usize;
            a[at.clone()]
                .iter()
                .zip(&b[at])
                .map(|(&a, &b)| u64::from(a.abs_diff(b)).pow(2))
                .sum::<u64>()
        })
        .sum();
    let mse = squared as f64 / f64::from(w * h);
    10.0 * (255.0 * 255.0 / mse).log10()
}
