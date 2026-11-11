//! Stories of the onboarding screens: every step on its `AuthScreen`, in frames the size of the
//! default window's and the small window's area under the title bar, and the parts they share.

use std::rc::Rc;

use gpui_kit::{
    AnyElement, App, Context, IntoElement, ParentElement as _, Styled as _, Window,
    assets::IconName, div, px,
};

use lobby_client::{Chip, ChipAt, Stage, copy};

use crate::components::{
    AddressForm, AuthScreen, CONNECT_IDS, CodeInput, Feedback, STEP_MARK, SURFACE_INNER_RADIUS,
    StepRail, back_button, busy_screen, control_row, feedback_line, inline_row, mark_color,
    primary_button, quiet_link, status_mark, step_input, voice_accent, waiting_mark,
};
use crate::gallery::{Gallery, Section};
use crate::kit::ActiveTheme as _;
use crate::scale::device_in;
use crate::ui;

use super::label;

/// The default window's and the small window's area under the 40 px title bar.
const DEFAULT: (f32, f32) = (680., 440.);
const SMALL: (f32, f32) = (480., 360.);

const LONG_NAME: &str = "The Friday Night Board Game and Pizza Club";

fn frame(screen: AuthScreen, (w, h): (f32, f32), cx: &App) -> AnyElement {
    div()
        .w(px(w))
        .h(px(h))
        .flex()
        .flex_col()
        .overflow_hidden()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().border)
        .child(screen)
        .into_any_element()
}

fn done(stage: Stage, value: Option<&str>) -> Chip {
    Chip {
        stage,
        at: ChipAt::Done {
            value: value.map(str::to_owned),
            back: matches!(stage, Stage::Lobby | Stage::Email),
        },
    }
}

fn at(stage: Stage, at: ChipAt) -> Chip {
    Chip { stage, at }
}

fn rail(chips: Vec<Chip>) -> StepRail {
    StepRail::new(chips)
        .lobby_mark("Test Lobby", mark_color("https://lobby.example.com"))
        .on_go(|_, _, _| {})
}

pub(super) fn auth(
    g: &mut Gallery,
    _window: &mut Window,
    cx: &mut Context<Gallery>,
) -> Vec<Section> {
    let f = g.held.auth_fields.clone();
    let host = "lobby.example.com";
    let email = "carol@lobby.test";
    let long_email = "carol.marie.van.der.berg-lindqvist@friends-of-the-lobby.example";
    let address_rail = || {
        vec![
            at(Stage::Lobby, ChipAt::Current),
            at(Stage::Email, ChipAt::Ahead),
            at(Stage::Code, ChipAt::Ahead),
        ]
    };
    let connect = |field: usize, line: Option<Feedback>| {
        AuthScreen::new(
            rail(address_rail()),
            copy::CONNECT_TITLE,
            copy::CONNECT_SUBTITLE,
        )
        .child(AddressForm::new(&f[field], CONNECT_IDS, |_, _, _| {}).line(line))
    };
    let email_screen = |field: usize, line: Option<Feedback>, subtitle: String, cx: &App| {
        AuthScreen::new(
            rail(vec![
                done(Stage::Lobby, Some(host)),
                at(Stage::Email, ChipAt::Current),
                at(Stage::Code, ChipAt::Ahead),
            ]),
            copy::welcome_to("Test Lobby"),
            subtitle,
        )
        .child(control_row(
            step_input(&f[field], "email"),
            primary_button("send-code", copy::SEND_CODE, copy::SENDING_CODE, false, cx),
        ))
        .child(feedback_line(line, cx))
    };
    let codes = &g.held.codes;
    let code = |i: usize| CodeInput::new(&codes[i].0, &codes[i].1, Rc::new(|_, _, _| {}));
    let request_label =
        lobby_protocol::RequestLabel::try_from("KQV".to_owned()).expect("a request label");
    let code_screen = AuthScreen::new(
        rail(vec![
            done(Stage::Lobby, Some(host)),
            done(Stage::Email, Some(long_email)),
            at(Stage::Code, ChipAt::Current),
        ]),
        copy::CHECK_YOUR_EMAIL,
        copy::code_hint(Some(&request_label)),
    )
    .child(code(1).wrong(Some(1)))
    .child(feedback_line(
        Some(Feedback::Error(copy::CODE_WRONG.into())),
        cx,
    ))
    .link(quiet_link("resend-code", copy::SEND_NEW_CODE, |_, _, _| {}).icon(IconName::RotateCw));
    let name_rail = |name: Option<&str>| {
        let mut chips = vec![
            done(Stage::Lobby, Some(host)),
            done(Stage::Email, Some(email)),
            done(Stage::Code, None),
        ];
        chips.push(match name {
            Some(name) => done(Stage::Name, Some(name)),
            None => at(Stage::Name, ChipAt::Current),
        });
        chips
    };
    let create = AuthScreen::new(
        rail(name_rail(None)),
        copy::set_up_title(LONG_NAME),
        copy::CREATE_LOBBY_HINT,
    )
    .child(control_row(
        step_input(&f[4], "name"),
        primary_button("join", copy::CREATE_LOBBY, copy::SETTING_UP, false, cx),
    ))
    .child(feedback_line(None, cx));
    let ask = AuthScreen::new(
        rail(name_rail(None)),
        copy::ask_to_join_title("Test Lobby"),
        copy::ASK_TO_JOIN_HINT,
    )
    .child(control_row(
        step_input(&f[5], "name"),
        primary_button("join", copy::ASK_TO_JOIN, copy::ASKING, false, cx),
    ))
    .child(feedback_line(
        Some(Feedback::Error(copy::USERNAME_TAKEN.into())),
        cx,
    ));
    let theme = cx.theme();
    let (accent, info, border) = (voice_accent(theme), theme.info, theme.border);
    let waiting = AuthScreen::new(
        rail(name_rail(Some("carol"))),
        copy::WAITING_TITLE,
        copy::AWAITING_VOUCH,
    )
    .lead(waiting_mark(IconName::Hourglass, accent, STEP_MARK))
    .child(inline_row(
        ui::Button::new("check-again")
            .variant(ui::Variant::Outline)
            .size(ui::Size::Step)
            .rounded(device_in(SURFACE_INNER_RADIUS, cx))
            .icon(IconName::RefreshCw)
            .label(copy::CHECK_NOW),
        back_button("other-email", copy::USE_DIFFERENT_EMAIL, cx),
    ))
    .child(feedback_line(
        Some(Feedback::Hint(copy::AWAITING_VOUCH_LATER.into())),
        cx,
    ));
    let closed = AuthScreen::new(
        rail(name_rail(Some("carol"))),
        copy::DECLINED_TITLE,
        copy::DECLINED,
    )
    .lead(status_mark(IconName::UserX, info, STEP_MARK))
    .child(back_button("closed-back", copy::USE_DIFFERENT_EMAIL, cx));
    let lines = [
        ("Error", Some(Feedback::Error(copy::CODE_WRONG.into()))),
        ("Notice", Some(Feedback::Notice(copy::CODE_RESENT.into()))),
        ("Info", Some(Feedback::Info(copy::SIGNED_OUT.into()))),
        (
            "Hint",
            Some(Feedback::Hint(copy::AWAITING_VOUCH_LATER.into())),
        ),
        ("Nothing (the space stays)", None),
    ];
    let not_set_up = format!("{} {}", copy::EMAIL_HINT, copy::NOT_SET_UP_HINT);
    vec![
        Section::new(
            "connect",
            "Connect, at the default window",
            frame(connect(0, None), DEFAULT, cx),
        ),
        Section::new(
            "connect-unreachable",
            "Connect: a local address that did not answer, in the small window",
            frame(
                connect(
                    1,
                    Some(Feedback::Error(
                        copy::couldnt_reach("localhost:8080").into(),
                    )),
                ),
                SMALL,
                cx,
            ),
        ),
        Section::new(
            "email",
            "Email, at the default window",
            frame(
                email_screen(2, None, copy::EMAIL_HINT.to_owned(), cx),
                DEFAULT,
                cx,
            ),
        ),
        Section::new(
            "email-error",
            "Email in a Lobby nobody has set up, with an error, in the small window",
            frame(
                email_screen(
                    3,
                    Some(Feedback::Error(copy::ENTER_EMAIL.into())),
                    not_set_up,
                    cx,
                ),
                SMALL,
                cx,
            ),
        ),
        Section::new(
            "code",
            "Code: a long email wraps on the rail, and a wrong code",
            frame(code_screen, SMALL, cx),
        ),
        Section::new(
            "create-lobby",
            "Name, setting up a Lobby with a long name",
            frame(create, SMALL, cx),
        ),
        Section::new(
            "ask-to-join",
            "Name, Asking to Join, with a refused name",
            frame(ask, SMALL, cx),
        ),
        Section::new("waiting", "Waiting for a Vouch", frame(waiting, SMALL, cx)),
        Section::new("closed", "Closed: declined", frame(closed, SMALL, cx)),
        Section::new(
            "code-input",
            "The code input: empty, a wrong code, waiting",
            div()
                .flex()
                .flex_col()
                .gap_3()
                .child(code(0))
                .child(code(1).wrong(Some(1)))
                .child(code(2).disabled(true)),
        ),
        Section::new(
            "feedback",
            "The reserved feedback line",
            div()
                .w(px(380.))
                .flex()
                .flex_col()
                .gap_2()
                .children(lines.map(|(name, line)| {
                    div().flex().flex_col().child(label(name, cx)).child(
                        div()
                            .border_b_1()
                            .border_color(border)
                            .child(feedback_line(line, cx)),
                    )
                })),
        ),
        Section::new(
            "busy",
            "The busy screen (quitting, leaving a Lobby)",
            div()
                .w(px(420.))
                .h(px(160.))
                .flex()
                .flex_col()
                .rounded_md()
                .border_1()
                .border_color(border)
                .child(busy_screen("gallery-busy", "Signing out…", cx)),
        ),
    ]
}
