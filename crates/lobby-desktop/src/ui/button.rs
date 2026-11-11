//! Lobby's button. `base::Button` owns press, Enter and Space, focus and the accessibility role;
//! this adds Lobby's look for a [`Variant`] at a [`Size`], an icon, loading and a tooltip.
//!
//! ```ignore
//! Button::new("settings-toggle")
//!     .size(Size::Bar)
//!     .icon(IconName::Settings)
//!     .accessibility_label("Settings")
//!     .tooltip("Settings (Ctrl+,)")
//!     .selected(open)
//!     .on_click(|_, window, cx| ..)
//! ```

use std::rc::Rc;

use gpui_kit::{
    AnyElement, App, Background, ClickEvent, ElementId, Hsla, InteractiveElement, Interactivity,
    IntoElement, KeyBinding, MouseButton, NoAction, ParentElement, Refineable as _, RenderOnce,
    SharedString, StatefulInteractiveElement as _, StyleRefinement, Styled, Toggled, Window,
    base::{self, GlobalState, Selectable, StyledExt as _, h_flex},
    div,
    prelude::FluentBuilder as _,
    relative,
};

use super::{Icon, Size, Spinner, focus, tooltip::with_tooltip};
use crate::kit::{ActiveTheme as _, Colorize as _, menu::DropdownMenu};

/// A press is dimmed this much while it runs, on every variant alike.
const LOADING_OPACITY: f32 = 0.8;

/// The key context of a control that Enter and Space press while it has the focus. GPUI runs a
/// key binding before the focused element sees the key, so without this a dialog's Enter =
/// Confirm would beat its focused Cancel.
pub const PRESSABLE: &str = "Pressable";

pub(super) fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("enter", NoAction, Some(PRESSABLE)),
        KeyBinding::new("space", NoAction, Some(PRESSABLE)),
    ]);
}

/// What a button means, which sets its colours.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Variant {
    /// The default commit of its surface, the one Enter runs.
    Primary,
    /// A quiet command among others: toolbars, the title bar, Cancel.
    #[default]
    Ghost,
    /// A filled, edged command that stands on its own, such as a toggle that is off.
    Secondary,
    /// A command that needs an edge to read as one, beside a primary or on its own on a page.
    Outline,
    /// A commit that destroys or leaves something.
    Danger,
    /// A domain colour its component owns, such as a Member row's wash.
    Custom(Custom),
}

/// The colours of a [`Variant::Custom`] button. Selected and pressed share one colour.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Custom {
    bg: Hsla,
    fg: Hsla,
    hover: Hsla,
    active: Hsla,
    disabled: Option<Hsla>,
}

impl Custom {
    /// Flat: `bg` behind `fg` in every state until [`Self::hover`] and [`Self::active`] say
    /// otherwise.
    pub fn new(bg: Hsla, fg: Hsla) -> Self {
        Self {
            bg,
            fg,
            hover: bg,
            active: bg,
            disabled: None,
        }
    }

    pub fn hover(mut self, bg: Hsla) -> Self {
        self.hover = bg;
        self
    }

    /// The fill while pressed and while selected.
    pub fn active(mut self, bg: Hsla) -> Self {
        self.active = bg;
        self
    }

    /// The fill while disabled, in place of a faint wash of the rest fill.
    pub fn disabled(mut self, bg: Hsla) -> Self {
        self.disabled = Some(bg);
        self
    }
}

#[derive(IntoElement)]
pub struct Button {
    id: ElementId,
    base: base::Button,
    variant: Variant,
    size: Size,
    icon: Option<Icon>,
    label: Option<SharedString>,
    accessibility_label: Option<SharedString>,
    tooltip: Option<SharedString>,
    children: Vec<AnyElement>,
    selected: bool,
    toggled: Option<bool>,
    disabled: bool,
    loading: bool,
    on_click: Option<ClickHandler>,
}

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

impl Button {
    pub fn new(id: impl Into<ElementId>) -> Self {
        let id = id.into();
        Self {
            base: base::Button::new(id.clone()),
            id,
            variant: Variant::default(),
            size: Size::default(),
            icon: None,
            label: None,
            accessibility_label: None,
            tooltip: None,
            children: Vec::new(),
            selected: false,
            toggled: None,
            disabled: false,
            loading: false,
            on_click: None,
        }
    }

    pub fn variant(mut self, variant: Variant) -> Self {
        self.variant = variant;
        self
    }

    pub fn size(mut self, size: Size) -> Self {
        self.size = size;
        self
    }

    /// Drawn before the label at the size's icon size. A button with an icon and nothing else is
    /// square.
    pub fn icon(mut self, icon: impl Into<Icon>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    /// The words on the button, which are also its accessible name.
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// The name a screen reader announces in place of the label: an icon-only button needs one.
    pub fn accessibility_label(mut self, label: impl Into<SharedString>) -> Self {
        self.accessibility_label = Some(label.into());
        self
    }

    /// Shown after the pointer rests on the button, and announced as its description.
    pub fn tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.tooltip = Some(text.into());
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Makes the button a toggle that is on or off for a screen reader, such as Mute. Only
    /// [`Self::selected`] changes its look.
    pub fn toggled(mut self, on: bool) -> Self {
        self.toggled = Some(on);
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// While its work runs: the button ignores presses, dims and turns its icon into a spinner.
    pub fn loading(mut self, loading: bool) -> Self {
        self.loading = loading;
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl Selectable for Button {
    fn selected(self, selected: bool) -> Self {
        Button::selected(self, selected)
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl DropdownMenu for Button {}

impl Styled for Button {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
    }
}

impl ParentElement for Button {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl InteractiveElement for Button {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.base.interactivity()
    }
}

/// A button's fill, ink and edge in one state. Only an outlined variant draws the edge; the others
/// colour a border their caller gives them.
#[derive(Clone, Copy)]
struct Paint {
    bg: Background,
    fg: Hsla,
    edge: Option<Hsla>,
}

impl Paint {
    fn new(bg: impl Into<Background>, fg: Hsla) -> Self {
        Self {
            bg: bg.into(),
            fg,
            edge: None,
        }
    }

    fn edge(mut self, color: Hsla) -> Self {
        self.edge = Some(color);
        self
    }

    fn refinement(self) -> StyleRefinement {
        let style = StyleRefinement::default().bg(self.bg).text_color(self.fg);
        match self.edge {
            Some(color) => style.border_color(color),
            None => style,
        }
    }
}

/// Rest, hover, pressed, selected and disabled.
type States = (Paint, Paint, Paint, Paint, Paint);

/// Gives every state the edge `edge`, and the disabled state `faded`.
fn edged(states: States, edge: Hsla, faded: Hsla) -> States {
    let (rest, hover, active, selected, disabled) = states;
    (
        rest.edge(edge),
        hover.edge(edge),
        active.edge(edge),
        selected.edge(edge),
        disabled.edge(faded),
    )
}

/// Everything a variant at a size looks like. `rest` carries the geometry too; the states carry
/// only what they change.
struct Look {
    rest: StyleRefinement,
    hover: StyleRefinement,
    active: StyleRefinement,
    selected: StyleRefinement,
    disabled: StyleRefinement,
}

/// The one table of what a button looks like. It reads the kit's `button_*` theme slots, which
/// the kit's own buttons (in its dialogs and toasts) read too, so the two never drift apart.
fn look(variant: Variant, size: Size, cx: &App) -> Look {
    let theme = cx.theme();
    let tokens = &theme.tokens;
    let clear = theme.transparent;
    let off = theme.muted_foreground.opacity(0.5);
    let filled = |rest: Background, hover, active, fg, edge, fill: Hsla| {
        edged(
            (
                Paint::new(rest, fg),
                Paint::new(hover, fg),
                Paint::new(active, fg),
                Paint::new(active, fg),
                Paint::new(rest.opacity(0.15), off),
            ),
            edge,
            fill.opacity(0.15),
        )
    };
    let (rest, hover, active, selected, disabled) = match variant {
        Variant::Primary => filled(
            tokens.button_primary.into(),
            tokens.button_primary_hover,
            tokens.button_primary_active,
            theme.button_primary_foreground,
            theme.primary,
            theme.button_primary,
        ),
        Variant::Secondary => {
            let ink = theme.button_foreground;
            edged(
                (
                    Paint::new(tokens.button, ink),
                    Paint::new(tokens.button_hover, ink),
                    Paint::new(tokens.button_active, ink),
                    Paint::new(tokens.button_active, ink),
                    Paint::new(theme.input_background().opacity(0.5), off),
                ),
                theme.input,
                theme.input.opacity(0.5),
            )
        }
        Variant::Danger => filled(
            tokens.button_danger.into(),
            tokens.button_danger_hover,
            tokens.button_danger_active,
            theme.button_danger_foreground,
            theme.button_danger,
            theme.button_danger,
        ),
        Variant::Ghost => {
            let ink = theme.secondary_foreground;
            let accent: Background = tokens.accent.into();
            let hover = if theme.mode.is_dark() {
                accent.opacity(0.5)
            } else {
                accent
            };
            (
                Paint::new(clear, ink),
                Paint::new(hover, theme.accent_foreground),
                Paint::new(tokens.button_active, ink),
                Paint::new(tokens.secondary_active, ink),
                Paint::new(clear, off),
            )
        }
        Variant::Outline => {
            let (ink, edge) = (theme.button_foreground, theme.input);
            let pressed = edge.mix_oklab(clear, 0.7);
            edged(
                (
                    Paint::new(theme.input_background(), ink),
                    Paint::new(edge.mix_oklab(clear, 0.5), ink),
                    Paint::new(pressed, ink),
                    Paint::new(pressed, ink),
                    Paint::new(theme.input_background().opacity(0.5), off),
                ),
                edge,
                edge.opacity(0.5),
            )
        }
        Variant::Custom(c) => edged(
            (
                Paint::new(c.bg, c.fg),
                Paint::new(c.hover, c.fg),
                Paint::new(c.active, c.fg),
                Paint::new(c.active, c.fg),
                Paint::new(c.disabled.unwrap_or(c.bg.opacity(0.15)), off),
            ),
            c.bg,
            c.bg.opacity(0.15),
        ),
    };
    let mut geometry = StyleRefinement::default()
        .flex_shrink_0()
        .cursor_default()
        .h(size.height())
        .px(size.pad_x())
        .rounded(size.radius(cx))
        .line_height(relative(1.25));
    if let Some(weight) = size.weight() {
        geometry = geometry.font_weight(weight);
    }
    if matches!(variant, Variant::Secondary | Variant::Outline) {
        geometry = geometry.border_1();
    }
    geometry.refine(&rest.refinement());
    Look {
        rest: geometry,
        hover: hover.refinement(),
        active: active.refinement(),
        selected: selected.refinement(),
        disabled: disabled.refinement(),
    }
}

impl RenderOnce for Button {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Look {
            rest,
            hover,
            active,
            selected,
            disabled,
        } = look(self.variant, self.size, cx);
        let interactive = !(self.disabled || self.loading);
        let square = self.icon.is_some() && self.label.is_none() && self.children.is_empty();
        let mut base = self.base;
        let instance = base.style().clone();
        let focus = window
            .use_keyed_state(self.id.clone(), cx, |_, cx| cx.focus_handle())
            .read(cx)
            .clone();
        let focused = focus.is_focused(window);
        let (loading, on_click) = (self.loading, self.on_click);
        // A menu's trigger has no command of its own: its popover opens on Enter and Space.
        let pressable = on_click.is_some();

        base.style().refine(&rest);
        if square {
            base.style()
                .refine(&StyleRefinement::default().w(self.size.height()).px_0());
        }
        base.style().refine(&instance);
        let icon_size = self.size.icon();
        let icon = self.icon.map(|icon| {
            if loading {
                Spinner::new().size(icon_size).into_any_element()
            } else {
                icon.size(icon_size).into_any_element()
            }
        });
        let content = h_flex()
            .id("label")
            .size_full()
            .min_w_0()
            .overflow_hidden()
            .whitespace_nowrap()
            .justify_center()
            .text_sm()
            .gap(self.size.gap())
            .children(icon)
            .when_some(self.label.clone(), |this, label| {
                this.child(
                    div()
                        .min_w_0()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(label),
                )
            })
            .children(self.children);
        let ring = focused.then(|| focus::ring(base.style(), window, cx));
        let ring_color = cx.theme().ring;
        let name = self.accessibility_label.or(self.label);
        let tooltip = self.tooltip;

        let button = base
            .when(!self.selected && interactive, |this| {
                this.active(move |mut style| {
                    style.refine(&active);
                    style
                })
            })
            // GPUI refreshes the hover state only while a hover style is registered, so a selected
            // or inert button keeps an empty one rather than none.
            .hover(move |mut style| {
                if interactive && !self.selected {
                    style.refine(&hover);
                }
                style
            })
            // Base puts the selected and disabled styles over the caller's, so the caller's are
            // laid on again inside each.
            .styles(|styles| {
                styles
                    .selected(|style| style.refine_style(&selected).refine_style(&instance))
                    .disabled(|style| style.refine_style(&disabled).refine_style(&instance))
            })
            .selected(self.selected)
            .disabled(self.disabled)
            .when_some(self.toggled, |this, on| {
                this.aria_toggled(if on { Toggled::True } else { Toggled::False })
            })
            .track_focus(&focus)
            .when(pressable, |this| this.key_context(PRESSABLE))
            // A focused button's own edge, where it has one, takes the ring's colour too.
            .when(focused, |this| this.border_color(ring_color))
            .when_some(name, |this, name| this.accessibility_label(name))
            .when_some(tooltip.clone(), |this, text| this.aria_description(text))
            .child(content)
            .when(loading && !self.disabled, |this| {
                this.opacity(LOADING_OPACITY)
            })
            .when(!self.disabled, |this| {
                this.on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    if loading {
                        cx.stop_propagation();
                        return;
                    }
                    // Focus and any text selection stay where they are: a press of Bold keeps
                    // the composer's caret and selection.
                    window.prevent_default();
                    GlobalState::suppress_text_selection(cx);
                })
            })
            .map(|this| match (loading, on_click) {
                (true, _) => this.on_click(|_, _, cx| cx.stop_propagation()),
                (false, Some(on_click)) => {
                    this.on_click(move |event, window, cx| on_click(event, window, cx))
                }
                (false, None) => this,
            });
        let button = match tooltip {
            Some(text) => with_tooltip(button, text),
            None => button,
        };
        button.children(ring)
    }
}
