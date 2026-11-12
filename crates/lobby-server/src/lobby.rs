//! The Lobby's live state: one `std::sync::Mutex`, synchronous methods that take it once, and
//! fan-out by `try_send` under it. Lock order is therefore the global broadcast order, and a state
//! change and the events describing it happen in one critical section, which keeps READY consistent
//! with what follows.

mod ink;
mod kick;

pub use self::{
    ink::{INK_QUEUE_BATCHES, InkQueue},
    kick::{CloseReason, Kick},
};

use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use axum::extract::ws::Utf8Bytes;
use lobby_protocol::{
    AccessRequest, AccessRequestId, AccessRequestOutcome, AccessRequestRemoved, BlobId, LobbyInfo,
    LobbyInstanceId, Member, MemberRemoved, Message, MessageId, MessageRemoved, Presence, Ready,
    ReadyHistory, Rid, ServerMsg, TypingStart, UnixMs, UserId, Username, VoiceEndReason,
    VoiceSpeaking,
};
use tokio::{
    sync::{Notify, mpsc},
    time::Instant,
};
use tracing::{debug, error, info, warn};

use crate::{
    config::VoiceConfig,
    db::{sessions::SessionId, users::Profile},
    limits::TokenBucket,
    refusal,
    sfu::{SfuCmd, SfuEvent, SfuHandle, SfuOp},
    tiers::ConnId,
    voice::{VoiceCommand, VoiceEffect, VoiceInput, VoiceMachine},
};

/// A Member's sends up to five back to back ("ok⏎ lol⏎") at once, then one per `ws.send_every`.
pub const SEND_BURST: u32 = 5;
/// How long a revocation is remembered for registrations whose auth lookup started before it:
/// far longer than an IDENTIFY's 5 s DB budget.
pub const REVOCATION_MEMORY: Duration = Duration::from_secs(60);

#[derive(Clone)]
pub struct Lobby {
    inner: Arc<Mutex<LobbyState>>,
    /// A command moved the earliest voice deadline closer: the jobs task re-reads it.
    voice_wakeup: Arc<Notify>,
}

pub struct Seed {
    pub info: LobbyInfo,
    pub profiles: Vec<Profile>,
    pub access_requests: Vec<AccessRequest>,
    pub last_message_seq: u64,
    pub last_change_rev: u64,
    /// `ws.send_every`.
    pub send_every: Duration,
}

pub struct Conn {
    pub id: ConnId,
    pub session: SessionId,
    pub tx: mpsc::Sender<Utf8Bytes>,
    /// Ink only: it drops when full, so it never kicks the connection.
    pub ink: InkQueue,
    pub kick: Kick,
    /// [`Lobby::auth_epoch`] read before the session's auth lookup: a revocation after it
    /// refuses the registration, which the lookup could not see.
    pub auth_epoch: u64,
}

/// READY's history and the change revision read with it, in one snapshot.
pub struct HistoryRead {
    pub history: ReadyHistory,
    pub rev: u64,
}

/// A session (sign-out, refresh reuse) or every session of a User (sign out everywhere, removal)
/// deleted at `epoch`.
struct Revocation {
    epoch: u64,
    at: Instant,
    what: Revoked,
}

enum Revoked {
    Session(SessionId),
    User(UserId),
}

impl Revoked {
    fn covers(&self, user: &UserId, session: SessionId) -> bool {
        match self {
            Revoked::Session(s) => *s == session,
            Revoked::User(u) => u == user,
        }
    }
}

struct MemberRow {
    profile: Profile,
    presence: Presence,
    send_throttle: TokenBucket,
}

impl MemberRow {
    fn new(profile: Profile, send_every: Duration) -> MemberRow {
        MemberRow {
            profile,
            presence: Presence::Offline,
            send_throttle: TokenBucket::new(send_every, SEND_BURST),
        }
    }
}

struct LobbyState {
    info: LobbyInfo,
    members: BTreeMap<UserId, MemberRow>,
    conns: HashMap<UserId, Conn>,
    access_requests: BTreeMap<AccessRequestId, AccessRequest>,
    revocations: VecDeque<Revocation>,
    epoch: u64,
    last_message_seq: u64,
    last_change_rev: u64,
    voice: VoiceMachine,
    sfu: SfuHandle,
    /// Shutting down: every connection is about to close, so nobody is told that the others go
    /// offline (each client keeps its last known roster and reconciles from READY after the
    /// restart, instead of moving everyone into Offline as the connections close one by one).
    draining: bool,
    send_every: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RegisterError {
    #[error("not an active Member")]
    NotAMember,
    #[error("session revoked")]
    Revoked,
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Counts {
    pub members: usize,
    pub connections: usize,
    pub online: usize,
    pub in_voice: usize,
}

impl Lobby {
    pub fn new(seed: Seed, sfu: SfuHandle, caps: VoiceConfig) -> Lobby {
        let members = seed
            .profiles
            .into_iter()
            .map(|profile| (profile.id.clone(), MemberRow::new(profile, seed.send_every)))
            .collect();
        let access_requests = seed
            .access_requests
            .into_iter()
            .map(|r| (r.id.clone(), r))
            .collect();
        Lobby {
            inner: Arc::new(Mutex::new(LobbyState {
                info: seed.info,
                members,
                conns: HashMap::new(),
                access_requests,
                revocations: VecDeque::new(),
                epoch: 0,
                last_message_seq: seed.last_message_seq,
                last_change_rev: seed.last_change_rev,
                voice: VoiceMachine::new(caps),
                sfu,
                draining: false,
                send_every: seed.send_every,
            })),
            voice_wakeup: Arc::new(Notify::new()),
        }
    }

    /// A panic under the lock is a bug: the poison takes the process down.
    fn lock(&self) -> MutexGuard<'_, LobbyState> {
        self.inner.lock().expect("lobby state poisoned")
    }

    pub fn info(&self) -> LobbyInfo {
        self.lock().info.clone()
    }

    /// The instance and the newest Message's id, which decide the history READY carries.
    pub fn history_point(&self) -> (LobbyInstanceId, u64) {
        let state = self.lock();
        (state.info.instance_id.clone(), state.last_message_seq)
    }

    /// Registers `conn` as `user`'s one connection and enqueues its READY, in one critical section.
    /// An older connection is replaced (4009). The Member shows as `presence` from the first
    /// `MEMBER_UPSERT` on, so a reconnect never flashes the wrong one. `history` was read before,
    /// from the [`Lobby::history_point`] read before it.
    pub fn register(
        &self,
        user: &UserId,
        conn: Conn,
        presence: Presence,
        history: HistoryRead,
    ) -> Result<(), RegisterError> {
        let mut state = self.lock();
        if !state.members.contains_key(user) {
            return Err(RegisterError::NotAMember);
        }
        if state.revoked_since(conn.auth_epoch, conn.session, user) {
            return Err(RegisterError::Revoked);
        }
        if let Some(old) = state.conns.get(user).map(|c| c.id) {
            info!(%user, old = old.0, new = conn.id.0, "connection replaced");
            // The old connection's Voice ends first, so its VOICE_ENDED goes out before 4009.
            state.conn_gone(user, old, VoiceEndReason::Replaced, Instant::now());
            if let Some(old) = state.conns.remove(user) {
                old.kick.kick(CloseReason::Replaced);
            }
        }
        let _ = state.set_presence(user, presence);
        let ready = ServerMsg::Ready(Box::new(state.ready(user, conn.id, history)));
        let _ = conn.tx.try_send(Utf8Bytes::from(ready.encode()));
        state.conns.insert(user.clone(), conn);
        Ok(())
    }

    /// A no-op unless `conn` is still `user`'s active connection: a replaced connection's cleanup
    /// never takes the newer one offline.
    pub fn unregister(&self, user: &UserId, conn: ConnId) {
        let mut state = self.lock();
        if state.conn(user, conn).is_some() {
            state.drop_conn(user, VoiceEndReason::ConnectionLost);
        }
    }

    /// `true` if presence changed, which is the only case that broadcasts.
    pub fn set_presence(&self, user: &UserId, conn: ConnId, presence: Presence) -> bool {
        let mut state = self.lock();
        if !presence.is_settable() || state.conn(user, conn).is_none() {
            return false;
        }
        state.set_presence(user, presence)
    }

    /// Read this before an auth lookup and hand it to [`Lobby::register`] or
    /// [`Lobby::set_session`].
    pub fn auth_epoch(&self) -> u64 {
        self.lock().epoch
    }

    /// After a re-IDENTIFY. `false` if the session was revoked after `auth_epoch` (the caller
    /// closes with 4005).
    pub fn set_session(
        &self,
        user: &UserId,
        conn: ConnId,
        session: SessionId,
        auth_epoch: u64,
    ) -> bool {
        let mut state = self.lock();
        if state.revoked_since(auth_epoch, session, user) {
            return false;
        }
        if let Some(c) = state.conns.get_mut(user).filter(|c| c.id == conn) {
            c.session = session;
        }
        true
    }

    /// Every session of `user` was deleted: closes their connection and refuses registrations whose
    /// session lookup came before now.
    pub fn kick_user(&self, user: &UserId) {
        self.lock().kick(Revoked::User(user.clone()));
    }

    /// The bootstrap Member set the Lobby up and owns it; nobody else is connected to be told.
    pub fn bootstrapped(&self, profile: Profile) {
        let mut state = self.lock();
        state.info.set_up = true;
        state.info.owner = Some(profile.id.clone());
        state.member_joined(profile);
    }

    /// The owner removed `user` from the Lobby, in one critical section after the commit:
    /// 1. the epoch is revoked, so a registration whose auth lookup ran before is refused;
    /// 2. their Voice ends while the row still exists, so everyone (watchers of their Screen Share
    ///    too) gets the share-ended, left-Voice `MEMBER_UPSERT`, and they get
    ///    `VOICE_ENDED{kicked}`;
    /// 3. the row and the connection leave the roster, so the connection's later `unregister` is a
    ///    no-op and no Offline upsert can bring the Member back;
    /// 4. `MEMBER_REMOVED` to everyone else;
    /// 5. their connection closes with 4006 once the queued frames are flushed.
    ///
    /// `true` if a live connection was closed. Running it again changes nothing.
    pub fn member_removed(&self, user: &UserId) -> bool {
        let mut state = self.lock();
        state.revoke_and_end_voice(Revoked::User(user.clone()));
        let conn = state.conns.remove(user);
        if state.members.remove(user).is_some() {
            info!(%user, "member removed");
            state.fan_out(&ServerMsg::MemberRemoved(MemberRemoved {
                user_id: user.clone(),
            }));
        }
        conn.map(|c| c.kick.kick(CloseReason::Removed)).is_some()
    }

    /// `LOBBY_UPDATE` to everyone. Called while the committing writer is held, so commit order is
    /// broadcast order.
    pub fn owner_changed(&self, owner: &UserId) {
        let mut state = self.lock();
        state.info.owner = Some(owner.clone());
        let lobby = state.info.clone();
        state.fan_out(&ServerMsg::LobbyUpdate { lobby });
    }

    /// `LOBBY_UPDATE` with the new icon (none after a removal), while the committing writer is
    /// held. Answers the Lobby as everyone now sees it.
    pub fn icon_changed(&self, icon: Option<BlobId>) -> LobbyInfo {
        let mut state = self.lock();
        state.info.icon = icon;
        let lobby = state.info.clone();
        state.fan_out(&ServerMsg::LobbyUpdate {
            lobby: lobby.clone(),
        });
        lobby
    }

    /// `MEMBER_UPSERT` with the new name, while the committing writer is held.
    pub fn renamed(&self, user: &UserId, username: &Username) {
        let mut state = self.lock();
        let Some(row) = state.members.get_mut(user) else {
            return;
        };
        row.profile.username = username.clone();
        state.member_changed(user);
    }

    /// `MEMBER_UPSERT` with the new avatar (none after a removal), while the committing writer is
    /// held.
    pub fn avatar_changed(&self, user: &UserId, avatar: Option<BlobId>) {
        let mut state = self.lock();
        let Some(row) = state.members.get_mut(user) else {
            return;
        };
        row.profile.avatar = avatar;
        state.member_changed(user);
    }

    pub fn access_request_upserted(&self, request: AccessRequest) {
        let mut state = self.lock();
        state
            .access_requests
            .insert(request.id.clone(), request.clone());
        state.fan_out(&ServerMsg::AccessRequestUpsert { request });
    }

    pub fn access_request_declined(&self, id: &AccessRequestId, by: &UserId, at: UnixMs) {
        self.lock()
            .access_request_removed(id, AccessRequestOutcome::Declined, by, at);
    }

    /// Requests dropped for waiting too long. The wire has no outcome for that and 0.1.0 clients
    /// exist, so each goes out as declined by the owner; clients only drop the request.
    pub fn access_requests_expired(&self, ids: &[AccessRequestId], at: UnixMs) {
        let mut state = self.lock();
        let Some(owner) = state.info.owner.clone() else {
            return;
        };
        for id in ids {
            state.access_request_removed(id, AccessRequestOutcome::Declined, &owner, at);
        }
    }

    /// A Vouch: `ACCESS_REQUEST_REMOVED{vouched_in}`, then the new Member's `MEMBER_UPSERT`, in one
    /// critical section.
    pub fn vouched_in(&self, id: &AccessRequestId, by: &UserId, at: UnixMs, profile: Profile) {
        let mut state = self.lock();
        state.access_request_removed(id, AccessRequestOutcome::VouchedIn, by, at);
        state.member_joined(profile);
    }

    pub fn kick_session(&self, session: SessionId) {
        self.lock().kick(Revoked::Session(session));
    }

    /// Ignored unless `conn` is still the Member's connection: a replaced one is closing and its
    /// Voice already ended.
    pub fn voice(
        &self,
        user: &UserId,
        conn: ConnId,
        rid: Option<Rid>,
        cmd: VoiceCommand,
        now: Instant,
    ) {
        let moved_closer = {
            let mut state = self.lock();
            if state.conn(user, conn).is_none() {
                debug!(%user, conn = conn.0, "voice command from a connection that is gone");
                return;
            }
            let before = state.voice.next_deadline();
            state.voice_step(
                VoiceInput::Cmd {
                    user: user.clone(),
                    conn,
                    rid,
                    cmd,
                },
                now,
            );
            let after = state.voice.next_deadline();
            after.is_some_and(|at| before.is_none_or(|was| at < was))
        };
        if moved_closer {
            self.voice_wakeup.notify_one();
        }
    }

    /// Called synchronously by the SFU thread, so no event is ever lost.
    pub fn on_sfu_event(&self, ev: SfuEvent, now: Instant) {
        self.lock().voice_step(VoiceInput::Sfu(ev), now);
    }

    /// The Voice watchdogs: ends sessions still Joining, or with a Pending Screen Share, past
    /// their deadline and returns the next deadline.
    pub fn tick_voice(&self, now: Instant) -> Option<Instant> {
        let mut state = self.lock();
        state.voice_step(VoiceInput::Tick, now);
        state.voice.next_deadline()
    }

    pub fn voice_wakeup(&self) -> Arc<Notify> {
        self.voice_wakeup.clone()
    }

    /// Shutdown: every Voice session ends with `VOICE_ENDED{shutdown}` before the connections
    /// are closed, so the frame is queued ahead of each close. From here on the Lobby drains:
    /// a connection that closes no longer announces its Member as offline to the others.
    pub fn end_all_voice(&self) {
        let mut state = self.lock();
        state.draining = true;
        let now = Instant::now();
        for (user, conn) in state.voice.sessions() {
            state.conn_gone(&user, conn, VoiceEndReason::Shutdown, now);
        }
    }

    /// The send throttle; `Err` carries the instant the next send is accepted.
    pub fn try_send_slot(&self, user: &UserId, now: Instant) -> Result<(), Instant> {
        match self.lock().members.get_mut(user) {
            Some(row) => row.send_throttle.try_acquire(now),
            None => Ok(()),
        }
    }

    /// Called while the DB writer is held, so commit order is broadcast order.
    pub fn broadcast_message(&self, message: &Message) {
        let mut state = self.lock();
        let seq = message.id.get() as u64;
        state.last_message_seq = state.last_message_seq.max(seq);
        state.fan_out(&ServerMsg::MessageCreate {
            message: message.clone(),
        });
    }

    /// Called while the DB writer is held, as [`Self::broadcast_message`] is.
    pub fn message_updated(&self, message: &Message) {
        let mut state = self.lock();
        state.last_change_rev = state.last_change_rev.max(message.rev.unwrap_or(0));
        state.fan_out(&ServerMsg::MessageUpdate {
            message: message.clone(),
        });
    }

    /// Called while the DB writer is held, as [`Self::broadcast_message`] is.
    pub fn message_removed(&self, message_id: MessageId, rev: u64) {
        let mut state = self.lock();
        state.last_change_rev = state.last_change_rev.max(rev);
        state.fan_out(&ServerMsg::MessageRemoved(MessageRemoved {
            message_id,
            rev,
        }));
    }

    pub fn typing(&self, user: &UserId, conn: ConnId) {
        let mut state = self.lock();
        if state.conn(user, conn).is_none() {
            return;
        }
        state.fan_out_except(
            user,
            &ServerMsg::TypingStart(TypingStart {
                user_id: user.clone(),
            }),
        );
    }

    /// The idempotent answer to an already stored nonce goes to the sender only.
    pub fn reply_to(&self, user: &UserId, conn: ConnId, msg: &ServerMsg) {
        let mut state = self.lock();
        state.send_to(user, conn, Utf8Bytes::from(msg.encode()));
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn counts(&self) -> Counts {
        let state = self.lock();
        Counts {
            members: state.members.len(),
            connections: state.conns.len(),
            online: state
                .members
                .values()
                .filter(|m| m.presence != Presence::Offline)
                .count(),
            in_voice: state.voice.participants(),
        }
    }
}

impl LobbyState {
    fn ready(&self, user: &UserId, conn: ConnId, history: HistoryRead) -> Ready {
        let mut members: Vec<Member> = self
            .members
            .values()
            .map(|row| {
                row.profile
                    .member(row.presence, self.voice.view(&row.profile.id))
            })
            .collect();
        members.sort_by_cached_key(|m| m.username.as_str().to_lowercase());
        let mut access_requests: Vec<AccessRequest> =
            self.access_requests.values().cloned().collect();
        access_requests.sort_by(|a, b| {
            AccessRequest::list_order(a.created_at, &a.id)
                .cmp(&AccessRequest::list_order(b.created_at, &b.id))
        });
        Ready {
            conn_id: conn.0.to_string(),
            me: user.clone(),
            email: self.members[user].profile.email.clone(),
            lobby: self.info.clone(),
            members,
            access_requests,
            last_message_seq: self.last_message_seq,
            last_change_rev: self.last_change_rev,
            history: history.history,
            history_rev: history.rev,
        }
    }

    fn revoke(&mut self, what: Revoked, now: Instant) {
        while self
            .revocations
            .front()
            .is_some_and(|r| now.saturating_duration_since(r.at) >= REVOCATION_MEMORY)
        {
            self.revocations.pop_front();
        }
        self.epoch += 1;
        self.revocations.push_back(Revocation {
            epoch: self.epoch,
            at: now,
            what,
        });
    }

    fn revoked_since(&self, epoch: u64, session: SessionId, user: &UserId) -> bool {
        self.revocations
            .iter()
            .rev()
            .take_while(|r| r.epoch > epoch)
            .any(|r| r.what.covers(user, session))
    }

    /// Revokes `what` and ends the Voice of every connection it covers, which are about to close:
    /// `VOICE_ENDED{kicked}` is queued before the close, which flushes it.
    fn revoke_and_end_voice(&mut self, what: Revoked) -> Vec<(UserId, ConnId)> {
        let covered: Vec<(UserId, ConnId)> = self
            .conns
            .iter()
            .filter(|(user, c)| what.covers(user, c.session))
            .map(|(user, c)| (user.clone(), c.id))
            .collect();
        self.revoke(what, Instant::now());
        for (user, conn) in &covered {
            self.conn_gone(user, *conn, VoiceEndReason::Kicked, Instant::now());
        }
        covered
    }

    fn kick(&mut self, what: Revoked) {
        let why = CloseReason::SessionRevoked;
        let kind = match what {
            Revoked::Session(_) => "session",
            Revoked::User(_) => "user",
        };
        for (user, conn) in self.revoke_and_end_voice(what) {
            info!(%user, conn = conn.0, ?why, "{kind} kicked");
            if let Some(c) = self.conns.get(&user) {
                c.kick.kick(why);
            }
        }
    }

    fn conn(&self, user: &UserId, conn: ConnId) -> Option<&Conn> {
        self.conns.get(user).filter(|c| c.id == conn)
    }

    fn member_joined(&mut self, profile: Profile) {
        let id = profile.id.clone();
        info!(user = %id, username = %profile.username, "member joined");
        let row = MemberRow::new(profile, self.send_every);
        self.members.insert(id.clone(), row);
        self.member_changed(&id);
    }

    fn access_request_removed(
        &mut self,
        id: &AccessRequestId,
        outcome: AccessRequestOutcome,
        by: &UserId,
        at: UnixMs,
    ) {
        self.access_requests.remove(id);
        self.fan_out(&ServerMsg::AccessRequestRemoved(AccessRequestRemoved {
            id: id.clone(),
            outcome,
            handled_by: by.clone(),
            handled_at: at,
        }));
    }

    /// Broadcasts `MEMBER_UPSERT` only on a change.
    fn set_presence(&mut self, user: &UserId, presence: Presence) -> bool {
        let Some(row) = self.members.get_mut(user) else {
            return false;
        };
        if row.presence == presence {
            return false;
        }
        row.presence = presence;
        debug!(%user, ?presence, "presence changed");
        self.member_changed(user);
        true
    }

    fn member_changed(&mut self, user: &UserId) {
        let Some(row) = self.members.get(user) else {
            return;
        };
        let member = row.profile.member(row.presence, self.voice.view(user));
        self.fan_out(&ServerMsg::MemberUpsert { member });
    }

    fn conn_gone(&mut self, user: &UserId, conn: ConnId, reason: VoiceEndReason, now: Instant) {
        let gone = VoiceInput::ConnGone {
            user: user.clone(),
            conn,
            reason,
        };
        self.voice_step(gone, now);
    }

    fn drop_conn(&mut self, user: &UserId, reason: VoiceEndReason) {
        let Some(conn) = self.conns.remove(user) else {
            return;
        };
        self.conn_gone(user, conn.id, reason, Instant::now());
        if self.draining {
            if let Some(row) = self.members.get_mut(user) {
                row.presence = Presence::Offline;
            }
        } else {
            let _ = self.set_presence(user, Presence::Offline);
        }
    }

    fn voice_step(&mut self, input: VoiceInput, now: Instant) {
        let mut queue: VecDeque<VoiceEffect> = self.voice.step(input, now).into();
        while let Some(effect) = queue.pop_front() {
            match effect {
                VoiceEffect::Sfu(cmd) => {
                    if let Err(SfuCmd { user, vsid, op }) = self.sfu.try_send(cmd) {
                        error!(%user, vsid = vsid.get(), "sfu command queue full or stopped");
                        // A refused Leave needs nothing more: the session is already over.
                        if op != SfuOp::Leave {
                            let input = VoiceInput::SfuRefused { user, vsid };
                            queue.extend(self.voice.step(input, now));
                        }
                    }
                }
                VoiceEffect::MemberChanged(user) => self.member_changed(&user),
                VoiceEffect::Speaking { user, speaking } => {
                    self.fan_out(&ServerMsg::VoiceSpeaking(VoiceSpeaking {
                        user_id: user,
                        speaking,
                    }));
                }
                VoiceEffect::ToConn { user, conn, msg } => {
                    self.send_to(&user, conn, Utf8Bytes::from(msg.encode()));
                }
                VoiceEffect::Refused {
                    user,
                    conn,
                    rid,
                    refusal,
                } => {
                    let error = refusal::voice(&refusal, rid, now);
                    let msg = ServerMsg::Error(error);
                    self.send_to(&user, conn, Utf8Bytes::from(msg.encode()));
                }
                VoiceEffect::Ink { to, relay } => {
                    let frame = Utf8Bytes::from(ServerMsg::Ink(relay).encode());
                    let now = Instant::now();
                    for (user, conn) in to {
                        if let Some(c) = self.conn(&user, conn) {
                            c.ink.push(frame.clone(), now);
                        }
                    }
                }
            }
        }
    }

    /// Serialized once, then `try_send` to every connection. A full queue means a consumer
    /// that stopped reading: it is kicked with 4008 and leaves like any other connection.
    fn fan_out(&mut self, msg: &ServerMsg) {
        self.fan_out_to(msg, None);
    }

    fn fan_out_except(&mut self, except: &UserId, msg: &ServerMsg) {
        self.fan_out_to(msg, Some(except));
    }

    fn fan_out_to(&mut self, msg: &ServerMsg, except: Option<&UserId>) {
        let frame = Utf8Bytes::from(msg.encode());
        let mut gone = Vec::new();
        for (user, conn) in self.conns.iter().filter(|(u, _)| Some(*u) != except) {
            match conn.tx.try_send(frame.clone()) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    warn!(%user, conn = conn.id.0, "slow consumer");
                    conn.kick.kick(CloseReason::SlowConsumer);
                    gone.push(user.clone());
                }
                Err(mpsc::error::TrySendError::Closed(_)) => gone.push(user.clone()),
            }
        }
        for user in gone {
            self.drop_conn(&user, VoiceEndReason::ConnectionLost);
        }
    }

    fn send_to(&mut self, user: &UserId, conn: ConnId, frame: Utf8Bytes) {
        let Some(c) = self.conn(user, conn) else {
            return;
        };
        if c.tx.try_send(frame).is_err() {
            c.kick.kick(CloseReason::SlowConsumer);
            self.drop_conn(user, VoiceEndReason::ConnectionLost);
        }
    }
}

/// A Lobby of `profiles` (the first is the owner) that talks to the returned SFU inbox.
#[cfg(test)]
pub(crate) fn test_lobby(profiles: &[Profile]) -> (Lobby, crate::sfu::SfuInbox) {
    use lobby_protocol::{LobbyInstanceId, PROTOCOL_VERSION};

    let (sfu, inbox) = crate::sfu::channel();
    let caps = VoiceConfig {
        max_participants: 25,
        max_sharers: 4,
    };
    let seed = Seed {
        info: LobbyInfo {
            instance_id: LobbyInstanceId::random(),
            name: "Test".into(),
            icon: None,
            upload_max_bytes: 1,
            magic_code_sends: 3,
            join_requests_enabled: true,
            set_up: !profiles.is_empty(),
            owner: profiles.first().map(|p| p.id.clone()),
            protocol_version: PROTOCOL_VERSION,
            server_version: "0".into(),
        },
        profiles: profiles.to_vec(),
        access_requests: Vec::new(),
        last_message_seq: 7,
        last_change_rev: 0,
        send_every: Duration::from_millis(200),
    };
    (Lobby::new(seed, sfu, caps), inbox)
}

#[cfg(test)]
mod tests;
