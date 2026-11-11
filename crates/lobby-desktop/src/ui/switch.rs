//! Lobby's switch. `base::Switch` owns the toggle: Space and Enter, focus, the accessibility role
//! and the disabled state; this draws the track, the thumb and their travel.
//!
//! ```ignore
//! Switch::new("share-let-draw")
//!     .checked(on)
//!     .accessibility_label("Let others draw")
//!     .on_change(|next, window, cx| ..)
//! ```

use std::rc::Rc;

use gpui_kit::{
    AnyElement, App, Background, ElementId, IntoElement, ParentElement as _, Pixels,
    Refineable as _, RenderOnce, SharedString, StyleRefinement, Styled, Window,
    base::{self, SwitchThumb, SwitchTrack, spring},
    prelude::FluentBuilder as _,
};

use super::focus;
use crate::{
    kit::ActiveTheme as _,
    scale::{device, u},
};

/// The track's size at 100%, and the thumb's, at the two sizes.
const TRACK: (f32, f32) = (36., 20.);
const THUMB: f32 = 16.;
const TRACK_SMALL: (f32, f32) = (28., 16.);
const THUMB_SMALL: f32 = 12.;
/// The room between the track's edge and the thumb.
const INSET: f32 = 2.;

type Change = Rc<dyn Fn(bool, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct Switch {
    id: ElementId,
    style: StyleRefinement,
    checked: bool,
    disabled: bool,
    small: bool,
    embedded: bool,
    accessibility_label: Option<SharedString>,
    on_change: Option<Change>,
}

impl Switch {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            style: StyleRefinement::default(),
            checked: false,
            disabled: false,
            small: false,
            embedded: false,
            accessibility_label: None,
            on_change: None,
        }
    }

    /// The value the owner holds: the switch only asks for the next one, through
    /// [`Self::on_change`].
    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// The compact switch, for a bar's height.
    pub fn small(mut self) -> Self {
        self.small = true;
        self
    }

    /// The switch is the end of a line that takes the press and the focus: it is no tab stop of
    /// its own and draws no ring, which the line would clip.
    pub fn embedded(mut self) -> Self {
        self.embedded = true;
        self
    }

    /// The name a screen reader announces. A switch has no text of its own.
    pub fn accessibility_label(mut self, label: impl Into<SharedString>) -> Self {
        self.accessibility_label = Some(label.into());
        self
    }

    /// Runs with the value a press or key asks for.
    pub fn on_change(mut self, handler: impl Fn(bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }
}

impl Styled for Switch {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for Switch {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (checked, disabled) = (self.checked, self.disabled);
        let ((track_w, track_h), thumb) = if self.small {
            (TRACK_SMALL, THUMB_SMALL)
        } else {
            (TRACK, THUMB)
        };
        let theme = cx.theme();
        let checked_bg: Background = theme.tokens.primary.into();
        let unchecked_bg: Background = theme.tokens.switch.into();
        // GPUI multiplies each primitive's alpha by the element's opacity, so fading the whole
        // control would let the track show through the thumb. Fading the track alone gives the
        // pixels a grouped fade would: the thumb is the page's colour.
        let disabled_bg = if checked { checked_bg } else { unchecked_bg }.opacity(0.5);
        let thumb_bg: Background = theme.tokens.switch_thumb.into();
        let (clear, ring_color, muted) = (theme.transparent, theme.ring, theme.muted_foreground);
        // A pill, and as round as the theme allows when it squares its corners.
        let radius = if theme.radius >= device(4., window) {
            device(track_h, window)
        } else {
            theme.radius
        };
        let spring_move = theme.motion_tokens().spring_move;
        let handle = window
            .use_keyed_state(self.id.clone(), cx, |_, cx| cx.focus_handle())
            .read(cx)
            .clone();
        let focused = handle.is_focused(window) && !self.embedded && !disabled;

        // The thumb's place is geometry, not a semantic state: a checked style that set `left`
        // would outrank the instance style. The spring owns it end to end and turns round from
        // wherever the thumb is when the switch is toggled again mid-travel.
        let travel: Pixels = device(track_w - thumb - INSET * 2., window);
        let thumb_x = spring(
            (self.id.clone(), "thumb"),
            if checked { travel } else { Pixels::ZERO },
            spring_move,
            window,
            cx,
        );

        let mut track = SwitchTrack::new((self.id.clone(), "track"))
            .checked(checked)
            .disabled(disabled)
            .w(u(track_w))
            .h(u(track_h))
            .flex_shrink_0()
            .rounded(radius)
            .flex()
            .items_center()
            // The thumb's inset is a 1px edge plus 1px of padding: the focus ring tints the edge,
            // and that line keeps the ring visible round an unchecked track.
            .border_1()
            .border_color(clear)
            .p_px()
            .when(!checked, |track| track.bg(unchecked_bg))
            .styles(|styles| {
                styles
                    .checked(|style| style.bg(checked_bg))
                    .disabled(|style| style.bg(disabled_bg))
            });
        let ring: Option<AnyElement> = focused.then(|| focus::ring(track.style(), window, cx));
        let track = track
            .when(focused, |track| track.border_color(ring_color))
            .child(
                SwitchThumb::new(checked)
                    .rounded(radius)
                    .size(u(thumb))
                    .left(thumb_x)
                    .bg(thumb_bg),
            )
            .children(ring);

        let on_change = self.on_change;
        let mut root = base::Switch::new(self.id)
            .checked(checked)
            .disabled(disabled)
            .tab_stop(!self.embedded)
            .track_focus(&handle)
            .styles(|styles| styles.disabled(|style| style.text_color(muted).cursor_not_allowed()))
            .when_some(self.accessibility_label, |root, label| {
                root.accessibility_label(label)
            })
            .when_some(on_change, |root, on_change| {
                root.on_change(move |next, _, window, cx| on_change(next, window, cx))
            });
        root.style().refine(&self.style);
        root.child(track)
    }
}
