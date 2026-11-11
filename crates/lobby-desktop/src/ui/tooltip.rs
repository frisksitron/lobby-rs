//! The tooltips of Lobby's own controls. Each window holds one overlay, in a root plugin, so a
//! tooltip hangs from its control, rises in when it appears and slides along a row of neighbours
//! instead of blinking out and in. The show delay and the grace period between neighbours are the
//! kit's (`base::TooltipDefaults`), so these tooltips and the kit's keep one rhythm.

use std::{cell::RefCell, rc::Rc, rc::Weak, time::Duration};

use gpui_kit::{
    AnyElement, AnyView, App, AppContext as _, Bounds, Context, DispatchPhase, Entity,
    GlobalElementId, IntoElement, MouseButton, MouseMoveEvent, ParentElement, Pixels, Render,
    SharedString, StatefulInteractiveElement, Styled, Window,
    base::{
        ElementExt as _, Root, RootPlugin, StyledExt as _, Tooltip, TooltipOverlay, TooltipRequest,
        TooltipTransition,
        animation::{EffectTransition, ease_in_out_cubic, ease_out_cubic},
    },
    canvas, div, px,
};

use crate::kit::ActiveTheme as _;

const ENTER: Duration = Duration::from_millis(150);
const SLIDE: Duration = Duration::from_millis(200);
/// How far a tooltip rises as it fades in.
const RISE: Pixels = px(4.); // px: the transition takes pixels
/// Neighbours whose tops lie this close share a row, so their tooltip slides between them; across
/// rows it moves at once.
const SAME_ROW: Pixels = px(10.); // px: compared with measured bounds

/// Registers the overlay for the windows opened after it.
pub(crate) fn init(cx: &mut App) {
    Root::register_plugin::<TooltipHost>(cx, TooltipHost::new);
}

/// One window's tooltip: the overlay that shows it, the control the pointer is on, and the
/// control last pressed, whose tooltip stays away until the pointer leaves it.
pub(crate) struct TooltipHost {
    overlay: Entity<TooltipOverlay>,
    hovered: Option<(GlobalElementId, Weak<()>)>,
    pressed: Option<Bounds<Pixels>>,
}

impl TooltipHost {
    fn new(_: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            overlay: cx.new(|_| TooltipOverlay::new().render_with(transition)),
            hovered: None,
            pressed: None,
        }
    }

    fn of(window: &Window, cx: &App) -> Option<Entity<Self>> {
        window.root::<Root>()??.read(cx).plugin::<Self>()
    }

    /// A tooltip whose control left the window goes with it: the control never sees the pointer
    /// leave. Checked after the frame drops the state of the elements it no longer draws.
    fn drop_orphan(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((id, _)) = self.hovered.clone() else {
            return;
        };
        let host = cx.weak_entity();
        window.defer(cx, move |_, cx| {
            let _ = host.update(cx, |host, cx| {
                let orphaned = host
                    .hovered
                    .as_ref()
                    .is_some_and(|(hovered, alive)| *hovered == id && alive.upgrade().is_none());
                if orphaned {
                    host.hovered = None;
                    host.overlay.update(cx, |overlay, cx| overlay.hide(cx));
                }
            });
        });
    }

    /// Ends a press on the first move outside the pressed control. It listens on the window, as a
    /// control that a press removed or moved never reports the pointer leaving.
    fn press_listener(&self, cx: &Context<Self>) -> Option<impl IntoElement + use<>> {
        let bounds = self.pressed?;
        let host = cx.weak_entity();
        Some(canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                    if phase != DispatchPhase::Capture || bounds.contains(&event.position) {
                        return;
                    }
                    let _ = host.update(cx, |host, cx| {
                        if host.pressed == Some(bounds) {
                            host.pressed = None;
                            cx.notify();
                        }
                    });
                });
            },
        ))
    }
}

impl RootPlugin for TooltipHost {
    fn prepare(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.drop_orphan(window, cx);
    }
}

impl Render for TooltipHost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .child(self.overlay.clone())
            .children(self.press_listener(cx))
    }
}

/// Where a control is this frame, and a token that lives as long as the control is drawn.
#[derive(Default)]
struct Trigger {
    id: Option<GlobalElementId>,
    alive: Weak<()>,
    bounds: Bounds<Pixels>,
}

#[derive(Default)]
struct Alive(Rc<()>);

/// Gives `control` the tooltip `text`. A window without the host draws none.
pub(crate) fn with_tooltip<E>(control: E, text: SharedString) -> E
where
    E: StatefulInteractiveElement + ParentElement,
{
    let trigger = Rc::new(RefCell::new(Trigger::default()));
    let seen = trigger.clone();
    let hovered = trigger.clone();
    control
        .on_prepaint(move |bounds, window, _| {
            let id = window.with_global_id("tooltip-trigger".into(), |id, _| id.clone());
            // Element state lives while the element is drawn, so this token dies with it.
            let alive = window.with_element_state(&id, |state: Option<Alive>, _| {
                let state = state.unwrap_or_default();
                (Rc::downgrade(&state.0), state)
            });
            *seen.borrow_mut() = Trigger {
                id: Some(id),
                alive,
                bounds,
            };
        })
        .on_hover(move |entered, window, cx| {
            let trigger = hovered.borrow();
            let (Some(id), Some(host)) = (trigger.id.clone(), TooltipHost::of(window, cx)) else {
                return;
            };
            let bounds = trigger.bounds;
            let alive = trigger.alive.clone();
            let overlay = host.update(cx, |host, _| {
                if *entered {
                    // A press keeps the tooltip away until the pointer leaves the control.
                    if host.pressed == Some(bounds) {
                        return None;
                    }
                    host.hovered = Some((id, alive));
                } else if host.hovered.as_ref().is_some_and(|(h, _)| *h == id) {
                    host.hovered = None;
                } else {
                    // Hover events run in reverse paint order: the next control may have taken
                    // the tooltip before this one hears that the pointer left.
                    return None;
                }
                Some(host.overlay.clone())
            });
            let Some(overlay) = overlay else {
                return;
            };
            let text = text.clone();
            overlay.update(cx, |overlay, cx| {
                if *entered {
                    let request = TooltipRequest::new(bounds, move |_, cx| {
                        let text = text.clone();
                        AnyView::from(cx.new(|_| TooltipText(text)))
                    });
                    overlay.request_show(request, window, cx);
                } else {
                    overlay.request_hide(window, cx);
                }
            });
        })
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            let Some(host) = TooltipHost::of(window, cx) else {
                return;
            };
            let bounds = trigger.borrow().bounds;
            host.update(cx, |host, cx| {
                host.pressed = Some(bounds);
                host.overlay.update(cx, |overlay, cx| overlay.hide(cx));
                cx.notify();
            });
        })
}

/// The tooltip itself: a line of text on the popover surface.
struct TooltipText(SharedString);

impl Render for TooltipText {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        // The outer box holds the margin, so the gap to the control is the same on every side.
        div().child(
            Tooltip::new("tooltip-popup")
                .h_flex()
                .font_family(theme.font_family.clone())
                .m_3()
                .py_0p5()
                .px_2()
                .gap_3()
                .justify_between()
                .bg(theme.tokens.popover)
                .text_color(theme.popover_foreground)
                .text_sm()
                .border_1()
                .border_color(theme.border)
                .rounded(theme.radius)
                .shadow_md()
                .child(div().child(self.0.clone())),
        )
    }
}

/// Rises and fades in on entering; along a row of neighbours it slides from the last control to
/// this one instead.
fn transition(
    view: AnyView,
    transition: TooltipTransition,
    _: &mut Window,
    _: &mut App,
) -> AnyElement {
    let el = div().child(view);
    match transition {
        TooltipTransition::Enter { epoch } => EffectTransition::new(ENTER)
            .ease(ease_out_cubic)
            .slide_y(RISE, Pixels::ZERO)
            .fade(0., 1.)
            .apply(el, ("tooltip-enter", epoch))
            .into_any_element(),
        TooltipTransition::Switch {
            epoch,
            previous,
            current,
        } if (current.origin.y - previous.origin.y).abs() < SAME_ROW => {
            let from = previous.center().x - current.center().x;
            EffectTransition::new(SLIDE)
                .ease(ease_in_out_cubic)
                .slide_x(from, Pixels::ZERO)
                .apply(el, ("tooltip-slide", epoch))
                .into_any_element()
        }
        TooltipTransition::Switch { .. } => el.into_any_element(),
    }
}
