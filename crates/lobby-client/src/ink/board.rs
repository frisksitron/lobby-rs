//! The ink on one Screen Share. Pure: no IO, and time comes in as an argument, so a test moves the
//! clock by hand.
//!
//! Someone else's stroke comes in batches about a flush interval apart. Shown as they come, the
//! line would grow in jumps, so the board plays each batch's points out one by one, a little
//! behind their arrival, and the line's tip moves between them every frame.

use std::time::{Duration, Instant};

use lobby_protocol::{DrawingEpoch, InkBatch, InkPoint, ShareId, StrokeId, UserId};

use super::INK_FLUSH_INTERVAL;

/// A stroke stays fully drawn this long after the pen lifts.
pub const INK_HOLD: Duration = Duration::from_secs(2);
/// Then it fades out over this long.
pub const INK_FADE: Duration = Duration::from_secs(1);
/// A stroke that goes this long without a batch counts as lifted at its last batch. A held pen
/// that sits still sends nothing, so this has to outlast a pause, and it bounds how long a lost
/// end can keep ink on screen.
pub const INK_END_LOST_AFTER: Duration = Duration::from_secs(5);
pub const BOARD_STROKES_MAX: usize = 64;
pub const BOARD_POINTS_MAX: usize = 4096;
/// A relayed batch has played out this long after it arrives: a flush interval to play its
/// points over, and 50 ms for the next batch to come late in.
pub const INK_PLAYBACK_DELAY: Duration = Duration::from_millis(100);

/// What one stroke looks like at an instant.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StrokeView<'a> {
    pub author: &'a UserId,
    pub id: StrokeId,
    /// 1.0 until the fade starts, then down to 0.0 (a stroke at 0.0 is not listed).
    pub opacity: f32,
    /// The pen came up and the line has played out.
    pub lifted: bool,
    runs: &'a [Run],
    now: Instant,
}

impl<'a> StrokeView<'a> {
    /// The line as it shows now, in separate runs where batches were lost. A run none of whose
    /// points are due yet is left out.
    pub fn runs(&self) -> impl Iterator<Item = ShownRun<'a>> + use<'a> {
        let now = self.now;
        self.runs.iter().filter_map(move |run| run.shown(now))
    }
}

/// A run as it shows at an instant: the points due by then, and how far the line has got
/// toward the next one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShownRun<'a> {
    pub points: &'a [InkPoint],
    /// Past the last point, in the same units.
    pub tip: Option<(f32, f32)>,
}

/// One unbroken stretch of a stroke, and when each of its points shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Run {
    points: Vec<InkPoint>,
    /// One for each point, in order of time.
    times: Vec<Span>,
}

/// The line sets off toward a point `from`, and gets there when it is `due`. `from` is the due
/// time of the point before, unless the line had stopped there: then it is when it moved on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Span {
    from: Instant,
    due: Instant,
}

impl Run {
    fn push(&mut self, point: InkPoint, at: Instant) {
        self.points.push(point);
        self.times.push(Span { from: at, due: at });
    }

    /// How many points are due by `now`.
    fn due(&self, now: Instant) -> usize {
        self.times.partition_point(|span| span.due <= now)
    }

    fn shown(&self, now: Instant) -> Option<ShownRun<'_>> {
        let shown = self.due(now);
        let last = *self.points.get(shown.checked_sub(1)?)?;
        let tip = self
            .points
            .get(shown)
            .zip(self.times.get(shown))
            .filter(|(_, span)| span.from < now)
            .map(|(&next, span)| {
                let t = (now - span.from).as_secs_f32() / (span.due - span.from).as_secs_f32();
                let along = |a: u16, b: u16| f32::from(a) + (f32::from(b) - f32::from(a)) * t;
                (along(last.x, next.x), along(last.y, next.y))
            });
        Some(ShownRun {
            points: &self.points[..shown],
            tip,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Stroke {
    author: UserId,
    id: StrokeId,
    runs: Vec<Run>,
    /// The `seq` the next relayed batch must carry to continue the line.
    next_seq: u32,
    /// When the pen came up and the line had played out.
    lifted: Option<Instant>,
    heard: Instant,
    /// Drawn by the board's owner. The relay never adds to it.
    local: bool,
}

impl Stroke {
    fn new(author: &UserId, id: StrokeId, now: Instant, local: bool) -> Stroke {
        Stroke {
            author: author.clone(),
            id,
            runs: Vec::new(),
            next_seq: 0,
            lifted: None,
            heard: now,
            local,
        }
    }

    fn points(&self) -> usize {
        self.runs.iter().map(|run| run.points.len()).sum()
    }

    /// When the last point shows.
    fn played(&self) -> Instant {
        self.runs
            .iter()
            .rev()
            .find_map(|run| run.times.last())
            .map_or(self.heard, |span| span.due)
    }

    /// When the pen lifted and the line had played out, or when a lost end is taken for that.
    fn ended(&self) -> Instant {
        self.lifted
            .unwrap_or_else(|| self.played() + INK_END_LOST_AFTER)
    }

    fn opacity(&self, now: Instant) -> f32 {
        let since = now.saturating_duration_since(self.ended());
        if since <= INK_HOLD {
            1.0
        } else {
            let faded = (since - INK_HOLD).as_secs_f32() / INK_FADE.as_secs_f32();
            (1.0 - faded).max(0.0)
        }
    }

    fn is(&self, author: &UserId, id: StrokeId) -> bool {
        self.id == id && &self.author == author
    }

    /// Relayed `points` that arrived at `now`, `fresh` when they start a run of their own. They
    /// play out by `now + INK_PLAYBACK_DELAY`. The segment showing at `now` keeps its pace, so
    /// the tip never steps back; every point after it, new or still waiting, shares out the time
    /// left evenly, so batches that come together catch up instead of queueing. A run's first
    /// point takes no time: there is no segment to it.
    fn play(&mut self, points: &[InkPoint], fresh: bool, now: Instant) {
        let end = now + INK_PLAYBACK_DELAY;
        let waiting = self.runs.iter().enumerate().find_map(|(r, run)| {
            let i = run.due(now);
            (i < run.points.len()).then_some((r, i))
        });
        if fresh {
            self.runs.push(Run::default());
        }
        let r = self.runs.len() - 1;
        let added = self.runs[r].points.len();
        for point in points {
            self.runs[r].push(*point, end);
        }
        // With nothing waiting, the batch plays over the last flush interval before `end`, so
        // the next batch, due about then, finds points still to play.
        let ((r, i), start) = match waiting {
            Some((r, i)) => ((r, i + 1), self.runs[r].times[i].due),
            None => ((r, added), end - INK_FLUSH_INTERVAL),
        };
        let replayed = |n: usize, run: &Run| (if n == 0 { i } else { 0 })..run.points.len();
        let steps: usize = self.runs[r..]
            .iter()
            .enumerate()
            .map(|(n, run)| replayed(n, run).filter(|&at| at > 0).count())
            .sum();
        let step = end.saturating_duration_since(start) / steps.max(1) as u32;
        let mut due = start;
        for (n, run) in self.runs[r..].iter_mut().enumerate() {
            for at in replayed(n, run) {
                let from = due;
                if at > 0 {
                    due += step;
                }
                run.times[at] = Span { from, due };
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InkBoard {
    share: ShareId,
    epoch: DrawingEpoch,
    /// Oldest first, which is also the paint order.
    strokes: Vec<Stroke>,
}

impl InkBoard {
    pub fn new(share: ShareId, epoch: DrawingEpoch) -> InkBoard {
        InkBoard {
            share,
            epoch,
            strokes: Vec::new(),
        }
    }

    pub fn share(&self) -> ShareId {
        self.share
    }

    pub fn epoch(&self) -> DrawingEpoch {
        self.epoch
    }

    /// Whether this board is for `share` at `epoch`. A board for any other state is cleared.
    pub fn is_for(&self, share: ShareId, epoch: DrawingEpoch) -> bool {
        self.share == share && self.epoch == epoch
    }

    pub fn is_empty(&self) -> bool {
        self.strokes.is_empty()
    }

    pub fn len(&self) -> usize {
        self.strokes.len()
    }

    pub fn points(&self) -> usize {
        self.strokes.iter().map(Stroke::points).sum()
    }

    /// A relayed batch, arrived at `now`. A batch that continues the stroke extends its line. One
    /// after a gap in `seq` starts a new run from its own `prev`, so no segment joins across what
    /// was lost. A repeated or late batch changes nothing. Returns whether the batch belonged
    /// here.
    pub fn draw(&mut self, author: &UserId, batch: &InkBatch, now: Instant) -> bool {
        if !self.is_for(batch.share_id, batch.epoch) {
            return false;
        }
        let at = match self.strokes.iter().position(|s| s.is(author, batch.stroke)) {
            Some(at) => at,
            None => {
                self.strokes
                    .push(Stroke::new(author, batch.stroke, now, false));
                self.strokes.len() - 1
            }
        };
        let stroke = &mut self.strokes[at];
        if stroke.local || batch.seq < stroke.next_seq {
            return true;
        }
        if batch.seq == stroke.next_seq && !stroke.runs.is_empty() {
            if !batch.points.is_empty() {
                stroke.play(&batch.points, false, now);
            }
        } else if !batch.points.is_empty() || batch.prev.is_some() {
            let points: Vec<InkPoint> = batch
                .prev
                .into_iter()
                .chain(batch.points.iter().copied())
                .collect();
            stroke.play(&points, true, now);
        }
        stroke.next_seq = batch.seq.saturating_add(1);
        stroke.heard = now;
        if batch.end {
            // A pen held still before it came up has long played out.
            stroke.lifted = Some(stroke.played().max(now));
        }
        self.keep_within_limits(author, batch.stroke);
        true
    }

    /// A point the board's owner just drew, painted at once. The relay of the same stroke is
    /// ignored from then on.
    pub fn echo(&mut self, author: &UserId, id: StrokeId, point: InkPoint, now: Instant) {
        let at = match self.strokes.iter().position(|s| s.is(author, id)) {
            Some(at) => at,
            None => {
                self.strokes.push(Stroke::new(author, id, now, true));
                self.strokes.len() - 1
            }
        };
        let stroke = &mut self.strokes[at];
        stroke.local = true;
        stroke.heard = now;
        if stroke.runs.is_empty() {
            stroke.runs.push(Run::default());
        }
        let run = stroke.runs.last_mut().expect("pushed above if missing");
        run.push(point, now);
        self.keep_within_limits(author, id);
    }

    pub fn lift(&mut self, author: &UserId, id: StrokeId, now: Instant) {
        if let Some(stroke) = self.strokes.iter_mut().find(|s| s.is(author, id)) {
            stroke.lifted = Some(now);
        }
    }

    /// Drops the oldest strokes while the board is over its limits. The stroke just touched stays;
    /// if it is alone and still too long, its oldest points go.
    fn keep_within_limits(&mut self, author: &UserId, id: StrokeId) {
        while self.strokes.len() > BOARD_STROKES_MAX || self.points() > BOARD_POINTS_MAX {
            let oldest = self
                .strokes
                .iter()
                .enumerate()
                .filter(|(_, s)| !s.is(author, id))
                .min_by_key(|(at, s)| (s.heard, *at))
                .map(|(at, _)| at);
            let Some(oldest) = oldest else {
                self.trim_head(author, id);
                return;
            };
            self.strokes.remove(oldest);
        }
    }

    fn trim_head(&mut self, author: &UserId, id: StrokeId) {
        let mut over = self.points().saturating_sub(BOARD_POINTS_MAX);
        let Some(stroke) = self.strokes.iter_mut().find(|s| s.is(author, id)) else {
            return;
        };
        while over > 0 {
            let Some(run) = stroke.runs.first_mut() else {
                break;
            };
            let cut = over.min(run.points.len());
            run.points.drain(..cut);
            run.times.drain(..cut);
            over -= cut;
            if run.points.is_empty() {
                stroke.runs.remove(0);
            }
        }
    }

    /// Forgets the strokes that faded out.
    pub fn prune(&mut self, now: Instant) {
        self.strokes.retain(|s| s.opacity(now) > 0.0);
    }

    /// The strokes to paint at `now`, oldest first.
    pub fn strokes(&self, now: Instant) -> Vec<StrokeView<'_>> {
        self.strokes
            .iter()
            .filter_map(|s| {
                let opacity = s.opacity(now);
                (opacity > 0.0).then_some(StrokeView {
                    author: &s.author,
                    id: s.id,
                    opacity,
                    lifted: s.ended() <= now,
                    runs: &s.runs,
                    now,
                })
            })
            .collect()
    }
}
