//! What lives behind the signed-in Member's own Member Card, so it outlasts the card being open:
//! the name field (a rename in place, whose typed draft survives other clicks in the card) and
//! the OS file picker for the picture, which keeps running after the card closes.
//!
//! The name is Saved only when it is a username the Member does not already have, and not while a
//! Save is in flight. A taken name is said where the email was; any other refusal is the Session's
//! Issue. The picture is picked, squared by the Lobby and sent; the Lobby tells everyone, this
//! Member included.

use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, Subscription, Task, Window,
    component::input::{InputEvent, InputState},
};
use lobby_client::{AccountOp, Intent, UiEffect, copy};
use lobby_protocol::Username;

use crate::{
    components::{Rename, Said},
    session_entity::{SessionEntity, SessionUiEvent},
};

/// The name was Saved, so the field is gone and the keyboard needs a new place in the card.
pub struct Renamed;

impl EventEmitter<Renamed> for OwnCard {}

pub struct OwnCard {
    session: Entity<SessionEntity>,
    name: Entity<InputState>,
    editing: bool,
    /// The Lobby refused the last Save, about the text that was saved: it goes with the next edit.
    taken: bool,
    /// The OS file picker, open.
    picking: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl OwnCard {
    pub fn new(
        session: Entity<SessionEntity>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let current = current_name(&session, cx);
        let name = cx.new(|cx| InputState::new(window, cx).default_value(current));
        let subscriptions = vec![
            cx.subscribe_in(
                &name,
                window,
                |this, _, ev: &InputEvent, window, cx| match ev {
                    InputEvent::PressEnter { shift: false, .. } => this.save(window, cx),
                    InputEvent::Change => {
                        this.taken = false;
                        cx.notify();
                    }
                    _ => {}
                },
            ),
            cx.observe(&session, |_, _, cx| cx.notify()),
            cx.subscribe_in(
                &session,
                window,
                |this, _, ev: &SessionUiEvent, window, cx| {
                    for effect in &ev.0.0 {
                        match effect {
                            UiEffect::Renamed(name) => {
                                let name = name.to_string();
                                this.name.update(cx, |n, cx| n.set_value(name, window, cx));
                                if std::mem::take(&mut this.editing) {
                                    cx.emit(Renamed);
                                }
                            }
                            UiEffect::NameTaken => this.taken = true,
                            _ => {}
                        }
                    }
                    cx.notify();
                },
            ),
        ];
        Self {
            session,
            name,
            editing: false,
            taken: false,
            picking: None,
            _subscriptions: subscriptions,
        }
    }

    pub fn field(&self) -> &Entity<InputState> {
        &self.name
    }

    /// The card opens with the name as it is and nothing being edited.
    pub fn reopen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editing = false;
        self.taken = false;
        let current = current_name(&self.session, cx);
        self.name
            .update(cx, |n, cx| n.set_value(current, window, cx));
        cx.notify();
    }

    pub fn is_editing(&self) -> bool {
        self.editing
    }

    pub fn typed(&self, cx: &gpui_kit::App) -> String {
        self.name.read(cx).value().to_string()
    }

    pub fn start_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editing = true;
        self.taken = false;
        self.name.update(cx, |n, cx| n.focus(window, cx));
        cx.notify();
    }

    /// Esc: the field goes back to the name the Member has.
    pub fn cancel_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reopen(window, cx);
    }

    /// The name Save would send: a username that is not the one the Member has.
    fn change(&self, cx: &gpui_kit::App) -> Result<Option<Username>, &'static str> {
        let text = self.typed(cx);
        let typed = Username::parse(&text).map_err(|_| copy::name_rule(&text))?;
        Ok((typed.as_str() != current_name(&self.session, cx)).then_some(typed))
    }

    fn running(&self, op: AccountOp, cx: &gpui_kit::App) -> bool {
        self.session
            .read(cx)
            .state()
            .is_some_and(|s| s.account.running(op))
    }

    fn saving(&self, cx: &gpui_kit::App) -> bool {
        self.running(AccountOp::Rename, cx)
    }

    /// Enter and Save. The Lobby's answer comes back as the name it wrote, a taken name, or the
    /// Session's Issue. Enter on the name the Member has closes the field.
    pub fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editing || self.saving(cx) {
            return;
        }
        match self.change(cx) {
            Ok(Some(username)) => {
                self.taken = false;
                self.session
                    .update(cx, |s, cx| s.dispatch(Intent::Rename(username), cx));
                cx.notify();
            }
            Ok(None) => self.cancel_rename(window, cx),
            Err(_) => {}
        }
    }

    pub fn rename(&self, cx: &gpui_kit::App) -> Rename {
        if !self.editing {
            return Rename::Idle;
        }
        let saving = self.saving(cx);
        let change = self.change(cx);
        let said = if self.taken {
            Some(Said {
                text: copy::NAME_TAKEN.into(),
                refusal: true,
            })
        } else {
            match (self.typed(cx).trim().is_empty(), &change) {
                (false, Err(rule)) => Some(Said {
                    text: (*rule).into(),
                    refusal: false,
                }),
                _ => None,
            }
        };
        Rename::Editing {
            can_save: !saving && matches!(change, Ok(Some(_))),
            saving,
            said,
        }
    }

    pub fn has_picture(&self, cx: &gpui_kit::App) -> bool {
        let Some(state) = self.session.read(cx).state() else {
            return false;
        };
        state
            .me
            .as_ref()
            .and_then(|me| state.members.get(me))
            .is_some_and(|m| m.avatar.is_some())
    }

    /// A picture is being picked, sent or removed.
    pub fn picture_busy(&self, cx: &gpui_kit::App) -> bool {
        self.picking.is_some() || self.running(AccountOp::Avatar, cx)
    }

    /// Picks a picture and sends it; the Lobby tells everyone, this Member included, so the
    /// avatar changes when the Lobby's word comes back. A refusal is the Session's Issue.
    pub fn change_picture(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.picture_busy(cx) {
            return;
        }
        let picked = crate::files::pick_one(cx);
        self.picking = Some(cx.spawn_in(window, async move |this, cx| {
            let path = picked.await;
            let _ = this.update(cx, |v, cx| {
                v.picking = None;
                if let Some(path) = path {
                    v.session
                        .update(cx, |s, cx| s.dispatch(Intent::SetAvatar(path), cx));
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub fn remove_picture(&mut self, cx: &mut Context<Self>) {
        if self.picture_busy(cx) || !self.has_picture(cx) {
            return;
        }
        self.session
            .update(cx, |s, cx| s.dispatch(Intent::RemoveAvatar, cx));
        cx.notify();
    }

    pub fn email(&self, cx: &gpui_kit::App) -> Option<String> {
        let email = self.session.read(cx).state()?.account.email.as_ref()?;
        Some(email.as_str().to_owned())
    }
}

fn current_name(session: &Entity<SessionEntity>, cx: &gpui_kit::App) -> String {
    session
        .read(cx)
        .state()
        .and_then(|s| s.my_username())
        .map(ToString::to_string)
        .unwrap_or_default()
}
