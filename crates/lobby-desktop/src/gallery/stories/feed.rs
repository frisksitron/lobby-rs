use std::{rc::Rc, time::Duration};

use gpui_kit::{
    AnyElement, App, Context, IntoElement, ParentElement as _, Styled as _, Window, div, px,
};
use lobby_client::{
    History, Older, Phase,
    compose::Format,
    message_text::{MentionNames, MessageText},
};
use lobby_protocol::Username;

use crate::components::{
    Cards, FeedRow, FormatBar, MessageRow, Pending, Slot, TypingLine, feed_row, mark_color,
};
use crate::gallery::{
    Gallery, Section,
    fixtures::{attachment, notice},
};
use crate::kit::ActiveTheme as _;

use super::caption;

pub(super) fn message(key: &str, author: &str, first: bool, text: &str) -> MessageRow {
    let names = MentionNames::of(&[Username::parse("bob").expect("a username")]);
    let text = MessageText::of_in(text, &names);
    MessageRow {
        key: key.to_owned().into(),
        id: None,
        author: author.to_owned().into(),
        avatar: None,
        mine: false,
        first,
        time: "21:40".into(),
        full_time: "Saturday 24 October 2026, 21:40".into(),
        markdown: text.markdown.into(),
        text: text.plain.into(),
        mentions_me: false,
        pending: None,
        edited: false,
        deactivated: false,
        attachments: Vec::new(),
    }
}

pub(super) fn cards(slot: Slot, max_w: f32) -> Cards {
    Cards {
        images: Rc::new(move |_, _| slot.clone()),
        avatars: Rc::new(|_, _| None),
        max_w,
        on_open: Rc::new(|_, _, _, _| {}),
        on_change: Rc::new(|_, _, _, _| {}),
    }
}

pub(super) fn feed(g: &mut Gallery, _: &mut Window, cx: &mut Context<Gallery>) -> Vec<Section> {
    let photo = attachment(1, "holiday.jpg", "image/jpeg", 1_258_291, Some((480, 320)));
    let notes = attachment(2, "notes.pdf", "application/pdf", 48_000, None);
    let rows = vec![
        FeedRow::History,
        FeedRow::Day {
            key: "d-today".into(),
            label: "Today".into(),
        },
        FeedRow::Message(message(
            "m1",
            "alice",
            true,
            "Anyone up for a round tonight?",
        )),
        FeedRow::Message(MessageRow {
            edited: true,
            ..message("m2", "alice", false, "Bringing snacks either way.")
        }),
        FeedRow::Message(MessageRow {
            mentions_me: true,
            ..message("m2b", "alice", false, "@bob can you bring a headset?")
        }),
        FeedRow::Message(message(
            "m3",
            "Bob Marley",
            true,
            "**Yes!** I set up the server: `cargo run -p lobby-server`\n- bring a headset\n- see <https://example.com>",
        )),
        FeedRow::Message(MessageRow {
            deactivated: true,
            ..message("m4", "frank", true, "I left the Lobby; my Messages stay.")
        }),
        FeedRow::New,
        FeedRow::Message(MessageRow {
            attachments: vec![photo.clone(), notes.clone()],
            edited: true,
            ..message("m5", "carol", true, "Photos from last time")
        }),
        FeedRow::Message(MessageRow {
            mine: true,
            pending: Some(Pending::Sending),
            ..message("p1", "bob", true, "On its way…")
        }),
        FeedRow::Message(MessageRow {
            mine: true,
            pending: Some(Pending::Waiting),
            ..message(
                "p2",
                "bob",
                false,
                "Written while offline: it goes when the Lobby is back.",
            )
        }),
    ];
    let shown = cards(Slot::Ready(g.held.picture.clone()), 480.);
    let column = |rows: &[FeedRow], older: Older, cards: &Cards, cx: &mut App| {
        div()
            .w(px(640.))
            .max_w_full()
            .py_2()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .flex()
            .flex_col()
            .children((0..rows.len()).map(|ix| {
                feed_row(
                    rows,
                    ix,
                    older,
                    &(Rc::new(|_: &mut Window, _: &mut App| {}) as crate::components::FeedAction),
                    |_, _| {},
                    cards,
                    cx,
                )
            }))
            .into_any_element()
    };
    let history = |older: Older, cx: &mut App| {
        caption(
            format!("{older:?}"),
            column(&[FeedRow::History], older, &shown, cx),
            cx,
        )
    };
    let typing = div()
        .relative()
        .w(px(640.))
        .max_w_full()
        .h(px(crate::components::feed::FOOT_ROOM))
        .rounded_md()
        .border_1()
        .border_color(cx.theme().border)
        .child(TypingLine::new(Some("bob is typing…".into())));
    vec![
        Section::new(
            "rows",
            "A feed: history, day and New dividers, groups, files, one's own on its way",
            column(&rows, Older::Available, &shown, cx),
        ),
        Section::new(
            "history",
            "The top row",
            div().flex().flex_col().gap_2().children([
                history(Older::Available, cx),
                history(Older::Loading, cx),
                history(Older::Beginning, cx),
            ]),
        ),
        Section::new("typing", "The typing line", typing),
    ]
}

pub(super) fn attachments(
    g: &mut Gallery,
    _: &mut Window,
    cx: &mut Context<Gallery>,
) -> Vec<Section> {
    let wide = attachment(1, "holiday.jpg", "image/jpeg", 1_258_291, Some((480, 320)));
    let tall = attachment(2, "poster.png", "image/png", 312_000, Some((360, 480)));
    let tiny = attachment(3, "icon.png", "image/png", 820, Some((16, 16)));
    let files = [
        attachment(4, "notes.pdf", "application/pdf", 48_000, None),
        attachment(5, "photos.zip", "application/zip", 52_428_800, None),
        attachment(6, "readme.txt", "text/plain", 24, None),
        attachment(7, "song.mp3", "audio/mpeg", 4_200_000, None),
        attachment(8, "clip.mp4", "video/mp4", 31_000_000, None),
        attachment(9, "setup.bat", "application/octet-stream", 1_024, None),
        attachment(
            10,
            "a file with a name long enough to be cut short.docx",
            "application/octet-stream",
            98_304,
            None,
        ),
    ];
    let pictures = vec![wide, tall, tiny];
    let run = |slot: Slot, attachments: &[lobby_protocol::Attachment], cx: &mut App| {
        crate::components::attachment_cards(&"gallery".into(), attachments, &cards(slot, 480.), cx)
    };
    vec![
        Section::new(
            "pictures-shown",
            "Pictures, shown (wide, tall, tiny)",
            run(Slot::Ready(g.held.picture.clone()), &pictures, cx),
        ),
        Section::new(
            "pictures-loading",
            "Pictures, loading",
            run(Slot::Loading, &pictures, cx),
        ),
        Section::new(
            "pictures-failed",
            "Pictures that could not be shown",
            run(Slot::Failed, &pictures, cx),
        ),
        Section::new(
            "files",
            "Other files",
            div().w(px(480.)).child(run(Slot::Loading, &files, cx)),
        ),
    ]
}

pub(super) fn composer(_: &mut Gallery, _: &mut Window, cx: &mut Context<Gallery>) -> Vec<Section> {
    let bar = |b: FormatBar, cx: &App| {
        div()
            .p_1()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .child(b)
            .into_any_element()
    };
    let none = || FormatBar::new(Rc::new(|_, _, _| {}));
    vec![
        Section::new("bar", "The formatting bar", bar(none(), cx)),
        Section::new(
            "active",
            "Bold and code at the caret",
            bar(none().active(vec![Format::Bold, Format::Code]), cx),
        ),
        Section::new(
            "compact",
            "Compact (a folded window)",
            bar(none().compact(true), cx),
        ),
    ]
}

pub(super) fn empty_states(
    _: &mut Gallery,
    _: &mut Window,
    cx: &mut Context<Gallery>,
) -> Vec<Section> {
    let frame = |el: AnyElement, cx: &App| {
        div()
            .w(px(480.))
            .h(px(300.))
            .flex()
            .flex_col()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .child(el)
            .into_any_element()
    };
    let color = mark_color("https://dev.lobby.example");
    let place = |history: &History, invite: Option<&str>, copied, unreached, roomy, cx: &App| {
        frame(
            crate::lobby_view::feed_placeholder(
                history,
                "Dev Lobby",
                invite.map(|i| i.to_owned().into()),
                copied,
                unreached,
                roomy,
                gpui_kit::WeakEntity::new_invalid(),
                cx,
            ),
            cx,
        )
    };
    vec![
        Section::new(
            "unreached",
            "A Lobby not reached yet",
            place(
                &History::Loading,
                None,
                false,
                Some((
                    color,
                    notice(
                        Phase::Reconnecting {
                            attempt: 1,
                            max: 10,
                            retry_in: Duration::from_secs(4),
                        },
                        false,
                        None,
                    ),
                )),
                true,
                cx,
            ),
        ),
        Section::new(
            "loading",
            "Loading the newest Messages",
            place(&History::Loading, None, false, None, true, cx),
        ),
        Section::new(
            "invite",
            "A new Lobby's invite",
            place(
                &History::Loaded,
                Some("https://dev.lobby.example"),
                false,
                None,
                true,
                cx,
            ),
        ),
        Section::new(
            "invite-copied",
            "Its address copied",
            place(
                &History::Loaded,
                Some("https://dev.lobby.example"),
                true,
                None,
                true,
                cx,
            ),
        ),
        Section::new(
            "empty",
            "No Messages yet",
            place(&History::Loaded, None, false, None, true, cx),
        ),
        Section::new(
            "empty-tight",
            "No Messages yet, beside a watched Screen Share",
            place(&History::Loaded, None, false, None, false, cx),
        ),
    ]
}
