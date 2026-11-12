//! Lobby's motion. What answers the person's own act (a switch's thumb, a popup or a sheet coming
//! and going) eases out at full speed from the first frame. A press's release and what others
//! set off (a new Message, typing, speaking) blend on a near-critical spring. Both are base's (the
//! executor clock), so each lands on its target at once under reduced motion. GPUI has no
//! transform: a surface moves by a content offset and a colour changes by a mix in OKLab.

use std::{f32::consts::TAU, time::Duration};

use gpui_kit::{
    App, Background, ElementId, Hsla, InteractiveElement as _, ParentElement, Pixels, Point,
    Styled, Window,
    base::{Spring, Transition, spring, transition},
    div, point,
};

use crate::{kit::Colorize as _, scale::u};

/// Colour, press release, small marks: 95% in ~95 ms. It keeps its velocity when it turns, so a
/// mark that comes and goes never jumps.
pub const QUICK: Spring = Spring::new(Duration::from_millis(126));
/// A new row's spring, which sets off gently: no one asked for the row.
const ARRIVE: Duration = Duration::from_millis(189);

/// The answer to the person's own toggle, such as a switch's thumb. A spring sets off at rest and
/// takes a frame or two to get going, which reads as a delay after the click.
pub fn respond() -> Transition {
    Transition::new(Duration::from_millis(160))
}

/// A popup or a sheet opening, as [`respond`] does: about 0.8 of the spring it replaced to settle,
/// as the switch's thumb is.
fn appear() -> Transition {
    Transition::new(Duration::from_millis(190))
}

/// A popup or a sheet closing.
fn vanish() -> Transition {
    Transition::new(Duration::from_millis(110))
}

/// How far a popup, a sheet or a new row travels as it comes in.
const RISE: f32 = 4.;

/// ω·t at which a critically damped spring has covered 99% of its way.
const SETTLED: f32 = 6.64;

/// A value that is 1 while `on` and falls back to 0 on [`QUICK`]: a press shows at once and
/// blends away.
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

/// The ease-out of [`respond`] as a plain curve, for the kit's dialogs, which take an easing
/// rather than a [`Transition`]: full speed at t = 0, arriving at t = 1.
pub fn ease_out(t: f32) -> f32 {
    1. - (1. - t.clamp(0., 1.)).powi(3)
}

/// How far a row of a virtual list has come in, `since` it arrived; `None` once it is there. A
/// row's spring state would die as it scrolls away, so its owner keeps the instant and this gives
/// the spring's course from it. The owner skips the arrival under reduced motion.
pub fn arrived(since: Duration) -> Option<f32> {
    let k = TAU * since.as_secs_f32() / ARRIVE.as_secs_f32();
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

/// How far a popup has come in. It opens fading in and moving [`RISE`] away from its anchor, and
/// closes on a fade where it stands, each at full speed from the first frame. It is drawn until
/// the fade ends.
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
        let (target, policy) = if drawn {
            (1_f32, appear())
        } else {
            (0., Transition::new(Duration::ZERO))
        };
        let travel = transition((id, "travel"), target, policy, window, cx);
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
        let (target, policy) = if open {
            (1_f32, appear())
        } else {
            (0., vanish())
        };
        transition((id, "shown"), target, policy, window, cx)
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
