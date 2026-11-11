//! The Lobby address form that Connect and Add Lobby share: the address field beside Connect, and
//! the reserved line under them. Its Entry decides every answer; the form draws it.

use std::rc::Rc;

use gpui_kit::{
    App, ClickEvent, Entity, IntoElement, ParentElement as _, RenderOnce, Styled as _, Window, div,
};
use lobby_client::copy;

use crate::kit::input::InputState;

use super::{
    Feedback,
    auth::{ClickHandler, control_row, feedback_line, primary_button, tag},
    controls::step_input,
};

/// The element ids of one use of the form, so a test can tell Connect's from Add Lobby's.
#[derive(Debug, Clone, Copy)]
pub struct AddressIds {
    pub field: &'static str,
    pub connect: &'static str,
}

pub const CONNECT_IDS: AddressIds = AddressIds {
    field: "lobby-url",
    connect: "connect",
};

pub const ADD_LOBBY_IDS: AddressIds = AddressIds {
    field: "add-lobby-url",
    connect: "add-lobby-connect",
};

#[derive(IntoElement)]
pub struct AddressForm {
    field: Entity<InputState>,
    ids: AddressIds,
    busy: bool,
    line: Option<Feedback>,
    on_connect: ClickHandler,
}

impl AddressForm {
    pub fn new(
        field: &Entity<InputState>,
        ids: AddressIds,
        on_connect: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            field: field.clone(),
            ids,
            busy: false,
            line: None,
            on_connect: Rc::new(on_connect),
        }
    }

    pub fn busy(mut self, busy: bool) -> Self {
        self.busy = busy;
        self
    }

    /// What the reserved line says: Entry's words, else the caller's own default.
    pub fn line(mut self, line: Option<Feedback>) -> Self {
        self.line = line;
        self
    }
}

impl RenderOnce for AddressForm {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let ids = self.ids;
        let on_connect = self.on_connect;
        let connect = primary_button(ids.connect, copy::CONNECT, copy::CONNECTING, self.busy, cx)
            .on_click(move |ev, window, cx| on_connect(ev, window, cx));
        let row = control_row(
            tag(
                ids.field,
                step_input(&self.field, ids.field).disabled(self.busy),
            ),
            tag(ids.connect, connect),
        );
        div()
            .w_full()
            .flex()
            .flex_col()
            .child(row)
            .child(feedback_line(self.line, cx))
    }
}
