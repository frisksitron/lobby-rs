//! `LobbyState`: the pure per-Lobby state, folded from server events. No IO
//! and no clock; `Session` owns it and decides the effects.

use std::{
    collections::{BTreeMap, BTreeSet},
    ops::{Deref, DerefMut},
    sync::Arc,
    time::Instant,
};

use lobby_protocol::{
    AccessRequest, AccessRequestId, DrawingEpoch, Email, HeldHistory, LobbyInfo, Member, Message,
    MessageChanges, MessageId, MessagePage, Presence, ReadyHistory, ScreenShare, ServerMsg,
    ShareId, UnixMs, UserId, Username, VoiceSessionId, VoiceState,
};

use crate::{
    Phase,
    drafts::{DraftAttachment, Drafts, SendBlock},
    ink::InkBoard,
    media::{ScreenSource, VoiceStats},
    message_text::MentionNames,
};

/// A part of the state that the snapshots the Session published share with it, copied on write:
/// a clone is a count, an untouched one compares by pointer, and the first change after a
/// publish copies it once.
#[derive(Debug, Default, Clone, Eq)]
pub struct Shared<T>(Arc<T>);

impl<T: PartialEq> PartialEq for Shared<T> {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || *self.0 == *other.0
    }
}

impl<T> Deref for Shared<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T: Clone> DerefMut for Shared<T> {
    fn deref_mut(&mut self) -> &mut T {
        Arc::make_mut(&mut self.0)
    }
}

/// What only the Lobby's owner does to another Member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerAction {
    MakeOwner,
    Remove,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingMessage {
    pub nonce: String,
    pub draft: String,
    pub content: String,
    /// Nothing went out yet: written while not connected (it goes when the connection is back),
    /// or waiting for the answer to the send before it.
    pub queued: bool,
    /// The Lobby refused it once for the send throttle and it was sent again; a second refusal
    /// is a failure.
    pub throttled: bool,
    pub at: UnixMs,
    /// What goes with it: the Ready drafts taken at Send. The row shows them from the
    /// first frame, so the confirmed Message replaces it at the same height.
    pub attachments: Vec<DraftAttachment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum History {
    NotLoaded,
    Loading,
    Loaded,
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Older {
    #[default]
    Unknown,
    Available,
    Loading,
    Beginning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Typist {
    pub since: Instant,
    pub until: Instant,
}

/// This client's side of Voice. Whether the Member is still connecting or In Voice, or muted, is
/// the server's snapshot (`me_voice`), shown the same on every client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VoicePhase {
    #[default]
    Idle,
    Joining,
    InVoice {
        vsid: VoiceSessionId,
        /// The media path broke and ICE restarts are under way; Voice goes on meanwhile.
        reconnecting: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VoiceUi {
    pub phase: VoicePhase,
    pub muted: bool,
    /// Deafened plays nothing and implies muted.
    pub deafened: bool,
    /// Until then the server refuses an unmute or an undeafen.
    pub unmute_after: Option<Instant>,
    /// Until then the server refuses a Join. It outlives the call the refusal ended.
    pub join_after: Option<Instant>,
    /// The media connection over the last second, while In Voice.
    pub stats: Option<VoiceStats>,
    /// How many times the Member talked into a muted microphone in this call and was nudged. The
    /// mute button pulses once each time it grows.
    pub mute_nudges: u32,
    /// Who speaks in the call as this client has it: itself by its own VAD, the others by the V
    /// flag of the frames playing now. The rings of everyone in the call follow this, timed to
    /// the sound, not the Lobby's `VOICE_SPEAKING`.
    pub speaking: BTreeSet<UserId>,
}

/// This client's side of Screen Share. Whether a share is Streaming (public) is the server's
/// snapshot (`VoiceState.screenShare`), shown the same on every client.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ShareUi {
    pub sharing: Option<ScreenSource>,
    pub strain: Option<StrainWarning>,
    /// While a Watch is on: the one answer to whose share the viewer shows.
    pub watch: Option<WatchUi>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrainRole {
    Viewer,
    Sharer,
}

/// What a link carries against what the share needs, in bits per second.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shortfall {
    pub reaching: u64,
    pub needs: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrainCause {
    /// Packets are lost or arrive with holes. The shortfall is told when both numbers are known
    /// and the link carries less than the share needs.
    Network(Option<Shortfall>),
    /// The decoder or the encoder falls behind.
    Computer,
}

/// The one warning a share can show: a strain that held for a while and has not been healthy
/// since.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StrainWarning {
    pub role: StrainRole,
    pub cause: StrainCause,
}

/// What the viewer of another Member's share draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchUi {
    /// Whose pictures the viewer shows, or, before the first came, whom it connects to. A
    /// Watch of another Sharer keeps this until their first picture is in the feed.
    pub sharer: UserId,
    /// A picture of `sharer`'s share is in the feed.
    pub live: bool,
    /// The other Sharer asked for, whose first picture has not come. The server moves the
    /// Watch's Ink at its answer, so it takes none on `sharer`'s share meanwhile.
    pub coming: Option<UserId>,
    pub strain: Option<StrainWarning>,
}

/// The Member's own account operations: one of each kind at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AccountOp {
    Rename,
    Avatar,
    Icon,
    Leave,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AccountUi {
    running: BTreeSet<AccountOp>,
    /// Where this Member's Magic Codes go, from READY: Settings → Account shows it.
    pub email: Option<Email>,
}

impl AccountUi {
    pub fn running(&self, op: AccountOp) -> bool {
        self.running.contains(&op)
    }

    pub(crate) fn start(&mut self, op: AccountOp) -> bool {
        self.running.insert(op)
    }

    pub(crate) fn finish(&mut self, op: AccountOp) {
        self.running.remove(&op);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LobbyState {
    pub phase: Phase,
    pub me: Option<UserId>,
    pub lobby: Option<LobbyInfo>,
    pub members: BTreeMap<UserId, Member>,
    /// Keyed by id, so REST pages and live `MESSAGE_CREATE`s merge without races. Shared, so a
    /// round that leaves the Messages alone neither compares nor copies them.
    pub chat: Shared<BTreeMap<MessageId, Message>>,
    /// How many times a loaded Message's author name or avatar was rewritten by a rename or a new
    /// avatar: a view that keeps rows built from `chat` sees it move.
    pub author_changes: u64,
    /// How many times a loaded Message was edited or deleted, for the same views.
    pub chat_changes: u64,
    pub changes: Changes,
    pub pending: Vec<PendingMessage>,
    pub drafts: Drafts,
    pub history: History,
    pub older: Older,
    pub typing: BTreeMap<UserId, Typist>,
    pub voice: VoiceUi,
    pub share: ShareUi,
    /// The ink on each Screen Share this client sees with drawing on. A board lives while its
    /// share's drawing epoch holds, so a new epoch or the end of the share clears it.
    pub ink: BTreeMap<ShareId, InkBoard>,
    pub access_requests: Vec<AccessRequest>,
    /// Requests this client Vouched for or Declined, until the server says they are handled.
    pub deciding: BTreeSet<AccessRequestId>,
    /// The owner's actions sent and not yet answered, one per Member.
    pub owner_acting: BTreeMap<UserId, OwnerAction>,
    pub account: AccountUi,
}

impl Default for LobbyState {
    fn default() -> Self {
        Self {
            phase: Phase::Connecting { attempt: 0 },
            me: None,
            lobby: None,
            members: BTreeMap::new(),
            chat: Shared::default(),
            author_changes: 0,
            chat_changes: 0,
            changes: Changes::default(),
            pending: Vec::new(),
            drafts: Drafts::default(),
            history: History::NotLoaded,
            older: Older::Unknown,
            typing: BTreeMap::new(),
            voice: VoiceUi::default(),
            share: ShareUi::default(),
            ink: BTreeMap::new(),
            access_requests: Vec::new(),
            deciding: BTreeSet::new(),
            owner_acting: BTreeMap::new(),
            account: AccountUi::default(),
        }
    }
}

/// What this client heard of the Lobby's edits and deletions of Messages.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Changes {
    /// The newest edit or the deletion of each Message heard of, loaded here or not: a copy
    /// read before it, in a page still on its way, must not undo it.
    newest: BTreeMap<MessageId, Change>,
    sync: ChangeSync,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Change {
    Edited {
        rev: u64,
        content: String,
        at: Option<UnixMs>,
    },
    Removed,
}

/// How far the feed has heard the Lobby's change revisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeSync {
    /// Every change up to `rev` is in the feed, and live ones keep it so.
    Synced(u64),
    /// Every change up to `from` is, and those up to READY's `to` are on their way.
    CatchingUp { from: u64, to: u64 },
    /// Every change up to `from` is, and the catch-up after it failed: the next READY asks again
    /// from `from`, whatever came live since.
    Behind { from: u64 },
}

impl Default for ChangeSync {
    fn default() -> Self {
        Self::Synced(0)
    }
}

impl Changes {
    pub fn sync(&self) -> ChangeSync {
        self.sync
    }

    /// The revision the feed has every change up to, which a catch-up starts after. A live
    /// change heard past a gap does not move it.
    pub(crate) fn synced(&self) -> u64 {
        match self.sync {
            ChangeSync::Synced(rev)
            | ChangeSync::CatchingUp { from: rev, .. }
            | ChangeSync::Behind { from: rev } => rev,
        }
    }

    /// What a resumed feed keeps: every change heard, and a catch-up still on its way or failed
    /// is asked again by the next READY.
    fn resumed(&self) -> Changes {
        Changes {
            newest: self.newest.clone(),
            sync: ChangeSync::Synced(self.synced()),
        }
    }

    pub(crate) fn set_sync(&mut self, sync: ChangeSync) {
        self.sync = sync;
    }

    /// A live change, which the socket brings in order: unless a catch-up is on its way or
    /// failed, everything up to it is in the feed.
    fn live(&mut self, rev: u64) {
        if let ChangeSync::Synced(heard) = &mut self.sync {
            *heard = (*heard).max(rev);
        }
    }

    /// The catch-up up to `to` came back, or failed and is asked again at the next READY.
    pub(crate) fn caught_up(&mut self, to: u64, ok: bool) {
        if let ChangeSync::CatchingUp { from, to: asked } = self.sync
            && asked == to
        {
            self.sync = if ok {
                ChangeSync::Synced(to)
            } else {
                ChangeSync::Behind { from }
            };
        }
    }
}

/// Whether there is history before the oldest Message of `page`, as the page says.
fn older_after(page: &MessagePage) -> Older {
    if page.has_more {
        Older::Available
    } else {
        Older::Beginning
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Applied {
    Nothing,
    Ready,
    Confirmed { nonce: String },
    Typing(UserId),
}

impl LobbyState {
    pub fn apply(&mut self, msg: &ServerMsg) -> Applied {
        match msg {
            ServerMsg::Ready(ready) => {
                // Another instance at the same origin: the Lobby was reset, and its Message ids
                // start over, so nothing local from before may mix with them.
                if self.was_reset(ready) {
                    self.chat = Shared::default();
                    self.changes = Changes::default();
                    self.older = Older::Unknown;
                }
                self.typing.clear();
                self.me = Some(ready.me.clone());
                self.account.email = Some(ready.email.clone());
                self.lobby = Some(ready.lobby.clone());
                self.members = ready
                    .members
                    .iter()
                    .map(|m| (m.id.clone(), m.clone()))
                    .collect();
                self.refresh_authors();
                self.sync_ink();
                self.access_requests = ready.access_requests.clone();
                self.deciding
                    .retain(|id| ready.access_requests.iter().any(|r| &r.id == id));
                Applied::Ready
            }
            ServerMsg::AccessRequestUpsert { request } => {
                // Where the Lobby's next READY would list it.
                self.access_requests.retain(|r| r.id != request.id);
                let place = AccessRequest::list_order(request.created_at, &request.id);
                let at = self
                    .access_requests
                    .partition_point(|r| AccessRequest::list_order(r.created_at, &r.id) < place);
                self.access_requests.insert(at, request.clone());
                Applied::Nothing
            }
            ServerMsg::AccessRequestRemoved(removed) => {
                self.access_requests.retain(|r| r.id != removed.id);
                self.deciding.remove(&removed.id);
                Applied::Nothing
            }
            ServerMsg::MemberUpsert { member } => {
                if member.presence == Presence::Offline {
                    self.typing.remove(&member.id);
                }
                self.members.insert(member.id.clone(), member.clone());
                self.set_deactivated(&member.id, false);
                self.refresh_authors();
                self.sync_ink();
                Applied::Nothing
            }
            ServerMsg::MemberRemoved(removed) => {
                self.members.remove(&removed.user_id);
                self.typing.remove(&removed.user_id);
                self.set_deactivated(&removed.user_id, true);
                self.sync_ink();
                Applied::Nothing
            }
            ServerMsg::TypingStart(t) => {
                let me = self.me.as_ref();
                if me == Some(&t.user_id) || !self.members.contains_key(&t.user_id) {
                    return Applied::Nothing;
                }
                Applied::Typing(t.user_id.clone())
            }
            ServerMsg::MessageCreate { message } => {
                // Their Message is what they were typing (there is no TYPING_STOP).
                self.typing.remove(&message.author.id);
                self.take_message(message, true);
                match self.settle_pending(std::slice::from_ref(message)).pop() {
                    Some(nonce) => Applied::Confirmed { nonce },
                    None => Applied::Nothing,
                }
            }
            ServerMsg::MessageUpdate { message } => {
                self.take_change(message);
                self.changes.live(message.rev.unwrap_or(0));
                Applied::Nothing
            }
            ServerMsg::MessageRemoved(removed) => {
                self.take_removal(removed.message_id);
                self.changes.live(removed.rev);
                Applied::Nothing
            }
            ServerMsg::VoiceSpeaking(s) => {
                if let Some(voice) = self
                    .members
                    .get_mut(&s.user_id)
                    .and_then(|m| m.voice.as_mut())
                {
                    voice.speaking = s.speaking;
                }
                Applied::Nothing
            }
            ServerMsg::LobbyUpdate { lobby } => {
                self.lobby = Some(lobby.clone());
                Applied::Nothing
            }
            ServerMsg::Hello(_)
            | ServerMsg::Ack(_)
            | ServerMsg::Error(_)
            | ServerMsg::RtcOffer(_)
            | ServerMsg::VoiceEnded(_)
            | ServerMsg::WatchDropped(_)
            | ServerMsg::Ink(_) => Applied::Nothing,
        }
    }

    /// What a new Session of this Lobby starts from: what the server said, the feed included, as
    /// on a reconnect. None of this client's own calls, sends or uploads carry over, and this
    /// Member, who left Voice with the old Session, is not shown in it. A feed whose backfill had
    /// not finished starts over: its newest Message may sit above a hole.
    pub fn resumed(&self) -> LobbyState {
        let mut members = self.members.clone();
        if let Some(me) = self.me.as_ref().and_then(|me| members.get_mut(me)) {
            me.voice = None;
        }
        let whole = self.history == History::Loaded;
        LobbyState {
            me: self.me.clone(),
            lobby: self.lobby.clone(),
            members,
            chat: if whole {
                self.chat.clone()
            } else {
                Shared::default()
            },
            author_changes: self.author_changes,
            // The kept feed asks READY for what follows it, so it keeps what it heard of edits.
            chat_changes: self.chat_changes,
            changes: if whole {
                self.changes.resumed()
            } else {
                Changes::default()
            },
            history: if whole {
                History::Loaded
            } else {
                History::NotLoaded
            },
            older: match self.older {
                _ if !whole => Older::Unknown,
                Older::Loading => Older::Available,
                older => older,
            },
            access_requests: self.access_requests.clone(),
            ..LobbyState::default()
        }
    }

    pub fn was_reset(&self, ready: &lobby_protocol::Ready) -> bool {
        self.lobby
            .as_ref()
            .is_some_and(|l| l.instance_id != ready.lobby.instance_id)
    }

    // Each of these looks before it writes: a write copies the Messages the last snapshot shares.
    fn set_deactivated(&mut self, author: &UserId, deactivated: bool) {
        let stale = |m: &Message| &m.author.id == author && m.author.deactivated != deactivated;
        if !self.chat.values().any(stale) {
            return;
        }
        for m in self.chat.values_mut() {
            if &m.author.id == author {
                m.author.deactivated = deactivated;
            }
        }
    }

    /// Messages carry their author's name and avatar as they were when they were read; where the
    /// roster says otherwise (a rename or a new picture, live or while this client was away), the
    /// Message follows it.
    fn refresh_authors(&mut self) {
        let stale = |m: &Message| {
            self.members.get(&m.author.id).is_some_and(|member| {
                m.author.username != member.username.as_str() || m.author.avatar != member.avatar
            })
        };
        if !self.chat.values().any(stale) {
            return;
        }
        for m in self.chat.values_mut() {
            let Some(member) = self.members.get(&m.author.id) else {
                continue;
            };
            if m.author.username != member.username.as_str() || m.author.avatar != member.avatar {
                m.author.username = member.username.to_string();
                m.author.avatar = member.avatar.clone();
                self.author_changes += 1;
            }
        }
    }

    pub fn owner(&self) -> Option<&UserId> {
        self.lobby.as_ref()?.owner.as_ref()
    }

    pub fn is_owner(&self) -> bool {
        self.me.is_some() && self.owner() == self.me.as_ref()
    }

    /// The names `@name` can mention in this Lobby: its Members now.
    pub fn mention_names(&self) -> MentionNames {
        MentionNames::of(self.members.values().map(|m| &m.username))
    }

    pub fn my_username(&self) -> Option<&Username> {
        Some(&self.members.get(self.me.as_ref()?)?.username)
    }

    pub fn me_presence(&self) -> Option<Presence> {
        let me = self.me.as_ref()?;
        Some(self.members.get(me)?.presence)
    }

    pub fn me_voice(&self) -> Option<VoiceState> {
        let me = self.me.as_ref()?;
        self.members.get(me)?.voice
    }

    /// `user`'s Screen Share while it streams: the Lobby lists a share only once its video
    /// reached the SFU. The one read of whether a Member's share can be watched.
    pub fn share_of(&self, user: &UserId) -> Option<ScreenShare> {
        self.members.get(user)?.voice?.screen_share
    }

    /// The epoch of `share` while its Sharer has "Let viewers draw" on.
    pub fn drawing_epoch(&self, share: ShareId) -> Option<DrawingEpoch> {
        self.members
            .values()
            .filter_map(|m| m.voice?.screen_share)
            .find(|s| s.share_id == share)?
            .drawing
    }

    /// `watch` is the snapshot's, or the Session's own in the middle of a round: the snapshot's
    /// is written at its end.
    pub(crate) fn drawable_shares<'a>(
        &'a self,
        watch: Option<&'a WatchUi>,
    ) -> impl Iterator<Item = (ShareId, DrawingEpoch)> + 'a {
        [
            watch.filter(|w| w.coming.is_none()).map(|w| &w.sharer),
            self.me.as_ref(),
        ]
        .into_iter()
        .flatten()
        .filter_map(|user| self.share_of(user))
        .filter_map(|s| Some((s.share_id, s.drawing?)))
    }

    pub fn may_draw(&self, share: ShareId) -> bool {
        self.drawable_shares(self.share.watch.as_ref())
            .any(|(id, _)| id == share)
    }

    /// The share a pen draws on when nothing else says: the one being watched, else the Member's
    /// own, while its drawing is on.
    pub fn pen_share(&self) -> Option<ShareId> {
        self.drawable_shares(self.share.watch.as_ref())
            .next()
            .map(|(share, _)| share)
    }

    fn sync_ink(&mut self) {
        let mut boards = std::mem::take(&mut self.ink);
        boards.retain(|share, board| self.drawing_epoch(*share) == Some(board.epoch()));
        self.ink = boards;
    }

    pub fn in_voice(&self) -> Vec<&Member> {
        let me = self.me.as_ref();
        let mut members: Vec<&Member> = self
            .members
            .values()
            .filter(|m| m.voice.is_some())
            .collect();
        members.sort_by_cached_key(|m| (Some(&m.id) != me, m.username.as_str().to_lowercase()));
        members
    }

    /// READY's history. The newest page starts the feed over, so it shows no hole where more was
    /// missed than the Lobby fills in. Returns the nonces of the sends the page settled.
    pub fn merge_ready(&mut self, history: &ReadyHistory) -> Vec<String> {
        match history {
            ReadyHistory::Latest(page) => {
                // A new map, not clear(): clearing the shared one would copy it first.
                self.chat = Shared::default();
                self.older = older_after(page);
                self.merge_page(page)
            }
            ReadyHistory::After(page) => self.merge_page(page),
        }
    }

    /// What READY's history follows on from at the next connect.
    pub fn held_history(&self) -> Option<HeldHistory> {
        Some(HeldHistory {
            instance_id: self.lobby.as_ref()?.instance_id.clone(),
            newest: self.newest_message()?,
        })
    }

    /// Returns the nonces of the sends the page settled.
    pub fn merge_older(&mut self, page: &MessagePage) -> Vec<String> {
        let settled = self.merge_page(page);
        self.older = older_after(page);
        settled
    }

    /// Why a Send of `draft` is not allowed right now, if it is not: the one answer for the Send
    /// button and for the Session's own check. A text the Lobby would refuse for what it is (too
    /// long, too nested) is allowed here and refused with its reason when sent.
    pub fn send_block(&self, draft: &str) -> Option<SendBlock> {
        self.drafts.send_block().or_else(|| {
            (draft.trim().is_empty() && self.drafts.is_empty()).then_some(SendBlock::Empty)
        })
    }

    /// Whether this Member may edit or delete Message `id`: it is loaded here and theirs. The
    /// Lobby checks it again.
    pub fn may_change(&self, id: MessageId) -> bool {
        self.chat
            .get(&id)
            .is_some_and(|m| self.me.as_ref() == Some(&m.author.id))
    }

    /// This Member's newest loaded Message.
    pub fn last_mine(&self) -> Option<MessageId> {
        let me = self.me.as_ref()?;
        self.chat
            .values()
            .rev()
            .find(|m| &m.author.id == me)
            .map(|m| m.id)
    }

    pub fn oldest_message(&self) -> Option<MessageId> {
        self.chat.keys().next().copied()
    }

    pub fn newest_message(&self) -> Option<MessageId> {
        self.chat.keys().next_back().copied()
    }

    pub fn typists(&self, now: Instant) -> Vec<&Member> {
        let mut typing: Vec<(&Typist, &Member)> = self
            .typing
            .iter()
            .filter(|(_, t)| t.until > now)
            .filter_map(|(id, t)| Some((t, self.members.get(id)?)))
            .collect();
        typing.sort_by_key(|(t, m)| (t.since, m.username.as_str().to_lowercase()));
        typing.into_iter().map(|(_, m)| m).collect()
    }

    /// Who is typing at `now`, in the chat's words.
    pub fn typing_line(&self, now: Instant) -> Option<String> {
        let names: Vec<String> = self
            .typists(now)
            .iter()
            .map(|m| m.username.to_string())
            .collect();
        crate::copy::typing_line(&names)
    }

    /// Idempotent by id. Returns the nonces of the sends the page settled: a page may hold this
    /// Member's pending send, its broadcast still in flight.
    pub fn merge_page(&mut self, page: &MessagePage) -> Vec<String> {
        for m in &page.messages {
            self.take_message(m, true);
        }
        self.settle_pending(&page.messages)
    }

    /// What changed while this client was away, from `GET /messages/changes`.
    pub fn merge_changes(&mut self, changes: &MessageChanges) {
        for m in &changes.updated {
            self.take_change(m);
        }
        for id in &changes.removed {
            self.take_removal(*id);
        }
    }

    /// Every copy of a Message, from a frame or a page, comes in here: the newest edit heard of
    /// wins, and a deleted Message never comes back. `add`: a Message not loaded here joins the
    /// feed, as a page's or a new one does.
    fn take_message(&mut self, m: &Message, add: bool) {
        let mut m = m.clone();
        // A page can be read before a rename or a new avatar that the roster already has.
        if let Some(member) = self.members.get(&m.author.id) {
            m.author.username = member.username.to_string();
            m.author.avatar = member.avatar.clone();
        }
        match self.changes.newest.get(&m.id) {
            Some(Change::Removed) => return,
            Some(Change::Edited { rev, content, at }) if Some(*rev) > m.rev => {
                m.content.clone_from(content);
                m.edited_at = *at;
                m.rev = Some(*rev);
            }
            _ => {
                if let Some(rev) = m.rev {
                    let edit = Change::Edited {
                        rev,
                        content: m.content.clone(),
                        at: m.edited_at,
                    };
                    self.changes.newest.insert(m.id, edit);
                }
            }
        }
        // Read first: a write to the shared Messages copies them, so only a change writes.
        match self.chat.get(&m.id) {
            Some(old) if *old == m => {}
            Some(_) => {
                self.chat.insert(m.id, m);
                self.chat_changes += 1;
            }
            None if add => {
                self.chat.insert(m.id, m);
            }
            None => {}
        }
    }

    /// An edit of a Message not loaded here is remembered for its page, which may be older.
    fn take_change(&mut self, m: &Message) {
        self.take_message(m, false);
    }

    fn take_removal(&mut self, id: MessageId) {
        self.changes.newest.insert(id, Change::Removed);
        if self.chat.contains_key(&id) {
            self.chat.remove(&id);
            self.chat_changes += 1;
        }
    }

    /// Drops this Member's pending sends that `messages` carry and returns their nonces. The
    /// Lobby keys a send by author and nonce, so another Member's Message with the same nonce
    /// settles nothing.
    fn settle_pending(&mut self, messages: &[Message]) -> Vec<String> {
        let mut settled = Vec::new();
        let me = self.me.as_ref();
        self.pending.retain(|p| {
            let carried = messages
                .iter()
                .any(|m| Some(&m.author.id) == me && m.nonce == p.nonce);
            if carried {
                settled.push(p.nonce.clone());
            }
            !carried
        });
        settled
    }

    pub fn take_pending(&mut self, nonce: &str) -> Option<PendingMessage> {
        let i = self.pending.iter().position(|p| p.nonce == nonce)?;
        Some(self.pending.remove(i))
    }

    /// The sidebar order: me first, then online and idle, then offline,
    /// then by name case-insensitively.
    pub fn sidebar(&self) -> Vec<&Member> {
        let me = self.me.as_ref();
        let mut members: Vec<&Member> = self.members.values().collect();
        members.sort_by_cached_key(|m| {
            (
                Some(&m.id) != me,
                m.presence == Presence::Offline,
                m.username.as_str().to_lowercase(),
            )
        });
        members
    }
}

#[cfg(test)]
mod tests {
    use lobby_protocol::AuthorRef;

    use super::*;

    fn message(id: i64, nonce: &str) -> Message {
        Message {
            id: MessageId::new(id).unwrap(),
            author: AuthorRef {
                id: UserId::random(),
                username: "x".into(),
                avatar: None,
                deactivated: false,
            },
            content: format!("{id}"),
            attachments: Vec::new(),
            nonce: nonce.into(),
            created_at: UnixMs(id),
            edited_at: None,
            rev: None,
        }
    }

    /// A page read before an edit or a delete, and here after it, does not undo it, whether
    /// the Message was loaded when the change came or not; a newer copy wins.
    #[test]
    fn a_page_read_before_a_change_does_not_undo_it() {
        let mut state = LobbyState::default();
        let originals = |ids: std::ops::RangeInclusive<i64>| MessagePage {
            messages: ids.map(|i| message(i, &format!("n{i}"))).collect(),
            has_more: false,
        };
        let edited = |id, content: &str, rev: u64| {
            let mut m = message(id, &format!("n{id}"));
            m.content = content.into();
            m.edited_at = Some(UnixMs(100 + rev as i64));
            m.rev = Some(rev);
            m
        };
        let content =
            |state: &LobbyState, id| state.chat[&MessageId::new(id).unwrap()].content.clone();
        state.merge_ready(&ReadyHistory::Latest(originals(5..=8)));
        let before = state.chat_changes;
        state.apply(&ServerMsg::MessageUpdate {
            message: edited(6, "fixed", 1),
        });
        state.apply(&ServerMsg::MessageRemoved(lobby_protocol::MessageRemoved {
            message_id: MessageId::new(7).unwrap(),
            rev: 2,
        }));
        state.apply(&ServerMsg::MessageUpdate {
            message: edited(2, "older, fixed", 3),
        });
        assert_eq!(
            state.chat_changes,
            before + 2,
            "a feed that keeps rows sees both"
        );
        state.merge_ready(&ReadyHistory::Latest(originals(5..=8)));
        state.merge_older(&originals(1..=4));
        let ids: Vec<i64> = state.chat.keys().map(|id| id.get()).collect();
        assert_eq!(ids, [1, 2, 3, 4, 5, 6, 8], "7 stays deleted");
        assert_eq!(content(&state, 6), "fixed");
        assert_eq!(
            content(&state, 2),
            "older, fixed",
            "edited before it was loaded"
        );
        state.merge_page(&MessagePage {
            messages: vec![edited(6, "fixed again", 4)],
            has_more: false,
        });
        assert_eq!(content(&state, 6), "fixed again");
    }
}
