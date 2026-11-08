//! The feed's rows as plain data: built from the Lobby state, grouped by author and time, with
//! day dividers and the New divider. The scroller view diffs them frame to frame.

use std::{
    collections::HashMap,
    ops::{Bound, Range},
};

use chrono::NaiveDate;
use gpui_kit::SharedString;
use lobby_client::{
    LobbyState, Older, Phase, day_label, full_time, local_day, local_time,
    message_text::{MentionNames, MessageText},
};
use lobby_protocol::{Attachment, BlobId, MessageId, UnixMs, UserId, Username};

pub const GROUP_WITHIN_MS: i64 = 5 * 60 * 1000;
/// A group ends once a Message comes this long after its first, however steady the trickle: one
/// time then stands for at most ten minutes, long enough for a burst and short enough to place
/// a Message by the header above it.
const GROUP_SPAN_MS: i64 = 10 * 60 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pending {
    Sending,
    Waiting,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageRow {
    pub key: SharedString,
    /// `None` while it is on its way.
    pub id: Option<MessageId>,
    pub author: SharedString,
    pub avatar: Option<BlobId>,
    pub mine: bool,
    pub first: bool,
    pub time: SharedString,
    pub full_time: SharedString,
    pub markdown: SharedString,
    pub text: SharedString,
    /// Someone else's Message that names this Member with `@name`.
    pub mentions_me: bool,
    pub pending: Option<Pending>,
    pub edited: bool,
    pub deactivated: bool,
    pub attachments: Vec<Attachment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedRow {
    History,
    Day {
        key: SharedString,
        label: SharedString,
    },
    New,
    Message(MessageRow),
}

impl FeedRow {
    pub fn as_message(&self) -> Option<&MessageRow> {
        match self {
            FeedRow::Message(m) => Some(m),
            _ => None,
        }
    }
}

/// What a Message's row needs that never changes for one version of it, made once. On Windows the
/// local-time calls cost about 2 microseconds a Message, so redoing them on every change of 5000
/// Messages took a whole frame.
#[derive(Clone)]
pub struct Converted {
    key: SharedString,
    markdown: SharedString,
    text: SharedString,
    /// It names `mentioned` with `@name`.
    mentions_me: bool,
    day: Option<NaiveDate>,
    time: SharedString,
    full_time: SharedString,
}

impl Converted {
    /// `mentioned`: this Member, for someone else's Message.
    fn of(
        key: SharedString,
        content: &str,
        at: UnixMs,
        names: &MentionNames,
        mentioned: Option<&Username>,
    ) -> Self {
        let text = MessageText::of_in(content, names);
        Self {
            key,
            mentions_me: mentioned.is_some_and(|me| text.mentions(me)),
            markdown: text.markdown.into(),
            text: text.plain.into(),
            day: local_day(at),
            time: local_time(at).into(),
            full_time: full_time(at).into(),
        }
    }
}

/// Each Message's [`Converted`], by its id and when it was last edited.
pub(super) type Conversions = HashMap<(MessageId, Option<UnixMs>), Converted>;

pub fn feed_rows(state: &LobbyState, today: NaiveDate, cache: &mut Conversions) -> Vec<FeedRow> {
    struct Item<'a> {
        id: Option<MessageId>,
        author_id: Option<&'a UserId>,
        author: SharedString,
        avatar: Option<BlobId>,
        at: UnixMs,
        converted: Converted,
        pending: Option<Pending>,
        edited: bool,
        deactivated: bool,
        attachments: Vec<Attachment>,
    }
    cache.retain(|(id, edited_at), _| {
        state
            .chat
            .get(id)
            .is_some_and(|m| m.edited_at == *edited_at)
    });
    let me = state.me.as_ref();
    let connected = state.phase == Phase::Connected;
    let names = state.mention_names();
    let mut items: Vec<Item<'_>> = state
        .chat
        .values()
        .map(|m| Item {
            id: Some(m.id),
            author_id: Some(&m.author.id),
            author: m.author.username.clone().into(),
            avatar: m.author.avatar.clone(),
            at: m.created_at,
            converted: cache
                .entry((m.id, m.edited_at))
                .or_insert_with(|| {
                    let mentioned = state
                        .my_username()
                        .filter(|_| Some(&m.author.id) != state.me.as_ref());
                    let key = message_key(m.id);
                    Converted::of(key, &m.content, m.created_at, &names, mentioned)
                })
                .clone(),
            pending: None,
            edited: m.edited_at.is_some(),
            deactivated: m.author.deactivated,
            attachments: m.attachments.clone(),
        })
        .collect();
    items.extend(state.pending.iter().map(|p| {
        let key = format!("p{}", p.nonce).into();
        let mut converted = Converted::of(key, &p.content, p.at, &names, None);
        converted.text = p.draft.trim().to_owned().into();
        Item {
            id: None,
            author_id: me,
            author: state
                .my_username()
                .map(ToString::to_string)
                .unwrap_or_default()
                .into(),
            avatar: me
                .and_then(|me| state.members.get(me))
                .and_then(|m| m.avatar.clone()),
            at: p.at,
            converted,
            pending: Some(if p.queued && !connected {
                Pending::Waiting
            } else {
                Pending::Sending
            }),
            edited: false,
            deactivated: false,
            attachments: p
                .attachments
                .iter()
                .filter_map(|d| d.attachment().cloned())
                .collect(),
        }
    }));
    let mut rows = Vec::with_capacity(items.len() + 8);
    if !items.is_empty() && state.older != Older::Unknown {
        rows.push(FeedRow::History);
    }
    let mut prev: Option<(Option<&UserId>, UnixMs, Option<NaiveDate>)> = None;
    let mut group_start = UnixMs(0);
    for item in items {
        let Converted {
            key,
            markdown,
            text,
            mentions_me,
            day,
            time,
            full_time,
        } = item.converted;
        let new_day = prev.as_ref().is_none_or(|(_, _, d)| *d != day);
        if new_day && let Some(d) = day {
            rows.push(FeedRow::Day {
                key: format!("d{d}").into(),
                label: day_label(d, today).into(),
            });
        }
        let first = match &prev {
            Some((author, at, _)) => {
                new_day
                    || *author != item.author_id
                    || item.author_id.is_none()
                    || item.at.0 < at.0
                    || item.at.0 - at.0 >= GROUP_WITHIN_MS
                    || item.at.0 - group_start.0 >= GROUP_SPAN_MS
            }
            None => true,
        };
        if first {
            group_start = item.at;
        }
        prev = Some((item.author_id, item.at, day));
        rows.push(FeedRow::Message(MessageRow {
            key,
            id: item.id,
            mine: item.author_id.is_some() && item.author_id == me,
            author: item.author,
            avatar: item.avatar,
            first,
            time,
            full_time,
            markdown,
            text,
            mentions_me,
            pending: item.pending,
            edited: item.edited,
            deactivated: item.deactivated,
            attachments: item.attachments,
        }));
    }
    rows
}

fn message_key(id: MessageId) -> SharedString {
    format!("m{}", id.get()).into()
}

/// The first Message from another Member after `seen`, the newest one the Member had in front of
/// them, which may have been deleted since. `None` when the loaded page does not reach back to
/// `seen`: the Messages before it are not here to compare with, so nothing is marked rather than
/// the whole page.
pub fn first_unread(state: &LobbyState, seen: MessageId) -> Option<MessageId> {
    let reaches_seen =
        state.chat.range(..=seen).next().is_some() || state.older == Older::Beginning;
    if !reaches_seen {
        return None;
    }
    state
        .chat
        .range((Bound::Excluded(seen), Bound::Unbounded))
        .find(|(_, m)| Some(&m.author.id) != state.me.as_ref())
        .map(|(id, _)| *id)
}

/// The New divider goes before `from`, or the next Message from another Member if `from` was
/// deleted.
pub fn with_new_divider(mut rows: Vec<FeedRow>, from: Option<MessageId>) -> Vec<FeedRow> {
    let at = from.and_then(|from| {
        rows.iter().position(|r| {
            r.as_message()
                .is_some_and(|m| !m.mine && m.id.is_some_and(|id| id >= from))
        })
    });
    if let Some(ix) = at {
        // The divider cuts the author's group: what follows it carries its own name and time.
        if let FeedRow::Message(m) = &mut rows[ix] {
            m.first = true;
        }
        rows.insert(ix, FeedRow::New);
    }
    rows
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Fingerprint {
    len: usize,
    first: Option<MessageId>,
    last: Option<MessageId>,
    pending: Vec<(String, bool, Vec<BlobId>)>,
    attachments: usize,
    deactivated: usize,
    author_changes: u64,
    chat_changes: u64,
    connected: bool,
    history: bool,
    today: NaiveDate,
    pub(super) me: Option<UserId>,
    /// The names a mention resolves against: rows built with other ones are built again.
    pub(super) names: MentionNames,
    new_divider: Option<MessageId>,
}

impl Fingerprint {
    pub(super) fn of(state: &LobbyState, today: NaiveDate, new_divider: Option<MessageId>) -> Self {
        Self {
            len: state.chat.len(),
            first: state.oldest_message(),
            last: state.newest_message(),
            pending: state
                .pending
                .iter()
                .map(|p| {
                    let ids = p
                        .attachments
                        .iter()
                        .filter_map(|d| d.attachment().map(|a| a.id.clone()))
                        .collect();
                    (p.nonce.clone(), p.queued, ids)
                })
                .collect(),
            attachments: state.chat.values().map(|m| m.attachments.len()).sum(),
            deactivated: state.chat.values().filter(|m| m.author.deactivated).count(),
            author_changes: state.author_changes,
            chat_changes: state.chat_changes,
            connected: state.phase == Phase::Connected,
            history: state.older != Older::Unknown,
            today,
            me: state.me.clone(),
            names: state.mention_names(),
            new_divider,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Change {
    Reset(usize),
    Splice(Range<usize>, usize),
    ToEnd,
    ToItem(usize),
}
