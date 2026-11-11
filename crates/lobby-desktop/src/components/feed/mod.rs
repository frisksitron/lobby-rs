//! The Chat feed: group-chat rows built from gpui-kit's `message` components, day dividers, "Load
//! Older Messages" at the top, and the typing line, "Jump to latest" and "New Messages" pill
//! floating over the foot, so nothing moves when they come and go. A Message on its way changes
//! colour only: its row has its final height from the first frame.
//!
//! The list is gpui-kit's `MessageScroller`. Rows are diffed against the last frame's (common
//! prefix and suffix), so a Message arriving, a pending one confirmed or a page prepended
//! re-measures only what changed. The scroller's list is top-aligned, so a blank first row fills
//! what a short chat leaves above it, keeping its Messages on the composer.

mod rows;

use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::{Arc, Weak},
    time::Duration,
};

use chrono::NaiveDate;
use gpui_kit::{
    Animation, AnimationExt as _, AnyElement, App, AppContext as _, Context, Entity, Hsla,
    ImageSource, InteractiveElement as _, IntoElement, ParentElement as _, Role, SharedString,
    StatefulInteractiveElement as _, StyleRefinement, Styled as _, Subscription,
    TestSupportExt as _, Window, assets::IconName, base::TextSelection, div, linear_color_stop,
    linear_gradient, prelude::FluentBuilder as _, pulsating_between, px, relative, rems,
    transparent_black,
};
use lobby_client::{History, LobbyState, Older, Phase, copy};
use lobby_protocol::MessageId;

use rows::{Change, Conversions, Fingerprint, first_unread};
pub use rows::{FeedRow, GROUP_WITHIN_MS, MessageRow, Pending, feed_rows, with_new_divider};

use super::{
    Avatar, AvatarSize, ICON_SM, ICON_XS, PULSE_FPS,
    attachment_card::{Cards, attachment_cards},
    context_menu::{menu_frame, menu_item},
    mention_chip::MentionChip,
};
use crate::{
    clipboard,
    kit::{
        ActiveTheme as _,
        menu::{ContextMenuExt as _, PopupMenu},
        message::{Message, MessageAvatar, MessageContent, MessageHeader},
        message_scroller::{MessageScroller, MessageScrollerState},
        text::{SelectionFormat, TextView, TextViewStyle},
        tooltip::Tooltip,
    },
    scale::{device_in, u},
    ui::{self, Icon, Separator, Spinner},
};

/// The avatar beside a Message.
pub const MESSAGE_AVATAR: AvatarSize = AvatarSize::Row;
/// Above the first Message of a run by one author.
pub const GROUP_GAP: f32 = 10.;
const GROUP_GAP_AFTER_DIVIDER: f32 = 4.;
/// Above a later Message of the same run.
const LATER_ROW_GAP: f32 = 4.;
pub const ROW_PAD_Y: f32 = 2.;
/// A Message that mentions you: a lavender tint over its row and a bar on the row's left edge.
/// Both lie inside the row's own box, so nothing moves.
const MENTION_TINT: f32 = 0.08;
const MENTION_BAR_W: f32 = 2.;
const MENTION_BAR_INSET: f32 = 4.;
pub const HEADER_HEIGHT: f32 = 20.;

pub const GUTTER_X: f32 = 16.;
/// The kit's gap after the avatar column.
const AVATAR_GAP: f32 = 8.;
pub const TEXT_SIZE: f32 = 16.;
pub const LINE_HEIGHT: f32 = 1.4;
/// Only a Message still on its way after this long gets a spinner; before that
/// its colour says it, so a quick send never flickers.
pub const SENDING_MARK_AFTER: Duration = Duration::from_secs(1);
/// Load Older, Loading and Beginning share one height, so the feed never shifts.
pub const HISTORY_ROW: f32 = 44.;
const MEASURE: f32 = 720.;
/// Room under the last row for the typing line, which floats there.
pub const FOOT_ROOM: f32 = 22.;
const PILL_BAND: f32 = 44.;

pub fn card_max_w(column_w: f32) -> f32 {
    column_w - (GUTTER_X + MESSAGE_AVATAR.px() + AVATAR_GAP) - GUTTER_X
}

pub type FeedAction = Rc<dyn Fn(&mut Window, &mut App)>;

/// What a right-click on one of this Member's own Messages offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageChange {
    Edit,
    Delete,
}

pub type ChangeMessage = Rc<dyn Fn(MessageId, MessageChange, &mut Window, &mut App)>;

pub struct Feed {
    scroller: Entity<MessageScrollerState>,
    _scrolled: Subscription,
    rows: Rc<Vec<FeedRow>>,
    print: Option<Fingerprint>,
    /// The state last synced. Weak, so it keeps no old state alive, and its allocation is never
    /// reused for another state that would compare equal.
    synced: Option<(Weak<LobbyState>, NaiveDate, Option<MessageId>)>,
    cache: Conversions,
    changes: Vec<Change>,
    unseen: Rc<Cell<usize>>,
    first_unseen: Rc<Cell<Option<MessageId>>>,
    /// The Message the New divider goes before; [`with_new_divider`] says where if it was deleted.
    new_divider: Option<MessageId>,
    /// The newest Message the Member had seen when this Lobby was opened, until the first page
    /// has been laid out and the New divider placed after it.
    seen_before: Option<MessageId>,
    spacer: Rc<Cell<f32>>,
    /// The rows or the window changed: the blank row grows back while the feed follows the tail,
    /// then is trimmed to what the rows leave free.
    refill: bool,
    window_h: f32,
    rem: f32,
    cards_max_w: Cell<f32>,
}

impl Feed {
    /// `seen_before` is the newest Message the Member had in front of them the last time they
    /// had this Lobby open: what came after it gets the New divider.
    pub fn new<V: 'static>(seen_before: Option<MessageId>, cx: &mut Context<V>) -> Self {
        let scroller = cx.new(|cx| MessageScrollerState::new(1, cx));
        let unseen = Rc::new(Cell::new(0));
        let first_unseen = Rc::new(Cell::new(None));
        let (count, first) = (unseen.clone(), first_unseen.clone());
        let _scrolled = cx.observe(&scroller, move |_, scroller, cx| {
            if !scroller.read(cx).is_scrolled_up() {
                count.set(0);
                first.set(None);
            }
            cx.notify();
        });
        Self {
            scroller,
            _scrolled,
            rows: Rc::new(Vec::new()),
            print: None,
            synced: None,
            cache: HashMap::new(),
            changes: Vec::new(),
            unseen,
            first_unseen,
            new_divider: None,
            seen_before,
            spacer: Rc::new(Cell::new(0.)),
            refill: true,
            window_h: 0.,
            rem: 0.,
            cards_max_w: Cell::new(0.),
        }
    }

    pub fn rows(&self) -> &Rc<Vec<FeedRow>> {
        &self.rows
    }

    pub fn scroller(&self) -> &Entity<MessageScrollerState> {
        &self.scroller
    }

    pub fn unseen(&self) -> usize {
        self.unseen.get()
    }

    pub fn at_bottom(&self, cx: &App) -> bool {
        !self.scroller.read(cx).is_scrolled_up()
    }

    pub fn scroll_to_end(&mut self, cx: &mut App) {
        self.scroller.update(cx, |s, cx| s.scroll_to_end(cx));
        self.unseen.set(0);
        self.first_unseen.set(None);
    }

    pub fn jump(&mut self, cx: &mut App) {
        let first = self.first_unseen.get();
        if self.unseen.get() > 0
            && let Some(first) = first
        {
            self.new_divider = Some(first);
        }
        self.scroll_to_end(cx);
    }

    pub fn sent(&mut self, cx: &mut App) {
        self.new_divider = None;
        self.scroll_to_end(cx);
    }

    pub fn cards_max_w(&self) -> f32 {
        self.cards_max_w.get()
    }

    /// The rows for `state`. The same `Arc` on the same day, with the same "New" divider, is a
    /// state already synced: nothing is walked.
    pub fn sync(&mut self, state: &Arc<LobbyState>, today: NaiveDate, cx: &App) {
        // Once READY's Messages are in: a Lobby switched back to first shows the feed it kept.
        let caught_up = state.phase == Phase::Connected && state.history == History::Loaded;
        if caught_up
            && !state.chat.is_empty()
            && let Some(seen) = self.seen_before.take()
        {
            self.new_divider = first_unread(state, seen);
        }
        let synced = (Arc::downgrade(state), today, self.new_divider);
        if self
            .synced
            .as_ref()
            .is_some_and(|(s, d, n)| s.ptr_eq(&synced.0) && *d == synced.1 && *n == synced.2)
        {
            return;
        }
        self.synced = Some(synced);
        let print = Fingerprint::of(state, today, self.new_divider);
        if self.print.as_ref() == Some(&print) {
            return;
        }
        if self
            .print
            .as_ref()
            .is_some_and(|p| p.me != print.me || p.names != print.names)
            || state.chat.is_empty()
        {
            self.cache.clear();
        }
        self.print = Some(print);
        let new = Rc::new(with_new_divider(
            feed_rows(state, today, &mut self.cache),
            self.new_divider,
        ));
        let old = std::mem::replace(&mut self.rows, new.clone());
        self.refill = true;
        if old.is_empty() || new.is_empty() {
            self.changes.push(Change::Reset(new.len() + 1));
            self.unseen.set(0);
            self.first_unseen.set(None);
            return;
        }
        let scroller = self.scroller.read(cx);
        let following = scroller.is_following_tail();
        let near = !scroller.is_scrolled_up();
        let top = scroller.logical_scroll_top();
        let prefix = old
            .iter()
            .zip(new.iter())
            .take_while(|(a, b)| a == b)
            .count();
        let suffix = old[prefix..]
            .iter()
            .rev()
            .zip(new[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        let removed = prefix + 1..old.len() - suffix + 1;
        let added = new.len() - prefix - suffix;
        self.changes.push(Change::Splice(removed.clone(), added));
        if suffix == 0 {
            let before: HashSet<&SharedString> = old[prefix..]
                .iter()
                .filter_map(|r| r.as_message().map(|m| &m.key))
                .collect();
            let fresh: Vec<&MessageRow> = new[prefix..]
                .iter()
                .filter_map(FeedRow::as_message)
                .filter(|m| !before.contains(&m.key))
                .collect();
            let sent = fresh.iter().any(|m| m.mine && m.pending.is_some());
            let mut others = fresh.iter().filter(|m| !m.mine);
            if near || sent {
                self.changes.push(Change::ToEnd);
                self.unseen.set(0);
                self.first_unseen.set(None);
            } else if let Some(first) = others.next() {
                if self.unseen.get() == 0 {
                    self.first_unseen.set(first.id);
                }
                self.unseen.set(self.unseen.get() + 1 + others.count());
            }
        } else if !following && top.item_ix < removed.end {
            // Older Messages arrived under the History row. The kit scrolls to a row's top only, so
            // the first Message that was in view goes to the top (with its day divider): what was
            // read stays in sight.
            let first = old.iter().find_map(FeedRow::as_message).and_then(|first| {
                new.iter()
                    .position(|r| r.as_message().is_some_and(|m| m.key == first.key))
            });
            let at = match first {
                Some(ix) if ix > 0 && matches!(new[ix - 1], FeedRow::Day { .. }) => ix - 1,
                Some(ix) => ix,
                None => new.len() - suffix,
            };
            self.changes.push(Change::ToItem(at + 1));
        }
    }

    /// Sizes the blank row: grown back to the window's height after a change while the feed follows
    /// the tail, then trimmed by whatever of it scrolled out above, so a short chat fills the view
    /// exactly.
    fn settle(&mut self, window: &Window, cx: &mut App) {
        let window_h = f32::from(window.viewport_size().height);
        if window_h != self.window_h {
            self.window_h = window_h;
            self.refill = true;
        }
        // A new UI scale changes every row's height.
        let rem = f32::from(window.rem_size());
        let rescaled = self.rem != 0. && rem != self.rem;
        self.rem = rem;
        self.refill |= rescaled;
        let changes = std::mem::take(&mut self.changes);
        let spacer = self.spacer.clone();
        let refill = self.refill;
        let refilled = self.scroller.update(cx, |s, cx| {
            if rescaled {
                s.remeasure(cx);
            }
            for change in changes {
                match change {
                    Change::Reset(count) => s.reset(count, cx),
                    Change::Splice(range, count) => {
                        s.splice(range, count, cx);
                    }
                    Change::ToEnd => s.scroll_to_end(cx),
                    Change::ToItem(ix) => {
                        s.scroll_to_item(ix, cx);
                    }
                }
            }
            let following = s.is_following_tail();
            let top = s.logical_scroll_top();
            let now = spacer.get();
            let next = if refill && following {
                window_h
            } else if top.item_ix >= 1 && top.item_ix < s.item_count() {
                0.
            } else if following && top.item_ix == 0 {
                (now - f32::from(top.offset_in_item)).max(0.)
            } else {
                now
            };
            if next != now {
                spacer.set(next);
                s.remeasure_items(0..1, cx);
            }
            refill && following
        });
        if refilled {
            self.refill = false;
        }
    }

    /// The rows and the foot. The typing line floats over them from the view around the feed, so
    /// its dots do not render the rows again.
    #[allow(clippy::too_many_arguments)]
    pub fn render<V: 'static>(
        &mut self,
        older: Older,
        on_load_older: FeedAction,
        on_jump: FeedAction,
        on_link: fn(&str, &mut App),
        cards: Cards,
        window: &mut Window,
        cx: &mut Context<V>,
    ) -> AnyElement {
        self.settle(window, cx);
        self.cards_max_w.set(cards.max_w);
        let rows = self.rows.clone();
        let spacer = self.spacer.clone();
        let background = cx.theme().background;
        let load = on_load_older;
        let unseen = self.unseen.get();
        let scrolled_up = !self.at_bottom(cx);
        // The room above and below is in the first and last rows, not the list's padding: the
        // wheel stops where the rows end, so it never reached the list's bottom padding.
        let scroller = MessageScroller::new("feed-scroller", self.scroller.clone(), {
            move |ix, _window, cx| match ix.checked_sub(1) {
                Some(ix) if ix + 1 == rows.len() => div()
                    .pb(u(FOOT_ROOM))
                    .child(feed_row(&rows, ix, older, &load, on_link, &cards, cx))
                    .into_any_element(),
                Some(ix) => feed_row(&rows, ix, older, &load, on_link, &cards, cx),
                None => div()
                    .pt(u(6.))
                    .child(div().h(px(spacer.get()))) // px: window pixels
                    .into_any_element(),
            }
        })
        .with_row_style(StyleRefinement::default().px_0().pb_0())
        .with_list_style(StyleRefinement::default().py_0())
        .jump_button(false);
        let foot = scrolled_up.then(|| {
            let jump = on_jump;
            let solid = PILL_BAND / (PILL_BAND + FOOT_ROOM);
            let pill = ui::Button::new("feed-jump")
                .debug_selector(|| "feed-jump".into())
                .size(ui::Size::Word)
                .rounded_full()
                .icon(IconName::ArrowDown)
                .map(|b| {
                    if unseen > 0 {
                        b.variant(ui::Variant::Primary)
                            .label(copy::new_messages(unseen))
                    } else {
                        b.variant(ui::Variant::Outline).label(copy::JUMP_TO_LATEST)
                    }
                })
                .on_click(move |_, window, cx| jump(window, cx));
            div()
                .absolute()
                .bottom_0()
                .left_0()
                .right_0()
                .h(u(FOOT_ROOM + PILL_BAND))
                .pb(u(FOOT_ROOM + 4.))
                .flex()
                .items_end()
                .justify_center()
                .bg(linear_gradient(
                    180.,
                    linear_color_stop(background.opacity(0.), 0.),
                    linear_color_stop(background, solid),
                ))
                // The pill takes its own clicks: a picture under it must not open too.
                .child(div().occlude().child(pill))
                .with_animation(
                    "feed-foot-in",
                    Animation::new(Duration::from_millis(180)),
                    |d, delta| d.opacity(delta),
                )
                .into_any_element()
        });
        div()
            .id("feed-list")
            .relative()
            .size_full()
            .child(scroller)
            .children(foot)
            .test_support()
            .into_any_element()
    }
}

pub fn feed_row(
    rows: &[FeedRow],
    ix: usize,
    older: Older,
    on_load_older: &FeedAction,
    on_link: fn(&str, &mut App),
    cards: &Cards,
    cx: &mut App,
) -> AnyElement {
    match &rows[ix] {
        FeedRow::History => history_row(older, on_load_older.clone(), cx),
        FeedRow::Day { key, label } => day_divider(key, label),
        FeedRow::New => new_divider(cx),
        FeedRow::Message(m) => {
            let after_divider = ix > 0
                && matches!(
                    rows[ix - 1],
                    FeedRow::Day { .. } | FeedRow::History | FeedRow::New
                );
            let style = chat_text_style(cx);
            message_row(m, after_divider, style, on_link, cards, cx)
        }
    }
}

fn chat_text_style(cx: &App) -> TextViewStyle {
    let mut code = StyleRefinement::default();
    code.padding.left = Some(u(10.).into());
    code.padding.right = Some(u(10.).into());
    code.padding.top = Some(u(6.).into());
    code.padding.bottom = Some(u(6.).into());
    code.max_size.height = Some(u(300.).into());
    code.overflow.y = Some(gpui_kit::Overflow::Scroll);
    let mut style = TextViewStyle::default()
        .paragraph_gap(rems(0.25))
        .heading_font_size(|level, base| match level {
            1 => base * 1.4,
            2 => base * 1.25,
            3 => base * 1.12,
            _ => base,
        })
        .code_block(code);
    style.heading_base_font_size = device_in(TEXT_SIZE, cx);
    style.is_dark = true;
    style
}

fn history_row(older: Older, on_load: FeedAction, cx: &App) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let inner = match older {
        Older::Available | Older::Unknown => ui::Button::new("load-older")
            .size(ui::Size::Word)
            .icon(IconName::ArrowUp)
            .label(copy::LOAD_OLDER)
            .on_click(move |_, window, cx| on_load(window, cx))
            .into_any_element(),
        Older::Loading => div()
            .id("loading-older")
            .flex()
            .items_center()
            .gap_2()
            .text_xs()
            .text_color(muted)
            .child(Spinner::new().size(u(ICON_SM)).color(muted))
            .child("Loading…")
            .into_any_element(),
        Older::Beginning => div()
            .id("history-beginning")
            .flex()
            .items_center()
            .gap_1p5()
            .text_xs()
            .text_color(muted)
            .child(Icon::new(IconName::Flag).size(u(ICON_XS)).text_color(muted))
            .child(copy::BEGINNING_OF_HISTORY)
            .into_any_element(),
    };
    div()
        .id("feed-history")
        .h(u(HISTORY_ROW))
        .w_full()
        .flex()
        .items_center()
        .justify_center()
        .child(inner)
        .test_support()
        .into_any_element()
}

fn divider(
    id: SharedString,
    label: SharedString,
    line: Hsla,
    text: Hsla,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id(id)
        .w_full()
        .px_4()
        .pt_3()
        .pb_1()
        .flex()
        .items_center()
        .gap_3()
        .child(div().flex_1().h(px(1.)).bg(line)) // px: hairline
        .child(
            div()
                .flex_shrink_0()
                .text_xs()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(text)
                .child(label),
        )
        .child(div().flex_1().h(px(1.)).bg(line)) // px: hairline
}

fn day_divider(key: &SharedString, label: &SharedString) -> AnyElement {
    div()
        .id(key.clone())
        .w_full()
        .px_4()
        .pt_2()
        .child(
            Separator::horizontal()
                .label(label.clone())
                .font_weight(gpui_kit::FontWeight::SEMIBOLD),
        )
        .test_support()
        .into_any_element()
}

/// "New" is red, apart from the lavender of Voice. It quietens once the reader has seen it (colour
/// only: the row keeps its place).
fn new_divider(cx: &App) -> AnyElement {
    let accent = cx.theme().danger;
    div()
        .id("feed-new")
        .w_full()
        .child(
            divider(
                "feed-new-line".into(),
                copy::NEW_DIVIDER.into(),
                accent,
                accent,
            )
            .with_animation(
                "feed-new-quiets",
                Animation::new(NEW_DIVIDER_QUIETS),
                |d, delta| d.opacity(1. - 0.55 * ((delta - 0.75) / 0.25).clamp(0., 1.)),
            ),
        )
        .test_support()
        .into_any_element()
}

const NEW_DIVIDER_QUIETS: Duration = Duration::from_secs(4);

fn message_row(
    m: &MessageRow,
    after_divider: bool,
    style: TextViewStyle,
    on_link: fn(&str, &mut App),
    cards: &Cards,
    cx: &mut App,
) -> AnyElement {
    let theme = cx.theme();
    let (muted, fg, hover, lavender) = (
        theme.muted_foreground,
        theme.foreground,
        theme.muted.opacity(0.45),
        theme.primary,
    );
    let avatar_w = MESSAGE_AVATAR.px();
    let gutter = MessageAvatar::new()
        .self_start()
        .w(u(avatar_w))
        .bg(transparent_black())
        .rounded(u(0.))
        .overflow_hidden();
    let avatar = if m.first {
        gutter.h(u(avatar_w)).child(
            div().when(m.deactivated, |d| d.opacity(0.6)).child(
                Avatar::member(m.author.clone())
                    .size(MESSAGE_AVATAR)
                    .image((cards.avatars)(m.avatar.as_ref(), cx)),
            ),
        )
    } else {
        let slot = gutter
            .h(u(TEXT_SIZE * LINE_HEIGHT))
            .flex()
            .items_center()
            .justify_center();
        match m.pending {
            Some(p) => slot.child(pending_mark(&m.key, p, muted)),
            None => {
                let full = m.full_time.clone();
                slot.child(
                    div()
                        .id(SharedString::from(format!("{}-hover-time", m.key)))
                        // Transparent rather than hidden: it keeps its hitbox, so hovering brings
                        // the full date's tooltip.
                        .opacity(0.)
                        .group_hover("feed-message", |s| s.opacity(1.))
                        .text_size(u(10.5))
                        .text_color(muted)
                        .child(m.time.clone())
                        .tooltip(move |window, cx| Tooltip::new(full.clone()).build(window, cx))
                        .test_support(),
                )
            }
        }
    };
    let header = m.first.then(|| {
        let full = m.full_time.clone();
        MessageHeader::new()
            .content_inset(false)
            .h(u(HEADER_HEIGHT))
            .gap_2()
            .items_center()
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(fg)
                    .when(m.deactivated, |d| d.text_color(muted))
                    .child(m.author.clone()),
            )
            .child(
                div()
                    .id(SharedString::from(format!("{}-time", m.key)))
                    .flex_shrink_0()
                    .text_xs()
                    .font_weight(gpui_kit::FontWeight::NORMAL)
                    .text_color(muted)
                    .child(m.time.clone())
                    .tooltip(move |window, cx| Tooltip::new(full.clone()).build(window, cx))
                    .test_support(),
            )
            .when(m.edited, |h| h.child(edited_mark(&m.key, muted)))
            .when_some(m.pending, |h, p| {
                h.child(
                    div()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap_1()
                        .text_xs()
                        .text_color(muted)
                        .child(pending_mark(&m.key, p, muted))
                        .when(p == Pending::Waiting, |d| {
                            d.child(div().min_w_0().truncate().child(copy::WAITING_TO_SEND))
                        }),
                )
            })
    });
    let text = (!m.markdown.trim().is_empty()).then(|| {
        TextView::markdown(
            SharedString::from(format!("{}-text", m.key)),
            m.markdown.clone(),
        )
        .selectable(true)
        .selection_format(SelectionFormat::Plain)
        .plugin(MentionChip)
        .style(style)
        .text_size(u(TEXT_SIZE))
        .line_height(relative(LINE_HEIGHT))
        // The content never holds an image; should one get through, it is not fetched.
        .image_source(|_| ImageSource::Custom(Arc::new(|_, _| None)))
        // The kit reports a link on the right button's release too, after the row's menu has
        // opened; only the left and middle buttons follow it.
        .on_link_click(move |url, click, _, cx| {
            if !click.is_right_click() {
                on_link(url, cx)
            }
        })
    });
    let files =
        (!m.attachments.is_empty()).then(|| attachment_cards(&m.key, &m.attachments, cards, cx));
    let content = MessageContent::new().child(
        div()
            .debug_selector({
                let key = m.key.clone();
                move || format!("message-text-{key}")
            })
            .w_full()
            .min_w_0()
            .max_w(u(MEASURE))
            .text_size(u(TEXT_SIZE))
            .line_height(relative(LINE_HEIGHT))
            .text_color(fg)
            .when(m.pending.is_some(), |d| d.text_color(muted))
            .children(text)
            // A later row has no header to carry the mark.
            .when(m.edited && !m.first, |d| {
                d.child(div().flex().child(edited_mark(&m.key, muted)))
            })
            .children(files.map(|f| div().when(m.pending.is_some(), |d| d.opacity(0.7)).child(f))),
    );
    let message = Message::new()
        .id(SharedString::from(format!("message-{}", m.key)))
        .role(Role::ListItem)
        .gap(u(2.))
        .with_stack_style(StyleRefinement::default().gap(u(1.)))
        .avatar_slot(avatar)
        .when_some(header, |msg, h| msg.header(h))
        .content(content);
    let top = match (m.first, after_divider) {
        (true, false) => GROUP_GAP,
        (true, true) => GROUP_GAP_AFTER_DIVIDER,
        (false, _) => LATER_ROW_GAP,
    };
    div()
        .id(SharedString::from(format!("row-{}", m.key)))
        .group("feed-message")
        .w_full()
        .px_2()
        .pt(u(top))
        .child(
            div()
                // GPUI keeps hover state by id: without one, entering the row never redraws it.
                .id(SharedString::from(format!("row-hover-{}", m.key)))
                .relative()
                .w_full()
                .rounded_md()
                .px_2()
                .py(u(ROW_PAD_Y))
                .when(m.mentions_me, |d| d.bg(lavender.opacity(MENTION_TINT)))
                .hover(move |s| s.bg(hover))
                .when(m.mentions_me, |d| {
                    d.child(
                        div()
                            .absolute()
                            .left_0()
                            .top(u(MENTION_BAR_INSET))
                            .bottom(u(MENTION_BAR_INSET))
                            .w(u(MENTION_BAR_W))
                            .rounded_full()
                            .bg(lavender),
                    )
                })
                .child(message),
        )
        .test_support()
        .context_menu(message_menu(
            m.text.clone(),
            m.id.filter(|_| m.mine),
            cards.on_change.clone(),
        ))
        .into_any_element()
}

fn edited_mark(key: &SharedString, muted: Hsla) -> impl IntoElement {
    div()
        .id(SharedString::from(format!("{key}-edited")))
        .flex_shrink_0()
        .text_xs()
        .text_color(muted)
        .child(copy::EDITED)
        .test_support()
}

/// Copy takes what is selected, which the menu reads as it opens: a click on the menu clears it.
/// The kit tells a link only when the right button is released, after the menu is built, so
/// there is no link item. Edit and Delete are for `mine`, one of this Member's sent Messages.
fn message_menu(
    text: SharedString,
    mine: Option<MessageId>,
    on_change: ChangeMessage,
) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
    move |menu, window, cx| {
        let selected = TextSelection::selected_text(window, cx);
        let mut menu = menu_frame(menu, window);
        if !selected.is_empty() {
            menu = menu.item(
                menu_item("menu-copy", copy::COPY)
                    .on_click(move |_, _, cx| clipboard::copy_text(selected.clone(), cx)),
            );
        }
        if !text.is_empty() {
            let text = text.clone();
            menu = menu.item(
                menu_item("menu-copy-message", copy::COPY_MESSAGE)
                    .on_click(move |_, _, cx| clipboard::copy_text(text.to_string(), cx)),
            );
        }
        if let Some(id) = mine {
            for (item, label, change) in [
                ("menu-edit", copy::EDIT_MESSAGE, MessageChange::Edit),
                ("menu-delete", copy::DELETE_MESSAGE, MessageChange::Delete),
            ] {
                let on_change = on_change.clone();
                menu = menu.item(
                    menu_item(item, label)
                        .on_click(move |_, window, cx| on_change(id, change, window, cx)),
                );
            }
        }
        menu
    }
}

fn pending_mark(key: &SharedString, p: Pending, muted: Hsla) -> AnyElement {
    match p {
        Pending::Waiting => div()
            .id(SharedString::from(format!("{key}-waiting")))
            .flex()
            .child(
                Icon::new(IconName::Clock)
                    .size(u(ICON_XS))
                    .text_color(muted),
            )
            .tooltip(|window, cx| Tooltip::new(copy::WAITING_TO_SEND).build(window, cx))
            .test_support()
            .into_any_element(),
        Pending::Sending => {
            let fade = Duration::from_millis(250);
            let total = SENDING_MARK_AFTER + fade;
            let from = SENDING_MARK_AFTER.as_secs_f32() / total.as_secs_f32();
            div()
                .id(SharedString::from(format!("{key}-sending")))
                .flex()
                .child(
                    div()
                        .flex()
                        .child(
                            Spinner::new()
                                .icon(IconName::LoaderCircle)
                                .size(u(ICON_XS))
                                .color(muted),
                        )
                        .with_animation(
                            SharedString::from(format!("{key}-sending-in")),
                            Animation::new(total),
                            move |d, delta| d.opacity(((delta - from) / (1. - from)).clamp(0., 1.)),
                        ),
                )
                .tooltip(|window, cx| Tooltip::new(copy::SENDING).build(window, cx))
                .test_support()
                .into_any_element()
        }
    }
}

pub fn typing_indicator(line: SharedString, cx: &App) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let avatar_w = MESSAGE_AVATAR.px();
    let dot = |i: usize| {
        div().size(u(4.)).rounded_full().bg(muted).with_animation(
            ("typing-dot", i),
            Animation::new(Duration::from_millis(1200))
                .repeat()
                .with_max_fps(PULSE_FPS)
                .with_easing(pulsating_between(0.25, 1.)),
            move |d, delta| {
                let phase = (delta + (2 - i) as f32 / 3.) % 1.;
                d.opacity(0.25 + 0.75 * phase)
            },
        )
    };
    div()
        .id("typing-line")
        .absolute()
        .bottom_0()
        .left_0()
        .right_0()
        .h(u(FOOT_ROOM))
        .px(u(GUTTER_X))
        .flex()
        .items_center()
        .text_xs()
        .text_color(muted)
        .child(
            div()
                .flex_shrink_0()
                .w(u(avatar_w))
                .flex()
                .items_center()
                .justify_center()
                .gap(u(3.))
                .children((0..3).map(dot)),
        )
        .child(div().ml(u(AVATAR_GAP)).min_w_0().truncate().child(line))
        .test_support()
        .into_any_element()
}
