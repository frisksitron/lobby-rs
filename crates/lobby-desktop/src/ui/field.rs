//! Lobby's text field, one line or several. The base state (`InputState`, `TextareaState`, and
//! `InputEvent` and the editing actions with them, imported by their `base::input` path) owns the
//! text, the caret, the keys, the selection and the accessibility node; this draws the frame,
//! gives the editor its colours and inset, and adds the right-click menu and the paste hook.
//!
//! ```ignore
//! let email = cx.new(|cx| InputState::new(window, cx).placeholder("you@example.com"));
//! Field::new(&email).id("email").size(Size::Step)
//! ```

use std::rc::Rc;

use gpui_kit::{
    App, ClipboardItem, Edges, ElementId, Entity, EntityId, FocusHandle, InteractiveElement as _,
    IntoElement, MouseButton, ParentElement as _, Pixels, Point, Refineable as _, RenderOnce, Role,
    StyleRefinement, Styled, Window,
    base::{
        RoleOverride,
        input::{
            self, InputAccessibility, InputBase, InputBaseState, InputContextMenuCapabilities,
            InputEditorStyle, InputModeKind, InputState, TextareaState,
        },
        v_flex,
    },
    div,
    prelude::FluentBuilder as _,
};

use super::{Size, focus};
use crate::{
    kit::{ActiveTheme as _, Colorize as _, native_menu::NativeMenu},
    scale::{device, u},
};

const LINE_H: f32 = 20.;

type PasteHandler = Rc<dyn Fn(&ClipboardItem, &mut Window, &mut App) -> bool>;

enum Editor {
    Line(Entity<InputState>),
    Lines(Entity<TextareaState>),
}

#[derive(IntoElement)]
pub struct Field {
    id: Option<ElementId>,
    editor: Editor,
    style: StyleRefinement,
    size: Size,
    disabled: bool,
    appearance: bool,
    focus_ring: bool,
    on_paste: Option<PasteHandler>,
}

impl Field {
    pub fn new(state: &Entity<InputState>) -> Self {
        Self::with_editor(Editor::Line(state.clone()))
    }

    /// A field of several lines, which grows as the state's `auto_grow` allows.
    pub fn multi_line(state: &Entity<TextareaState>) -> Self {
        Self::with_editor(Editor::Lines(state.clone()))
    }

    fn with_editor(editor: Editor) -> Self {
        Self {
            id: None,
            editor,
            style: StyleRefinement::default(),
            size: Size::default(),
            disabled: false,
            appearance: true,
            focus_ring: true,
            on_paste: None,
        }
    }

    /// The frame's identity, which the scenarios find it by. By default it is the state's.
    pub fn id(mut self, id: impl Into<ElementId>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// A one-line field takes the surface's height; any field takes its inset and type.
    pub fn size(mut self, size: Size) -> Self {
        self.size = size;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Without its appearance the field draws no fill, edge or ring: a frame around it does.
    pub fn appearance(mut self, appearance: bool) -> Self {
        self.appearance = appearance;
        self
    }

    /// Whether keyboard focus draws the ring and the ring-coloured edge.
    pub fn focus_ring(mut self, enabled: bool) -> Self {
        self.focus_ring = enabled;
        self
    }

    /// Sees what a paste would insert before the field does. Returning `true` takes the paste,
    /// and the field inserts nothing. A field that cannot be edited never asks.
    pub fn on_paste(
        mut self,
        handler: impl Fn(&ClipboardItem, &mut Window, &mut App) -> bool + 'static,
    ) -> Self {
        self.on_paste = Some(Rc::new(handler));
        self
    }
}

impl Styled for Field {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

fn metrics(size: Size) -> (f32, f32, f32) {
    match size {
        Size::Step => (12., 10., 16.),
        Size::Inline | Size::Bar | Size::Corner => (4., 0., 12.),
        Size::Row | Size::Word | Size::Page => (10., 8., 14.),
    }
}

struct Editing {
    entity: EntityId,
    focus: FocusHandle,
    focused: bool,
    editable: bool,
}

/// Base lays a multi-line editor's text out against its own inset, so the frame hands its padding
/// over on every render.
fn sync<M: InputModeKind>(
    state: &Entity<InputBaseState<M>>,
    paddings: Edges<Pixels>,
    disabled: bool,
    window: &Window,
    cx: &mut App,
) -> Editing {
    let theme = cx.theme();
    let style = InputEditorStyle {
        foreground: theme.foreground,
        muted_foreground: theme.muted_foreground,
        selection: theme.selection,
        caret: theme.caret,
        ..InputEditorStyle::default()
    };
    state.update(cx, |state, cx| {
        let placeholder =
            Some(state.presentation().placeholder().clone()).filter(|p| !p.is_empty());
        let role = if state.is_multi_line() {
            Role::MultilineTextInput
        } else {
            Role::TextInput
        };
        state.set_editor_style(style);
        state.set_editor_paddings(paddings);
        state.set_disabled(disabled, cx);
        state.on_context_menu(Rc::new(edit_menu));
        state.set_accessibility(
            InputAccessibility::new(Some(role))
                .with_label(placeholder.clone())
                .with_placeholder(placeholder),
        );
    });
    let entity = state.entity_id();
    let state = state.read(cx);
    Editing {
        entity,
        focus: state.presentation().focus_handle().clone(),
        focused: !disabled && state.has_selection_focus(window, cx),
        editable: state.is_editable(),
    }
}

fn edit_menu(
    _: input::NativeMenu,
    capabilities: InputContextMenuCapabilities,
    position: Point<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let editable = capabilities.is_editable();
    let copyable = capabilities.is_copyable();
    NativeMenu::new()
        .menu_with_disabled("Cut", !(editable && copyable), Box::new(input::Cut))
        .menu_with_disabled("Copy", !copyable, Box::new(input::Copy))
        .menu_with_disabled("Paste", !editable, Box::new(input::Paste))
        .separator()
        .menu("Select All", Box::new(input::SelectAll))
        .show(position, window, cx);
}

impl RenderOnce for Field {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (pad_x, pad_y, text) = metrics(self.size);
        let multi_line = matches!(self.editor, Editor::Lines(_));
        let (editing, child) = match &self.editor {
            Editor::Line(state) => (
                sync(state, Edges::default(), self.disabled, window, cx),
                input::Input::new(state).into_any_element(),
            ),
            Editor::Lines(state) => {
                let (x, y) = (device(pad_x, window), device(pad_y, window));
                let paddings = Edges {
                    top: y,
                    right: x,
                    bottom: y,
                    left: x,
                };
                let child = v_flex()
                    .size_full()
                    .child(div().relative().flex_1().child(input::Textarea::new(state)));
                (
                    sync(state, paddings, self.disabled, window, cx),
                    child.into_any_element(),
                )
            }
        };
        let Editing {
            entity,
            focus,
            focused,
            editable,
        } = editing;
        let theme = cx.theme();
        let ring_color = theme.ring;
        let bg = if self.disabled {
            theme.input.mix_oklab(theme.transparent, 0.8).opacity(0.5)
        } else {
            theme.input_background()
        };
        let (edge, radius) = (theme.input, theme.radius);
        let ringed = self.appearance && self.focus_ring;
        let disabled = self.disabled;
        let paste = self.on_paste.filter(|_| editable);

        let mut frame = InputBase::new(self.id.unwrap_or_else(|| ("input", entity).into()))
            .semantic_owner(entity)
            .role(RoleOverride::Presentational)
            .focused(focused)
            .disabled(disabled)
            .relative()
            .flex()
            .items_center()
            .w_full()
            .line_height(u(LINE_H))
            .text_size(u(text))
            .map(|frame| {
                if multi_line {
                    frame.h_auto()
                } else {
                    frame.h(self.size.height()).px(u(pad_x)).py(u(pad_y))
                }
            })
            .when(self.appearance, |frame| {
                frame.bg(bg).rounded(radius).border_1().border_color(edge)
            })
            .when(ringed, |frame| {
                frame.styles(|styles| styles.focused(|style| style.border_color(ring_color)))
            });
        frame.style().refine(&self.style);
        let ring = (focused && ringed).then(|| focus::ring(frame.style(), window, cx));
        frame
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                if !window.default_prevented() {
                    window.focus(&focus, cx);
                }
            })
            .when(disabled, |frame| {
                frame.capture_any_mouse_down(|_, _, cx| cx.stop_propagation())
            })
            .when_some(paste, |frame, handler| {
                frame.capture_action(move |_: &input::Paste, window, cx| {
                    if let Some(clipboard) = cx.read_from_clipboard()
                        && handler(&clipboard, window, cx)
                    {
                        cx.stop_propagation();
                    }
                })
            })
            .child(child)
            .children(ring)
    }
}
