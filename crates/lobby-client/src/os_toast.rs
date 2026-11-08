//! When a Message or a Decision is worth an operating-system toast, and the words in it. Pure:
//! the desktop reads its window and Do Not Disturb into a [`Moment`] and posts what this says.

use lobby_protocol::Message;

use crate::{copy, message_text::MessageText};

/// A toast is a glance: longer text is cut.
pub const BODY_MAX_CHARS: usize = 180;

/// What is true when something arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Moment {
    /// The Message is this Member's own, from this or another device.
    pub mine: bool,
    pub window_active: bool,
    pub hidden_to_tray: bool,
    pub dnd: bool,
    /// The Notification Center is open: it already shows it.
    pub center_open: bool,
}

/// A toast is for what the person cannot see: the window is behind others or hidden to the tray.
/// Do Not Disturb silences it, and so does an open Center.
pub fn should_os_toast(m: Moment) -> bool {
    !m.mine && !m.dnd && !m.center_open && (!m.window_active || m.hidden_to_tray)
}

/// The Message's plain text on one line, cut to [`BODY_MAX_CHARS`], or what it carries.
pub fn message_body(message: &Message) -> String {
    let plain = MessageText::of(&message.content).plain;
    let text = plain.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return copy::sent_attachments(message.attachments.len());
    }
    if text.chars().count() <= BODY_MAX_CHARS {
        return text;
    }
    let kept: String = text.chars().take(BODY_MAX_CHARS - 1).collect();
    format!("{}…", kept.trim_end())
}

#[cfg(test)]
mod tests {
    use lobby_protocol::{Attachment, ServerMsg};

    use super::*;
    use crate::testing::frames;

    const QUIET: Moment = Moment {
        mine: false,
        window_active: false,
        hidden_to_tray: false,
        dnd: false,
        center_open: false,
    };

    #[test]
    fn the_rule_over_every_input() {
        let cases = [
            ("behind other windows", QUIET, true),
            (
                "hidden to the tray, though GPUI says active",
                Moment {
                    window_active: true,
                    hidden_to_tray: true,
                    ..QUIET
                },
                true,
            ),
            (
                "the window is in front",
                Moment {
                    window_active: true,
                    ..QUIET
                },
                false,
            ),
            (
                "my own",
                Moment {
                    mine: true,
                    ..QUIET
                },
                false,
            ),
            ("Do Not Disturb", Moment { dnd: true, ..QUIET }, false),
            (
                "the Center is open",
                Moment {
                    center_open: true,
                    ..QUIET
                },
                false,
            ),
            (
                "hidden but Do Not Disturb",
                Moment {
                    hidden_to_tray: true,
                    dnd: true,
                    ..QUIET
                },
                false,
            ),
        ];
        for (what, moment, toast) in cases {
            assert_eq!(should_os_toast(moment), toast, "{what}");
        }
    }

    fn message(content: &str, attachments: usize) -> Message {
        let ServerMsg::MessageCreate { mut message } = frames::message(1, "bob", content, "n")
        else {
            unreachable!()
        };
        message.attachments = (0..attachments)
            .map(|i| Attachment {
                id: format!("blb_{i:024x}").parse().expect("valid id"),
                name: "a.png".into(),
                mime_type: "image/png".into(),
                size: 1,
                preview: None,
            })
            .collect();
        message
    }

    #[test]
    fn the_body_is_the_text_or_what_was_attached() {
        assert_eq!(message_body(&message("**hi**\n\nthere", 0)), "hi there");
        assert_eq!(message_body(&message("", 1)), "Sent an attachment");
        assert_eq!(message_body(&message("", 3)), "Sent 3 attachments");
        assert_eq!(message_body(&message("look", 2)), "look");
    }

    #[test]
    fn a_long_body_is_cut_with_an_ellipsis() {
        let exact = "a".repeat(BODY_MAX_CHARS);
        assert_eq!(message_body(&message(&exact, 0)), exact);
        let cut = message_body(&message(&"é".repeat(BODY_MAX_CHARS + 20), 0));
        assert_eq!(cut.chars().count(), BODY_MAX_CHARS);
        assert!(cut.ends_with('…'));
    }
}
