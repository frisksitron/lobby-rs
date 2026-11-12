//! A control with a fill or an edge at rest is a key. Its face keeps the control's box and a lip,
//! a step darker than the face at rest, hangs under it. A press drops the face onto the lip, and a
//! latched or inert key lies flush. A switch's or a slider's thumb is a small key, its lip inside.

use gpui_kit::{
    AbsoluteLength, AnyElement, App, Background, BoxShadow, DefiniteLength, Div, Hsla,
    InteractiveElement as _, IntoElement as _, Length, ParentElement as _, Pixels, StyleRefinement,
    Styled as _, Window, black, div, prelude::FluentBuilder as _,
};

use super::{focus, motion};
use crate::{kit::ActiveTheme as _, scale::device};

/// How much darker than its face at rest a lip is, in OKLab lightness. A fixed step, not a share
/// of the fill: a share of a dark fill is nothing, and its lip would melt into a dark page.
const LIP_SHADE: f32 = 0.15;
/// The same for an edged key whose fill is see-through, from its edge: a [`LIP_SHADE`] step off
/// the theme's edge lands on the page's own lightness.
const EDGE_LIP_SHADE: f32 = 0.2;
/// How much lighter than its face a key's top edge is, in OKLab lightness: its catch-light.
const CATCH: f32 = 0.05;
/// A key at least this tall, in design pixels, stands on a 2 px lip; a shorter one on 1 px.
const TALL: f32 = 32.;
/// The group of the control a key's face lies in, whose hover the face takes.
pub(super) const GROUP: &str = "key";

/// The lip under a control whose face rests as `fill` inside a drawn `edge`. `None` when it has
/// neither at rest: such a control is flat, and answers a press with colour alone.
pub(super) fn lip(fill: Background, edge: Option<Hsla>) -> Option<Hsla> {
    let fill = fill.as_solid().filter(|fill| !fill.is_transparent());
    match (fill, edge.filter(|edge| !edge.is_transparent())) {
        (Some(fill), _) if fill.is_opaque() => Some(shade(fill, LIP_SHADE)),
        (_, Some(edge)) => Some(shade(edge, EDGE_LIP_SHADE)),
        (wash, None) => wash.map(|wash| shade(wash, LIP_SHADE)),
    }
}

/// Where a key's face is on its lip.
#[derive(Clone, Copy)]
pub(super) struct Key {
    /// The lip's height, in design pixels.
    lip: f32,
    /// How far the face has gone down onto the lip, 0 to 1.
    sink: f32,
    /// How far the key stands off its surface: 1 at rest, 0 latched or inert.
    stand: f32,
    wall: Hsla,
}

impl Key {
    /// A key on the box `style`, whose lip is `wall`, `sink` of the way down under a press.
    /// Latched or inert, it lies flush with its neighbours; it stands back up at once, since a lip
    /// of one or two pixels can only grow in whole-pixel steps, which read as a stutter.
    pub(super) fn new(
        style: &StyleRefinement,
        wall: Hsla,
        sink: f32,
        latched: bool,
        window: &Window,
    ) -> Self {
        let short = design_height(style, window).is_some_and(|height| height < TALL);
        Self {
            lip: if short { 1. } else { 2. },
            sink,
            stand: if latched { 0. } else { 1. },
            wall,
        }
    }

    /// How far the face, and the content on it, is under its place in the box, in design pixels.
    pub(super) fn sunk(self) -> f32 {
        self.lip * self.sink
    }

    /// How far the key reaches under its box, in design pixels.
    fn below(self) -> f32 {
        self.lip * self.stand.max(self.sink)
    }

    /// The face on the box `style`, filled with `fill`, or with `hover` while the pointer is on
    /// its [`GROUP`]. Where the box has a border, the face draws it in `rim`, around the face and
    /// not the lip.
    pub(super) fn face(
        self,
        style: &StyleRefinement,
        fill: Background,
        hover: Option<Background>,
        rim: Hsla,
        window: &Window,
        cx: &App,
    ) -> AnyElement {
        let rem = window.rem_size();
        let length = |l: Option<AbsoluteLength>| l.map(|l| l.to_pixels(rem)).unwrap_or_default();
        let radii = &style.corner_radii;
        let rounded = |el: Div| {
            el.rounded_tl(length(radii.top_left))
                .rounded_tr(length(radii.top_right))
                .rounded_bl(length(radii.bottom_left))
                .rounded_br(length(radii.bottom_right))
        };
        // An absolute child is placed inside its parent's border: the face reaches over it.
        let edge = length(style.border_widths.top);
        let band = device(self.lip * (self.stand - self.sink).max(0.), window);
        let lip = (band > Pixels::ZERO).then(|| BoxShadow::new(Pixels::ZERO, -band, self.wall));
        let shadows = |fill: Background| -> Vec<BoxShadow> {
            // An edge is already a line of light around the face.
            let catch = fill
                .as_solid()
                .filter(|_| edge == Pixels::ZERO && self.stand > 0.)
                .and_then(|fill| Some(motion::mix(fill, catch_light(fill, cx)?, self.stand)))
                .map(|catch| BoxShadow::new(Pixels::ZERO, device(1., window), catch));
            lip.clone()
                .into_iter()
                .chain(catch)
                .map(BoxShadow::inset)
                .collect()
        };
        rounded(div())
            .absolute()
            .left(-edge)
            .right(-edge)
            .top(device(self.sunk(), window) - edge)
            .bottom(-edge - device(self.below(), window))
            .bg(fill)
            .shadow(shadows(fill))
            .when_some(hover, |face, hover| {
                face.group_hover(GROUP, |s| s.bg(hover).shadow(shadows(hover)))
            })
            // Paint order is fill, inset shadows, children, border: a border of the face's own
            // would run round the lip, so the rim is a child that stops where the lip starts.
            .when(edge > Pixels::ZERO, |face| {
                face.child(
                    rounded(div())
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .bottom(band)
                        .border(edge)
                        .border_color(rim),
                )
            })
            .into_any_element()
    }

    /// The keyboard focus ring around the key and its lip on the box `style`.
    pub(super) fn ring(self, style: &StyleRefinement, window: &Window, cx: &App) -> AnyElement {
        focus::ring_reaching(style, device(self.below(), window), window, cx)
    }
}

/// The lip and the catch-light inside a switch's or a slider's thumb, whose face is `thumb`.
pub(super) fn knob(thumb: Background, window: &Window, cx: &App) -> Vec<BoxShadow> {
    let Some(fill) = thumb.as_solid() else {
        return Vec::new();
    };
    let line = device(1., window);
    let lip = BoxShadow::new(Pixels::ZERO, -line, shade(fill, LIP_SHADE));
    let catch = catch_light(fill, cx).map(|catch| BoxShadow::new(Pixels::ZERO, line, catch));
    std::iter::once(lip)
        .chain(catch)
        .map(BoxShadow::inset)
        .collect()
}

/// The height the box `style` asks for, in design pixels, when it is a fixed one.
fn design_height(style: &StyleRefinement, window: &Window) -> Option<f32> {
    match style.size.height? {
        Length::Definite(DefiniteLength::Absolute(height)) => {
            Some(height.to_pixels(window.rem_size()) / device(1., window))
        }
        _ => None,
    }
}

/// `fill` [`CATCH`] lighter, toward the theme's light end; `None` when it is already that close.
fn catch_light(fill: Hsla, cx: &App) -> Option<Hsla> {
    let theme = cx.theme();
    let light = if theme.mode.is_dark() {
        theme.foreground
    } else {
        theme.background
    };
    let room = lightness(light) - lightness(fill);
    (room > CATCH).then(|| motion::mix(fill, light.alpha(fill.a), CATCH / room))
}

/// `color` made `by` darker in OKLab lightness, its hue kept.
fn shade(color: Hsla, by: f32) -> Hsla {
    let l = lightness(color);
    if l <= 0. {
        return color;
    }
    motion::mix(color, black().alpha(color.a), (by / l).min(1.))
}

/// OKLab lightness, 0 to 1.
fn lightness(color: Hsla) -> f32 {
    let rgb = color.to_rgb();
    let linear = |c: f32| {
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let (r, g, b) = (linear(rgb.r), linear(rgb.g), linear(rgb.b));
    let l = 0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b;
    let m = 0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b;
    let s = 0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b;
    0.210_454_26 * l.cbrt() + 0.793_617_8 * m.cbrt() - 0.004_072_047 * s.cbrt()
}
