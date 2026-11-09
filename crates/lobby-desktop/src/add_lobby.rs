//! "Add Lobby…" from inside a Lobby: a small dialog with the shared address form, so the person
//! stays in their Lobby (connected, In Voice) until the new one has answered. Its Entry makes the
//! same checks as the URL step's; the Shell takes the Entry on from there.
//!
//! The primary button keeps its width through "Connecting…" and the field is disabled meanwhile,
//! so the attempt and the field cannot disagree.

use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, Styled as _, Subscription, TestSupportExt as _, Window,
    component::{
        ActiveTheme as _,
        input::{InputEvent, InputState},
    },
    div,
};
use lobby_client::{Client, EntryIntent, EntryStep, LobbyOrigin, copy};
use lobby_protocol::LobbyInfo;

use crate::components::{ADD_LOBBY_IDS, AddressForm, Feedback, quiet_link};
use crate::entry_entity::{EntryChanged, EntryEntity};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddLobbyEvent {
    /// The Lobby answered: its Entry goes on to Sign In, or is signed in already.
    Reached {
        entry: Entity<EntryEntity>,
        origin: LobbyOrigin,
        info: Box<LobbyInfo>,
        signed_in: bool,
    },
    Cancel,
}

pub struct AddLobbyView {
    entry: Entity<EntryEntity>,
    current: Option<(LobbyOrigin, String)>,
    pub url: Entity<InputState>,
    reached: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<AddLobbyEvent> for AddLobbyView {}

impl AddLobbyView {
    pub fn new(
        client: Client,
        current: Option<(LobbyOrigin, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let url = cx.new(|cx| InputState::new(window, cx).placeholder(copy::ADDRESS_PLACEHOLDER));
        let entry = cx.new(|cx| EntryEntity::new(client.entry(current.clone()), cx));
        let subscriptions = vec![
            cx.subscribe_in(
                &url,
                window,
                |this, _, ev: &InputEvent, window, cx| match ev {
                    InputEvent::PressEnter { shift: false, .. } => this.submit(window, cx),
                    InputEvent::Change if this.said(cx).is_some() => {
                        this.entry
                            .update(cx, |e, cx| e.act(EntryIntent::Edited, cx));
                    }
                    _ => {}
                },
            ),
            cx.subscribe(&entry, |this, _, _: &EntryChanged, cx| this.follow(cx)),
        ];
        Self {
            entry,
            current,
            url,
            reached: false,
            _subscriptions: subscriptions,
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.url.update(cx, |u, cx| u.focus(window, cx));
    }

    /// Puts `address` in the field and the focus there. It never connects: pressing Connect is
    /// the person's yes.
    pub fn set_address(&self, address: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.url
            .update(cx, |u, cx| u.set_value(address.to_owned(), window, cx));
        self.focus(window, cx);
    }

    pub fn busy(&self, cx: &gpui_kit::App) -> bool {
        self.entry.read(cx).busy()
    }

    fn said(&self, cx: &gpui_kit::App) -> Option<Feedback> {
        self.entry.read(cx).said()
    }

    pub fn error(&self, cx: &gpui_kit::App) -> Option<String> {
        match self.said(cx) {
            Some(Feedback::Error(e)) => Some(e.to_string()),
            _ => None,
        }
    }

    pub fn submit(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if self.busy(cx) {
            return;
        }
        let address = self.url.read(cx).value().to_string();
        self.entry
            .update(cx, |e, cx| e.act(EntryIntent::Connect(address), cx));
    }

    fn follow(&mut self, cx: &mut Context<Self>) {
        let view = self.entry.read(cx).view();
        if view.step != EntryStep::Address
            && !self.reached
            && let (Some(origin), Some(info)) = (&view.origin, &view.info)
        {
            self.reached = true;
            let signed_in = view.step == EntryStep::SignedIn;
            cx.emit(AddLobbyEvent::Reached {
                entry: self.entry.clone(),
                origin: origin.clone(),
                info: Box::new(info.clone()),
                signed_in,
            });
        }
        cx.notify();
    }
}

impl Render for AddLobbyView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let hint = self
            .current
            .as_ref()
            .map(|(_, name)| Feedback::Hint(copy::add_lobby_hint(name).into()));
        let form = AddressForm::new(
            &self.url,
            ADD_LOBBY_IDS,
            cx.listener(|this, _, window, cx| this.submit(window, cx)),
        )
        .busy(self.busy(cx))
        .line(self.said(cx).or(hint));
        div()
            .id("add-lobby")
            .debug_selector(|| "add-lobby".into())
            .w_full()
            .flex()
            .flex_col()
            .child(form)
            .child(
                div().flex().child(
                    quiet_link(
                        "add-lobby-cancel",
                        "Cancel",
                        cx.listener(|_, _, _, cx| cx.emit(AddLobbyEvent::Cancel)),
                    )
                    .debug_selector(|| "add-lobby-cancel".into()),
                ),
            )
            .text_color(cx.theme().foreground)
            .test_support()
    }
}
