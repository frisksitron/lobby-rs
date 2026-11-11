//! Lobby's slider for one value over a range. `base::Slider` owns the pointer, the value and its
//! accessibility actions (`SliderState`, `SliderEvent` and the range are base's, imported by their
//! path); this draws the bar, the thumb and its ring, and adds the arrow keys.
//!
//! ```ignore
//! let volume = cx.new(|_| SliderState::new().min(0.).max(200.).step(1.).default_value(100.));
//! Slider::new(&volume).accessibility_label("Volume")
//! ```

use gpui_kit::{
    App, Context, ElementId, Entity, InteractiveElement as _, IntoElement, KeyDownEvent,
    MouseButton, MouseDownEvent, MouseUpEvent, ParentElement as _, Refineable as _, RenderOnce,
    SharedString, StatefulInteractiveElement as _, StyleRefinement, Styled, Window,
    base::{self, SliderIndicator, SliderThumb, SliderTrack, slider::SliderState, spring},
    div,
    prelude::FluentBuilder as _,
    px, relative,
};

use super::focus::{RING_OPACITY, RING_WIDTH};
use crate::{kit::ActiveTheme as _, scale::u};

const THUMB: f32 = 16.;
const THUMB_RISE: f32 = 5.;
const BAR: f32 = 6.;
const GRIP: f32 = 24.;

#[derive(Default)]
struct Touch {
    hovered: bool,
    pressed: bool,
}

#[derive(IntoElement)]
pub struct Slider {
    state: Entity<SliderState>,
    style: StyleRefinement,
    disabled: bool,
    accessibility_label: Option<SharedString>,
}

impl Slider {
    pub fn new(state: &Entity<SliderState>) -> Self {
        Self {
            state: state.clone(),
            style: StyleRefinement::default(),
            disabled: false,
            accessibility_label: None,
        }
    }

    /// A disabled slider takes no value and draws no thumb.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn accessibility_label(mut self, label: impl Into<SharedString>) -> Self {
        self.accessibility_label = Some(label.into());
        self
    }
}

impl Styled for Slider {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

fn key_move(key: &str, state: &SliderState) -> Option<f32> {
    let step = state.accessibility_step();
    let range = state.max_value() - state.min_value();
    match key {
        "left" | "down" => Some(-step),
        "right" | "up" => Some(step),
        "home" => Some(-range),
        "end" => Some(range),
        _ => None,
    }
}

impl RenderOnce for Slider {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let disabled = self.disabled;
        let entity_id = self.state.entity_id();
        let end = self.state.read(cx).percentage().end;
        let theme = cx.theme();
        let bar = theme.tokens.slider_bar;
        let thumb_face = theme.tokens.slider_thumb;
        let (clear, foreground, ring) = (theme.transparent, theme.foreground, theme.ring);
        let pill = theme.radius_full();
        let spring_control = theme.motion_tokens().spring_control;

        let focus = window
            .use_keyed_state(("slider-focus", entity_id), cx, |_, cx| {
                cx.focus_handle().tab_stop(true)
            })
            .read(cx)
            .clone();
        let focused = focus.is_focused(window) && !disabled;
        let key: ElementId = ("slider-thumb-ring", entity_id).into();
        let touch = window.use_keyed_state((key.clone(), "touch"), cx, |_, _| Touch::default());
        let active = {
            let touch = touch.read(cx);
            focused || touch.hovered || touch.pressed
        };
        let grown = spring(
            (key, "ring"),
            if active { 1. } else { 0. },
            spring_control,
            window,
            cx,
        );
        let ring_width = px(RING_WIDTH) * grown; // px: the ring is an edge, as wide at every scale

        let set_pressed = |pressed: bool, touch: &Entity<Touch>, cx: &mut App| {
            touch.update(cx, |touch, cx| {
                if touch.pressed != pressed {
                    touch.pressed = pressed;
                    cx.notify();
                }
            });
        };
        let down = {
            let touch = touch.clone();
            move |_: &MouseDownEvent, _: &mut Window, cx: &mut App| set_pressed(true, &touch, cx)
        };
        let up = {
            let touch = touch.clone();
            move |_: &MouseUpEvent, _: &mut Window, cx: &mut App| set_pressed(false, &touch, cx)
        };
        let hover = {
            let touch = touch.clone();
            move |entered: &bool, _: &mut Window, cx: &mut App| {
                let entered = *entered;
                touch.update(cx, |touch, cx| {
                    if touch.hovered != entered {
                        touch.hovered = entered;
                        cx.notify();
                    }
                });
            }
        };

        let thumb = SliderThumb::new(&self.state)
            .disabled(disabled)
            .when(!disabled, |thumb| {
                thumb
                    .absolute()
                    .top(u(-THUMB_RISE))
                    .left(relative(end))
                    .ml(u(-THUMB / 2.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .flex_shrink_0()
                    .rounded(pill)
                    .bg(bar.opacity(0.5))
                    .size(u(THUMB))
                    .p_px()
                    .on_hover(hover)
                    // The base thumb stops a press to own the drag, so it is read on the way down.
                    .capture_any_mouse_down(down)
                    .capture_any_mouse_up(up.clone())
                    .on_mouse_up_out(MouseButton::Left, up)
                    .child(
                        div()
                            .flex_none()
                            .absolute()
                            .top(-ring_width)
                            .left(-ring_width)
                            .right(-ring_width)
                            .bottom(-ring_width)
                            .rounded(pill)
                            .border(ring_width)
                            .border_color(ring.alpha(RING_OPACITY * grown)),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .size_full()
                            .rounded(pill)
                            .bg(thumb_face),
                    )
            });

        let on_key = {
            let state = self.state.clone();
            move |event: &KeyDownEvent, window: &mut Window, cx: &mut App| {
                let Some(delta) = key_move(event.keystroke.key.as_str(), state.read(cx)) else {
                    return;
                };
                state.update(cx, |state, cx: &mut Context<SliderState>| {
                    state.step_thumb(false, delta, window, cx)
                });
                cx.stop_propagation();
            }
        };

        let mut base = base::Slider::new(&self.state)
            .disabled(disabled)
            .when_some(self.accessibility_label, |base, label| {
                base.accessibility_label(label)
            })
            .when(!disabled, |base| base.track_focus(&focus))
            .flex()
            .flex_1()
            .items_center()
            .justify_center()
            .w_full()
            .bg(clear)
            .text_color(foreground);
        base.style().refine(&self.style);
        let base = base.child(
            SliderTrack::new(&self.state)
                .disabled(disabled)
                // A press moves the value without taking the focus, as a button's does: the ring
                // is for the keyboard. It is stopped here, below the slider that takes the focus.
                .when(!disabled, |track| {
                    track.on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                })
                .flex()
                .items_center()
                .h(u(GRIP))
                .w_full()
                .flex_shrink_0()
                .child(
                    SliderIndicator::new(&self.state)
                        .relative()
                        .w_full()
                        .h(u(BAR))
                        .bg(bar.opacity(0.2))
                        .active(move |style| style.bg(bar.opacity(0.4)))
                        .rounded(pill)
                        .child(
                            div()
                                .absolute()
                                .h_full()
                                .left(relative(0.))
                                .right(relative(1. - end))
                                .bg(bar)
                                .rounded(pill),
                        )
                        .child(thumb),
                ),
        );
        div()
            .flex()
            .flex_1()
            .w_full()
            .when(!disabled, |keys| keys.on_key_down(on_key))
            .child(base)
    }
}
