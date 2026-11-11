//! The composer: one framed field holding the text, the attachments waiting to go, and the
//! formatting bar with Send at its end. It owns everything typed, pasted, dropped or put back by a
//! failed send; what the person should hear about goes up to the Lobby view as a
//! [`ComposerEvent`], which owns the notifications and the feed.

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    time::{Duration, Instant},
};

use gpui_kit::{
    App, AppContext as _, ClipboardEntry, ClipboardItem, Context, DragMoveEvent, Entity,
    EventEmitter, ExternalPaths, Focusable as _, ImageFormat, InteractiveElement as _, IntoElement,
    KeyDownEvent, Keystroke, MouseButton, ParentElement as _, Render, SharedString, Styled as _,
    Subscription, Task, TestSupportExt as _, Window,
    assets::IconName,
    base::input::{self, Cut, InputEvent, Paste, SelectAll, TextareaState},
    canvas, div,
    prelude::FluentBuilder as _,
};
use lobby_client::{
    AttachError, DraftId, DraftSource, DraftState, Intent, Issue, IssueKind, PastedFormat,
    SendBlock,
    compose::{self, Enter, Format, MentionTarget},
    copy,
};
use lobby_protocol::MessageId;

use super::{NARROW_BELOW, forward};
use crate::scale::{design_viewport, device_in, u};
use crate::{
    components::{
        FORMAT_BAR_HEIGHT, FormatBar, INLINE_H, MediaImages, SURFACE_INNER_RADIUS, SURFACE_PAD,
        SURFACE_RADIUS,
        context_menu::{menu_frame, menu_item},
        draft_strip, format_for, mention_chip, mention_menu,
    },
    kit::{
        ActiveTheme as _,
        menu::{ContextMenuExt as _, PopupMenu},
    },
    lobbies::Lobbies,
    session_entity::SessionEntity,
    ui,
};

/// The mention menu floats this far above the composer's frame.
const MENTION_MENU_GAP: f32 = 6.;

/// The one-row composer's controls stand in a row this tall, as a line of text does.
const NARROW_ROW_H: f32 = 36.;

// Attach and Send sit in the composer's corners as the Voice foot sits in the sheet's, the same
// distance from both edges, so they round to the same corner. Each row centres them, which
// leaves part of that distance as the row's own air.
/// Under the formatting bar's row.
const WIDE_PAD_BOTTOM: f32 = SURFACE_PAD - (FORMAT_BAR_HEIGHT - INLINE_H) / 2.;
/// Above and under the one-row composer's row.
const NARROW_PAD_Y: f32 = SURFACE_PAD - (NARROW_ROW_H - INLINE_H) / 2.;
/// How far right the Send glyph moves to sit optically centred.
const SEND_NUDGE: f32 = 1.;

/// A failed draft comes back into an empty composer only once the person has not typed for this
/// long, so a quick typist's next words never run into it (they go under it instead).
pub const RESTORE_AFTER_PAUSE: Duration = Duration::from_millis(700);

/// The longest Lobby name the composer's placeholder spells out. The placeholder cannot ellipsize
/// itself and would run into Send.
const PLACEHOLDER_NAME_MAX: usize = 28;

/// The composer's placeholder is the same whatever the connection does.
pub fn composer_placeholder(lobby: &str) -> String {
    let lobby = lobby.trim();
    if lobby.is_empty() {
        return copy::COMPOSER_PLACEHOLDER.to_owned();
    }
    let shown = if lobby.chars().count() > PLACEHOLDER_NAME_MAX {
        let cut: String = lobby.chars().take(PLACEHOLDER_NAME_MAX - 1).collect();
        let cut = match cut.rfind(' ') {
            Some(at) if at > 8 => &cut[..at],
            _ => cut.as_str(),
        };
        format!("{}…", cut.trim_end())
    } else {
        lobby.to_owned()
    };
    format!("{} {shown}", copy::COMPOSER_PLACEHOLDER)
}

/// The composer after a failed send. An empty composer gets the draft back as it was, caret at
/// its end. Otherwise the draft goes on its own line above what was typed since, with the caret
/// left where the person is typing.
pub fn restore_draft(current: &str, caret: usize, draft: &str) -> (String, usize) {
    if current.trim().is_empty() {
        (draft.to_owned(), draft.len())
    } else {
        (
            format!("{draft}\n{current}"),
            caret.min(current.len()) + draft.len() + 1,
        )
    }
}

pub fn withdraw_draft(current: &str, draft: &str) -> Option<String> {
    restored_draft_range(current, draft)
        .map(|range| format!("{}{}", &current[..range.start], &current[range.end..]))
}

fn restored_draft_range(current: &str, draft: &str) -> Option<std::ops::Range<usize>> {
    if current == draft {
        return Some(0..current.len());
    }
    if let Some(rest) = current.strip_suffix(draft)
        && let Some(rest) = rest.strip_suffix('\n')
    {
        return Some(rest.len()..current.len());
    }
    if let Some(rest) = current.strip_prefix(draft)
        && let Some(rest) = rest.strip_prefix('\n')
    {
        return Some(0..current.len() - rest.len());
    }
    current
        .find(&format!("\n{draft}\n"))
        .map(|at| at..at + draft.len() + 1)
}

pub enum ComposerEvent {
    /// Something the person should know: the Lobby view raises it.
    Notify(Issue),
    Dismiss(IssueKind),
    /// A Message went out: the feed follows it to the bottom.
    Sent,
}

/// The field's text and selection.
type Caret = (SharedString, std::ops::Range<usize>);

pub struct ComposerView {
    session: Entity<SessionEntity>,
    /// Keeps the unsent text per Lobby, so a switch or a restart does not lose it.
    lobbies: Entity<Lobbies>,
    images: Entity<MediaImages>,
    input: Entity<TextareaState>,
    placeholder: String,
    /// Read at the last render, so a change event re-renders only when it flips.
    empty: bool,
    formats_at: Option<(Caret, Vec<Format>)>,
    parsing: Option<Task<()>>,
    last_typed: Option<Instant>,
    submitted: Vec<(u64, String)>,
    /// Drafts put back by failed sends and not sent since; their notification goes once all of them
    /// turn out delivered.
    restored: Vec<String>,
    restore_queue: Vec<String>,
    restore_task: Option<Task<()>>,
    drop_over: Option<(gpui_kit::Bounds<gpui_kit::Pixels>, Vec<PathBuf>)>,
    pastes: HashMap<u64, Task<()>>,
    next_paste: u64,
    picking: Option<Task<()>>,
    /// Failed drafts already notified (one failing again after a Retry is notified again).
    failed_drafts: HashSet<DraftId>,
    editing: Option<Editing>,
    /// The Member lit in the mention menu, as a place in the offered ones.
    mention_pick: usize,
    /// Where the `@` is of a mention menu Esc closed: it stays closed until the caret leaves it.
    mention_dismissed: Option<usize>,
    /// The mention menu was drawn at the last render, so a change event re-renders when it goes.
    mention_open: bool,
    _subscriptions: Vec<Subscription>,
}

/// The field holds one of this Member's sent Messages; what it held before waits in `stash`.
struct Editing {
    id: MessageId,
    stash: String,
}

impl EventEmitter<ComposerEvent> for ComposerView {}

impl ComposerView {
    pub fn new(
        session: Entity<SessionEntity>,
        lobbies: Entity<Lobbies>,
        images: Entity<MediaImages>,
        placeholder: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 6)
                .submit_on_enter(true)
                .placeholder(placeholder.clone())
                .context_menu(false)
        });
        let focus = input.read(cx).focus_handle(cx);
        let this = cx.weak_entity();
        let subscriptions = vec![
            // The frame shows the focus ring: re-render when it comes and goes.
            cx.on_focus_in(&focus, window, |_, _, cx| cx.notify()),
            cx.on_focus_out(&focus, window, |_, _, _, cx| cx.notify()),
            cx.observe(&session, |this, _, cx| this.on_session_changed(cx)),
            cx.intercept_keystrokes(move |ev, window, cx| {
                let _ = this.update(cx, |v, cx| {
                    if v.mention_key(&ev.keystroke, window, cx) {
                        cx.stop_propagation();
                    }
                });
            }),
            cx.subscribe_in(
                &input,
                window,
                |this, _, ev: &InputEvent, window, cx| match ev {
                    InputEvent::PressEnter { shift: false, .. } => this.enter(window, cx),
                    InputEvent::Change => this.changed(cx),
                    _ => {}
                },
            ),
        ];
        let mut this = Self {
            session,
            lobbies,
            images,
            input,
            placeholder,
            empty: true,
            formats_at: None,
            parsing: None,
            last_typed: None,
            submitted: Vec::new(),
            restored: Vec::new(),
            restore_queue: Vec::new(),
            restore_task: None,
            drop_over: None,
            pastes: HashMap::new(),
            next_paste: 0,
            picking: None,
            failed_drafts: HashSet::new(),
            editing: None,
            mention_pick: 0,
            mention_dismissed: None,
            mention_open: false,
            _subscriptions: subscriptions,
        };
        if let Some(text) = this.unsent(cx) {
            let end = text.len();
            this.set_text(text, end, window, cx);
        }
        this
    }

    fn unsent(&self, cx: &App) -> Option<String> {
        let origin = self.session.read(cx).origin()?;
        self.lobbies.read(cx).unsent(origin).map(str::to_owned)
    }

    pub fn value(&self, cx: &App) -> SharedString {
        self.input.read(cx).value()
    }

    pub fn is_editing(&self) -> bool {
        self.editing.is_some()
    }

    /// The field takes the Message's text until Enter saves it or Esc cancels; what was there
    /// comes back after. Not while a send is still on its way, whose text the field holds.
    pub fn start_edit(
        &mut self,
        id: MessageId,
        content: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let session = self.session.read(cx);
        self.submitted.retain(|(n, _)| !session.taken(*n));
        if !self.submitted.is_empty() {
            return;
        }
        let stash = match self.editing.take() {
            Some(editing) => editing.stash,
            None => self.value(cx).to_string(),
        };
        self.editing = Some(Editing { id, stash });
        let end = content.len();
        self.set_text(content, end, window, cx);
        self.focus(window, cx);
        cx.notify();
    }

    fn save_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let content = self.value(cx).to_string();
        if let Some(Editing { id, .. }) = self.editing {
            self.session.update(cx, |s, cx| {
                s.dispatch(Intent::EditMessage { id, content }, cx)
            });
        }
        self.leave_edit(window, cx);
    }

    /// The field gets back what it held before the edit.
    fn leave_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Editing { stash, .. }) = self.editing.take() {
            let end = stash.len();
            self.set_text(stash, end, window, cx);
            cx.notify();
        }
    }

    /// Up in an empty field edits this Member's newest Message.
    fn edit_last(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let last = self.session.read(cx).state().and_then(|s| {
            let id = s.last_mine()?;
            Some((id, s.chat[&id].content.clone()))
        });
        match last {
            Some((id, content)) => {
                self.start_edit(id, content, window, cx);
                true
            }
            None => false,
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |c, cx| c.focus(window, cx));
    }

    pub fn set_placeholder(&mut self, placeholder: String, window: &mut Window, cx: &mut App) {
        if placeholder != self.placeholder {
            self.placeholder = placeholder.clone();
            self.input
                .update(cx, |c, cx| c.set_placeholder(placeholder, window, cx));
        }
    }

    /// "Send again" has something to send: a draft still waiting, or text.
    pub fn can_resend(&self, cx: &App) -> bool {
        !self.restore_queue.is_empty() || !self.value(cx).trim().is_empty()
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        let value = self.value(cx);
        self.last_typed = Some(cx.background_executor().now());
        if !value.trim().is_empty() && self.editing.is_none() {
            self.session
                .update(cx, |s, cx| s.dispatch(Intent::Typing, cx));
        }
        self.remember_text(cx);
        self.mention_pick = 0;
        let at = {
            let c = self.input.read(cx);
            compose::mention_query(&value, c.cursor()).map(|q| q.range.start)
        };
        if at != self.mention_dismissed {
            self.mention_dismissed = None;
        }
        if value.trim().is_empty() != self.empty || self.mention_open || at.is_some() {
            cx.notify()
        }
    }

    /// The field's text goes to the Lobbies. The kit's `set_value` raises no change event, so
    /// typing and [`Self::set_text`] both call this. During an edit the field holds the Message,
    /// so the draft it set aside is what is kept.
    fn remember_text(&self, cx: &mut Context<Self>) {
        let Some(origin) = self.session.read(cx).origin().cloned() else {
            return;
        };
        let text = match &self.editing {
            Some(editing) => editing.stash.clone().into(),
            None => self.value(cx),
        };
        self.lobbies
            .update(cx, |l, cx| l.set_unsent(&origin, &text, cx));
    }

    /// The `@name` the caret is in and the Members it offers, unless Esc closed that menu.
    fn mention_offer(&self, cx: &App) -> Option<(compose::MentionQuery, Vec<MentionTarget>)> {
        let c = self.input.read(cx);
        let query = compose::mention_query(&c.value(), c.cursor())?;
        if self.mention_dismissed == Some(query.range.start) {
            return None;
        }
        let state = self.session.read(cx).state()?;
        let members = state.members.values().map(|m| &m.username);
        let matches = compose::mention_matches(&query.typed, members);
        (!matches.is_empty()).then_some((query, matches))
    }

    /// Tab, Enter, Esc and the arrows while the mention menu is open. `false`: not for the menu.
    /// The text field binds these keys as actions, which run before a key listener, so this is
    /// called from a keystroke interceptor.
    fn mention_key(
        &mut self,
        keystroke: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.input.read(cx).focus_handle(cx).is_focused(window) {
            return false;
        }
        let Some((query, matches)) = self.mention_offer(cx) else {
            return false;
        };
        let m = keystroke.modifiers;
        if m.control || m.alt || m.platform || m.shift {
            return false;
        }
        let n = matches.len();
        match keystroke.key.as_str() {
            "tab" | "enter" => self.complete_mention(window, cx),
            "escape" => {
                self.mention_dismissed = Some(query.range.start);
                cx.notify();
            }
            "down" => {
                self.mention_pick = (self.mention_pick + 1) % n;
                cx.notify();
            }
            "up" => {
                self.mention_pick = (self.mention_pick + n - 1) % n;
                cx.notify();
            }
            _ => return false,
        }
        true
    }

    fn complete_mention(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((query, matches)) = self.mention_offer(cx) else {
            return;
        };
        let pick = &matches[self.mention_pick.min(matches.len() - 1)];
        let edit = compose::complete_mention(&self.value(cx), &query, pick);
        self.last_typed = Some(cx.background_executor().now());
        self.mention_pick = 0;
        self.apply_edit(edit, window, cx);
    }

    fn on_session_changed(&mut self, cx: &mut Context<Self>) {
        let Some(s) = self.session.read(cx).state() else {
            return;
        };
        let failed: Vec<(DraftId, String)> = s
            .drafts
            .items()
            .iter()
            .filter_map(|d| match &d.state {
                DraftState::Failed { reason, .. } => Some((d.id, format!("{}: {reason}", d.name))),
                _ => None,
            })
            .collect();
        self.notify_failed_drafts(failed, cx);
        cx.notify();
    }

    fn notify_failed_drafts(&mut self, failed: Vec<(DraftId, String)>, cx: &mut Context<Self>) {
        let had = !self.failed_drafts.is_empty();
        self.failed_drafts
            .retain(|id| failed.iter().any(|(f, _)| f == id));
        for (id, message) in failed {
            if self.failed_drafts.insert(id) {
                let issue = Issue::new(IssueKind::UploadFailed, message);
                cx.emit(ComposerEvent::Notify(issue));
            }
        }
        if had && self.failed_drafts.is_empty() {
            cx.emit(ComposerEvent::Dismiss(IssueKind::UploadFailed));
        }
    }

    /// Asks the Session to send what was typed since the texts still on their way. Whether it
    /// may, and what a refusal says, are the Session's: it answers with
    /// [`UiEffect::Sent`](lobby_client::UiEffect::Sent) when it took the send (see
    /// [`Self::sent`]) and with an Issue when it did not, and the text stays.
    pub fn submit(&mut self, cx: &mut Context<Self>) {
        // An answered text still here was refused: it stays in the field, as typed.
        let session = self.session.read(cx);
        self.submitted.retain(|(n, _)| !session.taken(*n));
        let waiting: String = self.submitted.iter().map(|(_, d)| d.as_str()).collect();
        let value = self.value(cx);
        // The person edited a text still on its way: nothing goes until it is answered.
        let Some(draft) = value.strip_prefix(waiting.as_str()) else {
            return;
        };
        let draft = draft.to_owned();
        let n = self
            .session
            .update(cx, |s, cx| s.dispatch(Intent::Send(draft.clone()), cx));
        self.submitted.push((n, draft));
    }

    /// The Session took the send of `draft`: it is in the feed, so the composer lets go of that
    /// text and keeps what was typed after it.
    pub fn sent(&mut self, draft: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(i) = self.submitted.iter().position(|(_, d)| d == draft) {
            let at: usize = self.submitted[..i].iter().map(|(_, d)| d.len()).sum();
            self.submitted.remove(i);
            let (current, caret) = {
                let c = self.input.read(cx);
                (c.value().to_string(), c.cursor())
            };
            if current
                .get(at..)
                .is_some_and(|rest| rest.starts_with(draft))
            {
                let text = format!("{}{}", &current[..at], &current[at + draft.len()..]);
                let caret = if caret >= at + draft.len() {
                    caret - draft.len()
                } else {
                    caret.min(at)
                };
                self.set_text(text, caret, window, cx);
            }
        }
        // What was restored and has left the field went out with this Message; a draft still
        // waiting for a pause in typing, or put back after this text, did not, and its
        // notification stays.
        let current = self.value(cx);
        let queue = &self.restore_queue;
        self.restored
            .retain(|d| queue.contains(d) || restored_draft_range(&current, d).is_some());
        if self.restored.is_empty() {
            cx.emit(ComposerEvent::Dismiss(IssueKind::SendFailed));
            cx.emit(ComposerEvent::Dismiss(IssueKind::SendRefused));
        }
        cx.emit(ComposerEvent::Sent);
        cx.notify();
    }

    /// The notification's "Send again": a draft still waiting for the person to pause comes back
    /// now, and goes.
    pub fn send_again(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.last_typed = None;
        self.flush_restores(window, cx);
        self.submit(cx)
    }

    /// A send failed: its draft comes back into the composer.
    pub fn restore(&mut self, draft: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.restored.push(draft.to_owned());
        self.restore_queue.push(draft.to_owned());
        self.flush_restores(window, cx);
    }

    /// A send that failed turned out delivered: its draft leaves the composer if still unchanged.
    pub fn recovered(&mut self, draft: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(i) = self.restored.iter().position(|d| d == draft) else {
            return;
        };
        self.restored.remove(i);
        if let Some(i) = self.restore_queue.iter().position(|d| d == draft) {
            self.restore_queue.remove(i);
        }
        let current = self.value(cx);
        if let Some(text) = withdraw_draft(&current, draft) {
            let end = text.len();
            self.set_text(text, end, window, cx);
        }
        if self.restored.is_empty() {
            cx.emit(ComposerEvent::Dismiss(IssueKind::SendFailed));
        }
    }

    /// Puts failed drafts back in the composer, after the texts Enter sent that are not answered
    /// yet, where each one's `Sent` still finds its text. Into a composer empty besides them only
    /// once the person pauses ([`RESTORE_AFTER_PAUSE`]): a quick typist who already started the
    /// next Message gets the draft on its own line above it instead of their words glued onto it.
    fn flush_restores(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.restore_queue.is_empty() {
            return;
        }
        let (current, caret) = {
            let c = self.input.read(cx);
            (c.value().to_string(), c.cursor())
        };
        let waiting: String = self.submitted.iter().map(|(_, d)| d.as_str()).collect();
        let ahead = if current.starts_with(&waiting) {
            waiting.len()
        } else {
            0
        };
        let typed = &current[ahead..];
        let now = cx.background_executor().now();
        let quiet = self
            .last_typed
            .map_or(RESTORE_AFTER_PAUSE, |at| now.duration_since(at));
        if typed.trim().is_empty() && quiet < RESTORE_AFTER_PAUSE {
            let wait = cx.background_executor().timer(RESTORE_AFTER_PAUSE - quiet);
            self.restore_task = Some(cx.spawn_in(window, async move |this, cx| {
                wait.await;
                let _ = this.update_in(cx, |v, window, cx| v.flush_restores(window, cx));
            }));
            return;
        }
        self.restore_task = None;
        let draft = std::mem::take(&mut self.restore_queue).join("\n");
        let (text, at) = restore_draft(typed, caret.saturating_sub(ahead), &draft);
        self.set_text(
            format!("{}{text}", &current[..ahead]),
            ahead + at,
            window,
            cx,
        );
        cx.notify();
    }

    fn set_text(
        &mut self,
        text: String,
        caret: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.input.update(cx, |c, cx| {
            let caret = caret.min(text.len());
            c.set_value(text, window, cx);
            c.set_selected_range(caret..caret, cx);
        });
        self.remember_text(cx);
    }

    /// Enter in the composer: inside a list it continues the list (an empty item ends it),
    /// inside an open code fence it is a new line; otherwise it sends.
    pub fn enter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (text, caret) = {
            let c = self.input.read(cx);
            (c.value().to_string(), c.cursor())
        };
        match compose::on_enter(&text, caret) {
            Enter::Send if self.editing.is_some() => self.save_edit(window, cx),
            Enter::Send => self.submit(cx),
            Enter::Edit(edit) => {
                self.last_typed = Some(cx.background_executor().now());
                self.apply_edit(edit, window, cx);
            }
        }
    }

    pub fn format(&mut self, format: Format, window: &mut Window, cx: &mut Context<Self>) {
        let (text, selection) = {
            let c = self.input.read(cx);
            (c.value().to_string(), c.selected_range())
        };
        let edit = compose::apply(format, &text, selection);
        self.last_typed = Some(cx.background_executor().now());
        self.apply_edit(edit, window, cx);
        self.focus(window, cx);
    }

    /// A paste into the composer: a URL over selected words links them. `false`: paste as
    /// usual.
    pub fn paste_url(&mut self, pasted: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let (text, selection) = {
            let c = self.input.read(cx);
            (c.value().to_string(), c.selected_range())
        };
        match compose::paste_link(&text, selection, pasted) {
            Some(edit) => {
                self.last_typed = Some(cx.background_executor().now());
                self.apply_edit(edit, window, cx);
                true
            }
            None => false,
        }
    }

    fn apply_edit(&mut self, edit: compose::Edit, window: &mut Window, cx: &mut Context<Self>) {
        let compose::Edit { text, selection } = edit;
        self.input.update(cx, |c, cx| {
            c.set_value(text.clone(), window, cx);
            c.set_selected_range(selection, cx);
        });
        self.remember_text(cx);
        if !text.trim().is_empty() {
            self.session
                .update(cx, |s, cx| s.dispatch(Intent::Typing, cx));
        }
        cx.notify();
    }

    pub fn formats_on(&self, cx: &App) -> Vec<Format> {
        let c = self.input.read(cx);
        compose::active_formats(&c.value(), c.selected_range())
    }

    fn caret(&self, cx: &App) -> Caret {
        let input = self.input.read(cx);
        (input.value(), input.selected_range())
    }

    /// What the bar shows as on. A long draft takes a millisecond or more to parse, so the parse
    /// runs off the UI thread, one at a time, and the bar keeps what it showed until it is done.
    /// A parse the caret or the text moved away from is dropped, never shown: the render after
    /// it parses again.
    fn formats_at_caret(&mut self, cx: &mut Context<Self>) -> Vec<Format> {
        let caret = self.caret(cx);
        let shown = match &self.formats_at {
            Some((at, formats)) if *at == caret => return formats.clone(),
            Some((_, formats)) => formats.clone(),
            None => Vec::new(),
        };
        if self.parsing.is_none() {
            let (text, selection) = caret.clone();
            let parse = cx
                .background_executor()
                .spawn(async move { compose::active_formats(&text, selection) });
            self.parsing = Some(cx.spawn(async move |this, cx| {
                let formats = parse.await;
                let _ = this.update(cx, |v, cx| {
                    v.parsing = None;
                    if v.caret(cx) == caret {
                        v.formats_at = Some((caret, formats));
                    }
                    cx.notify();
                });
            }));
        }
        shown
    }

    pub fn attach_paths(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        for path in paths {
            let intent = match DraftSource::from_path(path) {
                Ok(source) => Intent::Attach(source),
                Err(e) => Intent::AttachFailed(e),
            };
            self.session.update(cx, |s, cx| s.dispatch(intent, cx));
        }
        cx.notify();
    }

    pub fn pick_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = crate::files::pick_files(cx);
        self.picking = Some(cx.spawn_in(window, async move |this, cx| {
            let picked = picked.await.ok().flatten();
            let _ = this.update(cx, |v, cx| {
                v.picking = None;
                if let Some(paths) = picked {
                    v.attach_paths(&paths, cx);
                }
            });
        }));
    }

    /// A paste that is not text: copied files attach like a drop; a copied picture becomes a
    /// PNG off the UI thread first. `false`: nothing here to attach.
    pub fn paste_files(
        &mut self,
        item: &ClipboardItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let entries = item.entries();
        if let Some(paths) = entries.iter().find_map(|e| match e {
            ClipboardEntry::ExternalPaths(p) => Some(p.paths().to_vec()),
            _ => None,
        }) {
            self.attach_paths(&paths, cx);
            return true;
        }
        let Some(image) = entries.iter().find_map(|e| match e {
            ClipboardEntry::Image(i) => Some(i.clone()),
            _ => None,
        }) else {
            return false;
        };
        let format = match image.format {
            ImageFormat::Png => PastedFormat::Png,
            ImageFormat::Bmp => PastedFormat::Bmp,
            ImageFormat::Jpeg => PastedFormat::Jpeg,
            ImageFormat::Gif => PastedFormat::Gif,
            ImageFormat::Webp => PastedFormat::Webp,
            _ => {
                let refused = Intent::AttachFailed(AttachError::Unreadable);
                self.session.update(cx, |s, cx| s.dispatch(refused, cx));
                return true;
            }
        };
        let n = self.next_paste;
        self.next_paste += 1;
        let encoded = cx
            .background_executor()
            .spawn(async move { lobby_client::paste_png(&image.bytes, format) });
        let task = cx.spawn_in(window, async move |this, cx| {
            let encoded = encoded.await;
            let _ = this.update(cx, |v, cx| {
                v.pastes.remove(&n);
                let intent = match encoded {
                    Ok(source) => Intent::Attach(source),
                    Err(e) => Intent::AttachFailed(e),
                };
                v.session.update(cx, |s, cx| s.dispatch(intent, cx));
            });
        });
        self.pastes.insert(n, task);
        true
    }

    /// Why Send is disabled, as the Session decides it; `None` when it works.
    pub fn send_block(&self, cx: &App) -> Option<SendBlock> {
        self.send_block_of(&self.value(cx), cx)
    }

    fn send_block_of(&self, text: &str, cx: &App) -> Option<SendBlock> {
        match self.session.read(cx).state() {
            Some(state) => state.send_block(text),
            None => Some(SendBlock::Empty),
        }
    }

    /// The fill behind each `@name` the field holds that a sent Message would show as a chip.
    /// Laid over the field after it: the field's layout is set in its paint.
    fn mention_chips(&self, value: &str, cx: &App) -> Option<impl IntoElement> {
        if !value.contains('@') {
            return None;
        }
        let names = self.session.read(cx).state()?.mention_names();
        let input = self.input.clone();
        Some(
            canvas(
                |_, _, _| {},
                move |bounds, _, window, cx| {
                    mention_chip::paint_in_field(&bounds, &input, &names, window, cx);
                },
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full(),
        )
    }

    /// The field, then the attach button, the formatting bar and Send. In a narrow window the bar
    /// folds into one "Aa" menu beside Send so the feed keeps its room at 480×400. `value` is the
    /// field's text, copied out of it once per frame.
    fn controls(
        &mut self,
        narrow: bool,
        value: &SharedString,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let blocked = self.send_block_of(value, cx);
        let editing = self.editing.is_some();
        let can_send = if editing {
            !value.trim().is_empty()
        } else {
            blocked.is_none()
        };
        let text = div()
            .id("composer-text")
            .debug_selector(|| "composer-text".into())
            .flex_1()
            .w_full()
            .min_w_0()
            .child(
                ui::Field::multi_line(&self.input)
                    .appearance(false)
                    .on_paste({
                        let composer = cx.weak_entity();
                        move |item, window, cx| {
                            let text = item
                                .entries()
                                .iter()
                                .any(|e| matches!(e, ClipboardEntry::String(_)));
                            composer
                                .update(cx, |v, cx| match item.text().filter(|_| text) {
                                    Some(text) => v.paste_url(&text, window, cx),
                                    None => v.paste_files(item, window, cx),
                                })
                                .unwrap_or(false)
                        }
                    }),
            )
            .children(self.mention_chips(value, cx))
            .test_support()
            .context_menu(input_menu(self.input.clone()));
        let corner = device_in(SURFACE_INNER_RADIUS, cx);
        let attach = ui::Button::new("attach-button")
            .accessibility_label("Attach files")
            .icon(IconName::Paperclip)
            .size(ui::Size::Inline)
            .rounded(corner)
            .tooltip("Attach files")
            .on_click(cx.listener(|this, _, window, cx| this.pick_files(window, cx)));
        let bar = FormatBar::new(forward(cx, |v: &mut Self, format, window, cx| {
            v.format(format, window, cx)
        }))
        .compact(narrow)
        .active(self.formats_at_caret(cx));
        let send = ui::Button::new("send")
            .accessibility_label("Send")
            .debug_selector(|| "send".into())
            // The plane's weight is in its wide tail, so its box sits a little right of centre.
            .icon(
                ui::Icon::new(IconName::SendHorizontal)
                    .relative()
                    .left(u(SEND_NUDGE)),
            )
            .size(ui::Size::Inline)
            .rounded(corner)
            .when(can_send, |b| b.variant(ui::Variant::Primary))
            .disabled(!can_send)
            .tooltip(match blocked {
                _ if editing => SharedString::from("Save (Enter)"),
                Some(SendBlock::Uploading) => "Waiting for uploads".into(),
                Some(SendBlock::Failed) => "Retry or remove the failed files".into(),
                Some(SendBlock::TooMany) => format!(
                    "Up to {} files go with one Message",
                    lobby_protocol::limits::ATTACHMENTS_MAX
                )
                .into(),
                Some(SendBlock::Empty) | None => "Send (Enter)".into(),
            })
            .on_click(cx.listener(|this, _, window, cx| {
                if this.editing.is_some() {
                    this.save_edit(window, cx)
                } else {
                    this.submit(cx)
                }
            }));
        if narrow {
            div()
                .w_full()
                .flex()
                .items_end()
                .gap_1()
                .py(u(NARROW_PAD_Y))
                .child(text)
                .child(
                    div()
                        .h(u(NARROW_ROW_H))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .gap_0p5()
                        .child(attach)
                        .child(bar)
                        .child(send),
                )
        } else {
            div()
                .w_full()
                .flex()
                .flex_col()
                .pt_1()
                .pb(u(WIDE_PAD_BOTTOM))
                .child(text)
                .child(
                    div()
                        .w_full()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(attach)
                        .child(div().flex_1().min_w_0().child(bar))
                        .child(send),
                )
        }
    }
}

/// The field's right-click menu, the themed one rather than the OS's. Each item acts through the
/// field's own actions, so it does what the keys do.
fn input_menu(
    input: Entity<TextareaState>,
) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
    move |menu, window, cx| {
        let menu_focus = menu.focus_handle(cx);
        input.update(cx, |state, cx| {
            state.set_selection_focus(Some(menu_focus), cx)
        });
        let state = input.read(cx);
        let selected = !state.selected_range().is_empty();
        let has_text = !state.value().is_empty();
        let mut menu = menu_frame(menu, window).action_context(state.focus_handle(cx));
        if selected {
            menu = menu
                .item(menu_item("menu-cut", copy::CUT).action(Box::new(Cut)))
                .item(menu_item("menu-copy", copy::COPY).action(Box::new(input::Copy)));
        }
        menu = menu.item(menu_item("menu-paste", copy::PASTE).action(Box::new(Paste)));
        if has_text {
            menu = menu
                .item(menu_item("menu-select-all", copy::SELECT_ALL).action(Box::new(SelectAll)));
        }
        menu
    }
}

impl Render for ComposerView {
    /// One framed field, so nothing moves while the text grows. The frame takes the focus ring.
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let narrow = design_viewport(window).0 < NARROW_BELOW;
        let value = self.value(cx);
        self.empty = value.trim().is_empty();
        let drafts: Vec<lobby_client::DraftAttachment> = self
            .session
            .read(cx)
            .state()
            .map(|s| s.drafts.items().to_vec())
            .unwrap_or_default();
        let focused = self
            .input
            .read(cx)
            .focus_handle(cx)
            .contains_focused(window, cx);
        let theme = cx.theme();
        // The composer is lifted like the member list: the same surface.
        let (border, background, ring) = (theme.border, theme.sidebar, theme.ring);
        let muted = theme.muted_foreground;
        // The attachments waiting to go are not part of the edit: they show again after it.
        let hint = self.editing.is_some().then(|| {
            div()
                .id("composer-editing")
                .debug_selector(|| "composer-editing".into())
                .pt(u(6.))
                .text_xs()
                .text_color(muted)
                .truncate()
                .child(copy::EDITING_MESSAGE)
                .test_support()
        });
        let strip = (!drafts.is_empty() && hint.is_none()).then(|| {
            let retry = forward(cx, |v: &mut Self, id, _, cx| {
                v.session
                    .update(cx, |s, cx| s.dispatch(Intent::RetryAttachment(id), cx));
            });
            let remove = forward(cx, |v: &mut Self, id, _, cx| {
                v.session
                    .update(cx, |s, cx| s.dispatch(Intent::RemoveAttachment(id), cx));
            });
            draft_strip(&drafts, &self.images, background, retry, remove, window, cx)
        });
        let controls = self.controls(narrow, &value, cx);
        let offer = self.mention_offer(cx);
        self.mention_open = offer.is_some();
        let menu = offer.map(|(_, matches)| {
            let pick = forward(cx, |v: &mut Self, i: usize, window, cx| {
                v.mention_pick = i;
                v.complete_mention(window, cx);
            });
            div()
                .absolute()
                .bottom_full()
                .left_0()
                .mb(u(MENTION_MENU_GAP))
                .child(mention_menu(
                    &matches,
                    self.mention_pick.min(matches.len() - 1),
                    pick,
                    cx,
                ))
        });
        div()
            .id("composer")
            .debug_selector(|| "composer".into())
            .relative()
            .w_full()
            .flex()
            .px(u(SURFACE_PAD))
            .rounded(u(SURFACE_RADIUS))
            .border_1()
            .border_color(if focused { ring } else { border })
            .bg(background)
            .when(focused, |d| d.shadow_sm())
            .cursor_text()
            // Ctrl+B, Ctrl+I, … (no keybinding of the text field uses them on Windows).
            .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                if let Some(format) = format_for(ev) {
                    this.format(format, window, cx);
                    cx.stop_propagation();
                    return;
                }
                if ev.keystroke.key == "escape" && this.editing.is_some() {
                    this.leave_edit(window, cx);
                    cx.stop_propagation();
                }
            }))
            // Up is the field's MoveUp action, which a key listener never sees.
            .capture_action(cx.listener(|this, _: &input::MoveUp, window, cx| {
                let empty = this.editing.is_none() && this.value(cx).is_empty();
                if !(empty && this.edit_last(window, cx)) {
                    cx.propagate();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.focus(window, cx)),
            )
            // Files dragged from Explorer: the frame lights up, and a drop attaches them. GPUI
            // reads hover as false after a keystroke and a file drag does not change that, so
            // `on_drop` misses a drop made after typing: the drag is tracked by position, and
            // that case is the mouse-up "outside".
            .when(self.drop_over.is_some(), |f| {
                f.border_color(ring).bg(ring.opacity(0.06))
            })
            .on_drag_move(
                cx.listener(|this, e: &DragMoveEvent<ExternalPaths>, _, cx| {
                    let over = e
                        .bounds
                        .contains(&e.event.position)
                        .then(|| (e.bounds, e.drag(cx).paths().to_vec()));
                    if over.is_some() != this.drop_over.is_some() {
                        cx.notify();
                    }
                    this.drop_over = over;
                }),
            )
            .on_file_drop_exit(cx.listener(|this, _, _, cx| {
                if this.drop_over.take().is_some() {
                    cx.notify();
                }
            }))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.drop_over = None;
                this.attach_paths(paths.paths(), cx);
            }))
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, e: &gpui_kit::MouseUpEvent, window, cx| {
                    let Some((bounds, paths)) = this.drop_over.take() else {
                        return;
                    };
                    cx.notify();
                    if bounds.contains(&e.position) && cx.stop_active_drag(window) {
                        this.attach_paths(&paths, cx);
                    }
                }),
            )
            .flex_col()
            .children(menu)
            .children(hint)
            .children(strip)
            .child(controls)
            .test_support()
    }
}
