//! The Motion story: each moment of Lobby's motion on the real component, with a button that sets
//! it off, so it can be felt. Reduced motion lands each one at once.

use std::{rc::Rc, sync::Arc};

use gpui_kit::{
    AnyElement, AppContext as _, ClickEvent, Context, Entity, FocusHandle, IntoElement as _,
    ParentElement as _, SharedString, Styled as _, Window,
    assets::IconName,
    base::{OtpState, slider::SliderState},
    div,
    prelude::FluentBuilder as _,
    px,
};
use lobby_client::{History, IssueKind, LobbyState, Older, Phase};
use lobby_protocol::{AuthorRef, MagicCode, Message, MessageId, Presence, UnixMs};

use crate::components::{
    AvatarSize, AvatarState, CodeInput, CodeTyping, Feed, FeedAction, MemberAvatar,
    NotificationBell, SURFACE_INNER_RADIUS, Slot, TypingLine,
};
use crate::gallery::{
    Gallery, Section,
    fixtures::{speaking, user_id, voice},
};
use crate::kit::{ActiveTheme as _, dialog};
use crate::lobby_view::{FOLDED_SHEET_W, unfolded_sheet};
use crate::scale::{device_in, u};
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
    volume: Entity<SliderState>,
    mic: Mic,
}

/// The press row's Mute and Deafen, which change as the Session changes them: Deafen mutes and
/// Undeafen gives back the mute from before it; Unmute undeafens too.
#[derive(Clone, Copy)]
struct Mic {
    muted: bool,
    deafened: bool,
    muted_before_deafen: bool,
}

impl Mic {
    fn toggle_mute(&mut self) {
        self.muted = !self.muted;
        self.deafened = false;
    }

    fn toggle_deafen(&mut self) {
        if self.deafened {
            self.muted = self.muted_before_deafen;
        } else {
            self.muted_before_deafen = self.muted;
            self.muted = true;
        }
        self.deafened = !self.deafened;
    }
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
            volume: cx.new(|_| {
                SliderState::new()
                    .min(0.)
                    .max(200.)
                    .step(1.)
                    .default_value(100.)
            }),
            mic: Mic {
                muted: true,
                deafened: false,
                muted_before_deafen: false,
            },
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

/// The controls Lobby presses most: its keys in each variant, the voice foot with Unmute latched
/// until it is pressed, a flat title bar button and a slider.
fn presses(
    volume: &Entity<SliderState>,
    mic: Mic,
    window: &mut Window,
    cx: &mut Context<Gallery>,
) -> AnyElement {
    let deafen_press = ui::PressLink::new("motion-deafen-press", window, cx);
    let toggle_mute = cx.listener(|g, _: &ClickEvent, _, cx| {
        g.held.motion.mic.toggle_mute();
        cx.notify();
    });
    let toggle_deafen = cx.listener(|g, _: &ClickEvent, _, cx| {
        g.held.motion.mic.toggle_deafen();
        cx.notify();
    });
    let theme = cx.theme();
    let rest = ui::Custom::new(theme.button_secondary, theme.button_secondary_foreground)
        .hover(theme.button_secondary_hover)
        .active(theme.button_secondary_active);
    let off = ui::Custom::new(theme.danger, theme.danger_foreground).hover(theme.danger_hover);
    let radius = device_in(SURFACE_INNER_RADIUS, cx);

    let button = |id: &'static str, variant: ui::Variant, label: &'static str| {
        ui::Button::new(id)
            .variant(variant)
            .size(ui::Size::Page)
            .label(label)
            .on_click(|_, _, _| {})
            .into_any_element()
    };
    let foot = |id: &'static str, name: &'static str, icon: IconName, selected: bool| {
        ui::Button::new(id)
            .variant(ui::Variant::Custom(if selected { off } else { rest }))
            .icon(icon)
            .accessibility_label(name)
            .w(u(40.))
            .h(u(36.))
            .p_0()
            .selected(selected)
    };
    let (mute, mute_icon) = if mic.muted {
        ("Unmute", IconName::MicOff)
    } else {
        ("Mute", IconName::Mic)
    };
    let (deafen, deafen_icon) = if mic.deafened {
        ("Undeafen", IconName::HeadphoneOff)
    } else {
        ("Deafen", IconName::Headphones)
    };
    let foot = div()
        .flex()
        .items_center()
        .gap(px(1.))
        .child(
            foot("motion-mute", mute, mute_icon, mic.muted)
                .when(!mic.muted, |b| b.pressed_with(&deafen_press))
                .on_click(toggle_mute)
                .rounded_l(radius)
                .rounded_r_none(),
        )
        .child(
            foot("motion-deafen", deafen, deafen_icon, mic.deafened)
                .presses(&deafen_press)
                .on_click(toggle_deafen)
                .rounded_none(),
        )
        .child(
            foot("motion-share", "Share Screen", IconName::ScreenShare, false)
                .on_click(|_, _, _| {})
                .rounded_r(radius)
                .rounded_l_none(),
        );
    let gear = ui::Button::new("motion-gear")
        .size(ui::Size::Bar)
        .icon(IconName::Settings)
        .accessibility_label("Settings")
        .on_click(|_, _, _| {});
    let slider = div()
        .w(px(140.))
        .child(ui::Slider::new(volume).accessibility_label("Volume"));

    row([
        button("motion-press", ui::Variant::Primary, "Join Voice"),
        button("motion-danger", ui::Variant::Danger, "Leave Lobby"),
        button("motion-outline", ui::Variant::Outline, "Cancel"),
        button("motion-secondary", ui::Variant::Secondary, "Turn on"),
        foot.into_any_element(),
        gear.into_any_element(),
        slider.into_any_element(),
    ])
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

    let presses = presses(&m.volume, m.mic, window, cx);

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
            "1. Press a key: its face drops onto its lip at once and comes back up on Quick. Latched (Unmute) or inert, it lies flush. Deafening mutes, so Deafen takes Mute down with it while Mute is up. A flat button, such as the gear, answers with colour alone",
            presses,
        ),
        Section::new(
            "popup",
            "2. Open a popup: the Notification Center rises 4 px and fades in, at full speed from the first frame, and fades out the same way",
            div().h(px(56.)).child(bell),
        ),
        Section::new(
            "dialog",
            "3. Open a dialog: it fades in as it settles 4 px into place",
            row([dialog.into_any_element()]),
        ),
        Section::new(
            "switch",
            "4. Toggle a switch: the thumb moves at full speed from the first frame and the track's colour follows it",
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
            "8. Open the members sheet in the small window: it slides 4 px in from the edge and fades, at full speed from the first frame, and goes back the same way",
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
