mod cards;

use std::time::Duration;

use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, Hsla, Subscription, Task, Window,
    base::{
        OtpEvent, OtpState,
        input::{InputEvent, InputState},
    },
};
use lobby_client::{Chip, EntryIntent, EntryStep, EntryView, Fill, LobbyOrigin, Stage, copy};
use lobby_protocol::{LobbyInfo, MagicCode};
use tracing::info;

use crate::components::{
    Feedback,
    code_input::{CodeTyped, CodeTyping},
    mark_color,
};
use crate::entry_entity::{EntryChanged, EntryEntity};

pub const ADMITTED_PAUSE: Duration = Duration::from_millis(900);
pub const WRONG_CODE_SHOWN: Duration = Duration::from_millis(600);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthStepName {
    Address,
    Email,
    Code,
    Name,
    AwaitingVouch,
    Closed,
    SignedIn,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthViewEvent {
    SignedIn,
    ChangeLobby,
}

pub struct AuthView {
    entry: Entity<EntryEntity>,
    first_info: LobbyInfo,
    pub email: Entity<InputState>,
    pub name: Entity<InputState>,
    pub code: Entity<OtpState>,
    pub typing: Entity<CodeTyping>,
    /// Why the person is here again, until they first act.
    notice: Option<Feedback>,
    /// The epoch whose step the fields were last set up for.
    epoch: u64,
    /// The wrong codes counted so far; one more turns the cells red.
    wrong_seen: u64,
    pub code_wrong: Option<u64>,
    /// A Vouch came: Waiting for a Vouch shows its check mark for [`ADMITTED_PAUSE`].
    admitted: bool,
    timers: Vec<Task<()>>,
    /// The Lobby this Sign In was reached from (a switch or Add Lobby inside it): the email
    /// step's Back goes there, and says so.
    pub back_to: Option<String>,
    /// The Lobby's mark colour, as the title bar draws it.
    pub mark: Hsla,
    last_step: EntryStep,
    /// The last rail Entry showed: Signed in has none, and the check mark keeps Waiting's.
    last_rail: Vec<Chip>,
    /// The ones that belong to the window the view is in ([`Self::bind_window`]).
    window_subscriptions: Vec<Subscription>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<AuthViewEvent> for AuthView {}

impl AuthView {
    /// `notice` is shown on the first step: why the person is here again (signed out, session
    /// ended).
    pub fn new(
        entry: Entity<EntryEntity>,
        origin: LobbyOrigin,
        info: LobbyInfo,
        notice: Option<Feedback>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let email = cx.new(|cx| InputState::new(window, cx).placeholder(copy::EMAIL_PLACEHOLDER));
        let name = cx.new(|cx| InputState::new(window, cx).placeholder(copy::NAME_PLACEHOLDER));
        let code = cx.new(|cx| OtpState::new(MagicCode::LEN, window, cx));
        let typing = cx.new(|_| CodeTyping::new(&code));
        let typed = |field: &Entity<InputState>,
                     window: &mut Window,
                     cx: &mut Context<Self>,
                     enter: fn(&mut Self, &mut Window, &mut Context<Self>)| {
            cx.subscribe_in(
                field,
                window,
                move |this, _, ev: &InputEvent, window, cx| match ev {
                    InputEvent::PressEnter { shift: false, .. } => enter(this, window, cx),
                    InputEvent::Change => this.act(EntryIntent::Edited, cx),
                    _ => {}
                },
            )
        };
        let subscriptions = vec![
            typed(&email, window, cx, Self::send_code),
            typed(&name, window, cx, Self::pick_name),
            cx.subscribe_in(
                &code,
                window,
                |this, _, ev: &OtpEvent, window, cx| match ev {
                    OtpEvent::Complete => this.submit_code(window, cx),
                    OtpEvent::Change => this.code_changed(cx),
                    _ => {}
                },
            ),
            cx.subscribe_in(
                &typing,
                window,
                |this, _, ev: &CodeTyped, window, cx| match ev {
                    CodeTyped::Complete => this.submit_code(window, cx),
                    CodeTyped::Change => this.code_changed(cx),
                },
            ),
            cx.subscribe_in(&entry, window, |this, _, _: &EntryChanged, window, cx| {
                this.follow(window, cx);
            }),
        ];
        let last_step = entry.read(cx).view().step.clone();
        let mark = mark_color(origin.as_str());
        let mut view = Self {
            entry,
            first_info: info,
            email,
            name,
            code,
            typing,
            notice: None,
            epoch: u64::MAX,
            wrong_seen: 0,
            code_wrong: None,
            admitted: false,
            timers: Vec::new(),
            back_to: None,
            mark,
            last_step,
            last_rail: Vec::new(),
            window_subscriptions: Vec::new(),
            _subscriptions: subscriptions,
        };
        view.bind_window(window, cx);
        view.follow(window, cx);
        view.notice = notice;
        view
    }

    /// What this view watches on its window, which GPUI keeps on that one window: bound again
    /// when the Shell replaces the window.
    pub fn bind_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.window_subscriptions =
            vec![cx.observe_window_activation(window, |this, window, cx| {
                this.window_activated(window, cx);
            })];
    }

    /// Coming back to the window is a reason to ask now whether the Member was let in.
    pub fn window_activated(&mut self, window: &Window, cx: &mut Context<Self>) {
        if window.is_window_active() {
            self.act(EntryIntent::WindowActive, cx);
        }
    }

    pub fn view<'a>(&self, cx: &'a gpui_kit::App) -> &'a EntryView {
        self.entry.read(cx).view()
    }

    pub fn info<'a>(&'a self, cx: &'a gpui_kit::App) -> &'a LobbyInfo {
        self.view(cx).info.as_ref().unwrap_or(&self.first_info)
    }

    pub fn busy(&self, cx: &gpui_kit::App) -> bool {
        self.admitted || self.entry.read(cx).busy()
    }

    /// The screen's feedback line: what Entry says, else the notice it was opened with.
    pub fn feedback(&self, cx: &gpui_kit::App) -> Option<Feedback> {
        self.entry.read(cx).said().or_else(|| self.notice.clone())
    }

    pub fn rail(&self) -> &[Chip] {
        &self.last_rail
    }

    pub fn step_name(&self, cx: &gpui_kit::App) -> AuthStepName {
        if self.admitted {
            return AuthStepName::AwaitingVouch;
        }
        match self.view(cx).step {
            EntryStep::Address => AuthStepName::Address,
            EntryStep::Email => AuthStepName::Email,
            EntryStep::Code { .. } => AuthStepName::Code,
            EntryStep::Name { .. } => AuthStepName::Name,
            EntryStep::AwaitingVouch { .. } => AuthStepName::AwaitingVouch,
            EntryStep::Closed { .. } => AuthStepName::Closed,
            EntryStep::SignedIn => AuthStepName::SignedIn,
        }
    }

    fn act(&mut self, intent: EntryIntent, cx: &mut Context<Self>) {
        match intent {
            // Typing only clears what was said about the text before it.
            EntryIntent::Edited if self.entry.read(cx).said().is_none() => return,
            EntryIntent::Edited | EntryIntent::WindowActive => {}
            _ => self.notice = None,
        }
        self.entry.update(cx, |e, cx| e.act(intent, cx));
    }

    fn field_of(&self, step: &EntryStep) -> Option<&Entity<InputState>> {
        match step {
            EntryStep::Email => Some(&self.email),
            EntryStep::Name { .. } => Some(&self.name),
            _ => None,
        }
    }

    /// Puts the keyboard back on the step's field, after the person was elsewhere.
    pub fn refocus(&self, window: &mut Window, cx: &mut Context<Self>) {
        let step = &self.view(cx).step;
        if matches!(step, EntryStep::Code { .. }) {
            self.code.update(cx, |c, cx| c.focus(window, cx));
        } else if let Some(field) = self.field_of(step) {
            field.update(cx, |f, cx| f.focus(window, cx));
        }
    }

    /// Entry published a newer view: a new step sets its fields up once, a wrong code turns the
    /// cells red, and Signed in or Back to the address is the Shell's to take on.
    fn follow(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let view = self.entry.read(cx).view().clone();
        if !view.rail.is_empty() {
            self.last_rail = view.rail.clone();
        }
        if view.wrong > self.wrong_seen {
            self.wrong_seen = view.wrong;
            self.wrong_code(cx);
        }
        if view.epoch != self.epoch {
            self.epoch = view.epoch;
            self.enter_step(&view, window, cx);
        }
        self.last_step = view.step;
        cx.notify();
    }

    fn enter_step(&mut self, view: &EntryView, window: &mut Window, cx: &mut Context<Self>) {
        self.code_wrong = None;
        info!(step = ?self.step_name(cx), "auth step");
        if let Some(field) = self.field_of(&view.step).cloned() {
            field.update(cx, |f, cx| f.focus(window, cx));
        }
        match &view.step {
            EntryStep::Code { .. } => {
                // A stale code would submit itself again.
                self.code.update(cx, |c, cx| {
                    c.set_value("", window, cx);
                    c.focus(window, cx);
                });
            }
            EntryStep::SignedIn => self.signed_in(cx),
            EntryStep::Address => cx.emit(AuthViewEvent::ChangeLobby),
            _ => {}
        }
        match view.fill.clone() {
            Some(Fill::Email(email)) => {
                self.email.update(cx, |f, cx| {
                    f.set_value(email.as_str().to_owned(), window, cx)
                });
            }
            Some(Fill::Username(suggestion)) if self.name.read(cx).value().trim().is_empty() => {
                self.name
                    .update(cx, |u, cx| u.set_value(suggestion, window, cx));
            }
            Some(Fill::Username(_)) | None => {}
        }
    }

    /// From Waiting for a Vouch the check mark shows first.
    fn signed_in(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.last_step, EntryStep::AwaitingVouch { .. }) {
            return cx.emit(AuthViewEvent::SignedIn);
        }
        self.admitted = true;
        self.timers.push(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(ADMITTED_PAUSE).await;
            let _ = this.update(cx, |_, cx| cx.emit(AuthViewEvent::SignedIn));
        }));
    }

    fn wrong_code(&mut self, cx: &mut Context<Self>) {
        let attempt = self.wrong_seen;
        self.code_wrong = Some(attempt);
        self.timers.retain(|t| !t.is_ready());
        self.timers.push(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(WRONG_CODE_SHOWN).await;
            crate::in_window::update(&this, cx, |v, window, cx| {
                if v.code_wrong == Some(attempt) {
                    v.code_wrong = None;
                    v.code.update(cx, |c, cx| {
                        c.set_value("", window, cx);
                        c.focus(window, cx);
                    });
                    cx.notify();
                }
            });
        }));
    }

    pub fn back(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.act(EntryIntent::Back, cx);
    }

    pub fn go_to(&mut self, stage: Stage, _: &mut Window, cx: &mut Context<Self>) {
        self.act(EntryIntent::GoTo(stage), cx);
    }

    pub fn send_code(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let email = self.email.read(cx).value().to_string();
        self.act(EntryIntent::SendCode(email), cx);
    }

    fn resend_code(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.act(EntryIntent::Resend, cx);
    }

    fn code_changed(&mut self, cx: &mut Context<Self>) {
        self.act(EntryIntent::Edited, cx);
        cx.notify();
    }

    fn paste_code(&mut self, code: String, window: &mut Window, cx: &mut Context<Self>) {
        self.code.update(cx, |c, cx| c.set_value(code, window, cx));
        self.submit_code(window, cx);
    }

    pub fn submit_code(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let code = self.code.read(cx).value().to_string();
        if MagicCode::new(code.clone()).is_ok() {
            self.code_wrong = None;
            self.act(EntryIntent::SubmitCode(code), cx);
        }
    }

    pub fn pick_name(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let name = self.name.read(cx).value().to_string();
        self.act(EntryIntent::PickName(name), cx);
    }

    pub fn check_join(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.act(EntryIntent::CheckNow, cx);
    }
}
