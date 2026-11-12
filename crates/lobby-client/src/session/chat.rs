//! Messages: sending, the failed-send memory, history, drafts' uploads and typing.

use std::time::{Duration, Instant};

use lobby_protocol::{
    BlobId, ClientFrame, ClientMsg, ErrorBody, ErrorCode, Message, MessageChanges, MessageDelete,
    MessageEdit, MessageId, MessagePage, MessageSend, Ready, ReadyHistory, Rid, UserId, content,
    limits,
};
use tracing::{debug, info, warn};

use super::io::Op;
use super::{Command, PendingCommands, Session, UiEffect, UiEffects};
use crate::{
    ApiError, ConnError, Connection, Phase,
    api::Page,
    copy,
    drafts::{AttachError, DraftId, DraftSource, SendBlock, blob_ids},
    issues::{Issue, IssueKind},
    state::{ChangeSync, History, Older, PendingMessage, Shared, Typist},
};

/// How long a send that failed without an answer (deadline or disconnect) is remembered: the
/// server may have stored it after all.
pub const FAILED_NONCE_MEMORY: Duration = Duration::from_secs(60);

/// The page that follows READY's, which only its answer may fill in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Following {
    /// The newest page, for a READY whose own page was read empty.
    Latest,
    /// A backfill after a reconnect, in pages up to Message `to` (`READY.lastMessageSeq`).
    After { after: MessageId, to: u64 },
}

impl Following {
    fn page(self) -> Page {
        match self {
            Following::Latest => Page::Latest,
            Following::After { after, .. } => Page::After(after),
        }
    }
}

/// Why the Session refused a send itself: the words of its Issue.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(super) enum SendError {
    /// Nothing is said: the Send button is disabled then.
    #[error("Send is not allowed: {0:?}")]
    Blocked(SendBlock),
    #[error("{}", copy::MESSAGE_TOO_LONG)]
    TooLong,
    #[error("{}", copy::MESSAGE_TOO_NESTED)]
    TooNested,
    #[error("{}", copy::SENDING_TOO_FAST)]
    TooFast,
}

/// The one answer to whether a refused send offers "Send again", whether this device or the
/// Lobby refused it: not for a Message refused for what it is, which the same text would be again.
fn send_issue_kind(code: ErrorCode) -> IssueKind {
    match code {
        ErrorCode::MessageTooLong
        | ErrorCode::MessageTooNested
        | ErrorCode::InvalidRequest
        | ErrorCode::AttachmentInvalid
        | ErrorCode::Forbidden => IssueKind::SendRefused,
        _ => IssueKind::SendFailed,
    }
}

#[derive(Debug)]
struct FailedSend {
    nonce: String,
    draft: String,
    attachments: Vec<BlobId>,
    until: Instant,
}

#[derive(Debug, Default)]
pub(super) struct FailedSends(Vec<FailedSend>);

impl FailedSends {
    fn insert(&mut self, nonce: String, draft: String, attachments: Vec<BlobId>, now: Instant) {
        self.prune(now);
        self.0.push(FailedSend {
            nonce,
            draft,
            attachments,
            until: now + FAILED_NONCE_MEMORY,
        });
    }

    fn prune(&mut self, now: Instant) {
        self.0.retain(|f| f.until > now);
    }

    fn take_by(&mut self, now: Instant, is: impl Fn(&FailedSend) -> bool) -> Option<FailedSend> {
        self.prune(now);
        let i = self.0.iter().position(is)?;
        Some(self.0.remove(i))
    }
}

/// A queued send the connection could not take (its queue was full) is tried again this soon.
const QUEUE_FULL_RETRY: Duration = Duration::from_millis(250);

/// A send the throttle refused (more sends than its burst, or another device of this Member sent
/// just before) goes again after the Lobby's `retryAfterMs`, but never later than this.
pub(super) const THROTTLE_RETRY_MAX: Duration = Duration::from_secs(2);

fn random_nonce() -> String {
    let bytes: [u8; 16] = rand::random();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl Session {
    pub(super) fn attach(&mut self, source: DraftSource) {
        let max = self.upload_max();
        match self.state.drafts.add(source, max) {
            Ok(id) => {
                info!(draft = %id, "attached");
                self.pump_uploads();
            }
            Err(e) => self.attach_refused(&e),
        }
    }

    /// What a refusal to attach shows, whether the Session or the pick itself (a folder, an
    /// unreadable file, a clipboard picture that did not decode) refused.
    pub(super) fn attach_refused(&mut self, e: &AttachError) {
        warn!(error = %e, "attachment refused");
        let kind = match e {
            AttachError::TooLarge => IssueKind::PayloadTooLarge,
            AttachError::TooMany | AttachError::NotAFile | AttachError::Unreadable => {
                IssueKind::AttachRefused
            }
        };
        self.raise(kind, e.to_string());
    }

    pub(super) fn retry_attachment(&mut self, id: DraftId) {
        if self.state.drafts.retry(id) {
            self.pump_uploads();
        }
    }

    pub(super) fn remove_attachment(&mut self, id: DraftId) {
        if let Some(key) = self.state.drafts.remove(id) {
            self.io.cancel_upload(key.draft);
        }
        self.pump_uploads();
    }

    pub(super) fn pump_uploads(&mut self) {
        for (key, source) in self.state.drafts.start_next() {
            self.request(Op::Upload { key, source });
        }
    }

    pub(super) fn on_history(
        &mut self,
        page: Page,
        result: Result<MessagePage, ApiError>,
        now: Instant,
    ) -> UiEffects {
        #[cfg(feature = "test-support")]
        {
            self.history_pages += 1;
        }
        // An older page answers the Load Older still asked for, from the oldest Message held.
        // The feed may have started over meanwhile (a Lobby reset, or too much missed): the
        // page belongs to the old feed, and would leave a hole under the History row.
        let following = match page {
            Page::Before(before) => {
                if self.state.older != Older::Loading || self.state.oldest_message() != Some(before)
                {
                    debug!(
                        before = before.get(),
                        "an older page for a feed that started over"
                    );
                    return UiEffects::default();
                }
                None
            }
            Page::After(_) | Page::Latest => {
                let Some(asked) = self.following.take_if(|f| f.page() == page) else {
                    debug!(?page, "a page asked under an earlier READY");
                    return UiEffects::default();
                };
                Some(asked)
            }
        };
        let p = match result {
            Ok(p) => p,
            Err(e) => {
                warn!(?page, error = %e, "history failed");
                return match following {
                    // The button is still there to try again; the notification says it failed.
                    None => {
                        self.state.older = Older::Available;
                        UiEffects(vec![UiEffect::Issue(Issue::new(
                            IssueKind::OlderFailed,
                            copy::OLDER_FAILED,
                        ))])
                    }
                    Some(_) => {
                        self.state.history = History::Failed(copy::HISTORY_FAILED.to_owned());
                        UiEffects::default()
                    }
                };
            }
        };
        let settled = match following {
            None => self.state.merge_older(&p),
            // The newest page, so nothing is missing after it, and it says whether older exist.
            Some(Following::Latest) => {
                self.state.history = History::Loaded;
                self.state.merge_older(&p)
            }
            Some(Following::After { to, .. }) => {
                let settled = self.state.merge_page(&p);
                // The next page from the last one this page held: live Messages above
                // `lastMessageSeq` are in the chat already and must not be skipped to.
                match p.messages.last().map(|m| m.id) {
                    Some(last) if p.has_more && (last.get() as u64) < to => {
                        debug!(after = last.get(), to, "backfill continues");
                        self.follow(Following::After { after: last, to });
                    }
                    _ => {
                        info!("backfill done");
                        self.state.history = History::Loaded;
                    }
                }
                settled
            }
        };
        for nonce in &settled {
            self.pending.remove(nonce);
        }
        debug!(?page, count = p.messages.len(), "history page");
        self.recovered(&p.messages, now)
    }

    /// READY's history goes into the feed. What follows its page (more than a page was missed,
    /// or Messages were committed while READY was made) is fetched in pages of 100 up to
    /// `lastMessageSeq`, and the edits and deletions missed are caught up on.
    pub(super) fn on_ready_history(&mut self, ready: &Ready, now: Instant) -> UiEffects {
        for nonce in self.state.merge_ready(&ready.history) {
            self.pending.remove(&nonce);
        }
        self.catch_up_changes(ready);
        let to = ready.last_message_seq;
        self.following = None;
        match self.state.newest_message() {
            Some(after) if (after.get() as u64) < to => {
                info!(after = after.get(), to, "backfill");
                self.follow(Following::After { after, to });
            }
            // The page was read empty, and Messages were committed before the registration.
            None if to > 0 => {
                info!(to, "the empty READY page missed Messages: the newest page");
                self.follow(Following::Latest);
            }
            _ => self.state.history = History::Loaded,
        }
        let (ReadyHistory::Latest(page) | ReadyHistory::After(page)) = &ready.history;
        debug!(count = page.messages.len(), "READY history");
        self.recovered(&page.messages, now)
    }

    /// The edits and deletions up to READY's `lastChangeRev`: since the revision the feed is
    /// synced to, or since `historyRev` for a page that starts the feed over, which shows the ones
    /// before. Past [`limits::CHANGES_MAX`] the feed starts over, and the newest page follows
    /// READY.
    fn catch_up_changes(&mut self, ready: &Ready) {
        let last_rev = ready.last_change_rev;
        let synced = match ready.history {
            ReadyHistory::Latest(_) => ready.history_rev,
            ReadyHistory::After(_) => self.state.changes.synced(),
        };
        let missed = last_rev.saturating_sub(synced);
        if missed > limits::CHANGES_MAX {
            info!(
                missed,
                "too many edits and deletions missed: the feed starts over"
            );
            self.state.chat = Shared::default();
            self.state.older = Older::Unknown;
            self.state.changes.set_sync(ChangeSync::Synced(last_rev));
        } else if missed > 0 {
            info!(
                after = synced,
                to = last_rev,
                "catching up on edits and deletions"
            );
            self.state.changes.set_sync(ChangeSync::CatchingUp {
                from: synced,
                to: last_rev,
            });
            self.request(Op::Changes {
                after: synced,
                to: last_rev,
            });
        } else {
            self.state
                .changes
                .set_sync(ChangeSync::Synced(synced.max(last_rev)));
        }
    }

    /// The edits and deletions since READY's `lastChangeRev` came live, and these are the ones
    /// before it.
    pub(super) fn on_changes(&mut self, to: u64, result: Result<MessageChanges, ApiError>) {
        match result {
            Ok(changes) => {
                debug!(
                    to,
                    updated = changes.updated.len(),
                    removed = changes.removed.len(),
                    "caught up on edits and deletions"
                );
                self.state.merge_changes(&changes);
                self.state.changes.caught_up(to, true);
            }
            Err(e) => {
                warn!(to, error = %e, "edits and deletions not caught up: again at the next READY");
                self.state.changes.caught_up(to, false);
            }
        }
    }

    fn follow(&mut self, following: Following) {
        self.following = Some(following);
        self.state.history = History::Loading;
        self.request(Op::Messages {
            page: following.page(),
            limit: limits::HISTORY_MAX,
        });
    }

    /// How many pages of history came back, the dropped ones included.
    #[cfg(feature = "test-support")]
    pub fn history_pages(&self) -> u64 {
        self.history_pages
    }

    /// The next connect's IDENTIFY says what the feed holds.
    pub(super) fn hold_history(&self) {
        self.conn.set_held(self.state.held_history());
    }

    /// The `ACK` of a send whose Message was stored and deleted since: a resend of a delivery
    /// whose answer was lost. It settles as deleted.
    pub(super) fn send_acked(&mut self, nonce: &str) {
        if self.state.take_pending(nonce).is_some() {
            info!(nonce, "a resent Message was deleted meanwhile");
        }
    }

    pub(super) fn load_older(&mut self) {
        if self.state.older != Older::Available {
            return;
        }
        let Some(oldest) = self.state.oldest_message() else {
            return;
        };
        debug!(before = oldest.get(), "loading older Messages");
        self.state.older = Older::Loading;
        self.request(Op::Messages {
            page: Page::Before(oldest),
            limit: limits::HISTORY_DEFAULT,
        });
    }

    pub(super) fn typing(&mut self, now: Instant) {
        if self.state.phase != Phase::Connected
            || self
                .typing_sent
                .is_some_and(|at| now.duration_since(at) < limits::TYPING_SEND_INTERVAL)
        {
            return;
        }
        if self.conn.send(&ClientFrame::new(ClientMsg::Typing)).is_ok() {
            self.typing_sent = Some(now);
        }
    }

    pub(super) fn recovered(&mut self, messages: &[Message], now: Instant) -> UiEffects {
        let mut fx = UiEffects::default();
        let Some(me) = self.state.me.clone() else {
            return fx;
        };
        for m in messages.iter().filter(|m| m.author.id == me) {
            if let Some(f) = self.failed.take_by(now, |f| f.nonce == m.nonce) {
                info!(id = m.id.get(), "a send reported as failed was delivered");
                // Its restored attachments went with it after all.
                self.state.drafts.withdraw(&m.attachments);
                fx.0.push(UiEffect::SendRecovered { draft: f.draft });
            }
        }
        fx
    }

    /// No answer is not a refusal: the server may have stored the Message. Its nonce is kept to
    /// recognize it, and a re-send of the same text reuses it, so the server replays the stored
    /// Message instead of storing a second one.
    pub(super) fn failed_without_answer(
        &mut self,
        p: PendingMessage,
        reason: &str,
        now: Instant,
    ) -> UiEffect {
        self.failed
            .insert(p.nonce, p.draft.clone(), blob_ids(&p.attachments), now);
        self.state.drafts.restore(p.attachments, false);
        UiEffect::SendFailed {
            draft: p.draft,
            issue: Issue::new(IssueKind::SendFailed, reason.to_owned()),
        }
    }

    /// A send the Session takes shows in the feed at once and clears the composer. One it
    /// refuses is its Issue, of the kind the Lobby's refusal for the same reason would have.
    /// Nothing is said for a blank one, or while an upload runs or failed: Send is disabled then.
    pub(super) fn send(&mut self, draft: &str, now: Instant) {
        let Err(e) = self.send_message(draft, now) else {
            let draft = draft.to_owned();
            self.queued.push(UiEffect::Sent { draft });
            return;
        };
        warn!(error = %e, "send refused");
        let code = match e {
            SendError::Blocked(_) => return,
            SendError::TooLong => ErrorCode::MessageTooLong,
            SendError::TooNested => ErrorCode::MessageTooNested,
            SendError::TooFast => ErrorCode::RateLimited,
        };
        self.raise(send_issue_kind(code), e.to_string());
    }

    /// Optimistic send: the Message shows as pending at once and is reconciled by author and
    /// nonce. While not connected it waits in the feed (queued) and goes out when the connection
    /// is back. The Ready drafts go with it (blank text is fine then).
    fn send_message(&mut self, draft: &str, now: Instant) -> Result<(), SendError> {
        if let Some(block) = self.state.send_block(draft) {
            return Err(SendError::Blocked(block));
        }
        let ids = self.state.drafts.ready_ids();
        // Leading spaces of the first line can be indentation that means something.
        let content = draft.trim_end().trim_start_matches(['\n', '\r']).to_owned();
        if content.chars().count() > limits::MESSAGE_MAX_CHARS {
            return Err(SendError::TooLong);
        }
        if content::parse(&content).is_err() {
            return Err(SendError::TooNested);
        }
        let retried = self
            .failed
            .take_by(now, |f| f.draft == draft && f.attachments == ids);
        let nonce = retried
            .as_ref()
            .map_or_else(random_nonce, |f| f.nonce.clone());
        let mut pending = PendingMessage {
            nonce,
            draft: draft.to_owned(),
            content,
            queued: true,
            throttled: false,
            at: lobby_protocol::UnixMs::now(),
            attachments: self.state.drafts.take_ready(),
        };
        // The next keystroke says "typing" again at once: the Message just sent cleared it (the
        // Lobby's TYPING budget starts afresh with an accepted send too).
        self.typing_sent = None;
        // One send is on its way at a time, so the Messages arrive in the order they were sent
        // even when the Lobby's throttle refuses one. The next ("lol" after "ok⏎") waits in the
        // feed for the answer to the one before, a round trip.
        if self.state.phase == Phase::Connected && self.state.pending.is_empty() {
            match Self::send_pending(&self.conn, &mut self.pending, &pending, now) {
                Ok(()) => pending.queued = false,
                Err(ConnError::NotConnected) => {}
                Err(ConnError::QueueFull) => {
                    if let Some(f) = retried {
                        self.failed.0.push(f);
                    }
                    self.state.drafts.restore(pending.attachments, false);
                    return Err(SendError::TooFast);
                }
            }
        }
        if pending.queued {
            debug!(nonce = %pending.nonce, "send queued");
        }
        self.state.pending.push(pending);
        Ok(())
    }

    /// Not optimistic: the feed shows the edit when the Lobby's `MESSAGE_UPDATE` comes. An edit
    /// that changes nothing sends nothing.
    pub(super) fn edit_message(&mut self, id: MessageId, content: String, now: Instant) {
        let content = content
            .trim_end()
            .trim_start_matches(['\n', '\r'])
            .to_owned();
        if !self.state.may_change(id) || self.state.chat[&id].content == content {
            return;
        }
        let refused = if content.trim().is_empty() {
            Some(copy::CHANGE_EMPTY)
        } else if content.chars().count() > limits::MESSAGE_MAX_CHARS {
            Some(copy::MESSAGE_TOO_LONG)
        } else if content::parse(&content).is_err() {
            Some(copy::MESSAGE_TOO_NESTED)
        } else {
            None
        };
        if let Some(why) = refused {
            self.raise(IssueKind::ChangeFailed, why);
            return;
        }
        let edit = ClientMsg::MessageEdit(MessageEdit {
            message_id: id,
            content,
        });
        self.change(edit, now);
    }

    pub(super) fn delete_message(&mut self, id: MessageId, now: Instant) {
        if self.state.may_change(id) {
            let delete = ClientMsg::MessageDelete(MessageDelete { message_id: id });
            self.change(delete, now);
        }
    }

    fn change(&mut self, msg: ClientMsg, now: Instant) {
        if self.command(msg, Command::MessageChange, now).is_err() {
            self.raise(IssueKind::ChangeFailed, copy::NOT_CONNECTED);
        }
    }

    fn send_pending(
        conn: &Connection,
        pending: &mut PendingCommands,
        p: &PendingMessage,
        now: Instant,
    ) -> Result<(), ConnError> {
        let rid: Rid = p.nonce.parse().expect("a nonce is a valid rid");
        let frame = ClientFrame::with_rid(
            rid,
            ClientMsg::MessageSend(MessageSend {
                content: p.content.clone(),
                attachment_ids: blob_ids(&p.attachments),
            }),
        );
        conn.send(&frame)?;
        pending.insert(&p.nonce, Command::MessageSend, now);
        Ok(())
    }

    /// Holds the queued sends until `at`, the Lobby's `retryAfterMs` after a throttle refusal.
    pub(super) fn queue_at(&mut self, at: Instant) {
        self.queued_next_at = Some(self.queued_next_at.map_or(at, |q| q.min(at)));
    }

    /// The oldest queued send goes out once the one before it was answered.
    pub(super) fn send_queued(&mut self, now: Instant) {
        if self.queued_next_at.is_some_and(|at| at > now) || self.state.phase != Phase::Connected {
            return;
        }
        self.queued_next_at = None;
        if self.state.pending.iter().any(|p| !p.queued) {
            return;
        }
        let Some(index) = self.state.pending.iter().position(|p| p.queued) else {
            return;
        };
        let p = &self.state.pending[index];
        if Self::send_pending(&self.conn, &mut self.pending, p, now).is_err() {
            self.queued_next_at = Some(now + QUEUE_FULL_RETRY);
            return;
        }
        info!(nonce = %p.nonce, "queued send went out");
        self.state.pending[index].queued = false;
    }

    pub(super) fn typing_seen(&mut self, user: UserId, now: Instant) {
        let since = self.state.typing.get(&user).map_or(now, |t| t.since);
        self.state.typing.insert(
            user,
            Typist {
                since,
                until: now + limits::TYPING_EXPIRY,
            },
        );
    }

    pub(super) fn send_refused(&mut self, rid: &str, e: &ErrorBody, now: Instant) -> UiEffects {
        let mut fx = UiEffects::default();
        if let (ErrorCode::RateLimited, Some(retry_after_ms), Some(p)) = (
            e.code,
            e.retry_after_ms,
            self.state
                .pending
                .iter_mut()
                .find(|p| p.nonce == rid && !p.throttled),
        ) {
            debug!(nonce = %rid, "send throttled: sending again");
            p.throttled = true;
            p.queued = true;
            let wait = Duration::from_millis(retry_after_ms);
            self.queue_at(now + wait.min(THROTTLE_RETRY_MAX));
            return fx;
        }
        if let Some(p) = self.state.take_pending(rid) {
            self.state
                .drafts
                .restore(p.attachments, e.code == ErrorCode::AttachmentInvalid);
            let issue = Issue::new(
                send_issue_kind(e.code),
                copy::server_error(e.code, &e.message),
            );
            fx.0.push(UiEffect::SendFailed {
                draft: p.draft,
                issue,
            });
        }
        fx
    }
}
