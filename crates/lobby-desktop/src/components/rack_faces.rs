//! The faces of the rack's modules: MIC's grille, ECHO's ripples, NOISE's grain over its bars,
//! COMP's needle, OUT's speaker cone and the limiter's LED. Each is drawn from the voice's levels
//! and nothing else, so it moves only while the voice does and rests still while none goes
//! through or out of the path. The rack's inks and its drawing helpers live here too.

use gpui_kit::{
    BorderStyle, Bounds, ContentMask, Hsla, IntoElement, ParentElement as _, PathBuilder, Pixels,
    Point, Styled as _, Window, canvas,
    component::{Colorize as _, Theme},
    fill, point, px, quad, size,
};

use super::picture_backdrop;

#[derive(Clone, Copy)]
pub(super) struct Inks {
    pub panel: Hsla,
    pub edge: Hsla,
    pub text: Hsla,
    pub caption: Hsla,
    pub lit: Hsla,
    pub unlit: Hsla,
    pub voice: Hsla,
    pub rest: Hsla,
    pub rim: Hsla,
    pub ring: Hsla,
    pub hole: Hsla,
    pub screen: Hsla,
    pub grille: Hsla,
    pub focus: Hsla,
}

impl Inks {
    pub fn of(theme: &Theme) -> Self {
        Self {
            panel: theme.sidebar,
            edge: theme.border,
            text: theme.foreground,
            caption: theme.muted_foreground,
            lit: theme.foreground.opacity(0.85),
            unlit: theme.border,
            voice: theme.primary,
            rest: theme.muted_foreground.mix_oklab(theme.sidebar, 0.6),
            rim: theme.background.opacity(0.85),
            ring: theme.muted_foreground.mix_oklab(theme.sidebar, 0.4),
            hole: picture_backdrop(theme),
            screen: theme.background,
            grille: theme.muted_foreground.mix_oklab(theme.sidebar, 0.22),
            focus: theme.ring,
        }
    }
}

/// Design pixels inside one painted box, at the window's UI scale.
#[derive(Clone, Copy)]
pub(super) struct Pen {
    pub origin: Point<Pixels>,
    pub scale: f32,
}

impl Pen {
    pub fn d(self, v: f32) -> Pixels {
        px(v * self.scale) // px: paths and quads take pixels; `scale` is the UI scale
    }

    pub fn at(self, (x, y): (f32, f32)) -> Point<Pixels> {
        point(self.origin.x + self.d(x), self.origin.y + self.d(y))
    }
}

pub(super) fn circle(window: &mut Window, center: Point<Pixels>, r: Pixels, color: Hsla) {
    let bounds = Bounds::new(center - point(r, r), size(r * 2., r * 2.));
    window.paint_quad(fill(bounds, color).corner_radii(r));
}

/// A circle's outline, `width` wide, inside radius `r`.
fn ring(window: &mut Window, center: Point<Pixels>, r: Pixels, width: Pixels, color: Hsla) {
    let bounds = Bounds::new(center - point(r, r), size(r * 2., r * 2.));
    window.paint_quad(quad(
        bounds,
        r,
        gpui_kit::transparent_black(),
        width,
        color,
        BorderStyle::Solid,
    ));
}

/// A cubic from `points[0]` to `points[3]` through the two controls between.
pub(super) fn stroke(window: &mut Window, points: &[Point<Pixels>; 4], width: Pixels, color: Hsla) {
    let mut path = PathBuilder::stroke(width);
    path.move_to(points[0]);
    path.cubic_bezier_to(points[3], points[1], points[2]);
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

/// An arc of radius `r` around `center`, in degrees counter-clockwise from three o'clock.
pub(super) fn arc(
    window: &mut Window,
    pen: Pen,
    center: (f32, f32),
    r: f32,
    (from, to): (f32, f32),
    width: f32,
    color: Hsla,
) {
    let at = |deg: f32| {
        let a = deg.to_radians();
        pen.at((center.0 + r * a.cos(), center.1 - r * a.sin()))
    };
    let steps = ((to - from).abs() / 6.).ceil().max(1.) as usize;
    let mut path = PathBuilder::stroke(pen.d(width));
    path.move_to(at(from));
    for i in 1..=steps {
        path.line_to(at(from + (to - from) * i as f32 / steps as f32));
    }
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

fn line(window: &mut Window, pen: Pen, a: (f32, f32), b: (f32, f32), width: f32, color: Hsla) {
    let mut path = PathBuilder::stroke(pen.d(width));
    path.move_to(pen.at(a));
    path.line_to(pen.at(b));
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

/// A box that paints `paint` over all of itself, clipped to itself, in design pixels.
fn face(scale: f32, paint: impl Fn(Pen, (f32, f32), &mut Window) + 'static) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let pen = Pen {
                origin: bounds.origin,
                scale,
            };
            let wh = (
                f32::from(bounds.size.width) / scale,
                f32::from(bounds.size.height) / scale,
            );
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                paint(pen, wh, window)
            });
        },
    )
    .size_full()
}

/// A display window set into the panel: a darker rounded inset, the stage for ECHO's ripples,
/// NOISE's bars and COMP's needle.
fn screen(window: &mut Window, pen: Pen, (w, h): (f32, f32), inks: Inks) {
    let bounds = Bounds::new(pen.at((0., 0.)), size(pen.d(w), pen.d(h)));
    window.paint_quad(quad(
        bounds,
        pen.d(SCREEN_RADIUS),
        inks.screen,
        px(1.), // px: a hairline
        inks.edge,
        BorderStyle::Solid,
    ));
}

const SCREEN_RADIUS: f32 = 5.;

/// A 0 to 1 value from a seed and an index, the same every time: where a grain falls is a
/// function of the levels, so a steady level draws the same grain.
fn scatter(seed: u32, i: u32) -> f32 {
    let mut x = seed.wrapping_mul(0x9E37_79B9) ^ i.wrapping_mul(0x85EB_CA6B);
    x ^= x >> 16;
    x = x.wrapping_mul(0x7FEB_352D);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846C_A68B);
    x ^= x >> 16;
    x as f32 / u32::MAX as f32
}

const GRILLE_PITCH: f32 = 5.;
const GRILLE_DOT: f32 = 0.8;

/// MIC's grille: a fine grid of holes over the whole box.
pub(super) fn grille(inks: Inks, scale: f32) -> impl IntoElement {
    face(scale, move |pen, (w, h), window| {
        let (cols, rows) = ((w / GRILLE_PITCH) as usize, (h / GRILLE_PITCH) as usize);
        let (x0, y0) = (
            (w - (cols as f32 - 1.) * GRILLE_PITCH) / 2.,
            (h - (rows as f32 - 1.) * GRILLE_PITCH) / 2.,
        );
        for row in 0..rows {
            // Every other row sits half a pitch over, as a punched grille does.
            let shift = if row % 2 == 1 { GRILLE_PITCH / 2. } else { 0. };
            for col in 0..cols {
                let x = x0 + col as f32 * GRILLE_PITCH + shift;
                if x > w - GRILLE_DOT {
                    continue;
                }
                let at = pen.at((x, y0 + row as f32 * GRILLE_PITCH));
                circle(window, at, pen.d(GRILLE_DOT), inks.grille);
            }
        }
    })
}

/// What ECHO's ripples show: how loud the microphone is and how much of it the canceller takes
/// out, each from 0 to 1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Ripples {
    pub level: f32,
    pub removed: f32,
}

const RINGS: usize = 6;
const RING_W: f32 = 1.2;
/// The rings drawn at rest, from the middle out.
const RESTING_RINGS: [f32; 3] = [0.3, 0.18, 0.08];
/// With the echo all taken out the rings draw in by this share, and fade this much faster.
const RIPPLE_SHRINK: f32 = 0.35;
const RIPPLE_FALLOFF: f32 = 3.;

/// ECHO's face: rings spreading from a point as far as the voice carries; while the canceller
/// is taking echo out they draw in and fade sooner. Faint and still at rest.
pub(super) fn ripples(now: Option<Ripples>, inks: Inks, scale: f32) -> impl IntoElement {
    face(scale, move |pen, (w, h), window| {
        screen(window, pen, (w, h), inks);
        let c = (w / 2., h / 2.);
        let spacing = (w / 2. - 2.) / RINGS as f32;
        let source = |window: &mut Window, alpha: f32| {
            circle(window, pen.at(c), pen.d(2.), inks.text.opacity(alpha))
        };
        let Some(Ripples { level, removed }) = now else {
            for (i, alpha) in RESTING_RINGS.iter().enumerate() {
                let r = spacing * (i + 1) as f32;
                ring(
                    window,
                    pen.at(c),
                    pen.d(r),
                    pen.d(RING_W),
                    inks.text.opacity(*alpha),
                );
            }
            source(window, 0.35);
            return;
        };
        let shrink = 1. - RIPPLE_SHRINK * removed;
        let reach = (0.2 + 0.8 * level) * shrink;
        for i in 0..RINGS {
            let t = (i + 1) as f32 / RINGS as f32;
            let fade = (1. - t / reach).max(0.).powf(1. + RIPPLE_FALLOFF * removed);
            if fade <= 0.01 {
                continue;
            }
            let r = spacing * (i + 1) as f32 * shrink;
            let alpha = 0.12 + 0.68 * fade;
            ring(
                window,
                pen.at(c),
                pen.d(r),
                pen.d(RING_W),
                inks.text.opacity(alpha),
            );
        }
        source(window, 0.45 + 0.5 * level);
    })
}

/// What NOISE's display shows: the voice it lets through and how much noise it takes out, each
/// from 0 to 1, and the seed its grain falls by.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Grain {
    pub level: f32,
    pub removed: f32,
    pub seed: u32,
}

/// A voice's rough shape across the bars, low to high.
const BAR_SHAPE: [f32; 9] = [0.45, 0.7, 0.95, 1., 0.85, 0.7, 0.55, 0.4, 0.28];
const BAR_GAP: f32 = 2.;
const BAR_REST: f32 = 0.12;
const SCREEN_PAD: f32 = 4.;
/// The grain at its thickest, while the suppressor takes out all it can.
const GRAIN_MAX: f32 = 70.;
const GRAIN_DOT: f32 = 1.25;

/// NOISE's face: a few bars of the voice it lets through, and over them the grain of the noise it
/// is taking out. Calm, with no grain, at rest.
pub(super) fn grain(now: Option<Grain>, inks: Inks, scale: f32) -> impl IntoElement {
    face(scale, move |pen, (w, h), window| {
        screen(window, pen, (w, h), inks);
        let (iw, ih) = (w - 2. * SCREEN_PAD, h - 2. * SCREEN_PAD);
        let n = BAR_SHAPE.len() as f32;
        let bar_w = (iw - BAR_GAP * (n - 1.)) / n;
        let bottom = SCREEN_PAD + ih;
        for (i, shape) in BAR_SHAPE.iter().enumerate() {
            let (height, color) = match now {
                Some(g) => {
                    let wobble = 0.85 + 0.3 * scatter(g.seed, i as u32 + 100);
                    let t = (BAR_REST + (1. - BAR_REST) * g.level * shape * wobble).min(1.);
                    (t * ih, inks.lit.opacity(0.35 + 0.5 * g.level))
                }
                None => (BAR_REST * shape * ih, inks.caption.opacity(0.5)),
            };
            let x = SCREEN_PAD + i as f32 * (bar_w + BAR_GAP);
            let bounds = Bounds::new(
                pen.at((x, bottom - height)),
                size(pen.d(bar_w), pen.d(height)),
            );
            window.paint_quad(fill(bounds, color).corner_radii(pen.d(1.)));
        }
        let Some(g) = now else {
            return;
        };
        let count = (GRAIN_MAX * g.removed).round() as u32;
        for i in 0..count {
            let x = SCREEN_PAD + scatter(g.seed, 2 * i) * (iw - GRAIN_DOT);
            let y = SCREEN_PAD + scatter(g.seed, 2 * i + 1) * (ih - GRAIN_DOT);
            let alpha = 0.35 + 0.45 * scatter(g.seed ^ 0x5bd1, i);
            let bounds = Bounds::new(pen.at((x, y)), size(pen.d(GRAIN_DOT), pen.d(GRAIN_DOT)));
            window.paint_quad(fill(bounds, inks.text.opacity(alpha)));
        }
    })
}

/// The needle's travel, in degrees from three o'clock: no reduction at the right, the most at
/// the left, as an analog gain reduction meter reads.
const NEEDLE_FROM: f32 = 35.;
const NEEDLE_SWEEP: f32 = 110.;
const NEEDLE_TICKS: usize = 5;

/// COMP's face: an analog meter whose needle swings left as the compressor turns the voice down.
/// `reduction` runs from 0 to 1 of the scale; the needle rests at 0 while no voice goes through.
pub(super) fn needle(reduction: Option<f32>, inks: Inks, scale: f32) -> impl IntoElement {
    face(scale, move |pen, (w, h), window| {
        screen(window, pen, (w, h), inks);
        let pivot = (w / 2., h - 4.);
        let r = (w / 2. - 6.).min(h - 9.);
        arc(
            window,
            pen,
            pivot,
            r,
            (NEEDLE_FROM, NEEDLE_FROM + NEEDLE_SWEEP),
            1.,
            inks.caption.opacity(0.7),
        );
        for i in 0..NEEDLE_TICKS {
            let a =
                (NEEDLE_FROM + NEEDLE_SWEEP * i as f32 / (NEEDLE_TICKS - 1) as f32).to_radians();
            let at = |k: f32| (pivot.0 + k * a.cos(), pivot.1 - k * a.sin());
            line(
                window,
                pen,
                at(r),
                at(r - 3.),
                1.,
                inks.caption.opacity(0.7),
            );
        }
        let lit = reduction.is_some();
        let a = (NEEDLE_FROM + NEEDLE_SWEEP * reduction.unwrap_or(0.).clamp(0., 1.)).to_radians();
        let tip = (pivot.0 + (r - 1.) * a.cos(), pivot.1 - (r - 1.) * a.sin());
        let ink = if lit { inks.text } else { inks.caption };
        line(window, pen, pivot, tip, 1.4, ink);
        circle(window, pen.at(pivot), pen.d(2.5), ink);
    })
}

/// How far OUT's cone rings move out at full level, by ring from the outside in.
const CONE_RINGS: [(f32, f32); 3] = [(0.78, 0.06), (0.56, 0.12), (0.32, 0.22)];

/// OUT's face: a speaker cone whose rings breathe with the level it sends.
pub(super) fn cone(level: Option<f32>, inks: Inks, scale: f32) -> impl IntoElement {
    face(scale, move |pen, (w, h), window| {
        let c = pen.at((w / 2., h / 2.));
        let big = w.min(h) / 2. - 1.;
        circle(window, c, pen.d(big), inks.screen);
        ring(window, c, pen.d(big), px(1.), inks.edge); // px: a hairline
        ring(window, c, pen.d(big - 3.), pen.d(1.5), inks.ring);
        let v = level.unwrap_or(0.);
        for (k, (r, breath)) in CONE_RINGS.iter().enumerate() {
            let r = big * r * (1. + breath * v);
            let alpha = match level {
                Some(v) => 0.25 + 0.5 * v,
                None => 0.18,
            };
            if k + 1 == CONE_RINGS.len() {
                circle(
                    window,
                    c,
                    pen.d(r),
                    inks.text.mix_oklab(inks.panel, 0.2 + 0.5 * v),
                );
            }
            ring(window, c, pen.d(r), pen.d(1.), inks.text.opacity(alpha));
        }
    })
}

const LED: f32 = 7.;
const LED_HALO: f32 = 6.;

/// The limiter's light: lit cream while it holds the voice under the ceiling.
pub(super) fn led(lit: bool, inks: Inks, scale: f32) -> impl IntoElement {
    gpui_kit::div()
        .size(crate::scale::u(LED + 2. * LED_HALO))
        .flex_shrink_0()
        .child(face(scale, move |pen, (w, h), window| {
            let c = pen.at((w / 2., h / 2.));
            if lit {
                circle(
                    window,
                    c,
                    pen.d(LED / 2. + LED_HALO),
                    inks.text.opacity(0.12),
                );
                circle(window, c, pen.d(LED / 2. + 2.), inks.text.opacity(0.25));
                circle(window, c, pen.d(LED / 2.), inks.text);
            } else {
                circle(window, c, pen.d(LED / 2.), inks.hole);
                ring(window, c, pen.d(LED / 2.), px(1.), inks.edge); // px: a hairline
            }
        }))
}
