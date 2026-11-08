//! Ink over a Screen Share's picture: each Member's strokes in their own colour, fading as the
//! board says, a name tag at the tip of a stroke being drawn, and the pen that turns the pointer
//! on the picture into strokes.

use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, HashMap},
    f32::consts::FRAC_1_SQRT_2,
    rc::Rc,
    time::Instant,
};

use gpui_kit::{
    App, Bounds, CursorStyle, DispatchPhase, Hitbox, HitboxBehavior, Hsla, InteractiveElement as _,
    IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _,
    Path, PathBuilder, PathStyle, Pixels, Point, SharedString, Size, StrokeOptions, Styled as _,
    TestSupportExt as _, Window, canvas, component::oklch, div, hsla, point, px,
};
use lobby_client::ink::{InkBoard, PenPoint};
use lobby_protocol::{DrawingEpoch, ShareId, StrokeId, UserId};
use lyon::tessellation::{LineCap, LineJoin};

use super::{
    Callback,
    avatar::{IDENTITY_HUES, identity_slot},
    ink_overlay::InkSpot,
};
use crate::scale::u;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PenInput {
    Down(PenPoint),
    Move(PenPoint),
    /// The button came up: where, if this window heard it.
    Up(Option<PenPoint>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct InkAuthor {
    pub name: SharedString,
    pub color: Hsla,
}

/// Every Member who may draw, with their name and ink colour.
pub type InkAuthors = BTreeMap<UserId, InkAuthor>;

/// What a Viewer's canvas shows over a share, and whether the pointer draws on it.
#[derive(Clone)]
pub struct InkLayer {
    pub board: Option<InkBoard>,
    pub authors: InkAuthors,
    /// This Member: their own stroke needs no name tag.
    pub me: Option<UserId>,
    /// Drawing mode: the pointer on the picture draws through this.
    pub pen: Option<Callback<PenInput>>,
    /// The board as it stands at this instant instead of the clock's, so a gallery story can
    /// hold a stroke mid-fade.
    pub at: Option<Instant>,
    /// Where the canvas is in its window, for the Sharer's overlay, which paints this share's
    /// ink in its place.
    pub spot: Option<InkSpot>,
}

/// Which stroke on which share: what a canvas keeps about a stroke from frame to frame is filed
/// under it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StrokeKey {
    share: ShareId,
    epoch: DrawingEpoch,
    author: UserId,
    id: StrokeId,
}

/// One stroke as it is painted now, in fractions of the picture.
#[derive(Debug, Clone, PartialEq)]
pub struct InkStroke {
    pub key: StrokeKey,
    pub author: SharedString,
    pub color: Hsla,
    pub opacity: f32,
    pub runs: Vec<Vec<Point<f32>>>,
    /// Still being drawn by someone else: their name rides at its tip.
    pub tagged: bool,
}

impl InkLayer {
    pub fn strokes(&self, now: Instant) -> Vec<InkStroke> {
        let Some(board) = &self.board else {
            return Vec::new();
        };
        let fraction = |(x, y): (f32, f32)| point(x / f32::from(u16::MAX), y / f32::from(u16::MAX));
        board
            .strokes(self.at.unwrap_or(now))
            .into_iter()
            .map(|s| {
                let author = self.authors.get(s.author);
                // The tip, where the line has got to between two points, ends the last run.
                let runs: Vec<Vec<Point<f32>>> = s
                    .runs()
                    .map(|run| {
                        let points = run.points.iter().map(|p| (f32::from(p.x), f32::from(p.y)));
                        points.chain(run.tip).map(fraction).collect()
                    })
                    .collect();
                InkStroke {
                    key: StrokeKey {
                        share: board.share(),
                        epoch: board.epoch(),
                        author: s.author.clone(),
                        id: s.id,
                    },
                    author: author.map_or_else(|| s.author.to_string().into(), |a| a.name.clone()),
                    color: author.map_or(UNKNOWN_INK, |a| a.color),
                    opacity: s.opacity,
                    tagged: !s.lifted && self.me.as_ref() != Some(s.author) && !runs.is_empty(),
                    runs,
                }
            })
            .collect()
    }
}

/// Ink of a Member who has left the Lobby.
const UNKNOWN_INK: Hsla = Hsla {
    h: 0.,
    s: 0.,
    l: 0.85,
    a: 1.,
};

const INK_HUES: usize = IDENTITY_HUES;

fn ink_color(slot: usize) -> Hsla {
    oklch(
        0.74,
        0.17,
        (slot % INK_HUES) as f32 * (360. / INK_HUES as f32),
    )
}

/// Each Member's ink colour: their avatar's hue, or the next one free when another Member took
/// it. Members are placed in name order, so every client gives a Member the same colour.
pub fn ink_authors<'a>(members: impl IntoIterator<Item = (&'a UserId, &'a str)>) -> InkAuthors {
    let mut members: Vec<(&UserId, &str)> = members.into_iter().collect();
    members.sort_by(|a, b| a.1.cmp(b.1).then(a.0.cmp(b.0)));
    let mut taken = [false; INK_HUES];
    members
        .into_iter()
        .map(|(id, name)| {
            let preferred = identity_slot(name);
            let slot = (0..INK_HUES)
                .map(|i| (preferred + i) % INK_HUES)
                .find(|s| !taken[*s])
                .unwrap_or(preferred);
            taken[slot] = true;
            let author = InkAuthor {
                name: name.to_owned().into(),
                color: ink_color(slot),
            };
            (id.clone(), author)
        })
        .collect()
}

/// The line's width as a share of the picture's width, so a drawing keeps its look at every size.
const INK_WIDTH: f32 = 0.005;
const INK_WIDTH_MIN: f32 = 3.;
const INK_WIDTH_MAX: f32 = 7.;
/// How far the dark rim reaches past the line on each side.
const INK_RIM: f32 = 1.;
const RIM_ALPHA: f32 = 0.45;

fn paint_stroke(stroke: &InkStroke, bounds: Bounds<Pixels>, window: &mut Window) {
    let width = (f32::from(bounds.size.width) * INK_WIDTH).clamp(INK_WIDTH_MIN, INK_WIDTH_MAX);
    let at = |p: &Point<f32>| {
        point(
            bounds.origin.x + bounds.size.width * p.x,
            bounds.origin.y + bounds.size.height * p.y,
        )
    };
    // The rim shows through the fading colour, so it fades faster: else the line darkens as it
    // goes instead of fading.
    let rim = hsla(0., 0., 0., RIM_ALPHA * stroke.opacity * stroke.opacity);
    for (w, color) in [
        (width + 2. * INK_RIM, rim),
        (width, stroke.color.opacity(stroke.opacity)),
    ] {
        for run in &stroke.runs {
            let points: Vec<Point<Pixels>> = run.iter().map(at).collect();
            for path in ink_paths(&points, w).into_iter().flatten() {
                window.paint_path(path, color);
            }
        }
    }
}

/// A path's vertices are counted in `u16`: a long scribble of wide turns overflows that, so a
/// run is drawn in pieces of at most this many segments.
const PATH_SEGMENTS_MAX: usize = 128;
/// How far either side of a sharp turn the line is rounded, in line widths.
const CORNER_ROUND: f32 = 2.;

/// A run as a line `width` wide, with round joins and caps. Each point is rounded off with a
/// quadratic curve from halfway along the segment before it to halfway along the one after, so a
/// staircase of pixel steps reads as a slope and a fast curve of few points as a curve. A turn
/// sharper than 60° is rounded only within `CORNER_ROUND` widths of the point, so a corner drawn
/// on purpose stays one. A lone point is a dot. Pieces of a long run meet where the line runs
/// straight on, with flat ends. None if lyon cannot tessellate a piece.
fn ink_paths(points: &[Point<Pixels>], width: f32) -> Option<Vec<Path<Pixels>>> {
    let &first = points.first()?;
    let corner = CORNER_ROUND * width;
    // Each segment: its control point if it is a curve, and where it ends.
    let mut segments: Vec<(Option<Point<Pixels>>, Point<Pixels>)> = Vec::new();
    let mut end = first;
    let len = |v: Point<Pixels>| f32::from(v.x).hypot(f32::from(v.y));
    for at in points.windows(3) {
        let here = at[1];
        let (back, on) = (at[0] - here, at[2] - here);
        let (back_len, on_len) = (len(back), len(on));
        let dot = f32::from(back.x) * f32::from(on.x) + f32::from(back.y) * f32::from(on.y);
        // The cosine of the turn: NaN where two points coincide, which counts as gentle.
        let sharp = -dot / (back_len * on_len) < 0.5;
        let toward = |v: Point<Pixels>, len: f32| {
            let cut = if sharp {
                (len / 2.).min(corner)
            } else {
                len / 2.
            };
            here + v * if len > 0. { cut / len } else { 0. }
        };
        let (from, to) = (toward(back, back_len), toward(on, on_len));
        if len(from - end) > 0.01 {
            segments.push((None, from));
        }
        segments.push((Some(here), to));
        end = to;
    }
    // For a lone point an empty segment, which lyon caps with a dot.
    segments.push((None, points[points.len() - 1]));
    let pieces = segments.len().div_ceil(PATH_SEGMENTS_MAX);
    let mut from = first;
    let cap = |end: bool| if end { LineCap::Round } else { LineCap::Butt };
    segments
        .chunks(PATH_SEGMENTS_MAX)
        .enumerate()
        .map(|(i, piece)| {
            let style = StrokeOptions::default()
                .with_line_width(width)
                .with_line_join(LineJoin::Round)
                .with_start_cap(cap(i == 0))
                .with_end_cap(cap(i + 1 == pieces));
            let mut path = PathBuilder::stroke(px(width)) // px: the line's width in window pixels
                .with_style(PathStyle::Stroke(style));
            path.move_to(from);
            for &(ctrl, to) in piece {
                match ctrl {
                    Some(ctrl) => path.curve_to(to, ctrl),
                    None => path.line_to(to),
                }
                from = to;
            }
            path.build().ok()
        })
        .collect()
}

/// Where the name tags on one canvas hang, kept from frame to frame so a tag holds its side.
#[derive(Clone, Default)]
pub struct InkTags(Rc<RefCell<HashMap<StrokeKey, Tag>>>);

#[derive(Debug, Clone, Copy, Default)]
struct Tag {
    /// The side it leans to off the tip: right of it, and below it.
    lean: Option<(bool, bool)>,
    /// Where it last hung, which differs from the lean at the picture's edges.
    side: Option<(bool, bool)>,
    /// As last laid out.
    size: Option<Size<Pixels>>,
}

/// The name of the Member drawing, in their colour, off the tip of their stroke. It leans across
/// the line ([`lean`]) and keeps to that side while it fits, so it does not jump about as the
/// line wavers. Until the tag has been laid out once its size is unknown, so it is not shown
/// yet: the `bool` says whether it is.
fn name_tag(
    stroke: &InkStroke,
    area: Size<Pixels>,
    tags: &InkTags,
) -> Option<(impl IntoElement + use<>, bool)> {
    let (w, h) = (f32::from(area.width), f32::from(area.height));
    let run: Vec<(f32, f32)> = stroke
        .runs
        .last()?
        .iter()
        .map(|p| (p.x * w, p.y * h))
        .collect();
    let tip = *run.last()?;
    let (at, shown) = {
        let mut all = tags.0.borrow_mut();
        let tag = all.entry(stroke.key.clone()).or_default();
        let leant = lean(tag.lean, heading(&run), tip, (w, h));
        if leant != tag.lean {
            tag.side = None;
        }
        tag.lean = leant;
        let lean = leant?;
        let size = tag.size.unwrap_or_default();
        let (tw, th) = (f32::from(size.width), f32::from(size.height));
        let had = tag.side;
        let (right, x) = tag_axis(lean.0, had.map(|s| s.0), tip.0, tw, w);
        let (below, y) = tag_axis(lean.1, had.map(|s| s.1), tip.1, th, h);
        let shown = tag.size.is_some();
        if shown {
            tag.side = Some((right, below));
        }
        ((x, y), shown)
    };
    let (tags, key) = (tags.clone(), stroke.key.clone());
    let measure = canvas(
        move |bounds, _, _| {
            if let Some(tag) = tags.0.borrow_mut().get_mut(&key) {
                tag.size = Some(bounds.size);
            }
        },
        |_, _, _, _| {},
    )
    .absolute()
    .size_full();
    let tag = div()
        .id(SharedString::from(format!("ink-tag-{}", stroke.author)))
        .absolute()
        .left(px(at.0)) // px: placed in window pixels, like the picture
        .top(px(at.1)) // px: placed in window pixels, like the picture
        .max_w(u(TAG_MAX_W))
        .px_1p5()
        .rounded_md()
        .bg(stroke.color)
        .text_xs()
        .text_color(hsla(0., 0., 0.08, 1.))
        .truncate()
        .opacity(if shown { 1. } else { 0. })
        .child(stroke.author.clone())
        .child(measure);
    Some((tag.test_support(), shown))
}

/// Where the line ending at the last of `run` is heading, as a unit vector: from the last point
/// at least `TAG_HEADING` back, so the jitter of a hand does not turn it. None for a line not
/// that long yet.
fn heading(run: &[(f32, f32)]) -> Option<(f32, f32)> {
    let tip = *run.last()?;
    run.iter().rev().find_map(|p| {
        let (dx, dy) = (tip.0 - p.0, tip.1 - p.1);
        let len = dx.hypot(dy);
        (len >= TAG_HEADING).then_some((dx / len, dy / len))
    })
}

/// How a tag at `tip` in the picture's `area` leans, right and below: across the line's heading
/// toward the middle of the picture, and a little behind the pen, so it covers neither the line
/// drawn nor the way ahead. It keeps the lean it `had` until the line turns to run within 30° of
/// it, ahead or behind, so it moves at a corner or round a loop, never as the line wavers. With
/// no heading yet a tag has no lean and is not shown.
fn lean(
    had: Option<(bool, bool)>,
    heading: Option<(f32, f32)>,
    tip: (f32, f32),
    area: (f32, f32),
) -> Option<(bool, bool)> {
    let Some((hx, hy)) = heading else {
        return had;
    };
    let sign = |after: bool| if after { 1. } else { -1. };
    if let Some((right, below)) = had
        && (sign(right) * hx + sign(below) * hy).abs() * FRAC_1_SQRT_2 <= TAG_ALONG
    {
        return had;
    }
    let inward = (area.0 / 2. - tip.0, area.1 / 2. - tip.1);
    let (ax, ay) = if inward.0 * -hy + inward.1 * hx >= 0. {
        (-hy, hx)
    } else {
        (hy, -hx)
    };
    Some((ax - hx / 2. > 0., ay - hy / 2. > 0.))
}

/// How far back from the tip the line's heading is taken.
const TAG_HEADING: f32 = 24.;
/// cos 30°: a tag whose lean lies this close to the line's heading, or the opposite way, is on
/// the line. A tag just leant lies at least 45° off it.
const TAG_ALONG: f32 = 0.866;

/// Where a tag `len` long starts on one axis of a picture `room` long, hung off the tip at `tip`:
/// after it (`true`) or before it. It keeps the side it `had` while that fits, flips when only
/// the other side fits, and goes back to `prefer` once that fits with room to spare. Where
/// neither side fits it stays inside the picture as far as it can.
fn tag_axis(prefer: bool, had: Option<bool>, tip: f32, len: f32, room: f32) -> (bool, f32) {
    let start = |after: bool| {
        if after {
            tip + TAG_GAP
        } else {
            tip - TAG_GAP - len
        }
    };
    let fits = |after: bool, spare: f32| {
        let at = start(after);
        at >= spare && at + len <= room - spare
    };
    let mut after = had.unwrap_or(prefer);
    if after != prefer && fits(prefer, TAG_SPARE) {
        after = prefer;
    } else if !fits(after, 0.) && fits(!after, 0.) {
        after = !after;
    }
    (after, start(after).min(room - len).max(0.))
}

/// Off the tip, in window pixels like the picture.
const TAG_GAP: f32 = 6.;
/// How much room a flipped tag needs on its first side before it goes back.
const TAG_SPARE: f32 = 16.;
const TAG_MAX_W: f32 = 120.;

/// The canvas over the fitted picture, `size` in window pixels. With a pen it takes the pointer:
/// a press on the picture puts the pen down and the stroke follows the pointer, kept to the
/// picture's edges, until the button comes up anywhere. `pressing` outlives the frame. Each
/// frame it lays out, the canvas writes its place to `spot`. `tags` outlives the frame too.
pub fn ink_canvas(
    strokes: Vec<InkStroke>,
    size: Size<Pixels>,
    pen: Option<Callback<PenInput>>,
    pressing: Rc<Cell<bool>>,
    spot: Option<InkSpot>,
    tags: &InkTags,
    window: &mut Window,
) -> impl IntoElement + use<> {
    tags.0
        .borrow_mut()
        .retain(|key, _| strokes.iter().any(|s| s.tagged && &s.key == key));
    let mut hidden = false;
    let tags: Vec<_> = strokes
        .iter()
        .filter(|s| s.tagged)
        .filter_map(|s| name_tag(s, size, tags))
        .map(|(tag, shown)| {
            hidden |= !shown;
            tag
        })
        .collect();
    // A tag shows once it has been laid out: the next frame.
    if hidden {
        window.request_animation_frame();
    }
    let drawing = pen.is_some();
    let surface = canvas(
        move |bounds, window, _| {
            if let Some(spot) = &spot {
                spot.set(Some(bounds));
            }
            drawing.then(|| window.insert_hitbox(bounds, HitboxBehavior::Normal))
        },
        move |bounds, hitbox: Option<Hitbox>, window, _| {
            for stroke in &strokes {
                paint_stroke(stroke, bounds, window);
            }
            if let (Some(pen), Some(hitbox)) = (pen, hitbox) {
                listen(pen, pressing, bounds, hitbox, window);
            }
        },
    )
    .size_full();
    div()
        .id("ink-canvas")
        .absolute()
        .inset_0()
        .child(surface)
        .children(tags)
        .test_support()
}

fn listen(
    pen: Callback<PenInput>,
    pressing: Rc<Cell<bool>>,
    bounds: Bounds<Pixels>,
    hitbox: Hitbox,
    window: &mut Window,
) {
    window.set_cursor_style(CursorStyle::Crosshair, &hitbox);
    let on_picture = move |at: Point<Pixels>| {
        let p = at - bounds.origin;
        PenPoint::new(p.x / bounds.size.width, p.y / bounds.size.height)
    };
    let (down_pen, down_pressing) = (pen.clone(), pressing.clone());
    window.on_mouse_event(
        move |e: &MouseDownEvent, phase, window: &mut Window, cx: &mut App| {
            if phase == DispatchPhase::Bubble
                && e.button == MouseButton::Left
                && hitbox.is_hovered(window)
            {
                down_pressing.set(true);
                down_pen(PenInput::Down(on_picture(e.position)), window, cx);
            }
        },
    );
    let (move_pen, move_pressing) = (pen.clone(), pressing.clone());
    window.on_mouse_event(
        move |e: &MouseMoveEvent, phase, window: &mut Window, cx: &mut App| {
            if phase != DispatchPhase::Bubble || !move_pressing.get() {
                return;
            }
            if e.pressed_button == Some(MouseButton::Left) {
                move_pen(PenInput::Move(on_picture(e.position)), window, cx);
            } else {
                // The button came up where this window did not hear it.
                move_pressing.set(false);
                move_pen(PenInput::Up(None), window, cx);
            }
        },
    );
    window.on_mouse_event(
        move |e: &MouseUpEvent, phase, window: &mut Window, cx: &mut App| {
            if phase == DispatchPhase::Bubble && e.button == MouseButton::Left && pressing.get() {
                pressing.set(false);
                pen(PenInput::Up(Some(on_picture(e.position))), window, cx);
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scribble_as_long_as_a_board_holds_is_painted_whole() {
        let points = 0..lobby_client::ink::BOARD_POINTS_MAX;
        let zigzag = points
            .clone()
            .map(|i| point(px(i as f32 * 0.5), px(if i % 2 == 0 { 0. } else { 3000. })));
        // Wide loops of long, gently turning steps: every point a long curve.
        let loops = points.map(|i| {
            let turn = i as f32 * 10f32.to_radians();
            point(px(1500. * turn.cos()), px(1500. * turn.sin()))
        });
        let width = INK_WIDTH_MAX + 2. * INK_RIM;
        for scribble in [zigzag.collect::<Vec<_>>(), loops.collect()] {
            assert!(ink_paths(&scribble, width).is_some());
        }
    }

    const AREA: (f32, f32) = (400., 300.);

    fn side(heading: (f32, f32), tip: (f32, f32)) -> Option<(bool, bool)> {
        lean(None, Some(heading), tip, AREA)
    }

    #[test]
    fn a_tag_leans_across_the_line_toward_the_middle_and_behind_the_pen() {
        let run = [(100., 100.), (110., 105.), (115., 110.)];
        assert_eq!(heading(&run), None, "too short for a heading");
        assert_eq!(lean(None, heading(&run), (115., 110.), AREA), None);
        let run = [(50., 50.), (60., 50.), (100., 50.)];
        assert_eq!(heading(&run), Some((1., 0.)));
        // Rightward near the top: below the line, behind the pen.
        assert_eq!(side((1., 0.), (100., 50.)), Some((false, true)));
        // Down and right on the left: above and to the right, clear of the line and its way.
        let down_right = (FRAC_1_SQRT_2, FRAC_1_SQRT_2);
        assert_eq!(side(down_right, (80., 80.)), Some((true, false)));
        // Up and right from low in the middle: above and to the left.
        assert_eq!(side((0.756, -0.654), (272., 180.)), Some((false, false)));
    }

    #[test]
    fn a_tag_keeps_its_lean_as_the_line_wavers_and_moves_when_a_corner_puts_it_on_the_line() {
        let had = side((1., 0.), (100., 50.));
        let (cos, sin) = (15f32.to_radians().cos(), 15f32.to_radians().sin());
        for wavered in [(cos, sin), (cos, -sin)] {
            assert_eq!(lean(had, Some(wavered), (110., 50.), AREA), had);
        }
        assert_eq!(lean(had, None, (110., 50.), AREA), had);
        // Up and right, then a corner down and right: the line would run under the tag.
        let had = side((0.756, -0.654), (272., 180.));
        let down_right = (FRAC_1_SQRT_2, FRAC_1_SQRT_2);
        assert_eq!(
            lean(had, Some(down_right), (320., 216.), AREA),
            Some((false, true))
        );
    }

    /// A tag 40 px long on a picture 400 px across.
    fn place(prefer: bool, had: Option<bool>, tip: f32) -> (bool, f32) {
        tag_axis(prefer, had, tip, 40., 400.)
    }

    #[test]
    fn a_tag_starts_on_the_side_toward_the_middle_and_flips_only_at_the_edge() {
        assert_eq!(place(true, None, 100.), (true, 106.));
        assert_eq!(place(false, None, 300.), (false, 254.));
        assert_eq!(place(true, Some(true), 354.), (true, 360.));
        assert_eq!(place(true, Some(true), 355.), (false, 309.));
    }

    #[test]
    fn a_tip_that_jitters_at_the_edge_flips_the_tag_once_and_not_back() {
        let mut had = None;
        let mut flips = 0;
        for i in 0..20 {
            let tip = if i % 2 == 0 { 354. } else { 356. };
            let (side, _) = place(true, had, tip);
            if had.is_some_and(|had| had != side) {
                flips += 1;
            }
            had = Some(side);
        }
        assert_eq!((had, flips), (Some(false), 1));
    }

    #[test]
    fn a_flipped_tag_goes_back_once_its_side_has_room_to_spare() {
        assert_eq!(place(true, Some(false), 340.), (false, 294.));
        assert_eq!(place(true, Some(false), 338.), (true, 344.));
    }

    #[test]
    fn a_tag_that_fits_on_neither_side_stays_inside_the_picture() {
        assert_eq!(tag_axis(true, None, 50., 120., 100.), (true, 0.));
        assert_eq!(tag_axis(false, None, 50., 80., 100.), (false, 0.));
        assert_eq!(tag_axis(true, None, 30., 80., 100.), (true, 20.));
    }
}
