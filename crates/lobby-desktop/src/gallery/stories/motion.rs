//! The Motion story: each moment of Lobby's motion on the real component, with a button that sets
//! it off, so it can be felt. Reduced motion lands each one at once.

use std::{rc::Rc, sync::Arc};

use gpui_kit::{
    AnyElement, AppContext as _, ClickEvent, Context, Entity, FocusHandle, IntoElement as _,
    ParentElement as _, SharedString, Styled as _, Window, base::OtpState, div,
    prelude::FluentBuilder as _, px,
};
use lobby_client::{History, IssueKind, LobbyState, Older, Phase};
use lobby_protocol::{AuthorRef, MagicCode, Message, MessageId, Presence, UnixMs};

use crate::components::{
    AvatarSize, AvatarState, CodeInput, CodeTyping, Feed, FeedAction, MemberAvatar,
    NotificationBell, Slot, TypingLine,
};
use crate::gallery::{
    Gallery, Section,
    fixtures::{speaking, user_id, voice},
};
use crate::kit::{ActiveTheme as _, dialog};
use crate::lobby_view::{FOLDED_SHEET_W, unfolded_sheet};
use crate::ui::{self, motion::Reveal};

use super::{
    caption,
    feed::cards,
    floating_sheet::{FOLDED_H, sheet, small_view},
    notifications::notification,
    wrap,
};

const SAYINGS: [(&str, &str); 4] = [
    ("alice", "Anyone up for a round tonight?"),
    ("carol", "Count me in."),
    ("alice", "Bringing snacks either way."),
    ("dave", "Five minutes, finishing dinner."),
];

/// What the Motion story has set off, kept between frames.
pub struct MotionDemo {
    switch: bool,
    center: bool,
    typing: bool,
    speaking: bool,
    sheet: bool,
    sheet_focus: FocusHandle,
    wrong: Option<u64>,
    code: (Entity<OtpState>, Entity<CodeTyping>),
    feed: Feed,
    state: Arc<LobbyState>,
}

impl MotionDemo {
    pub fn new(window: &mut Window, cx: &mut Context<Gallery>) -> Self {
        let otp = cx.new(|cx| OtpState::new(MagicCode::LEN, window, cx));
        let typing = cx.new(|_| CodeTyping::new(&otp));
        typing.update(cx, |t, cx| t.type_text("80421579", window, cx));
        let mut state = LobbyState {
            phase: Phase::Connected,
            me: Some(user_id("bob")),
            history: History::Loaded,
            older: Older::Beginning,
            ..LobbyState::default()
        };
        for _ in 0..2 {
            say(&mut state);
        }
        let state = Arc::new(state);
        let mut feed = Feed::new(None, cx);
        feed.sync(&state, lobby_client::today(), cx);
        Self {
            switch: false,
            center: false,
            typing: false,
            speaking: false,
            sheet: false,
            sheet_focus: cx.focus_handle(),
            wrong: None,
            code: (otp, typing),
            feed,
            state,
        }
    }

    /// Someone says the next line, while the feed follows its end.
    pub fn message_comes_in(&mut self, cx: &mut Context<Gallery>) {
        say(Arc::make_mut(&mut self.state));
        self.feed.sync(&self.state, lobby_client::today(), cx);
        cx.notify();
    }
}

fn say(state: &mut LobbyState) {
    let n = state.chat.len();
    let (author, text) = SAYINGS[n % SAYINGS.len()];
    let id = MessageId::new(n as i64 + 1).expect("ids start at 1");
    state.chat.insert(
        id,
        Message {
            id,
            author: AuthorRef {
                id: user_id(author),
                username: author.to_owned(),
                avatar: None,
                deactivated: false,
            },
            content: text.to_owned(),
            attachments: Vec::new(),
            nonce: format!("n{n}"),
            created_at: UnixMs::now(),
            edited_at: None,
            rev: None,
        },
    );
}

/// A button that changes the demo and draws again.
fn trigger(
    id: &'static str,
    label: impl Into<SharedString>,
    cx: &mut Context<Gallery>,
    act: impl Fn(&mut MotionDemo, &mut Window, &mut Context<Gallery>) + 'static,
) -> ui::Button {
    ui::Button::new(id)
        .variant(ui::Variant::Outline)
        .size(ui::Size::Page)
        .label(label)
        .on_click(cx.listener(move |g, _: &ClickEvent, window, cx| {
            act(&mut g.held.motion, window, cx);
            cx.notify();
        }))
}

fn row(children: impl IntoIterator<Item = AnyElement>) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap_4()
        .children(children)
        .into_any_element()
}

pub(super) fn motion(
    g: &mut Gallery,
    window: &mut Window,
    cx: &mut Context<Gallery>,
) -> Vec<Section> {
    let this = cx.weak_entity();
    let m = &g.held.motion;
    let (switch, center, typing, talking, wrong) =
        (m.switch, m.center, m.typing, m.speaking, m.wrong);

    let button = ui::Button::new("motion-press")
        .variant(ui::Variant::Primary)
        .size(ui::Size::Page)
        .label("Join Voice")
        .on_click(|_, _, _| {});

    let changed = this.clone();
    let bell = NotificationBell::new(
        vec![
            notification(
                IssueKind::VoiceEnded,
                "Voice ended",
                "The connection dropped.",
                true,
            ),
            notification(
                IssueKind::SendFailed,
                "Message not sent",
                "It is back.",
                true,
            ),
        ],
        0,
        Rc::new(|_, _, _| {}),
        center,
        Rc::new(move |open, _, cx| {
            let _ = changed.update(cx, |g, cx| {
                g.held.motion.center = open;
                cx.notify();
            });
        }),
        Rc::new(|_, _, _| {}),
    );

    let dialog = trigger("motion-dialog", "Open a dialog", cx, |_, window, cx| {
        dialog::open(window, cx, |d, _, _| {
            d.title("Leave Lobby?")
                .child("You can come back with a Magic Code.")
        });
    });

    let toggled = this.clone();
    let switch = ui::Switch::new("motion-switch")
        .checked(switch)
        .accessibility_label("Do Not Disturb")
        .on_change(move |next, _, cx| {
            let _ = toggled.update(cx, |g, cx| {
                g.held.motion.switch = next;
                cx.notify();
            });
        });

    let on: FeedAction = Rc::new(|_, _| {});
    let feed = g.held.motion.feed.render(
        Older::Beginning,
        on.clone(),
        on,
        |_, _| {},
        cards(Slot::Loading, 400.),
        window,
        cx,
    );
    let border = cx.theme().border;
    let feed = div()
        .relative()
        .w(px(480.))
        .h(px(220.))
        .overflow_hidden()
        .rounded_md()
        .border_1()
        .border_color(border)
        .child(feed);

    let line = typing.then(|| SharedString::from("bob is typing…"));
    let typing_box = div()
        .relative()
        .w(px(480.))
        .h(px(crate::components::feed::FOOT_ROOM))
        .rounded_md()
        .border_1()
        .border_color(border)
        .child(TypingLine::new(line));

    let voice = if talking { speaking() } else { voice() };
    let avatar = MemberAvatar::new(
        "motion-speaker",
        "carol",
        AvatarState {
            presence: Some(Presence::Online),
            known: true,
            voice: Some(voice),
        },
    )
    .size(AvatarSize::Hero);

    let open = g.held.motion.sheet;
    let reveal = Reveal::sheet("motion-sheet", open, window, cx);
    let folded = this.clone();
    let view = small_view(cx).when(reveal.is_drawn(), |view| {
        view.child(div().absolute().inset_0().child(unfolded_sheet(
            reveal,
            sheet(FOLDED_SHEET_W, FOLDED_H, true, None, cx).into_any_element(),
            &g.held.motion.sheet_focus,
            cx,
            move |_, cx| {
                let _ = folded.update(cx, |g, cx| {
                    g.held.motion.sheet = false;
                    cx.notify();
                });
            },
        )))
    });

    let (otp, code_typing) = &g.held.motion.code;
    let code = CodeInput::new(otp, code_typing, Rc::new(|_, _, _| {})).wrong(wrong);

    vec![
        Section::new(
            "press",
            "1. Press a button: it goes in at once and gives way 1.5 px; it lets go on Quick",
            wrap([caption("press and hold", button, cx)]),
        ),
        Section::new(
            "popup",
            "2. Open a popup: the Notification Center rises 4 px and fades in on Open, and fades out on Quick",
            div().h(px(56.)).child(bell),
        ),
        Section::new(
            "dialog",
            "3. Open a dialog: it fades in as it settles 4 px into place",
            row([dialog.into_any_element()]),
        ),
        Section::new(
            "switch",
            "4. Toggle a switch: the thumb moves on Move and the track's colour follows it",
            wrap([caption("Do Not Disturb", switch, cx)]),
        ),
        Section::new(
            "message",
            "5. A Message comes in: its row rises 4 px and fades in on Move, while the feed follows its end",
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(feed)
                .child(row([trigger(
                    "motion-message",
                    "A Message comes in",
                    cx,
                    |m, _, cx| m.message_comes_in(cx),
                )
                .into_any_element()])),
        ),
        Section::new(
            "typing",
            "6. Someone starts and stops typing: the line fades in and out on Quick",
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(typing_box)
                .child(row([trigger(
                    "motion-typing",
                    if typing {
                        "Stop typing"
                    } else {
                        "Start typing"
                    },
                    cx,
                    |m, _, _| m.typing = !m.typing,
                )
                .into_any_element()])),
        ),
        Section::new(
            "speaking",
            "7. Someone starts and stops speaking: the ring fades in and out on Quick",
            row([
                avatar.into_any_element(),
                trigger(
                    "motion-speaking",
                    if talking {
                        "Stop speaking"
                    } else {
                        "Start speaking"
                    },
                    cx,
                    |m, _, _| m.speaking = !m.speaking,
                )
                .into_any_element(),
            ]),
        ),
        Section::new(
            "sheet",
            "8. Open the members sheet in the small window: it slides 4 px in from the edge and fades on Open, and goes the same way on Quick",
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(view)
                .child(row([trigger(
                    "motion-sheet-toggle",
                    if open {
                        "Fold the sheet"
                    } else {
                        "Open the sheet"
                    },
                    cx,
                    |m, _, _| m.sheet = !m.sheet,
                )
                .into_any_element()])),
        ),
        Section::new(
            "shake",
            "9. A wrong Magic Code: one short shake, 4 px out and a small swing back, in 250 ms",
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(code)
                .child(row([trigger(
                    "motion-shake",
                    "Enter a wrong code",
                    cx,
                    |m, _, _| m.wrong = Some(m.wrong.map_or(1, |n| n + 1)),
                )
                .into_any_element()])),
        ),
    ]
}
