//! Lobby's motion: three near-critical springs and the few things built on them. Each runs on
//! base's `spring` (the executor clock, velocity kept across a retarget), so each lands on its
//! target at once under reduced motion. GPUI has no transform: a surface moves by a content
//! offset and a colour changes by a mix in OKLab.

use std::{f32::consts::TAU, time::Duration};

use gpui_kit::{
    App, Background, ElementId, Hsla, InteractiveElement as _, ParentElement, Pixels, Point,
    Styled, Window,
    base::{Spring, spring},
    div, point,
};

use crate::{kit::Colorize as _, scale::u};

/// Colour, hover and press release, small marks: 95% in ~95 ms.
pub const QUICK: Spring = Spring::new(Duration::from_millis(126));
const MOVE_RESPONSE: Duration = Duration::from_millis(189);
/// Something changing place, such as a switch's thumb: 95% in ~143 ms.
pub const MOVE: Spring = Spring::new(MOVE_RESPONSE);
/// A surface appearing: 95% in ~168 ms.
pub const OPEN: Spring = Spring::new(Duration::from_millis(222));

/// How far a popup, a sheet or a new row travels as it comes in.
const RISE: f32 = 4.;

/// ω·t at which a critically damped spring has covered 99% of its way.
const SETTLED: f32 = 6.64;

/// A value that is 1 while `on` and falls back to 0 on [`QUICK`]: a press or hover shows at once
/// and blends away.
pub fn snap_in(id: (ElementId, &'static str), on: bool, window: &mut Window, cx: &mut App) -> f32 {
    spring(
        id,
        if on { 1. } else { 0. },
        QUICK.with_travel(!on),
        window,
        cx,
    )
}

/// `from` blended `t` of the way to `to`. The ends are the colours themselves, untouched by the
/// trip through OKLab.
pub fn mix(from: Hsla, to: Hsla, t: f32) -> Hsla {
    if t <= 0. {
        from
    } else if t >= 1. {
        to
    } else {
        to.mix_oklab(from, t)
    }
}

/// [`mix`] for fills. A gradient does not blend: it takes over halfway.
pub fn mix_fill(from: Background, to: Background, t: f32) -> Background {
    match (from.as_solid(), to.as_solid()) {
        _ if t <= 0. => from,
        _ if t >= 1. => to,
        (Some(from), Some(to)) => mix(from, to, t).into(),
        _ if t < 0.5 => from,
        _ => to,
    }
}

/// A critically damped spring's way from 0 to 1, setting off at rest, when ω·t is `k`.
fn course(k: f32) -> f32 {
    1. - (1. + k) * (-k).exp()
}

/// [`OPEN`]'s course as an easing over a duration, for the kit's dialogs, which ease rather than
/// spring: a critically damped approach that arrives at t = 1.
pub fn open_curve(t: f32) -> f32 {
    (course(SETTLED * t.clamp(0., 1.)) / course(SETTLED)).min(1.)
}

/// How far a row of a virtual list has come in on [`MOVE`], `since` it arrived; `None` once it is
/// there. A row's spring state would die as it scrolls away, so its owner keeps the instant and
/// this gives the spring's course from it. The owner skips the arrival under reduced motion.
pub fn arrived(since: Duration) -> Option<f32> {
    let k = TAU * since.as_secs_f32() / MOVE_RESPONSE.as_secs_f32();
    (k < SETTLED).then(|| course(k))
}

/// A row [`arrived`] `came` of the way: faded, and short of its place by what is left of [`RISE`].
pub fn arriving<E: Styled>(row: E, came: f32) -> E {
    shift(row.opacity(came), RISE * (1. - came), Side::Bottom)
}

/// Moves `element` `by` design pixels toward `from`, as a content offset.
fn shift<E: Styled>(element: E, by: f32, from: Side) -> E {
    if by <= 0. {
        return element;
    }
    match from {
        Side::Top => element.relative().top(u(-by)),
        Side::Bottom => element.relative().top(u(by)),
        Side::Left => element.relative().left(u(-by)),
        Side::Right => element.relative().left(u(by)),
    }
}

/// The side a surface comes in from: a popup's anchor, a sheet's window edge.
#[derive(Clone, Copy)]
pub enum Side {
    Top,
    Bottom,
    Left,
    Right,
}

/// How far a popup has come in. It opens on [`OPEN`], fading in and moving [`RISE`] away from its
/// anchor, and closes on a [`QUICK`] fade where it stands. It is drawn until the fade ends.
#[derive(Clone, Copy)]
pub struct Reveal {
    open: bool,
    shown: f32,
    travel: f32,
}

impl Reveal {
    /// Call on every frame, open or not, from an element drawn on every frame: the springs live
    /// in its element state.
    pub fn new(id: impl Into<ElementId>, open: bool, window: &mut Window, cx: &mut App) -> Self {
        let id = id.into();
        let shown = Self::fade(id.clone(), open, window, cx);
        // Travel happens on the way in only: it rests at the end while the popup is drawn and
        // goes back to the start unseen.
        let drawn = open || shown > 0.;
        let travel = spring(
            (id, "travel"),
            if drawn { 1. } else { 0. },
            OPEN.with_travel(drawn),
            window,
            cx,
        );
        Self {
            open,
            shown,
            travel,
        }
    }

    /// [`Reveal::new`] for a sheet at a window's edge: it goes back the way it came, sliding out
    /// as it fades.
    pub fn sheet(id: impl Into<ElementId>, open: bool, window: &mut Window, cx: &mut App) -> Self {
        let shown = Self::fade(id.into(), open, window, cx);
        Self {
            open,
            shown,
            travel: shown,
        }
    }

    fn fade(id: ElementId, open: bool, window: &mut Window, cx: &mut App) -> f32 {
        let (target, policy) = if open { (1., OPEN) } else { (0., QUICK) };
        spring((id, "shown"), target, policy, window, cx)
    }

    pub fn is_drawn(self) -> bool {
        self.open || self.shown > 0.
    }

    pub fn is_closing(self) -> bool {
        !self.open && self.shown > 0.
    }

    /// How far it has faded in, for what fades with it, such as a backdrop.
    pub fn opacity(self) -> f32 {
        self.shown
    }

    /// How far [`Self::apply`] moves the surface from where it rests.
    pub fn offset(self, from: Side, window: &Window) -> Point<Pixels> {
        let by = self.travel_left();
        let at = |by: f32| u(by).to_pixels(window.rem_size());
        match from {
            Side::Top => point(Pixels::ZERO, at(-by)),
            Side::Bottom => point(Pixels::ZERO, at(by)),
            Side::Left => point(at(-by), Pixels::ZERO),
            Side::Right => point(at(by), Pixels::ZERO),
        }
    }

    fn travel_left(self) -> f32 {
        RISE * (1. - self.travel)
    }

    /// Moves and fades `surface`. Closing, it takes no input: a shield over it stops presses and
    /// the pointer, and lets nothing through to what lies under it.
    pub fn apply<E: Styled + ParentElement>(self, mut surface: E, from: Side) -> E {
        if self.shown < 1. {
            surface = surface.opacity(self.shown);
        }
        surface = shift(surface, self.travel_left(), from);
        if self.is_closing() {
            surface = surface.child(div().absolute().inset_0().occlude());
        }
        surface
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, rc::Rc};

    use gpui_kit::{Context, IntoElement, Render, TestAppContext, VisualTestContext};

    use super::*;

    struct Popup {
        open: bool,
        drawn: Rc<Cell<bool>>,
    }

    impl Render for Popup {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let reveal = Reveal::new("popup", self.open, window, cx);
            self.drawn.set(reveal.is_drawn());
            div()
        }
    }

    fn frame(window: &mut VisualTestContext) {
        window.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
    }

    #[gpui_kit::test]
    fn a_closed_popup_is_drawn_until_its_fade_ends(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(false));
        let drawn = Rc::new(Cell::new(false));
        let shown = drawn.clone();
        let (popup, window) = cx.add_window_view(move |_, _| Popup {
            open: true,
            drawn: shown,
        });
        let open = |open: bool, window: &mut VisualTestContext| {
            popup.update(window, |popup, _| popup.open = open);
            frame(window);
        };
        frame(window);

        open(false, window);
        assert!(drawn.get(), "a closing popup is drawn while it fades");
        window.executor().advance_clock(Duration::from_millis(400));
        frame(window);
        assert!(!drawn.get(), "the faded popup is gone");

        window.update(|_, cx| cx.set_reduce_motion(true));
        open(true, window);
        open(false, window);
        assert!(!drawn.get(), "under reduced motion a popup goes at once");
    }
}
