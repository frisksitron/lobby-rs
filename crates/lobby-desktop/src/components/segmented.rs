//! A choice of a few options in one track, such as a Screen Share's quality. The left and right
//! keys move the choice while the track has the focus.

use std::rc::Rc;

use gpui_kit::{
    App, ElementId, FocusHandle, InteractiveElement as _, IntoElement, ParentElement as _,
    RenderOnce, Role, SharedString, StatefulInteractiveElement as _, Styled as _,
    TestSupportExt as _, Toggled, Window,
    assets::IconName,
    component::{ActiveTheme as _, Icon, tooltip::Tooltip},
    div,
    prelude::FluentBuilder as _,
    px,
};

use super::{BORDER, ICON_SM, nested_radius};
use crate::scale::u;

/// The track, the padding around its segments and the segments' corners.
const TRACK_RADIUS: f32 = 8.;
const SEGMENT_PAD: f32 = 3.;
const SEGMENT_RADIUS: f32 = nested_radius(TRACK_RADIUS, BORDER + SEGMENT_PAD);
const TRACK_H: f32 = 34.;
const COMPACT_TRACK_H: f32 = 26.;

pub type SelectHandler = Rc<dyn Fn(&mut Window, &mut App)>;

struct Segment {
    label: SharedString,
    icon: Option<IconName>,
    selected: bool,
    on_select: SelectHandler,
    tip: Option<SharedString>,
}

#[derive(IntoElement)]
pub struct Segmented {
    id: ElementId,
    focus: Option<FocusHandle>,
    label: Option<SharedString>,
    compact: bool,
    options: Vec<Segment>,
}

impl Segmented {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            focus: None,
            label: None,
            compact: false,
            options: Vec::new(),
        }
    }

    pub fn focus(mut self, focus: &FocusHandle) -> Self {
        self.focus = Some(focus.clone());
        self
    }

    /// What a screen reader calls the choice.
    pub fn accessibility_label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Shorter, with the small text: for a choice inside a panel.
    pub fn compact(mut self) -> Self {
        self.compact = true;
        self
    }

    pub fn option(
        mut self,
        label: impl Into<SharedString>,
        icon: IconName,
        selected: bool,
        on_select: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        self.options.push(Segment {
            label: label.into(),
            icon: Some(icon),
            selected,
            on_select: Rc::new(on_select),
            tip: None,
        });
        self
    }

    pub fn word(
        mut self,
        label: impl Into<SharedString>,
        selected: bool,
        on_select: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        self.options.push(Segment {
            label: label.into(),
            icon: None,
            selected,
            on_select: Rc::new(on_select),
            tip: None,
        });
        self
    }

    /// A tooltip for the option added last.
    pub fn tip(mut self, tip: impl Into<SharedString>) -> Self {
        if let Some(last) = self.options.last_mut() {
            last.tip = Some(tip.into());
        }
        self
    }
}

impl RenderOnce for Segmented {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let (fg, muted, raised, track, border, ring) = (
            theme.foreground,
            theme.muted_foreground,
            theme.background,
            theme.secondary,
            theme.border,
            theme.ring,
        );
        let selector = self.id.to_string();
        let focused = self.focus.as_ref().is_some_and(|f| f.is_focused(window));
        let handlers: Vec<SelectHandler> =
            self.options.iter().map(|o| o.on_select.clone()).collect();
        let chosen = self.options.iter().position(|o| o.selected).unwrap_or(0);
        let compact = self.compact;
        let count = self.options.len();
        div()
            .id(self.id.clone())
            .debug_selector(move || selector)
            .role(Role::RadioGroup)
            .when_some(self.label.clone(), |d, label| d.aria_label(label))
            .w_full()
            .h(u(if compact { COMPACT_TRACK_H } else { TRACK_H }))
            .p(u(SEGMENT_PAD))
            .flex()
            .gap(u(SEGMENT_PAD))
            .rounded(u(TRACK_RADIUS))
            .bg(track)
            .border_1()
            .border_color(if focused { ring } else { track })
            .when(focused, |d| {
                d.shadow(vec![gpui_kit::BoxShadow {
                    color: ring.opacity(0.35),
                    offset: gpui_kit::point(px(0.), px(0.)), // px: BoxShadow takes pixels
                    blur_radius: px(0.),                     // px: BoxShadow takes pixels
                    spread_radius: px(3.),                   // px: BoxShadow takes pixels
                    inset: false,
                }])
            })
            .when_some(self.focus.clone(), |d, focus| {
                d.track_focus(&focus).on_key_down(move |ev, window, cx| {
                    let to = match ev.keystroke.key.as_str() {
                        "left" => chosen.checked_sub(1),
                        "right" => Some(chosen + 1).filter(|i| *i < handlers.len()),
                        _ => return,
                    };
                    cx.stop_propagation();
                    if let Some(select) = to.and_then(|i| handlers.get(i)) {
                        select(window, cx);
                    }
                })
            })
            .children(self.options.into_iter().enumerate().map(
                |(
                    i,
                    Segment {
                        label,
                        icon,
                        selected,
                        on_select,
                        tip,
                    },
                )| {
                    let focus = self.focus.clone();
                    div()
                        .id(SharedString::from(format!("{}-{}", self.id, slug(&label))))
                        .debug_selector({
                            let label = label.clone();
                            move || format!("segment-{label}")
                        })
                        .role(Role::RadioButton)
                        .aria_label(label.clone())
                        .aria_toggled(if selected {
                            Toggled::True
                        } else {
                            Toggled::False
                        })
                        .aria_position_in_set(i + 1)
                        .aria_size_of_set(count)
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .rounded(u(SEGMENT_RADIUS))
                        .flex()
                        .items_center()
                        .justify_center()
                        .gap_1p5()
                        .map(|d| if compact { d.text_xs() } else { d.text_sm() })
                        .cursor_pointer()
                        .map(|d| {
                            if selected {
                                d.bg(raised)
                                    .border_1()
                                    .border_color(border)
                                    .shadow_xs()
                                    .text_color(fg)
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                            } else {
                                d.text_color(muted).hover(|s| s.text_color(fg))
                            }
                        })
                        .children(icon.map(|icon| Icon::new(icon).size(u(ICON_SM))))
                        .child(label)
                        .when_some(tip, |d, tip| {
                            d.tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                        })
                        .on_click(move |_, window, cx| {
                            if let Some(focus) = &focus {
                                focus.focus(window, cx);
                            }
                            on_select(window, cx)
                        })
                        .test_support()
                },
            ))
    }
}

/// A label as a part of an element id: "Up to 1080p" is `up-to-1080p`.
fn slug(label: &str) -> String {
    label.to_lowercase().replace(' ', "-")
}
