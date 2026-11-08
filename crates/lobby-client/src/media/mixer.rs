//! The output mix: every slot's 20 ms block summed with its gain, then clamped. In place,
//! so the output callback never allocates.

use super::SAMPLE_RATE;

/// A gain change slides over this many samples (10 ms) and the block holds the new value after
/// it, so a slider or Deafen never clicks and nothing waits a whole block.
pub(crate) const RAMP: usize = SAMPLE_RATE as usize / 100;

/// The gain at sample `i` of a block: `from` sliding to `to` over [`RAMP`] samples.
fn gain_at(from: f32, to: f32, i: usize) -> f32 {
    if i >= RAMP {
        to
    } else {
        from + (to - from) * ((i + 1) as f32 / RAMP as f32)
    }
}

/// `acc += src * gain`, the gain sliding from `from` to `to` over the block's first [`RAMP`]
/// samples.
pub(crate) fn add_gain(acc: &mut [f32], src: &[f32], from: f32, to: f32) {
    if from == to {
        for (a, s) in acc.iter_mut().zip(src) {
            *a += *s * to;
        }
        return;
    }
    for (i, (a, s)) in acc.iter_mut().zip(src).enumerate() {
        *a += *s * gain_at(from, to, i);
    }
}

/// `buf *= gain`, ramped like [`add_gain`].
pub(crate) fn scale(buf: &mut [f32], from: f32, to: f32) {
    if from == 1.0 && to == 1.0 {
        return;
    }
    for (i, s) in buf.iter_mut().enumerate() {
        *s *= gain_at(from, to, i);
    }
}

/// Keeps the mix inside full scale; NaN (a broken decoder) becomes silence.
pub(crate) fn clamp(buf: &mut [f32]) {
    for s in buf {
        *s = if s.is_nan() { 0.0 } else { s.clamp(-1.0, 1.0) };
    }
}
