//! The pictures of the synthetic Screen Share source: a gradient with a bouncing square, or a
//! screen of text scrolling, alone or with a busy quarter of the picture over it. Each carries a
//! stamp along its top edge, the frame number and the wall-clock time it was captured, which a
//! Viewer reads back to count pictures and time them.

#![cfg_attr(
    all(not(windows), not(test), not(feature = "test-support")),
    expect(
        dead_code,
        reason = "only Windows capture and the test-support sources paint them"
    )
)]

use std::{
    ops::Range,
    time::{SystemTime, UNIX_EPOCH},
};

use super::screen::{Rect, TestMotion};

pub(crate) const DEFAULT_SIZE: (u32, u32) = (1920, 1080);
/// The smallest picture: the square, which starts at (100, 80), has room to bounce in it.
pub(crate) const SMALLEST: (u32, u32) = (640, 360);

const SQUARE: u32 = 240;

/// The stamp's cells span this many to a picture's width, so its look scales with the picture:
/// 12 px at 1080p, 8 px at 720p.
const PICTURE_CELLS: u32 = 160;
const FRAME_BITS: usize = 32;
const TIME_BITS: usize = 52;
const CHECK_BITS: usize = 16;
/// A white cell and a black one, then the frame number, the capture time and the check, each most
/// significant bit first.
pub(crate) const STAMP_CELLS: usize = 2 + FRAME_BITS + TIME_BITS + CHECK_BITS;
const TIME_MASK: u64 = (1 << TIME_BITS) - 1;

/// What a picture says about itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    pub frame: u32,
    /// Microseconds since the Unix epoch. Only its low 52 bits are kept, which lasts until 2112.
    pub captured_micros: u64,
}

impl Stamp {
    pub(crate) fn check(frame: u32, captured_micros: u64) -> u16 {
        let mut bytes = [0u8; 12];
        bytes[..4].copy_from_slice(&frame.to_be_bytes());
        bytes[4..].copy_from_slice(&(captured_micros & TIME_MASK).to_be_bytes());
        // CRC-16/CCITT-FALSE.
        let mut crc = 0xffff_u16;
        for byte in bytes {
            crc ^= u16::from(byte) << 8;
            for _ in 0..8 {
                crc = if crc & 0x8000 != 0 {
                    (crc << 1) ^ 0x1021
                } else {
                    crc << 1
                };
            }
        }
        crc
    }

    fn cells(self) -> [bool; STAMP_CELLS] {
        let value = (u128::from(self.frame) << (TIME_BITS + CHECK_BITS))
            | (u128::from(self.captured_micros & TIME_MASK) << CHECK_BITS)
            | u128::from(Self::check(self.frame, self.captured_micros));
        let mut cells = [false; STAMP_CELLS];
        cells[0] = true;
        for (i, cell) in cells[2..].iter_mut().enumerate() {
            *cell = value >> (FRAME_BITS + TIME_BITS + CHECK_BITS - 1 - i) & 1 == 1;
        }
        cells
    }

    /// The stamp `cells` say, if they start right and their check holds.
    #[cfg(feature = "test-support")]
    pub(crate) fn from_cells(cells: &[bool; STAMP_CELLS]) -> Option<Stamp> {
        if cells[..2] != [true, false] {
            return None;
        }
        let value = cells[2..]
            .iter()
            .fold(0u128, |value, &on| value << 1 | u128::from(on));
        let frame = (value >> (TIME_BITS + CHECK_BITS)) as u32;
        let captured_micros = (value >> CHECK_BITS) as u64 & TIME_MASK;
        (value as u16 == Self::check(frame, captured_micros)).then_some(Stamp {
            frame,
            captured_micros,
        })
    }
}

/// The columns of stamp cell `i` in a picture `width` wide. The stamp sits in the middle of the
/// top edge, clear of the corners.
pub(crate) fn cell_columns(width: u32, i: usize) -> Range<u32> {
    let cell = f64::from(width) / f64::from(PICTURE_CELLS);
    let first = f64::from((PICTURE_CELLS - STAMP_CELLS as u32) / 2);
    let edge = |i: usize| ((first + i as f64) * cell).round() as u32;
    edge(i)..edge(i + 1)
}

pub(crate) fn stamp_rows(width: u32) -> u32 {
    (f64::from(width) / f64::from(PICTURE_CELLS))
        .round()
        .max(1.0) as u32
}

pub fn unix_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_micros() as u64)
}

/// Draws `stamp` into the top rows of a BGRA picture `width` wide; returns what it covers.
fn draw_stamp(bgra: &mut [u8], width: u32, stamp: Stamp) -> Rect {
    let rows = stamp_rows(width);
    let (left, right) = (
        cell_columns(width, 0).start,
        cell_columns(width, STAMP_CELLS - 1).end,
    );
    for (i, on) in stamp.cells().into_iter().enumerate() {
        let level = if on { 255 } else { 0 };
        let cols = cell_columns(width, i);
        for px in bgra[cols.start as usize * 4..cols.end as usize * 4].chunks_exact_mut(4) {
            px.copy_from_slice(&[level, level, level, 255]);
        }
    }
    let stride = width as usize * 4;
    for row in 1..rows as usize {
        bgra.copy_within(
            left as usize * 4..right as usize * 4,
            row * stride + left as usize * 4,
        );
    }
    (left, 0, right - left, rows)
}

fn union(a: Rect, b: Rect) -> Rect {
    let (left, top) = (a.0.min(b.0), a.1.min(b.1));
    let (right, bottom) = ((a.0 + a.2).max(b.0 + b.2), (a.1 + a.3).max(b.1 + b.3));
    (left, top, right - left, bottom - top)
}

/// One picture at a time of the synthetic source, drawn in place.
pub(crate) struct Pattern {
    size: (u32, u32),
    picture: Vec<u8>,
    work: Workload,
}

enum Workload {
    Square(Square),
    Scroll(Page, Option<Busy>),
}

/// What the heavy scroll workloads draw in [`busy_region`] over the text.
enum Busy {
    Texture(Texture),
    Noise,
}

/// The busy quarter of the heavy scroll workloads' picture as left, top, width, height: right of
/// and below the centre, as a video playing in a window over a page of text.
pub fn busy_region((w, h): (u32, u32)) -> Rect {
    (
        (w * 3 / 8) & !1,
        (h * 3 / 8) & !1,
        (w / 2) & !1,
        (h / 2) & !1,
    )
}

impl Pattern {
    /// `tint` is one colour, as B, G, R, in place of the square workload's gradient.
    pub fn new(motion: TestMotion, size: (u32, u32), tint: Option<[u8; 3]>) -> Pattern {
        let busy = match motion {
            TestMotion::ScrollTexture => Some(Busy::Texture(Texture::new(busy_region(size)))),
            TestMotion::ScrollNoise => Some(Busy::Noise),
            _ => None,
        };
        let (work, picture) = match motion {
            TestMotion::Scroll | TestMotion::ScrollTexture | TestMotion::ScrollNoise => (
                Workload::Scroll(Page::new(size), busy),
                vec![0u8; (size.0 * size.1 * 4) as usize],
            ),
            _ => {
                let square = Square::new(motion, size, tint);
                let picture = square.base.clone();
                (Workload::Square(square), picture)
            }
        };
        Pattern {
            size,
            picture,
            work,
        }
    }

    /// Draws picture `n` without its stamp; returns what changed since the one before.
    pub fn paint(&mut self, n: u32) -> Rect {
        match &mut self.work {
            Workload::Square(square) => square.paint(&mut self.picture, self.size, n),
            Workload::Scroll(page, busy) => {
                page.paint(&mut self.picture, n);
                let region = busy_region(self.size);
                match busy {
                    Some(Busy::Texture(texture)) => {
                        texture.paint(&mut self.picture, self.size.0, region, n)
                    }
                    Some(Busy::Noise) => {
                        paint_fresh_noise(&mut self.picture, self.size.0, region, n)
                    }
                    None => {}
                }
                (0, 0, self.size.0, self.size.1)
            }
        }
    }

    /// Stamps the picture last, over its content; returns `changed` and the stamp's place.
    pub fn stamp(&mut self, changed: Rect, stamp: Stamp) -> Rect {
        union(changed, draw_stamp(&mut self.picture, self.size.0, stamp))
    }

    pub fn picture(&self) -> &[u8] {
        &self.picture
    }
}

struct Square {
    base: Vec<u8>,
    /// Fresh noise in the square, as costly as busy content; otherwise one colour changing hue.
    noise: bool,
    x: i32,
    y: i32,
    dx: i32,
    dy: i32,
    previous: Option<(u32, u32)>,
    noise_state: u32,
}

impl Square {
    fn new(motion: TestMotion, (w, h): (u32, u32), tint: Option<[u8; 3]>) -> Square {
        let mut base = vec![0u8; (w * h * 4) as usize];
        for (i, px) in base.chunks_exact_mut(4).enumerate() {
            let (x, y) = (i as u32 % w, i as u32 / w);
            let [b, g, r] = tint.unwrap_or([
                (40 + x * 120 / w) as u8,
                (30 + y * 60 / h) as u8,
                (60 + (x + y) * 80 / (w + h)) as u8,
            ]);
            px.copy_from_slice(&[b, g, r, 255]);
        }
        Square {
            base,
            noise: motion != TestMotion::Sixty,
            x: 100,
            y: 80,
            dx: 9,
            dy: 6,
            previous: None,
            noise_state: 0x9e37_79b9,
        }
    }

    /// Draws the square where it is, then moves it on.
    fn paint(&mut self, picture: &mut [u8], (w, h): (u32, u32), n: u32) -> Rect {
        if let Some((px, py)) = self.previous {
            blit(picture, &self.base, w, px, py);
        }
        let (sx, sy) = (self.x as u32, self.y as u32);
        if self.noise {
            fill_noise(picture, w, sx, sy, &mut self.noise_state);
        } else {
            fill_square(picture, w, sx, sy, n % 360);
        }
        let changed = match self.previous {
            None => (0, 0, w, h),
            Some((px, py)) => {
                let (left, top) = (px.min(sx), py.min(sy));
                let (right, bottom) = (px.max(sx) + SQUARE, py.max(sy) + SQUARE);
                (left, top, right - left, bottom - top)
            }
        };
        self.previous = Some((sx, sy));
        self.x += self.dx;
        self.y += self.dy;
        if self.x <= 0 || self.x as u32 + SQUARE >= w {
            self.dx = -self.dx;
            self.x = self.x.clamp(0, (w - SQUARE) as i32);
        }
        if self.y <= 0 || self.y as u32 + SQUARE >= h {
            self.dy = -self.dy;
            self.y = self.y.clamp(0, (h - SQUARE) as i32);
        }
        changed
    }
}

fn blit(dst: &mut [u8], src: &[u8], w: u32, x: u32, y: u32) {
    for row in y..y + SQUARE {
        let start = ((row * w + x) * 4) as usize;
        let end = start + (SQUARE * 4) as usize;
        dst[start..end].copy_from_slice(&src[start..end]);
    }
}

fn fill_square(dst: &mut [u8], w: u32, x: u32, y: u32, hue: u32) {
    let ramp = |offset: u32| -> u8 {
        let t = (hue + offset) % 360;
        match t {
            0..60 => 255,
            60..120 => (255 - (t - 60) * 4) as u8,
            120..240 => 15,
            240..300 => (15 + (t - 240) * 4) as u8,
            _ => 255,
        }
    };
    let px = [ramp(240), ramp(120), ramp(0), 255];
    let row_bytes: Vec<u8> = px.repeat(SQUARE as usize);
    for row in y..y + SQUARE {
        let start = ((row * w + x) * 4) as usize;
        dst[start..start + row_bytes.len()].copy_from_slice(&row_bytes);
    }
}

pub(super) fn noise_pixel(state: &mut u32) -> [u8; 4] {
    *state ^= *state << 13;
    *state ^= *state >> 17;
    *state ^= *state << 5;
    let [b, g, r, _] = state.to_le_bytes();
    [140 + (b & 63), 80 + (g & 63), 30 + (r & 63), 255]
}

fn fill_noise(dst: &mut [u8], w: u32, x: u32, y: u32, state: &mut u32) {
    for row in y..y + SQUARE {
        let start = ((row * w + x) * 4) as usize;
        for px in dst[start..start + (SQUARE * 4) as usize].chunks_exact_mut(4) {
            px.copy_from_slice(&noise_pixel(state));
        }
    }
}

const PAGE_BACKGROUND: [u8; 4] = [34, 30, 28, 255];
/// Light inks, as B, G, R, one per word.
const PAGE_INKS: [[u8; 3]; 5] = [
    [220, 220, 215],
    [250, 200, 140],
    [170, 230, 160],
    [140, 190, 250],
    [200, 160, 240],
];

/// A tall page of text-like glyph rows, drawn once. Picture `n` of the scroll is a window on it
/// that starts `n` steps down, and wraps. A step is 10 px at 2160p and shrinks with the height.
struct Page {
    width: u32,
    height: u32,
    rows: u32,
    step: u32,
    bgra: Vec<u8>,
}

impl Page {
    /// Glyphs are 5 × 7 dots, each `dot` px wide: 4 at 2160p, 2 at 1080p.
    fn new((width, height): (u32, u32)) -> Page {
        let dot = (height / 540).max(1);
        let (pitch, advance) = (10 * dot, 6 * dot);
        let rows = 2 * height;
        let mut bgra = PAGE_BACKGROUND.repeat((width * rows) as usize);
        let across = (width / advance).saturating_sub(8).max(1);
        let mut rng = Rng(0x2545_f491_4f6c_dd1d);
        for line in 0..rows / pitch - 1 {
            let top = line * pitch;
            let indent = 4 + rng.next() as u32 % 4 * 4;
            let length = (8 + rng.next() as u32 % 60).min(across.saturating_sub(indent));
            let (mut word_left, mut ink) = (0, PAGE_INKS[0]);
            for glyph in 0..length {
                if word_left == 0 {
                    word_left = 2 + rng.next() as u32 % 8;
                    ink = PAGE_INKS[rng.next() as usize % PAGE_INKS.len()];
                    continue;
                }
                word_left -= 1;
                let dots = rng.next() & (rng.next() | rng.next());
                let left = (4 + indent + glyph) * advance;
                for bit in 0..35 {
                    if dots >> bit & 1 == 1 {
                        let (gx, gy) = (bit % 5, bit / 5);
                        let (x, y) = (left + gx * dot, top + gy * dot);
                        for row in y..y + dot {
                            let at = ((row * width + x) * 4) as usize;
                            for px in bgra[at..at + (dot * 4) as usize].chunks_exact_mut(4) {
                                px.copy_from_slice(&[ink[0], ink[1], ink[2], 255]);
                            }
                        }
                    }
                }
            }
        }
        Page {
            width,
            height,
            rows,
            step: (height / 216).max(1),
            bgra,
        }
    }

    fn paint(&self, picture: &mut [u8], n: u32) {
        let stride = self.width as usize * 4;
        let offset = (u64::from(n) * u64::from(self.step) % u64::from(self.rows)) as usize;
        let height = self.height as usize;
        let first = (self.rows as usize - offset).min(height);
        picture[..first * stride]
            .copy_from_slice(&self.bgra[offset * stride..(offset + first) * stride]);
        picture[first * stride..].copy_from_slice(&self.bgra[..(height - first) * stride]);
    }
}

/// xorshift64.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// How far each of the texture's two windows moves per picture, in px.
const PAN_A: (i64, i64) = (5, 2);
const PAN_B: (i64, i64) = (-3, 4);

/// A tiling texture of coloured value noise, smooth at large scales with detail down to 2 px.
/// Picture `n` averages two windows on it panning different ways, so no one motion vector
/// predicts a block: like video, it costs the encoder a residual every picture.
struct Texture {
    /// Powers of two, so the noise lattice and the windows wrap.
    width: u32,
    height: u32,
    bgra: Vec<u8>,
}

impl Texture {
    fn new((_, _, rw, rh): Rect) -> Texture {
        let (width, height) = (rw.next_power_of_two(), rh.next_power_of_two());
        let mut bgra = vec![255u8; (width * height * 4) as usize];
        let stretch = |v: f32| ((v - 0.5) * 2.6 + 0.5).clamp(0.0, 1.0);
        for (i, px) in bgra.chunks_exact_mut(4).enumerate() {
            let (x, y) = (i as u32 % width, i as u32 / width);
            let field = |seed, octaves| fbm(x, y, (width, height), seed, octaves);
            let l = stretch(field(1, 8));
            let (c1, c2) = (field(2, 3) - 0.5, field(3, 3) - 0.5);
            let level = |tint: f32| (20.0 + 215.0 * l * (0.7 + tint)).clamp(0.0, 255.0) as u8;
            px[0] = level(-0.5 * (c1 + c2));
            px[1] = level(0.9 * c2);
            px[2] = level(0.9 * c1);
        }
        Texture {
            width,
            height,
            bgra,
        }
    }

    fn paint(&self, picture: &mut [u8], width: u32, (left, top, rw, rh): Rect, n: u32) {
        let at = |(dx, dy): (i64, i64)| {
            (
                (i64::from(n) * dx).rem_euclid(i64::from(self.width)) as u32,
                (i64::from(n) * dy).rem_euclid(i64::from(self.height)) as u32,
            )
        };
        let (a, b) = (at(PAN_A), at(PAN_B));
        let (wrap_x, wrap_y) = (self.width - 1, self.height - 1);
        for y in 0..rh {
            let start = ((top + y) * width + left) as usize * 4;
            let out = &mut picture[start..start + rw as usize * 4];
            let row_a = ((y + a.1) & wrap_y) * self.width;
            let row_b = ((y + b.1) & wrap_y) * self.width;
            for (x, px) in out.chunks_exact_mut(4).enumerate() {
                let x = x as u32;
                let pa = ((row_a + ((x + a.0) & wrap_x)) * 4) as usize;
                let pb = ((row_b + ((x + b.0) & wrap_x)) * 4) as usize;
                let (from_a, from_b) = (&self.bgra[pa..pa + 3], &self.bgra[pb..pb + 3]);
                for ((c, &ca), &cb) in px.iter_mut().zip(from_a).zip(from_b) {
                    *c = ((u16::from(ca) + u16::from(cb)) / 2) as u8;
                }
                px[3] = 255;
            }
        }
    }
}

/// Octaves of value noise from 256 px cells down, each 0.6 of the one before; in 0..1.
fn fbm(x: u32, y: u32, (w, h): (u32, u32), seed: u32, octaves: u32) -> f32 {
    let (mut sum, mut amplitude, mut total) = (0.0, 1.0, 0.0);
    let mut cell = 256;
    for _ in 0..octaves {
        sum += amplitude * value_noise(x, y, cell, (w, h), seed);
        total += amplitude;
        amplitude *= 0.6;
        cell = (cell / 2).max(1);
    }
    sum / total
}

fn value_noise(x: u32, y: u32, cell: u32, (w, h): (u32, u32), seed: u32) -> f32 {
    let (across, down) = ((w / cell).max(1), (h / cell).max(1));
    let (i, j) = (x / cell, y / cell);
    let smooth = |t: u32| {
        let t = t as f32 / cell as f32;
        t * t * (3.0 - 2.0 * t)
    };
    let (sx, sy) = (smooth(x % cell), smooth(y % cell));
    let at = |i: u32, j: u32| lattice(i % across, j % down, cell, seed);
    let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
    lerp(
        lerp(at(i, j), at(i + 1, j), sx),
        lerp(at(i, j + 1), at(i + 1, j + 1), sx),
        sy,
    )
}

fn lattice(i: u32, j: u32, cell: u32, seed: u32) -> f32 {
    let mut h = i.wrapping_mul(0x8da6_b343)
        ^ j.wrapping_mul(0xd816_3841)
        ^ cell.wrapping_mul(0xcb1a_b31f)
        ^ seed.wrapping_mul(0x1656_67b1);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297a_2d39);
    h ^= h >> 15;
    h as f32 / u32::MAX as f32
}

/// Fresh noise every picture, seeded by `n`: the worst case, as nothing predicts it.
fn paint_fresh_noise(picture: &mut [u8], width: u32, (left, top, rw, rh): Rect, n: u32) {
    let mut state = (n.wrapping_mul(0x9e37_79b9) ^ 0x6a09_e667) | 1;
    for y in top..top + rh {
        let start = ((y * width + left) * 4) as usize;
        for px in picture[start..start + (rw * 4) as usize].chunks_exact_mut(4) {
            px.copy_from_slice(&noise_pixel(&mut state));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{VideoFrame, testing::read_stamp};

    /// The smallest test pattern a share may ask for draws every workload, its square bouncing
    /// off each edge, and stamps each picture readably.
    #[test]
    fn every_workload_draws_at_the_smallest_size() {
        use TestMotion::*;
        for motion in [Still, Thirty, Sixty, Scroll, ScrollTexture, ScrollNoise] {
            let mut pattern = Pattern::new(motion, SMALLEST, None);
            for n in 0..200 {
                let stamp = Stamp {
                    frame: n,
                    captured_micros: u64::from(n) * 33_333,
                };
                let changed = pattern.paint(n);
                pattern.stamp(changed, stamp);
            }
            let frame = VideoFrame {
                width: SMALLEST.0,
                height: SMALLEST.1,
                bgra: pattern.picture().to_vec(),
                source: SMALLEST,
                decoded: 0,
                sharer: None,
            };
            assert_eq!(read_stamp(&frame).map(|s| s.frame), Some(199), "{motion:?}");
        }
    }
}
