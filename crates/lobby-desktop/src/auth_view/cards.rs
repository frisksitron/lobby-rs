//! The screen each step draws on the one `AuthScreen` layout, and `Render`.

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, TestSupportExt as _, Window,
    assets::IconName,
    component::{ActiveTheme as _, Disableable as _, Icon, button::Button},
    div,
    prelude::FluentBuilder as _,
};
use lobby_client::{Closed, EntryStep, NamePath, Resend, copy};

use super::AuthView;
use crate::components::{
    AuthScreen, CodeInput, Feedback, STEP_MARK, StepRail, back_button, busy_line, control_row,
    feedback_line, inline_row, primary_button, quiet_link, status_mark, step_button, step_input,
    tag, voice_accent, waiting_mark,
};

/// What every step's screen shares, read once per render.
struct Step {
    name: String,
    busy: bool,
    /// The Lobby asked for a pause: the step's button waits.
    held: bool,
    feedback: Option<Feedback>,
}

impl Step {
    /// The step's primary button, waiting out a pause unless it is the one in flight.
    fn primary(
        &self,
        id: &'static str,
        label: &str,
        busy_label: &str,
        cx: &gpui_kit::App,
    ) -> Button {
        primary_button(id, label, busy_label, self.busy, cx).disabled(self.held && !self.busy)
    }

    fn line(&self, default: Option<Feedback>, cx: &gpui_kit::App) -> impl IntoElement {
        feedback_line(self.feedback.clone().or(default), cx)
    }
}

impl AuthView {
    fn step(&self, cx: &Context<Self>) -> Step {
        Step {
            name: self.info(cx).name.clone(),
            busy: self.busy(cx),
            held: self.view(cx).held,
            feedback: self.feedback(cx),
        }
    }

    fn screen(
        &self,
        title: impl Into<SharedString>,
        subtitle: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) -> AuthScreen {
        let view = cx.weak_entity();
        let rail = StepRail::new(self.rail().to_vec())
            .lobby_mark(self.info(cx).name.clone(), self.mark)
            .on_go(move |stage, window, cx| {
                let _ = view.update(cx, |v, cx| v.go_to(stage, window, cx));
            });
        AuthScreen::new(rail, title, subtitle).entered(self.epoch.to_string())
    }

    fn email_screen(&self, s: &Step, cx: &mut Context<Self>) -> AuthScreen {
        let info = self.info(cx);
        let subtitle = if !info.set_up {
            format!("{} {}", copy::EMAIL_HINT, copy::NOT_SET_UP_HINT)
        } else if !info.join_requests_enabled {
            format!("{} {}", copy::EMAIL_HINT, copy::EMAIL_LOCKED_HINT)
        } else {
            copy::EMAIL_HINT.to_owned()
        };
        // The Lobby chip on the rail already changes the Lobby: this goes back to another one.
        let back = self.back_to.as_deref().map(|lobby| {
            tag(
                "auth-back",
                back_button("auth-back", copy::back_to(lobby), cx)
                    .on_click(cx.listener(|this, _, window, cx| this.back(window, cx))),
            )
        });
        let send = s
            .primary("send-code", copy::SEND_CODE, copy::SENDING_CODE, cx)
            .on_click(cx.listener(|this, _, window, cx| this.send_code(window, cx)));
        let screen = self
            .screen(copy::welcome_to(&s.name), subtitle, cx)
            .child(control_row(
                tag("email", step_input(&self.email, "email")),
                tag("send-code", send),
            ))
            .child(s.line(None, cx));
        match back {
            Some(back) => screen.link(back),
            None => screen,
        }
    }

    fn code_screen(&self, s: &Step, cx: &mut Context<Self>) -> AuthScreen {
        let busy = s.busy;
        let view = cx.weak_entity();
        let on_paste = std::rc::Rc::new(
            move |code: String, window: &mut Window, cx: &mut gpui_kit::App| {
                let _ = view.update(cx, |v, cx| v.paste_code(code, window, cx));
            },
        );
        let resend = self.view(cx).resend;
        let label = self.view(cx).code_label.clone();
        let default_line = match resend {
            Resend::Capped => Some(Feedback::Info(
                copy::codes_capped(self.info(cx).magic_code_sends, label.as_ref()).into(),
            )),
            _ => None,
        };
        let resend_link = if resend == Resend::Wait {
            // Words, not a dimmed button: a disabled control's text is too faint to be the reason.
            let muted = cx.theme().muted_foreground;
            div()
                .id("resend-code")
                .flex()
                .items_center()
                .gap_2()
                .text_sm()
                .text_color(muted)
                .child(Icon::new(IconName::RotateCw).text_color(muted))
                .child(copy::SEND_NEW_CODE_WAIT)
                .test_support()
                .into_any_element()
        } else {
            quiet_link(
                "resend-code",
                copy::SEND_NEW_CODE,
                cx.listener(|this, _, window, cx| this.resend_code(window, cx)),
            )
            .icon(Icon::new(IconName::RotateCw))
            .disabled(busy || resend != Resend::Ready)
            .into_any_element()
        };
        let line = if busy && s.feedback.is_none() {
            busy_line(copy::CHECKING, cx).into_any_element()
        } else {
            s.line(default_line, cx).into_any_element()
        };
        self.screen(copy::CHECK_YOUR_EMAIL, copy::code_hint(label.as_ref()), cx)
            .child(
                CodeInput::new(&self.code, &self.typing, on_paste)
                    .disabled(busy)
                    .wrong(self.code_wrong),
            )
            .child(line)
            .link(tag("resend-code", resend_link))
    }

    fn name_screen(&self, s: &Step, path: NamePath, cx: &mut Context<Self>) -> AuthScreen {
        let (title, subtitle, label, busy_label) = match path {
            NamePath::CreateLobby => (
                copy::set_up_title(&s.name),
                copy::CREATE_LOBBY_HINT,
                copy::CREATE_LOBBY,
                copy::SETTING_UP,
            ),
            NamePath::AskToJoin => (
                copy::ask_to_join_title(&s.name),
                copy::ASK_TO_JOIN_HINT,
                copy::ASK_TO_JOIN,
                copy::ASKING,
            ),
        };
        let join = s
            .primary("join", label, busy_label, cx)
            .on_click(cx.listener(|this, _, window, cx| this.pick_name(window, cx)));
        self.screen(title, subtitle, cx)
            .child(control_row(
                tag("name", step_input(&self.name, "name")),
                tag("join", join),
            ))
            .child(s.line(None, cx))
    }

    fn waiting_screen(&self, s: &Step, cx: &mut Context<Self>) -> AuthScreen {
        let (busy, admitted) = (s.busy, self.admitted);
        let lead = if admitted {
            status_mark(IconName::CircleCheck, cx.theme().success, STEP_MARK).into_any_element()
        } else {
            waiting_mark(IconName::Hourglass, voice_accent(cx.theme()), STEP_MARK)
                .into_any_element()
        };
        let title = if admitted {
            copy::VOUCHED_IN
        } else {
            copy::WAITING_TITLE
        };
        let check = step_button("check-again", cx)
            .outline()
            .icon(Icon::new(if busy {
                IconName::LoaderCircle
            } else {
                IconName::RefreshCw
            }))
            .label(if admitted {
                copy::OPENING_LOBBY
            } else if busy {
                copy::CHECKING
            } else {
                copy::CHECK_NOW
            })
            .loading(busy)
            .disabled(s.held && !busy)
            .on_click(cx.listener(|this, _, window, cx| this.check_join(window, cx)));
        let other = back_button("other-email", copy::USE_DIFFERENT_EMAIL, cx)
            .disabled(admitted)
            .on_click(cx.listener(|this, _, window, cx| this.back(window, cx)));
        self.screen(title, copy::AWAITING_VOUCH, cx)
            .lead(lead)
            .child(inline_row(
                tag("check-again", check),
                tag("other-email", other),
            ))
            .child(s.line(Some(Feedback::Hint(copy::AWAITING_VOUCH_LATER.into())), cx))
    }

    fn closed_screen(&self, s: &Step, why: Closed, cx: &mut Context<Self>) -> AuthScreen {
        let (icon, title, body) = match why {
            Closed::NotOpenYet => (
                IconName::Clock,
                copy::not_open_yet_title(&s.name),
                copy::NOT_OPEN_YET_LINE,
            ),
            Closed::Locked => (
                IconName::Lock,
                copy::closed_for_now_title(&s.name),
                copy::LOCKED_LINE,
            ),
            Closed::Declined => (
                IconName::UserX,
                copy::DECLINED_TITLE.to_owned(),
                copy::DECLINED,
            ),
        };
        let back = back_button("closed-back", copy::USE_DIFFERENT_EMAIL, cx)
            .on_click(cx.listener(|this, _, window, cx| this.back(window, cx)));
        self.screen(title, body, cx)
            .lead(status_mark(icon, cx.theme().info, STEP_MARK))
            .child(tag("closed-back", back))
            .when(s.feedback.is_some(), |screen| {
                screen.child(s.line(None, cx))
            })
    }
}

impl Render for AuthView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let s = self.step(cx);
        if self.admitted {
            return self.waiting_screen(&s, cx);
        }
        match self.view(cx).step.clone() {
            // Signed in and Back to the address are the Shell's next route.
            EntryStep::Email | EntryStep::Address | EntryStep::SignedIn => {
                self.email_screen(&s, cx)
            }
            EntryStep::Code { .. } => self.code_screen(&s, cx),
            EntryStep::Name { path, .. } => self.name_screen(&s, path, cx),
            EntryStep::AwaitingVouch { .. } => self.waiting_screen(&s, cx),
            EntryStep::Closed { why, .. } => self.closed_screen(&s, why, cx),
        }
    }
}
