//! The rack's knobs, COMP's Amount and MIC's GAIN: arrows, Page keys, Home and End, the wheel and a
//! vertical drag turn them, each through a percent.

use std::rc::Rc;

use gpui_kit::{
    AccessibleAction, App, Bounds, InteractiveElement as _, IntoElement, MouseButton,
    MouseMoveEvent, MouseUpEvent, ParentElement as _, PathBuilder, Pixels, Role, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, canvas,
    component::{Colorize as _, tooltip::Tooltip},
    div,
    prelude::FluentBuilder as _,
    px,
};

use super::rack_faces::{Inks, Pen, arc, circle};
use crate::scale::{u, window_scale};

const KNOB: f32 = 34.;
const KNOB_ARC_W: f32 = 2.5;
/// The box around the cap: the arc and the air outside it.
pub(super) const KNOB_BOX: f32 = KNOB + 2. * (KNOB_ARC_W + 3.);
/// The value's top, and its change per arrow key, per Page key, per wheel notch, and per pixel
/// of drag.
pub(super) const KNOB_MAX: u8 = 100;
const STEP: u8 = 5;
const PAGE: u8 = 20;
const DRAG_PX_PER_UNIT: f32 = 1.5;
const WHEEL_LINE: f32 = 20.;
/// The knob turns through 270 degrees, from bottom left to bottom right.
const KNOB_FROM: f32 = 225.;
const KNOB_SWEEP: f32 = 270.;

/// One knob: its element id, accessible name and tooltip, and the percent it stands at.
pub(super) struct Dial {
    pub id: &'static str,
    pub label: &'static str,
    pub tooltip: Option<SharedString>,
    pub value: u8,
    pub enabled: bool,
}

pub(super) type SetValue = Rc<dyn Fn(u8, &mut Window, &mut App)>;

pub(super) fn knob(
    dial: Dial,
    inks: Inks,
    on_set: SetValue,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement + use<> {
    let Dial {
        id,
        label,
        tooltip,
        value,
        enabled,
    } = dial;
    let focus = window
        .use_keyed_state(SharedString::from(format!("{id}-focus")), cx, |_, cx| {
            cx.focus_handle().tab_stop(true)
        })
        .read(cx)
        .clone();
    let focused = focus.is_focused(window);
    let drag = window.use_keyed_state(SharedString::from(format!("{id}-drag")), cx, |_, _| {
        None::<(f32, u8)>
    });
    let set = Rc::new(move |to: i32, window: &mut Window, cx: &mut App| {
        let to = to.clamp(0, i32::from(KNOB_MAX)) as u8;
        if enabled && to != value {
            on_set(to, window, cx);
        }
    });
    let now = i32::from(value);
    let scale = window_scale(window);
    let t = f32::from(value) / f32::from(KNOB_MAX);
    let dial = canvas(move |_, _, _| {}, {
        let (drag, set) = (drag.clone(), set.clone());
        move |bounds, _, window, _| {
            paint_knob(bounds, t, focused, inks, scale, window);
            window.on_mouse_event({
                let (drag, set) = (drag.clone(), set.clone());
                move |ev: &MouseMoveEvent, phase, window, cx| {
                    if !phase.bubble() {
                        return;
                    }
                    let Some((from_y, from)) = *drag.read(cx) else {
                        return;
                    };
                    if ev.pressed_button != Some(MouseButton::Left) {
                        drag.update(cx, |d, _| *d = None);
                        return;
                    }
                    let moved = (from_y - f32::from(ev.position.y)) / (DRAG_PX_PER_UNIT * scale);
                    set(i32::from(from) + moved.round() as i32, window, cx);
                }
            });
            window.on_mouse_event({
                let drag = drag.clone();
                move |_: &MouseUpEvent, phase, _, cx| {
                    if phase.bubble() && drag.read(cx).is_some() {
                        drag.update(cx, |d, _| *d = None);
                    }
                }
            });
        }
    })
    .size_full();
    let up = set.clone();
    let down = set.clone();
    let keys = set.clone();
    let wheel = set.clone();
    div()
        .id(id)
        .size(u(KNOB_BOX))
        .flex_shrink_0()
        .track_focus(&focus)
        .role(Role::Slider)
        .aria_label(label)
        .aria_numeric_value(f64::from(value))
        .aria_min_numeric_value(0.)
        .aria_max_numeric_value(f64::from(KNOB_MAX))
        .aria_numeric_value_step(f64::from(STEP))
        .when(enabled, |d| d.cursor_ns_resize())
        .when_some(tooltip, |d, tip| {
            d.tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        })
        .on_a11y_action(AccessibleAction::Increment, move |_, window, cx| {
            up(now + i32::from(STEP), window, cx)
        })
        .on_a11y_action(AccessibleAction::Decrement, move |_, window, cx| {
            down(now - i32::from(STEP), window, cx)
        })
        .on_key_down(move |ev, window, cx| {
            let (step, page) = (i32::from(STEP), i32::from(PAGE));
            let to = match ev.keystroke.key.as_str() {
                "up" | "right" => now + step,
                "down" | "left" => now - step,
                "pageup" => now + page,
                "pagedown" => now - page,
                "home" => 0,
                "end" => i32::from(KNOB_MAX),
                _ => return,
            };
            cx.stop_propagation();
            keys(to, window, cx);
        })
        .on_scroll(move |ev, window, cx| {
            let notch = px(WHEEL_LINE); // px: only counts notches, never drawn
            let dy = f32::from(ev.delta.pixel_delta(notch).y) / WHEEL_LINE;
            if dy == 0. {
                return;
            }
            cx.stop_propagation();
            let notches = if dy.abs() < 1. {
                dy.signum()
            } else {
                dy.round()
            };
            wheel(now + notches as i32 * i32::from(STEP), window, cx);
        })
        .on_mouse_down(MouseButton::Left, move |ev, window, cx| {
            drag.update(cx, |d, _| *d = Some((f32::from(ev.position.y), value)));
            focus.focus(window, cx);
            cx.stop_propagation();
        })
        .child(dial)
        .test_support()
}

/// The knob at `t` of its sweep: a track, the part turned lit, the cap and its pointer.
fn paint_knob(
    bounds: Bounds<Pixels>,
    t: f32,
    focused: bool,
    inks: Inks,
    scale: f32,
    window: &mut Window,
) {
    let pen = Pen {
        origin: bounds.origin,
        scale,
    };
    let c = (KNOB_BOX / 2., KNOB_BOX / 2.);
    let center = pen.at(c);
    let radius = KNOB / 2. + KNOB_ARC_W / 2. + 2.;
    let turned = KNOB_FROM - KNOB_SWEEP * t;
    let sweep = (KNOB_FROM, KNOB_FROM - KNOB_SWEEP);
    arc(window, pen, c, radius, sweep, KNOB_ARC_W, inks.unlit);
    if t > 0. {
        arc(
            window,
            pen,
            c,
            radius,
            (KNOB_FROM, turned),
            KNOB_ARC_W,
            inks.lit,
        );
    }
    if focused {
        circle(window, center, pen.d(KNOB / 2. + 1.), inks.focus);
    }
    circle(window, center, pen.d(KNOB / 2. - 1.), inks.edge);
    circle(
        window,
        center,
        pen.d(KNOB / 2. - 2.),
        inks.panel.mix_oklab(inks.text, 0.86),
    );
    let at = |r: f32| {
        let a = turned.to_radians();
        pen.at((c.0 + r * a.cos(), c.1 - r * a.sin()))
    };
    let mut pointer = PathBuilder::stroke(pen.d(2.));
    pointer.move_to(at(KNOB / 2. - 12.));
    pointer.line_to(at(KNOB / 2. - 5.));
    if let Ok(path) = pointer.build() {
        window.paint_path(path, inks.text);
    }
}
