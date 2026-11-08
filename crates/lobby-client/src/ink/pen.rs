//! The local pen: turns pen down, move and up into the `InkBatch`es the Lobby relays. Pure, with
//! time passed in.

use std::{
    f32::consts::TAU,
    time::{Duration, Instant},
};

use lobby_protocol::{DrawingEpoch, InkBatch, InkPoint, ShareId, StrokeId, limits::INK_POINTS_MAX};

use super::PenPoint;

/// The most a point waits for its batch. 20 batches a second sits well under the Lobby's budget
/// of about 40.
pub const INK_FLUSH_INTERVAL: Duration = Duration::from_millis(50);

/// The cutoff, in Hz, of the pen's smoothing at rest.
const SMOOTH_CUTOFF: f32 = 1.;
/// How much the cutoff rises with speed, in Hz for each picture's width a second.
const SMOOTH_BETA: f32 = 10.;
/// The cutoff, in Hz, of the speed the cutoff follows: lower, and a fast stroke's start lags.
const SPEED_CUTOFF: f32 = 5.;
/// Two moves at one instant are taken as this far apart.
const SMOOTH_STEP_MIN: Duration = Duration::from_millis(1);

/// The pen's position, smoothed: a One Euro filter (Casiez, Roussel and Vogel, 2012), a low-pass
/// filter whose cutoff rises with speed. Both axes share the cutoff, so the smoothing does not
/// bend a line's direction. Positions are fractions of the picture, speeds fractions a second.
#[derive(Debug, Clone, Copy)]
struct Smoothing {
    at: PenPoint,
    speed: (f32, f32),
    when: Instant,
}

impl Smoothing {
    fn new(at: PenPoint, now: Instant) -> Smoothing {
        Smoothing {
            at,
            speed: (0., 0.),
            when: now,
        }
    }

    fn to(&mut self, raw: PenPoint, now: Instant) -> PenPoint {
        let dt = now
            .saturating_duration_since(self.when)
            .max(SMOOTH_STEP_MIN)
            .as_secs_f32();
        let share = |cutoff: f32| dt / (dt + 1. / (TAU * cutoff));
        let toward = |from: f32, to: f32, share: f32| from + (to - from) * share;
        let moved = ((raw.x - self.at.x) / dt, (raw.y - self.at.y) / dt);
        let s = share(SPEED_CUTOFF);
        self.speed = (
            toward(self.speed.0, moved.0, s),
            toward(self.speed.1, moved.1, s),
        );
        let s = share(SMOOTH_CUTOFF + SMOOTH_BETA * self.speed.0.hypot(self.speed.1));
        self.at = PenPoint {
            x: toward(self.at.x, raw.x, s),
            y: toward(self.at.y, raw.y, s),
        };
        self.when = now;
        self.at
    }
}

#[derive(Debug)]
struct Stroke {
    share: ShareId,
    epoch: DrawingEpoch,
    id: StrokeId,
    next_seq: u32,
    /// The last point of the previous batch, for the next batch to start from.
    prev: Option<InkPoint>,
    points: Vec<InkPoint>,
    flush_at: Instant,
    smoothing: Smoothing,
    /// Where the pen last was, ahead of the smoothed line.
    pen: PenPoint,
}

impl Stroke {
    fn last(&self) -> Option<InkPoint> {
        self.points.last().copied().or(self.prev)
    }

    fn cut(&mut self, end: bool, now: Instant) -> Option<InkBatch> {
        if self.points.is_empty() && !end {
            return None;
        }
        let points = std::mem::take(&mut self.points);
        let batch = InkBatch {
            share_id: self.share,
            epoch: self.epoch,
            stroke: self.id,
            seq: self.next_seq,
            prev: self.prev,
            end,
            points,
        };
        self.next_seq += 1;
        self.prev = batch.points.last().copied().or(self.prev);
        self.flush_at = now + INK_FLUSH_INTERVAL;
        Some(batch)
    }
}

#[derive(Debug, Default)]
pub struct InkPen {
    strokes_drawn: u32,
    drawing: Option<Stroke>,
    ready: Vec<InkBatch>,
}

impl InkPen {
    /// The stroke the pen is drawing, with the share and epoch it was started against.
    pub fn drawing(&self) -> Option<(ShareId, DrawingEpoch, StrokeId)> {
        self.drawing.as_ref().map(|s| (s.share, s.epoch, s.id))
    }

    /// Starts a stroke at `at`; one still being drawn ends first. Returns the stroke and its first
    /// point.
    pub fn down(
        &mut self,
        share: ShareId,
        epoch: DrawingEpoch,
        at: PenPoint,
        now: Instant,
    ) -> (StrokeId, InkPoint) {
        self.up(now);
        self.strokes_drawn += 1;
        let id = StrokeId(self.strokes_drawn);
        let point = at.ink();
        self.drawing = Some(Stroke {
            share,
            epoch,
            id,
            next_seq: 0,
            prev: None,
            points: vec![point],
            flush_at: now + INK_FLUSH_INTERVAL,
            smoothing: Smoothing::new(at, now),
            pen: at,
        });
        (id, point)
    }

    /// Moves the pen to `at`. The stroke takes the point smoothed, and rounded as the Lobby
    /// relays it; that point is returned with the stroke, unless the pen is already there.
    pub fn to(&mut self, at: PenPoint, now: Instant) -> Option<(StrokeId, InkPoint)> {
        let stroke = self.drawing.as_mut()?;
        stroke.pen = at;
        let point = stroke.smoothing.to(at, now).ink();
        self.add(point, now)
    }

    /// The button came up at `at`, or where the pen last was when nobody heard where: the line
    /// ends there, not where the smoothing has got to. Returns the point as [`InkPen::to`] does;
    /// [`InkPen::up`] then ends the stroke.
    pub fn ends_at(&mut self, at: Option<PenPoint>, now: Instant) -> Option<(StrokeId, InkPoint)> {
        let at = at.or_else(|| Some(self.drawing.as_ref()?.pen))?;
        self.add(at.ink(), now)
    }

    /// A batch that fills up goes at once.
    fn add(&mut self, point: InkPoint, now: Instant) -> Option<(StrokeId, InkPoint)> {
        let stroke = self.drawing.as_mut()?;
        if stroke.last() == Some(point) {
            return None;
        }
        stroke.points.push(point);
        if stroke.points.len() >= INK_POINTS_MAX {
            self.ready.extend(stroke.cut(false, now));
        }
        Some((stroke.id, point))
    }

    /// Ends the stroke with a batch marked `end`. Returns its id.
    pub fn up(&mut self, now: Instant) -> Option<StrokeId> {
        let mut stroke = self.drawing.take()?;
        self.ready.extend(stroke.cut(true, now));
        Some(stroke.id)
    }

    /// Drops the stroke being drawn without an end: what it was drawn on is gone (a resize, the
    /// end of the share, the connection), so the Lobby would drop its batches anyway.
    pub fn abandon(&mut self) {
        self.drawing = None;
    }

    /// Sends what has waited for the flush interval.
    pub fn flush(&mut self, now: Instant) {
        if let Some(stroke) = self.drawing.as_mut()
            && stroke.flush_at <= now
        {
            self.ready.extend(stroke.cut(false, now));
        }
    }

    /// When `flush` next has something to send.
    pub fn deadline(&self) -> Option<Instant> {
        let stroke = self.drawing.as_ref()?;
        (!stroke.points.is_empty()).then_some(stroke.flush_at)
    }

    /// The batches to send, oldest first.
    pub fn take(&mut self) -> Vec<InkBatch> {
        std::mem::take(&mut self.ready)
    }
}
