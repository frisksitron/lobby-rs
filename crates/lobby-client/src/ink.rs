//! Ink on a Screen Share, without pixels: the board of strokes a Member sees fade, and the pen
//! that turns local pointer input into batches. Both are pure; the Session feeds them the clock.

mod board;
mod pen;

use lobby_protocol::InkPoint;

pub use board::{
    BOARD_POINTS_MAX, BOARD_STROKES_MAX, INK_END_LOST_AFTER, INK_FADE, INK_HOLD,
    INK_PLAYBACK_DELAY, InkBoard, ShownRun, StrokeView,
};
pub use pen::{INK_FLUSH_INTERVAL, InkPen};

/// The point at `x`, `y` of the source picture, each from 0 (left, top) to 1 (right, bottom).
pub fn point_at(x: f32, y: f32) -> InkPoint {
    let at = |v: f32| (v.clamp(0., 1.) * f32::from(u16::MAX)).round() as u16;
    InkPoint { x: at(x), y: at(y) }
}

/// Where the pointer is on the source picture, before the pen smooths and rounds it: each from 0
/// (left, top) to 1 (right, bottom).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PenPoint {
    pub x: f32,
    pub y: f32,
}

impl PenPoint {
    /// Kept to the picture.
    pub fn new(x: f32, y: f32) -> PenPoint {
        PenPoint {
            x: x.clamp(0., 1.),
            y: y.clamp(0., 1.),
        }
    }

    fn ink(self) -> InkPoint {
        point_at(self.x, self.y)
    }
}
