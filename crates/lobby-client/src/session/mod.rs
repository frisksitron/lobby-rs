//! `Session`: the whole per-Lobby logic without pixels, run by its one driver
//! ([`SessionHandle`]). A Member's action comes in as one [`Intent`]; the Session folds events
//! into `LobbyState`, keeps a deadline per outstanding command, and owns the IO it starts (REST
//! through its `IoRunner`, and the call's media), whose results come back as [`Completion`]s on
//! its event channel. It also owns this client's presence: what the person wants others to see
//! (Do Not Disturb, idle from the OS) goes with every IDENTIFY and is re-sent whenever the
//! server's snapshot says otherwise.

mod account;
mod admission;
mod chat;
mod driver;
mod ink;
mod intent;
mod io;
mod owner;
mod presence;
mod recovery;
mod strain;
mod voice;
mod watch;

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use lobby_protocol::{
    AccessRequest, AccessRequestId, Ack, ClientFrame, ClientMsg, ErrorBody, ErrorCode, Message,
    Presence, Rid, ServerMsg, UserId, Username, VoiceFlags,
};
use tokio::{runtime::Handle, sync::mpsc};
use tracing::{debug, info, warn};

use crate::{
    ClientTask, ConnError, Connection, LobbyOrigin, Phase,
    api::{ApiClient, Page},
    connection::{ConnEvent, SignedOutWhy},
    copy,
    ink::InkPen,
    issues::{Issue, IssueKind},
    media::{ShareOpId, VideoFeed, VoiceCallId, VoiceEvent, sounds::Sound},
    state::{AccountOp, Applied, LobbyState},
};

pub(crate) use self::driver::{RuntimeClock, drive};
use self::{
    account::Leave,
    chat::FailedSends,
    io::{Done, IoRunner, Op},
    voice::{Ended, VoiceCall},
};
pub use self::{
    account::PictureError,
    driver::{Clock, SessionHandle, SessionUpdate, Snapshot, Updates},
    intent::Intent,
    io::Completion,
};

pub const COMMAND_DEADLINE: Duration = Duration::from_secs(5);

const ANSWER_DEADLINE: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub enum SessionEvent {
    Conn(ConnEvent),
    Completed(Completion),
    Voice {
        call: VoiceCallId,
        event: VoiceEvent,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiEffect {
    /// The Session took the send of `draft`: the composer lets go of that text, and keeps what
    /// was typed after it.
    Sent {
        draft: String,
    },
    /// A send failed: put the text back in the composer and tell `issue`. Its kind is
    /// [`IssueKind::SendRefused`], which offers no "Send again", when the Lobby refused the
    /// Message for what it is (too long, too nested, invalid): the same text would be refused
    /// again. A send the Session refuses itself is an Issue of the same kind.
    SendFailed {
        draft: String,
        issue: Issue,
    },
    /// A send reported as failed was stored after all: take its text back out of the composer
    /// and drop the error.
    SendRecovered {
        draft: String,
    },
    NeedsAuth(SignedOutWhy),
    Issue(Issue),
    Reconnected,
    PersistLobby(Box<lobby_protocol::LobbyInfo>),
    /// The Lobby accepted an owner's action on `user`; what changed arrives as its event.
    OwnerActed {
        user: UserId,
    },
    /// A Message that arrived live and was not in the chat yet: never one from READY, a history
    /// page or a gap fill. The window decides whether it is worth an OS toast.
    MessageArrived(Message),
    /// An Ask to Join that arrived live: never one READY carries. The Center keeps those.
    AccessRequested(AccessRequest),
    /// A request that was waiting is gone: handled by a Member, or handled while this device was
    /// away (READY no longer carries it).
    AccessRequestGone(AccessRequestId),
    /// The Lobby confirmed a change to this Voice session, or another Member joined or left it.
    /// Never for an action it refused. The window decides whether sounds are on.
    Sound(Sound),
    /// The Lobby took the Member's new name: the name field shows it as the Lobby wrote it.
    Renamed(Username),
    /// The name asked for is someone else's: said under the field.
    NameTaken,
    /// The Lobby let this Member leave: the window shuts the Session down with
    /// [`SessionHandle::leave_lobby`] and goes on to the next Lobby.
    Left,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UiEffects(pub Vec<UiEffect>);

impl UiEffects {
    pub fn extend(&mut self, other: UiEffects) {
        self.0.extend(other.0);
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Which command a rid belongs to, so its terminal outcome is handled by the right logic. Voice
/// commands name their Voice session, and share and watch commands their share or watch: a late
/// outcome of an ended one is ignored, never applied to the next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    MessageSend,
    /// An edit or a delete: the broadcast it caused already changed the state, so only its
    /// failure means anything.
    MessageChange,
    VoiceJoin {
        call: VoiceCallId,
    },
    VoiceLeave,
    /// `previous` is what the controls showed before the request: a refusal puts it back.
    VoiceStateSet {
        call: VoiceCallId,
        previous: VoiceFlags,
    },
    RtcAnswer {
        call: VoiceCallId,
    },
    RtcRestart,
    ShareStart {
        call: VoiceCallId,
        share: ShareOpId,
    },
    ShareStop,
    Watch {
        call: VoiceCallId,
        watch: ShareOpId,
    },
    Unwatch,
    DrawingSet,
    PresenceSet,
}

#[derive(Debug, Default)]
struct PendingCommands {
    /// Each command sent and not answered, with its deadline. A Watch past it stays, with none:
    /// the server may still take or refuse it, and its answer counts.
    deadlines: HashMap<String, (Command, Option<Instant>)>,
}

impl PendingCommands {
    fn insert(&mut self, rid: &str, command: Command, now: Instant) {
        let wait = match command {
            Command::RtcAnswer { .. } => ANSWER_DEADLINE,
            _ => COMMAND_DEADLINE,
        };
        self.deadlines
            .insert(rid.to_owned(), (command, Some(now + wait)));
    }

    fn remove(&mut self, rid: &str) -> Option<Command> {
        self.deadlines.remove(rid).map(|(c, _)| c)
    }

    fn expired(&mut self, now: Instant) -> Vec<(String, Command)> {
        let mut gone = Vec::new();
        self.deadlines.retain(|rid, (command, deadline)| {
            if deadline.is_none_or(|d| d > now) {
                return true;
            }
            gone.push((rid.clone(), *command));
            *deadline = None;
            matches!(command, Command::Watch { .. })
        });
        gone
    }

    fn drain(&mut self) -> Vec<(String, Command)> {
        self.deadlines
            .drain()
            .map(|(rid, (c, _))| (rid, c))
            .collect()
    }

    fn next_deadline(&self) -> Option<Instant> {
        self.deadlines.values().filter_map(|(_, d)| *d).min()
    }
}

pub struct Session {
    origin: LobbyOrigin,
    pub state: LobbyState,
    conn: Connection,
    io: IoRunner,
    pending: PendingCommands,
    failed: FailedSends,
    call: Option<VoiceCall>,
    /// This Member's pen on a Screen Share.
    pen: InkPen,
    voice_calls: u64,
    rids: u64,
    share_ops: u64,
    dnd: bool,
    idle: bool,
    /// The latest `PRESENCE_SET` without its outcome yet (its rid), so it is not sent twice.
    presence_sent: Option<(String, Presence)>,
    presence_retry_at: Option<Instant>,
    /// READY arrived at least once: a later Connected is a reconnect.
    connected_once: bool,
    leave: Leave,
    /// The latest Watch's decoded pictures, until the viewer takes them.
    watch_feed: Option<VideoFeed>,
    /// Effects raised outside an event's handling (an intent's refusal, a media failure), for
    /// the driver's next [`Session::take_effects`].
    queued: Vec<UiEffect>,
    /// The queued sends wait until then: the Lobby's throttle asked for it.
    queued_next_at: Option<Instant>,
    /// A backfill after a reconnect runs up to this Message id (`READY.lastMessageSeq`).
    backfill_to: Option<u64>,
    /// The page after READY that the feed waits for. An answer to another one was asked under an
    /// earlier READY, whose feed the newer one may have started over.
    history_asked: Option<Page>,
    typing_sent: Option<Instant>,
    /// How many pages of history came back, the dropped ones included.
    #[cfg(feature = "test-support")]
    history_pages: u64,
    #[cfg(feature = "test-support")]
    voice_ended_seen: Vec<lobby_protocol::VoiceEnded>,
    #[cfg(feature = "test-support")]
    watch_loads: Vec<(Instant, crate::media::WatchLoad)>,
}

/// The Issue for a refusal that names a retry instant (a cooldown or a budget): it expires then.
fn retry_issue(e: &ErrorBody, now: Instant) -> Option<Issue> {
    e.retry_after_ms?;
    let kind = match e.code {
        ErrorCode::VoiceStateCooldown => IssueKind::UnmuteCooldown,
        ErrorCode::VoiceJoinCooldown => IssueKind::JoinCooldown,
        ErrorCode::SignalingRateLimited => IssueKind::SignalingRateLimited,
        ErrorCode::RateLimited => IssueKind::PresenceRateLimited,
        _ => return None,
    };
    // Our words, not the server's: they say what the wait means here (the server's message is
    // for its logs and for other clients).
    let message = copy::issue_copy(e.code);
    Some(Issue::new(kind, message).expiring(now, e.retry_after_ms))
}

impl Session {
    /// Its REST calls go through `api` and its media's events, like the connection's, to `events`.
    /// It starts from `state`: the default, or what an earlier Session of the Lobby left.
    pub(crate) fn new(
        origin: LobbyOrigin,
        state: LobbyState,
        conn: Connection,
        rt: Handle,
        api: Arc<ApiClient>,
        events: mpsc::Sender<SessionEvent>,
    ) -> Self {
        Self {
            origin,
            state,
            conn,
            io: IoRunner::new(rt, api, events),
            pending: PendingCommands::default(),
            failed: FailedSends::default(),
            call: None,
            pen: InkPen::default(),
            voice_calls: 0,
            rids: 0,
            share_ops: 0,
            dnd: false,
            idle: false,
            presence_sent: None,
            presence_retry_at: None,
            connected_once: false,
            leave: Leave::No,
            watch_feed: None,
            queued: Vec::new(),
            queued_next_at: None,
            backfill_to: None,
            history_asked: None,
            typing_sent: None,
            #[cfg(feature = "test-support")]
            history_pages: 0,
            #[cfg(feature = "test-support")]
            voice_ended_seen: Vec::new(),
            #[cfg(feature = "test-support")]
            watch_loads: Vec::new(),
        }
    }

    pub fn take_effects(&mut self) -> UiEffects {
        UiEffects(std::mem::take(&mut self.queued))
    }

    pub fn origin(&self) -> &LobbyOrigin {
        &self.origin
    }

    /// The one way a Member's action comes in. A refusal is an Issue among the next effects.
    pub fn dispatch(&mut self, intent: Intent, now: Instant) {
        match intent {
            Intent::Send(draft) => self.send(&draft, now),
            Intent::EditMessage { id, content } => self.edit_message(id, content, now),
            Intent::DeleteMessage(id) => self.delete_message(id, now),
            Intent::Typing => self.typing(now),
            Intent::LoadOlder => self.load_older(),
            Intent::Attach(source) => self.attach(source),
            Intent::AttachFailed(e) => self.attach_refused(&e),
            Intent::RetryAttachment(id) => self.retry_attachment(id),
            Intent::RemoveAttachment(id) => self.remove_attachment(id),
            Intent::JoinVoice { media, volumes } => self.join_voice(*media, volumes, now),
            Intent::LeaveVoice => self.leave_voice(now),
            Intent::ToggleMute => self.toggle_mute(now),
            Intent::ToggleDeafen => self.toggle_deafen(now),
            Intent::MemberVolume { user, volume } => self.set_member_volume(user, volume),
            Intent::ChooseDevice { kind, choice } => self.choose_device(kind, choice),
            Intent::StartShare { source, quality } => self.start_share(source, quality, now),
            Intent::StopShare => self.stop_share(now),
            Intent::Watch(sharer) => self.watch(&sharer, now),
            Intent::Unwatch => self.unwatch(now),
            Intent::SetDrawing(enabled) => self.set_drawing(enabled, now),
            Intent::PenDown { share, at } => self.pen_down(share, at, now),
            Intent::PenTo(at) => self.pen_to(at, now),
            Intent::PenUp(at) => self.pen_up(at, now),
            Intent::SetDnd(on) => self.set_dnd(on, now),
            Intent::SetIdle(idle) => self.set_idle(idle, now),
            Intent::Vouch(id) => self.decide(&id, true),
            Intent::Decline(id) => self.decide(&id, false),
            Intent::Owner { user, action } => self.owner_action(&user, action),
            Intent::Rename(username) => self.rename(username),
            Intent::SetAvatar(path) => self.set_avatar(path),
            Intent::RemoveAvatar => self.remove_avatar(),
            Intent::SetIcon(path) => self.set_icon(path),
            Intent::RemoveIcon => self.remove_icon(),
            Intent::LeaveLobby => self.leave_lobby_request(),
            Intent::HandOverAndLeave(heir) => self.hand_over_and_leave(heir),
            Intent::RetryNow => self.conn.retry_now(),
            Intent::ProbeNow => self.conn.probe_now(),
        }
        self.show_call();
    }

    pub(super) fn raise(&mut self, kind: IssueKind, message: impl Into<String>) {
        let issue = Issue::new(kind, message.into());
        self.queued.push(UiEffect::Issue(issue));
    }

    fn request(&mut self, op: Op) {
        self.io.run(op);
    }

    /// Sends `msg` under a fresh request id and holds `command` to its deadline. A frame the
    /// connection refuses is never held.
    fn command(
        &mut self,
        msg: ClientMsg,
        command: Command,
        now: Instant,
    ) -> Result<Rid, ConnError> {
        self.rids += 1;
        let rid: Rid = format!("cmd-{}", self.rids).parse().expect("a valid rid");
        self.conn.send(&ClientFrame::with_rid(rid.clone(), msg))?;
        self.pending.insert(rid.as_str(), command, now);
        Ok(rid)
    }

    pub fn handle(&mut self, ev: SessionEvent, now: Instant) -> UiEffects {
        let mut fx = self.handle_event(ev, now);
        self.show_call();
        fx.extend(self.take_effects());
        fx
    }

    fn handle_event(&mut self, ev: SessionEvent, now: Instant) -> UiEffects {
        match ev {
            SessionEvent::Voice { call, event } => {
                self.on_voice_event(call, event, now);
                UiEffects::default()
            }
            SessionEvent::Conn(ConnEvent::Phase(phase)) => self.on_phase(phase, now),
            SessionEvent::Conn(ConnEvent::Disconnected) => {
                debug!("disconnected");
                self.fail_all(copy::MESSAGE_NOT_DELIVERED, now)
            }
            SessionEvent::Conn(ConnEvent::SignedOut(why)) => self.on_signed_out(why, now),
            SessionEvent::Conn(ConnEvent::Msg(msg)) => self.on_msg(&msg, now),
            SessionEvent::Completed(Completion(done)) => self.completed(done, now),
        }
    }

    fn completed(&mut self, done: Done, now: Instant) -> UiEffects {
        match done {
            Done::Messages { page, result } => return self.on_history(page, result, now),
            Done::Changes { to, result } => self.on_changes(to, result),
            Done::Decided { id, vouch, result } => return self.decided(&id, vouch, result),
            Done::OwnerActed {
                user,
                action,
                result,
            } => return self.owner_acted(&user, action, result),
            Done::UploadProgress { key, sent } => {
                self.state.drafts.progress(key, sent);
            }
            // A failed upload raises no Issue: its card says it, with Retry.
            Done::Uploaded { key, result } => {
                match &result {
                    Ok(a) => info!(draft = %key.draft, blob = %a.id, "uploaded"),
                    Err(e) => warn!(draft = %key.draft, error = %e, "upload failed"),
                }
                if self.state.drafts.finish(key, result) {
                    self.pump_uploads();
                }
            }
            Done::Renamed(result) => self.renamed(result),
            Done::Avatar(result) => self.picture_done(AccountOp::Avatar, result),
            Done::Icon(result) => self.icon_done(result),
            Done::Left(result) => self.left(result),
        }
        UiEffects::default()
    }

    fn on_phase(&mut self, phase: Phase, now: Instant) -> UiEffects {
        let mut fx = UiEffects::default();
        // A Voice session that ended with the connection stays explained after the return: its
        // notification offers Rejoin until the person joins or dismisses it.
        if phase != Phase::Connected {
            fx.extend(self.fail_all(copy::MESSAGE_NOT_DELIVERED, now));
            // Voice ends with the WebSocket and is not rejoined by itself.
            self.end_call(Some(Ended::voice(copy::VOICE_DISCONNECTED)));
        }
        let back = phase == Phase::Connected && self.state.phase != Phase::Connected;
        info!(?phase, "phase");
        self.state.phase = phase;
        if back {
            if self.connected_once {
                fx.0.push(UiEffect::Reconnected);
            }
            self.connected_once = true;
            // IDENTIFY said what others should see; if the wish changed during the handshake,
            // the snapshot disagrees and this sends the difference.
            self.presence_retry_at = None;
            self.sync_presence(now);
            self.send_queued(now);
        }
        fx
    }

    /// The connection gave up for good. The close of a Leave says nothing yet: the Leave's answer
    /// tells whether the Member left or was removed.
    fn on_signed_out(&mut self, why: SignedOutWhy, now: Instant) -> UiEffects {
        let mut fx = self.on_phase(Phase::NeedsAuth, now);
        match (why, &self.leave) {
            (SignedOutWhy::Removed, Leave::Asked | Leave::Closed) => self.leave = Leave::Closed,
            _ => fx.0.push(UiEffect::NeedsAuth(why)),
        }
        fx
    }

    fn on_msg(&mut self, msg: &ServerMsg, now: Instant) -> UiEffects {
        let mut fx = UiEffects::default();
        let was_active = self.voice_active();
        if let ServerMsg::Ready(ready) = msg
            && self.state.was_reset(ready)
        {
            // The old instance's nonces mean nothing to the new one.
            info!("the Lobby was reset: local chat discarded");
            self.failed = FailedSends::default();
        }
        let news = self.news_of(msg);
        let applied = self.state.apply(msg);
        self.follow_roster(msg, was_active, now);
        self.follow_ink();
        match applied {
            Applied::Ready => {
                if let ServerMsg::Ready(ready) = msg {
                    fx.0.push(UiEffect::PersistLobby(Box::new(ready.lobby.clone())));
                    fx.extend(self.on_ready_history(ready, now));
                }
            }
            Applied::Confirmed { nonce } => {
                self.pending.remove(&nonce);
            }
            Applied::Typing(user) => self.typing_seen(user, now),
            Applied::Nothing => {}
        }
        match msg {
            // The saved Lobby (and with it the switcher) follows a new name or icon at once.
            ServerMsg::LobbyUpdate { lobby } => {
                fx.0.push(UiEffect::PersistLobby(Box::new(lobby.clone())));
            }
            ServerMsg::MessageCreate { message } => {
                fx.extend(self.recovered(std::slice::from_ref(message), now));
            }
            ServerMsg::Ack(ack) => self.on_ack(ack, now),
            ServerMsg::Error(e) => fx.extend(self.on_error(e, now)),
            ServerMsg::RtcOffer(offer) => self.on_offer(offer, now),
            ServerMsg::WatchDropped(dropped) => self.on_watch_dropped(dropped),
            ServerMsg::VoiceEnded(ended) => {
                #[cfg(feature = "test-support")]
                self.voice_ended_seen.push(*ended);
                self.on_voice_ended(ended);
            }
            ServerMsg::Ink(relay) => self.on_ink(relay, now),
            _ => {}
        }
        fx.0.extend(news);
        fx
    }

    /// What a live frame announces that the state did not know yet, read before the state takes
    /// the frame in.
    fn news_of(&self, msg: &ServerMsg) -> Vec<UiEffect> {
        let waiting = |id: &AccessRequestId| self.state.access_requests.iter().any(|r| &r.id == id);
        match msg {
            ServerMsg::MessageCreate { message } if !self.state.chat.contains_key(&message.id) => {
                vec![UiEffect::MessageArrived(message.clone())]
            }
            ServerMsg::AccessRequestUpsert { request } if !waiting(&request.id) => {
                vec![UiEffect::AccessRequested(request.clone())]
            }
            ServerMsg::AccessRequestRemoved(removed) if waiting(&removed.id) => {
                vec![UiEffect::AccessRequestGone(removed.id.clone())]
            }
            ServerMsg::MemberUpsert { .. } | ServerMsg::MemberRemoved(_) => {
                self.roster_sound(msg).into_iter().collect()
            }
            ServerMsg::Ready(ready) => self
                .state
                .access_requests
                .iter()
                .filter(|r| !ready.access_requests.iter().any(|n| n.id == r.id))
                .map(|r| UiEffect::AccessRequestGone(r.id.clone()))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// An ACK settles its command; what it means belongs to the command's feature.
    fn on_ack(&mut self, ack: &Ack, now: Instant) {
        match self.pending.remove(ack.rid.as_str()) {
            Some(Command::MessageSend) => self.send_acked(ack.rid.as_str()),
            Some(Command::PresenceSet) => self.presence_acked(ack.rid.as_str(), now),
            Some(
                command @ (Command::VoiceJoin { .. }
                | Command::VoiceStateSet { .. }
                | Command::VoiceLeave
                | Command::Watch { .. }),
            ) => self.voice_acked(command, ack),
            _ => {}
        }
    }

    /// An ERROR settles its command, and the command's feature says what failed.
    fn on_error(&mut self, e: &ErrorBody, now: Instant) -> UiEffects {
        let mut fx = UiEffects::default();
        let Some((rid, command)) = e
            .rid
            .as_ref()
            .and_then(|rid| Some((rid, self.pending.remove(rid.as_str())?)))
        else {
            // An error without a rid raises an Issue. An outcome past its deadline is only logged.
            warn!(code = ?e.code, message = %e.message, "server error");
            if e.rid.is_none() {
                let message = copy::server_error(e.code, &e.message);
                fx.0.push(UiEffect::Issue(Issue::new(IssueKind::ServerError, message)));
            }
            return fx;
        };
        // A cooldown or budget: the Issue counts it down, and is the refusal's one Issue (the
        // send throttle is too short to show, and its draft comes back with the reason anyway).
        let countdown = (command != Command::MessageSend)
            .then(|| retry_issue(e, now))
            .flatten();
        let retry_at = countdown.as_ref().and_then(|i| i.expires_at);
        let counted = countdown.is_some();
        fx.0.extend(countdown.map(UiEffect::Issue));
        let before = self.queued.len();
        match command {
            Command::MessageSend => fx.extend(self.send_refused(rid.as_str(), e, now)),
            Command::MessageChange => {
                self.raise(
                    IssueKind::ChangeFailed,
                    copy::server_error(e.code, &e.message),
                );
            }
            Command::PresenceSet => self.presence_refused(rid.as_str(), e, now),
            _ => self.voice_refused(command, e, retry_at, now),
        }
        if counted {
            // The countdown is the refusal's one Issue: those its feature raised go.
            let mut at = 0;
            self.queued.retain(|effect| {
                at += 1;
                at <= before || !matches!(effect, UiEffect::Issue(_))
            });
        }
        fx
    }

    pub fn tick(&mut self, now: Instant) -> UiEffects {
        let mut fx = self.tick_deadlines(now);
        self.show_call();
        fx.extend(self.take_effects());
        fx
    }

    fn tick_deadlines(&mut self, now: Instant) -> UiEffects {
        let mut fx = UiEffects::default();
        for (rid, command) in self.pending.expired(now) {
            match command {
                Command::MessageSend => {
                    warn!(rid, ?command, "no terminal outcome within the deadline");
                    if let Some(p) = self.state.take_pending(&rid) {
                        fx.0.push(self.failed_without_answer(p, copy::MESSAGE_NOT_DELIVERED, now));
                    }
                }
                Command::MessageChange => {
                    warn!(rid, ?command, "no terminal outcome within the deadline");
                    self.raise(IssueKind::ChangeFailed, copy::CHANGE_NOT_CONFIRMED);
                }
                Command::VoiceJoin { call } | Command::RtcAnswer { call }
                    if self.is_current(call) =>
                {
                    warn!(rid, ?command, "no terminal outcome within the deadline");
                    self.leave_with(Ended::voice(copy::VOICE_TIMED_OUT), now);
                }
                Command::VoiceStateSet { call, previous } if self.is_current(call) => {
                    warn!(rid, ?command, "no terminal outcome within the deadline");
                    self.apply_flags(previous);
                    self.raise(IssueKind::VoiceRefused, copy::VOICE_NOT_CONFIRMED);
                }
                Command::ShareStart { call, share } if self.is_current_share(call, share) => {
                    warn!(rid, ?command, "no terminal outcome within the deadline");
                    self.send_share_stop(now);
                    self.end_share(Some(copy::SHARE_NOT_CONFIRMED.to_owned()));
                }
                // The server may have switched all the same: the picture follows its media.
                Command::Watch { call, watch } => self.watch_timed_out(call, watch),
                // Leaving needs no outcome, and an ended session's commands no longer matter.
                Command::PresenceSet => {
                    warn!(rid, "PRESENCE_SET without an outcome");
                    self.forget_presence_sent(&rid);
                }
                Command::VoiceLeave
                | Command::VoiceJoin { .. }
                | Command::RtcAnswer { .. }
                | Command::RtcRestart
                | Command::VoiceStateSet { .. }
                | Command::ShareStart { .. }
                | Command::ShareStop
                | Command::Unwatch
                | Command::DrawingSet => {}
            }
        }
        self.state.typing.retain(|_, t| t.until > now);
        if self.presence_retry_at.is_some_and(|at| at <= now) {
            self.presence_retry_at = None;
            self.sync_presence(now);
        }
        self.send_queued(now);
        self.tick_call(now);
        self.tick_ink(now);
        fx
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        [
            self.pending.next_deadline(),
            self.call.as_ref().and_then(VoiceCall::next_deadline),
            self.presence_retry_at,
            // Kept while disconnected for the wait left at the next Connected, which sends the
            // queue itself: a deadline past then would be due on every round.
            self.queued_next_at
                .filter(|_| self.state.phase == Phase::Connected),
            self.pen.deadline(),
            self.state.typing.values().map(|t| t.until).min(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn fail_all(&mut self, reason: &str, now: Instant) -> UiEffects {
        let mut fx = UiEffects::default();
        for (rid, command) in self.pending.drain() {
            if command == Command::MessageSend
                && let Some(p) = self.state.take_pending(&rid)
            {
                fx.0.push(self.failed_without_answer(p, reason, now));
            }
        }
        // Re-sent after READY if the server's snapshot still disagrees.
        self.presence_sent = None;
        fx
    }

    /// Sign Out of this Lobby on this device, local-first: this Lobby's
    /// tokens are cleared at once (with the access token taken first, so the process dying now
    /// still leaves the device signed out), while Voice and the connection close; then, with the
    /// socket gone (so the server's 4005 never reaches a Session that is going anyway), the
    /// Lobby is told, at most 5 s, and a failure there changes nothing here.
    pub fn sign_out(self) -> ClientTask<()> {
        let (rt, api) = (self.io.rt.clone(), self.io.api.clone());
        let closing = self.shutdown();
        ClientTask::spawn(&rt, async move {
            let token = api.forget().await;
            let _ = closing.await;
            api.tell_signed_out(token, false).await;
        })
    }

    /// After the Lobby let this Member leave: Voice and the connection close, then this Lobby's
    /// tokens are forgotten, in memory and in the store. Nothing is told to the Lobby, which has
    /// ended every session already.
    pub fn leave_lobby(self) -> ClientTask<()> {
        let (rt, api) = (self.io.rt.clone(), self.io.api.clone());
        let closing = self.shutdown();
        ClientTask::spawn(&rt, async move {
            let _ = closing.await;
            api.clear().await;
        })
    }

    /// Stops Voice media, closes the WebSocket (1000) and aborts HTTP; resolves once the media
    /// threads (bounded at 1.5 s) and the actor have exited. The server ends Voice when the
    /// socket closes.
    pub fn shutdown(self) -> ClientTask<()> {
        let Session { conn, io, .. } = self;
        let (rt, stopping) = io.shutdown();
        ClientTask::spawn(&rt, async move {
            for task in stopping {
                let _ = task.await;
            }
            let _ = conn.shutdown().await;
        })
    }
}
