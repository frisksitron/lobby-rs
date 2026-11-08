//! The Voice state machine: pure, inside the `Lobby` mutex. `step` takes an input and `now` and
//! returns effects, which `Lobby` executes in the same critical section, so a state change and the
//! events describing it are never apart.
//!
//! Sessions go NotInVoice → Joining → Active → NotInVoice, with generations (`VoiceSessionId`), a
//! 12 s join watchdog, a participant cap and mute. An Active session may share its screen (Pending
//! until its video reaches the SFU, then Streaming and public; a share Pending for 30 s ends the
//! session) and watch one share. The join and unmute cooldowns are per Member and outlive a
//! session and a reconnect.

use std::{collections::HashMap, time::Duration};

use lobby_protocol::{
    Ack, DrawingEpoch, InkBatch, InkRelay, Quality, Rid, RtcOffer, ScreenShare, Sdp, ServerMsg,
    ShareId, ShareStart, SourceSize, UserId, VoiceEndReason, VoiceEnded, VoiceSessionId,
    VoiceState, WatchDropped,
};
use tokio::time::Instant;

use crate::{
    config::VoiceConfig,
    limits::Cooldown,
    sfu::{SfuCmd, SfuEvent, SfuEventKind, SfuOp},
    tiers::ConnId,
};

/// A session still Joining this long after `VOICE_JOIN` is ended.
pub const JOIN_WATCHDOG: Duration = Duration::from_secs(12);
/// A Screen Share still Pending this long after `SCREEN_SHARE_START`, with no video at the SFU,
/// ends the session the way a Joining one does. It holds one of the few sharer slots unseen.
pub const SHARE_WATCHDOG: Duration = Duration::from_secs(30);
/// The 5th real unmute or undeafen within 5 s is refused and starts a 10 s cooldown. Muting is
/// never limited.
pub const UNMUTE_LIMIT: usize = 5;
pub const UNMUTE_WINDOW: Duration = Duration::from_secs(5);
pub const UNMUTE_COOLDOWN: Duration = Duration::from_secs(10);
/// The 6th `VOICE_JOIN` within 15 s is refused and starts a 15 s cooldown. Every attempt counts,
/// a refused one included.
pub const JOIN_LIMIT: usize = 6;
pub const JOIN_WINDOW: Duration = Duration::from_secs(15);
pub const JOIN_COOLDOWN: Duration = Duration::from_secs(15);

const CONNECTION_FAILED: &str = "The connection failed. Try joining again.";

/// A refused command, or a session ending in an error. `Display` is the copy the client shows;
/// `ws` maps it to the wire once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum VoiceRefusal {
    #[error("Joined too often. Try again shortly.")]
    JoinCooldown(Instant),
    #[error("Can't join right now. Try again in a moment.")]
    AlreadyInVoice,
    #[error("No room left. Try again when someone leaves.")]
    VoiceFull,
    #[error("Couldn't join. Try again.")]
    JoinFailed,
    #[error("The connection timed out. Try joining again.")]
    Timeout,
    #[error("Your Screen Share never started.")]
    ShareTimeout,
    #[error("Not connected yet. Try again in a moment.")]
    NotConnected,
    #[error("Too many unmutes. Try again shortly.")]
    UnmuteCooldown(Instant),
    #[error("{}", CONNECTION_FAILED)]
    StaleAnswer,
    #[error("{}", CONNECTION_FAILED)]
    NegotiationFailed,
    #[error("ICE restart requires the active voice session")]
    RestartInvalid,
    #[error("Join Voice to share or watch a screen.")]
    NotInVoice,
    #[error("Too many at once. Try again when one ends.")]
    ShareLimit,
    #[error("No longer available. It may have ended.")]
    ShareNotAvailable,
    #[error("You can't watch your own.")]
    WatchSelf,
}

impl VoiceRefusal {
    /// When a cooldown ends and the refused command may be tried again.
    pub(crate) fn retry_at(self) -> Option<Instant> {
        match self {
            Self::JoinCooldown(at) | Self::UnmuteCooldown(at) => Some(at),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShareStage {
    /// No video has reached the SFU yet. The session is ended at `deadline`.
    Pending {
        deadline: Instant,
    },
    Streaming,
}

/// A session's Screen Share. Pending until the SFU reports its first video frame, then
/// Streaming: only a Streaming share shows in snapshots and can be watched. Every start gets a
/// fresh `id`, a restart after a stop included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Share {
    id: ShareId,
    audio: bool,
    source: Option<SourceSize>,
    stage: ShareStage,
    /// "Let viewers draw". `epoch` moves with every change of it and of `source`, so ink drawn
    /// against an older state is stale; it is kept while drawing is off.
    drawing: bool,
    epoch: DrawingEpoch,
    quality: Quality,
}

impl Share {
    fn streaming(&self) -> bool {
        self.stage == ShareStage::Streaming
    }

    fn advance_epoch(&mut self) {
        self.epoch = DrawingEpoch(self.epoch.0.wrapping_add(1));
    }

    /// Whether a batch drawn against `id` and `epoch` is current.
    fn takes_ink(&self, id: ShareId, epoch: DrawingEpoch) -> bool {
        self.streaming() && self.drawing && self.id == id && self.epoch == epoch
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Joining { deadline: Instant },
    Active,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Session {
    id: VoiceSessionId,
    conn: ConnId,
    phase: Phase,
    muted: bool,
    deafened: bool,
    speaking: bool,
    offer_rev: u32,
    share: Option<Share>,
    watching: Option<UserId>,
}

impl Session {
    /// When the watchdog ends this session and what it tells the Member, if it is Joining or its
    /// share is Pending.
    fn watchdog(&self) -> Option<(Instant, VoiceRefusal)> {
        match (self.phase, self.share.map(|share| share.stage)) {
            (Phase::Joining { deadline }, _) => Some((deadline, VoiceRefusal::Timeout)),
            (Phase::Active, Some(ShareStage::Pending { deadline })) => {
                Some((deadline, VoiceRefusal::ShareTimeout))
            }
            (Phase::Active, _) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum VoiceCommand {
    Join {
        muted: bool,
        deafened: bool,
    },
    Leave,
    SetState {
        muted: bool,
        deafened: bool,
    },
    Answer {
        vsid: VoiceSessionId,
        rev: u32,
        sdp: Sdp,
    },
    /// `RTC_RESTART`: the client lost its media path while its WebSocket lives.
    Restart {
        vsid: VoiceSessionId,
    },
    ShareStart(ShareStart),
    /// `SCREEN_SHARE_SOURCE`: applies to the sender's current share.
    ShareSource(SourceSize),
    ShareStop,
    Watch {
        sharer: UserId,
    },
    Unwatch,
    /// `DRAWING_SET`: the Sharer's "Let viewers draw".
    SetDrawing(bool),
    /// `INK`: lossy, so a refused batch is dropped without an answer.
    Ink(InkBatch),
}

impl VoiceCommand {
    /// Every command but the Sharer's reports and `INK` needs a rid, so its terminal outcome can
    /// be matched.
    pub(crate) fn needs_rid(&self) -> bool {
        !matches!(self, VoiceCommand::ShareSource(_) | VoiceCommand::Ink(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum VoiceInput {
    Cmd {
        user: UserId,
        conn: ConnId,
        rid: Option<Rid>,
        cmd: VoiceCommand,
    },
    /// The connection is gone, replaced, kicked or the server is stopping. Ends the session only if
    /// it belongs to `conn`.
    ConnGone {
        user: UserId,
        conn: ConnId,
        reason: VoiceEndReason,
    },
    Sfu(SfuEvent),
    SfuRefused {
        user: UserId,
        vsid: VoiceSessionId,
    },
    Tick,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum VoiceEffect {
    Sfu(SfuCmd),
    MemberChanged(UserId),
    Speaking {
        user: UserId,
        speaking: bool,
    },
    ToConn {
        user: UserId,
        conn: ConnId,
        msg: ServerMsg,
    },
    /// The command `rid` named was refused, or the session ended in an error: the connection is
    /// told, in the wire's words, by `ws`.
    Refused {
        user: UserId,
        conn: ConnId,
        rid: Option<Rid>,
        refusal: VoiceRefusal,
    },
    /// Ink goes to each connection's ink queue, which drops it before it kicks anyone.
    Ink {
        to: Vec<(UserId, ConnId)>,
        relay: InkRelay,
    },
}

#[derive(Debug)]
pub(crate) struct VoiceMachine {
    sessions: HashMap<UserId, Session>,
    next_id: u64,
    next_share_id: u64,
    caps: VoiceConfig,
    /// Per Member, kept across sessions and reconnects, so leaving cannot reset it.
    unmute_cooldowns: HashMap<UserId, Cooldown>,
    join_cooldowns: HashMap<UserId, Cooldown>,
}

/// `deafened ⇒ muted`; a request to unmute while Deafened also undeafens.
pub(crate) fn normalize(currently_deafened: bool, muted: bool, deafened: bool) -> (bool, bool) {
    if !muted && currently_deafened {
        return (false, false);
    }
    if deafened {
        (true, true)
    } else {
        (muted, false)
    }
}

impl VoiceMachine {
    pub fn new(caps: VoiceConfig) -> Self {
        Self {
            sessions: HashMap::new(),
            next_id: 0,
            next_share_id: 0,
            caps,
            unmute_cooldowns: HashMap::new(),
            join_cooldowns: HashMap::new(),
        }
    }

    pub fn view(&self, user: &UserId) -> Option<VoiceState> {
        self.sessions.get(user).map(|s| VoiceState {
            joining: matches!(s.phase, Phase::Joining { .. }),
            muted: s.muted,
            deafened: s.deafened,
            speaking: s.speaking,
            screen_share: s.share.filter(Share::streaming).map(|sh| ScreenShare {
                share_id: sh.id,
                audio: sh.audio,
                source: sh.source,
                drawing: sh.drawing.then_some(sh.epoch),
                quality: sh.quality,
            }),
        })
    }

    #[cfg(test)]
    pub fn watching(&self, user: &UserId) -> Option<&UserId> {
        self.sessions.get(user)?.watching.as_ref()
    }

    fn sharers(&self) -> usize {
        self.sessions.values().filter(|s| s.share.is_some()).count()
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        let deadlines = self.sessions.values().filter_map(Session::watchdog);
        deadlines.map(|(deadline, _)| deadline).min()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn participants(&self) -> usize {
        self.sessions.len()
    }

    pub fn sessions(&self) -> Vec<(UserId, ConnId)> {
        self.sessions
            .iter()
            .map(|(user, s)| (user.clone(), s.conn))
            .collect()
    }

    pub fn step(&mut self, input: VoiceInput, now: Instant) -> Vec<VoiceEffect> {
        let mut fx = Vec::new();
        match input {
            VoiceInput::Cmd {
                user,
                conn,
                rid,
                cmd,
            } => {
                if let Err(refusal) =
                    self.command(user.clone(), conn, rid.clone(), cmd, now, &mut fx)
                {
                    refuse(&mut fx, &user, conn, rid, refusal);
                }
            }
            VoiceInput::ConnGone { user, conn, reason } => {
                if self.sessions.get(&user).is_some_and(|s| s.conn == conn) {
                    self.end(&user, Some(reason), &mut fx);
                }
            }
            VoiceInput::Sfu(ev) => self.sfu_event(ev, &mut fx),
            VoiceInput::SfuRefused { user, vsid } => {
                let Some(s) = self.sessions.get(&user).filter(|s| s.id == vsid) else {
                    return fx;
                };
                if matches!(s.phase, Phase::Joining { .. }) {
                    let conn = s.conn;
                    refuse(&mut fx, &user, conn, None, VoiceRefusal::JoinFailed);
                }
                self.end(&user, Some(VoiceEndReason::Failed), &mut fx);
            }
            VoiceInput::Tick => {
                let expired: Vec<(UserId, ConnId, VoiceRefusal)> = self
                    .sessions
                    .iter()
                    .filter_map(|(user, s)| {
                        let (deadline, refusal) = s.watchdog()?;
                        (deadline <= now).then(|| (user.clone(), s.conn, refusal))
                    })
                    .collect();
                for (user, conn, refusal) in expired {
                    refuse(&mut fx, &user, conn, None, refusal);
                    self.end(&user, Some(VoiceEndReason::Timeout), &mut fx);
                }
            }
        }
        fx
    }

    fn owned(&mut self, user: &UserId, conn: ConnId) -> Option<&mut Session> {
        self.sessions.get_mut(user).filter(|s| s.conn == conn)
    }

    /// The session `conn` owns, once it is Active.
    fn active(&mut self, user: &UserId, conn: ConnId) -> Option<&mut Session> {
        self.owned(user, conn).filter(|s| s.phase == Phase::Active)
    }

    fn command(
        &mut self,
        user: UserId,
        conn: ConnId,
        rid: Option<Rid>,
        cmd: VoiceCommand,
        now: Instant,
        fx: &mut Vec<VoiceEffect>,
    ) -> Result<(), VoiceRefusal> {
        match cmd {
            VoiceCommand::Join { muted, deafened } => {
                self.join_cooldowns
                    .entry(user.clone())
                    .or_insert_with(|| Cooldown::new(JOIN_LIMIT, JOIN_WINDOW, JOIN_COOLDOWN))
                    .try_acquire(now)
                    .map_err(VoiceRefusal::JoinCooldown)?;
                if self.sessions.contains_key(&user) {
                    return Err(VoiceRefusal::AlreadyInVoice);
                }
                if self.sessions.len() >= self.caps.max_participants as usize {
                    return Err(VoiceRefusal::VoiceFull);
                }
                self.next_id += 1;
                let id = VoiceSessionId::new(self.next_id).expect("ids start at 1");
                let (muted, deafened) = normalize(false, muted, deafened);
                self.sessions.insert(
                    user.clone(),
                    Session {
                        id,
                        conn,
                        phase: Phase::Joining {
                            deadline: now + JOIN_WATCHDOG,
                        },
                        muted,
                        deafened,
                        speaking: false,
                        offer_rev: 0,
                        share: None,
                        watching: None,
                    },
                );
                fx.push(VoiceEffect::MemberChanged(user.clone()));
                if let Some(rid) = rid {
                    let ack = Ack {
                        voice_session_id: Some(id),
                        ..Ack::new(rid)
                    };
                    to_conn(fx, &user, conn, ServerMsg::Ack(ack));
                }
                sfu(fx, &user, id, SfuOp::Join { muted });
            }
            VoiceCommand::Leave => {
                if self.owned(&user, conn).is_some() {
                    self.end(&user, None, fx);
                }
                ack(fx, &user, conn, rid);
            }
            VoiceCommand::SetState { muted, deafened } => {
                let (was_muted, was_deafened) = self
                    .active(&user, conn)
                    .map(|s| (s.muted, s.deafened))
                    .ok_or(VoiceRefusal::NotConnected)?;
                let (muted, deafened) = normalize(was_deafened, muted, deafened);
                // Only real unmute and undeafen transitions count.
                if (was_muted && !muted) || (was_deafened && !deafened) {
                    self.unmute_cooldowns
                        .entry(user.clone())
                        .or_insert_with(|| {
                            Cooldown::new(UNMUTE_LIMIT, UNMUTE_WINDOW, UNMUTE_COOLDOWN)
                        })
                        .try_acquire(now)
                        .map_err(VoiceRefusal::UnmuteCooldown)?;
                }
                let s = self
                    .owned(&user, conn)
                    .expect("active session is still owned");
                let mute_changed = s.muted != muted;
                let changed = mute_changed || s.deafened != deafened;
                s.muted = muted;
                s.deafened = deafened;
                if muted {
                    s.speaking = false;
                }
                let vsid = s.id;
                if mute_changed {
                    sfu(fx, &user, vsid, SfuOp::SetMuted { muted });
                }
                if changed {
                    fx.push(VoiceEffect::MemberChanged(user.clone()));
                }
                if let Some(rid) = rid {
                    let ack = Ack {
                        muted: Some(muted),
                        deafened: Some(deafened),
                        ..Ack::new(rid)
                    };
                    to_conn(fx, &user, conn, ServerMsg::Ack(ack));
                }
            }
            VoiceCommand::Answer { vsid, rev, sdp } => {
                let valid = self
                    .owned(&user, conn)
                    .is_some_and(|s| s.id == vsid && s.offer_rev != 0 && s.offer_rev == rev);
                if !valid {
                    return Err(VoiceRefusal::StaleAnswer);
                }
                sfu(fx, &user, vsid, SfuOp::Answer { rev, sdp, rid });
            }
            VoiceCommand::Restart { vsid } => {
                if self.active(&user, conn).is_none_or(|s| s.id != vsid) {
                    return Err(VoiceRefusal::RestartInvalid);
                }
                sfu(fx, &user, vsid, SfuOp::Restart);
                ack(fx, &user, conn, rid);
            }
            VoiceCommand::ShareStart(start) => self.share_start(user, conn, rid, start, now, fx)?,
            VoiceCommand::ShareSource(size) => {
                if let Some(s) = self.owned(&user, conn)
                    && let Some(share) = s.share.as_mut()
                    && share.source != Some(size)
                {
                    share.source = Some(size);
                    share.advance_epoch();
                    if share.streaming() {
                        fx.push(VoiceEffect::MemberChanged(user.clone()));
                    }
                    let vsid = s.id;
                    sfu(fx, &user, vsid, SfuOp::ShareSource { size });
                }
                ack(fx, &user, conn, rid);
            }
            VoiceCommand::ShareStop => {
                if let Some(s) = self.owned(&user, conn)
                    && let Some(share) = s.share.take()
                {
                    let vsid = s.id;
                    self.stop_watchers(&user);
                    if share.streaming() {
                        fx.push(VoiceEffect::MemberChanged(user.clone()));
                    }
                    sfu(fx, &user, vsid, SfuOp::StopShare);
                }
                ack(fx, &user, conn, rid);
            }
            VoiceCommand::Watch { sharer } => self.watch(user, conn, rid, sharer, fx)?,
            VoiceCommand::Unwatch => {
                if let Some(s) = self.owned(&user, conn)
                    && s.watching.take().is_some()
                {
                    let vsid = s.id;
                    sfu(fx, &user, vsid, SfuOp::Unwatch);
                }
                ack(fx, &user, conn, rid);
            }
            VoiceCommand::SetDrawing(enabled) => {
                let s = self.active(&user, conn).ok_or(VoiceRefusal::NotInVoice)?;
                let share = s.share.as_mut().ok_or(VoiceRefusal::ShareNotAvailable)?;
                if share.drawing != enabled {
                    share.drawing = enabled;
                    share.advance_epoch();
                    if share.streaming() {
                        fx.push(VoiceEffect::MemberChanged(user.clone()));
                    }
                }
                ack(fx, &user, conn, rid);
            }
            VoiceCommand::Ink(batch) => self.ink(&user, conn, batch, fx),
        }
        Ok(())
    }

    /// `INK`: relayed to the Sharer and the Watchers of the share, never back to the author.
    /// Anything that does not fit the share's current state is dropped silently.
    fn ink(&self, author: &UserId, conn: ConnId, batch: InkBatch, fx: &mut Vec<VoiceEffect>) {
        let Some(me) = self
            .sessions
            .get(author)
            .filter(|s| s.conn == conn && s.phase == Phase::Active)
        else {
            return;
        };
        // A Sharer may watch another share meanwhile: the batch's share id says which one.
        let sharer = if me.share.is_some_and(|sh| sh.id == batch.share_id) {
            author
        } else if let Some(watched) = &me.watching {
            watched
        } else {
            return;
        };
        let current = self
            .sessions
            .get(sharer)
            .and_then(|s| s.share)
            .is_some_and(|sh| sh.takes_ink(batch.share_id, batch.epoch));
        if !current {
            return;
        }
        let to: Vec<(UserId, ConnId)> = self
            .sessions
            .iter()
            .filter(|(user, s)| {
                *user != author
                    && s.phase == Phase::Active
                    && (*user == sharer || s.watching.as_ref() == Some(sharer))
            })
            .map(|(user, s)| (user.clone(), s.conn))
            .collect();
        if to.is_empty() {
            return;
        }
        let relay = InkRelay {
            author_id: author.clone(),
            batch,
        };
        fx.push(VoiceEffect::Ink { to, relay });
    }

    /// `SCREEN_SHARE_START`: Pending until the SFU sees video; starting again only updates `audio`.
    fn share_start(
        &mut self,
        user: UserId,
        conn: ConnId,
        rid: Option<Rid>,
        ShareStart { audio, quality }: ShareStart,
        now: Instant,
        fx: &mut Vec<VoiceEffect>,
    ) -> Result<(), VoiceRefusal> {
        let sharers = self.sharers();
        let cap = self.caps.max_sharers as usize;
        let fresh = ShareId::new(self.next_share_id + 1).expect("ids start at 1");
        let s = self.active(&user, conn).ok_or(VoiceRefusal::NotInVoice)?;
        if let Some(share) = s.share.as_mut() {
            let changed = share.audio != audio;
            share.audio = audio;
            if changed && share.streaming() {
                fx.push(VoiceEffect::MemberChanged(user.clone()));
            }
            ack(fx, &user, conn, rid);
            return Ok(());
        }
        if sharers >= cap {
            return Err(VoiceRefusal::ShareLimit);
        }
        s.share = Some(Share {
            id: fresh,
            audio,
            source: None,
            stage: ShareStage::Pending {
                deadline: now + SHARE_WATCHDOG,
            },
            drawing: false,
            epoch: DrawingEpoch(0),
            quality,
        });
        let vsid = s.id;
        self.next_share_id = fresh.get();
        ack(fx, &user, conn, rid);
        sfu(fx, &user, vsid, SfuOp::StartShare { quality });
        Ok(())
    }

    /// `SCREEN_SHARE_WATCH`: only a Streaming share of someone else; it replaces the one watched so
    /// far.
    fn watch(
        &mut self,
        user: UserId,
        conn: ConnId,
        rid: Option<Rid>,
        sharer: UserId,
        fx: &mut Vec<VoiceEffect>,
    ) -> Result<(), VoiceRefusal> {
        let streaming = self
            .sessions
            .get(&sharer)
            .is_some_and(|s| s.share.is_some_and(|sh| sh.streaming()));
        let s = self.active(&user, conn).ok_or(VoiceRefusal::NotInVoice)?;
        if sharer == user {
            return Err(VoiceRefusal::WatchSelf);
        }
        if !streaming {
            return Err(VoiceRefusal::ShareNotAvailable);
        }
        s.watching = Some(sharer.clone());
        let vsid = s.id;
        sfu(fx, &user, vsid, SfuOp::Watch { sharer });
        ack(fx, &user, conn, rid);
        Ok(())
    }

    /// The share of `sharer` is gone: nobody watches it any more. The SFU re-points the
    /// Viewers' downlinks itself (`StopShare`, `Leave`).
    fn stop_watchers(&mut self, sharer: &UserId) {
        for s in self.sessions.values_mut() {
            if s.watching.as_ref() == Some(sharer) {
                s.watching = None;
            }
        }
    }

    fn sfu_event(&mut self, ev: SfuEvent, fx: &mut Vec<VoiceEffect>) {
        let SfuEvent { user, vsid, kind } = ev;
        let Some(s) = self.sessions.get_mut(&user).filter(|s| s.id == vsid) else {
            if let SfuEventKind::Offer { .. } = kind {
                sfu(fx, &user, vsid, SfuOp::Leave);
            }
            return;
        };
        let conn = s.conn;
        match kind {
            SfuEventKind::Offer { rev, sdp, tracks } => {
                s.offer_rev = rev;
                let offer = RtcOffer {
                    voice_session_id: vsid,
                    rev,
                    sdp,
                    tracks,
                };
                to_conn(fx, &user, conn, ServerMsg::RtcOffer(offer));
            }
            SfuEventKind::AnswerApplied { rid } => ack(fx, &user, conn, rid),
            SfuEventKind::NegotiationFailed { rid, fatal } => {
                refuse(fx, &user, conn, rid, VoiceRefusal::NegotiationFailed);
                if fatal {
                    self.end(&user, Some(VoiceEndReason::Failed), fx);
                }
            }
            SfuEventKind::Connected => {
                if matches!(s.phase, Phase::Joining { .. }) {
                    s.phase = Phase::Active;
                    fx.push(VoiceEffect::MemberChanged(user));
                }
            }
            SfuEventKind::WatchDropped {
                frames,
                downlink_bps,
            } => {
                let dropped = WatchDropped {
                    frames,
                    downlink_bps,
                };
                to_conn(fx, &user, conn, ServerMsg::WatchDropped(dropped));
            }
            SfuEventKind::Speaking(speaking) => {
                if s.phase == Phase::Active && !s.muted && s.speaking != speaking {
                    s.speaking = speaking;
                    fx.push(VoiceEffect::Speaking { user, speaking });
                }
            }
            SfuEventKind::Lost => self.end(&user, Some(VoiceEndReason::ConnectionLost), fx),
            SfuEventKind::ShareVideoStarted => {
                if let Some(share) = s.share.as_mut()
                    && !share.streaming()
                {
                    share.stage = ShareStage::Streaming;
                    fx.push(VoiceEffect::MemberChanged(user));
                }
            }
        }
    }

    /// Ends `user`'s session: the SFU frees the peer, everyone sees the Member leave Voice, and
    /// unless `reason == None` its connection gets `VOICE_ENDED`.
    fn end(&mut self, user: &UserId, reason: Option<VoiceEndReason>, fx: &mut Vec<VoiceEffect>) {
        let Some(s) = self.sessions.remove(user) else {
            return;
        };
        // The SFU drops the share and the watch with the peer.
        self.stop_watchers(user);
        sfu(fx, user, s.id, SfuOp::Leave);
        fx.push(VoiceEffect::MemberChanged(user.clone()));
        if let Some(reason) = reason {
            let ended = VoiceEnded {
                voice_session_id: s.id,
                reason,
            };
            to_conn(fx, user, s.conn, ServerMsg::VoiceEnded(ended));
        }
    }
}

fn to_conn(fx: &mut Vec<VoiceEffect>, user: &UserId, conn: ConnId, msg: ServerMsg) {
    fx.push(VoiceEffect::ToConn {
        user: user.clone(),
        conn,
        msg,
    });
}

fn sfu(fx: &mut Vec<VoiceEffect>, user: &UserId, vsid: VoiceSessionId, op: SfuOp) {
    fx.push(VoiceEffect::Sfu(SfuCmd {
        user: user.clone(),
        vsid,
        op,
    }));
}

fn ack(fx: &mut Vec<VoiceEffect>, user: &UserId, conn: ConnId, rid: Option<Rid>) {
    if let Some(rid) = rid {
        to_conn(fx, user, conn, ServerMsg::Ack(Ack::new(rid)));
    }
}

fn refuse(
    fx: &mut Vec<VoiceEffect>,
    user: &UserId,
    conn: ConnId,
    rid: Option<Rid>,
    refusal: VoiceRefusal,
) {
    fx.push(VoiceEffect::Refused {
        user: user.clone(),
        conn,
        rid,
        refusal,
    });
}

#[cfg(test)]
mod tests;
