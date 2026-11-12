//! Voice, Screen Share and Watch: the Session's side of one call.

use std::{
    collections::BTreeSet,
    sync::Arc,
    time::{Duration, Instant},
};

use lobby_protocol::{
    Ack, ClientFrame, ClientMsg, ErrorBody, ErrorCode, Quality, RtcAnswer, RtcOffer, RtcRestart,
    ServerMsg, ShareSource, ShareStart, ShareWatch, SourceSize, UserId, VoiceEndReason, VoiceEnded,
    VoiceFlags, VoiceSessionId, WatchDropped,
};
use tracing::{debug, info, warn};

use super::recovery::{PathRecovery, Step};
use super::strain::Strain;
use super::watch::Watch;
use super::{Command, Session, UiEffect};
use crate::{
    ConnError, Phase, copy,
    issues::IssueKind,
    media::{
        DEVICE_START_TIMEOUT, DeviceChoice, DeviceKind, MediaConfig, MediaError, OwnShareFeed,
        PathState, ScreenSource, ShareOpId, VideoFeed, VoiceCallId, VoiceEvent, sounds::Sound,
    },
    state::{ShareUi, VoicePhase, VoiceUi},
    volume::Volume,
};

pub(crate) const OFFER_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) struct Ended {
    kind: IssueKind,
    message: String,
}

impl Ended {
    pub(super) fn voice(message: impl Into<String>) -> Ended {
        Ended {
            kind: IssueKind::VoiceEnded,
            message: message.into(),
        }
    }

    pub(super) fn media(e: &MediaError) -> Ended {
        let (kind, message) = copy::media_failure(e);
        Ended { kind, message }
    }
}

/// Why the Session refused a Voice or Screen Share action itself: the words of its Issue.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(super) enum VoiceError {
    #[error("{}", copy::NOT_CONNECTED)]
    NotConnected,
    #[error("You have already joined.")]
    AlreadyInVoice,
    #[error("You have not joined.")]
    NotInVoice,
    #[error("{}", copy::VOICE_NOT_ACTIVE)]
    NotActive,
    #[error("{}", copy::SHARE_NOT_AVAILABLE)]
    ShareNotAvailable,
    #[error("{}", copy::SENDING_TOO_FAST)]
    TooFast,
    #[error("{0}")]
    Media(MediaError),
}

fn not_running() -> MediaError {
    MediaError::Transport("voice media is not running".into())
}

fn share_refusal(e: &VoiceError) -> String {
    match e {
        VoiceError::NotInVoice => copy::SHARE_NEEDS_VOICE.to_owned(),
        e => e.to_string(),
    }
}

impl From<ConnError> for VoiceError {
    fn from(e: ConnError) -> Self {
        match e {
            ConnError::NotConnected => VoiceError::NotConnected,
            ConnError::QueueFull => VoiceError::TooFast,
        }
    }
}

/// What lives only as long as one call. Ending the call drops it, and with it the share's
/// own view.
pub(super) struct VoiceCall {
    id: VoiceCallId,
    stage: CallStage,
    /// Until the devices report ready: `VOICE_JOIN` goes out beside their opening, and a call
    /// whose devices fail or take too long leaves again.
    devices_due: Option<Instant>,
    path: PathRecovery,
    /// This Member's VAD verdict, and the other Members speaking in what plays.
    speaking: bool,
    heard: Vec<UserId>,
    /// Whether the microphone was muted when Deafen went on: Undeafen goes back to that.
    muted_before_deafen: bool,
    share: Option<LocalShare>,
    watch: Option<Watch>,
    /// Asks of shares that ended before the answer: a refusal still says why.
    unanswered: Vec<ShareOpId>,
}

#[derive(Debug, Clone, Copy)]
enum CallStage {
    JoinSent {
        offer_due: Instant,
    },
    AwaitingOffer {
        vsid: VoiceSessionId,
        offer_due: Instant,
    },
    /// The roster, not the offer, says when the server has the session Active.
    Offered {
        vsid: VoiceSessionId,
    },
}

/// While this Member shares: where the encoder's copy of what is sent goes, and what the server
/// has heard of the source's size.
struct LocalShare {
    id: ShareOpId,
    own: Arc<OwnShareFeed>,
    source: SourceReports,
    strain: Strain,
}

/// A window resized by dragging changes size with nearly every picture. Each size the server
/// hears moves the drawing epoch and clears the Viewers' boards, and spends the share budget
/// (40 commands in 10 s): it hears at most one this often, the newest.
const SOURCE_REPORT_SPACING: Duration = Duration::from_millis(500);

/// The source sizes sent to the server: the first at once, then at most one per
/// [`SOURCE_REPORT_SPACING`], the newest held until it may go.
#[derive(Debug, Default)]
struct SourceReports {
    told: Option<SourceSize>,
    tried_at: Option<Instant>,
    held: Option<SourceSize>,
}

impl SourceReports {
    /// The capture's size now. Returns what to send at once.
    fn heard(&mut self, size: SourceSize, now: Instant) -> Option<SourceSize> {
        self.held = (self.told != Some(size)).then_some(size);
        self.due(now)
    }

    fn sent(&mut self, size: SourceSize, went: bool) {
        if went {
            self.told = Some(size);
        } else {
            self.held = Some(size);
        }
    }

    /// The held size, once it may go.
    fn due(&mut self, now: Instant) -> Option<SourceSize> {
        if self.deadline().is_some_and(|at| at > now) {
            return None;
        }
        let size = self.held.take()?;
        self.tried_at = Some(now);
        Some(size)
    }

    fn deadline(&self) -> Option<Instant> {
        self.held?;
        self.tried_at.map(|at| at + SOURCE_REPORT_SPACING)
    }
}

/// The view of one's own share ends with the share, before the encoder (which holds the feed
/// too) has stopped.
impl Drop for LocalShare {
    fn drop(&mut self) {
        self.own.close();
    }
}

impl VoiceCall {
    fn vsid(&self) -> Option<VoiceSessionId> {
        match self.stage {
            CallStage::JoinSent { .. } => None,
            CallStage::AwaitingOffer { vsid, .. } | CallStage::Offered { vsid } => Some(vsid),
        }
    }

    /// The controls show Joining until the ACK names the Voice session.
    fn phase(&self) -> VoicePhase {
        match self.vsid() {
            None => VoicePhase::Joining,
            Some(vsid) => VoicePhase::InVoice {
                vsid,
                reconnecting: self.path.reconnecting(),
            },
        }
    }

    pub(super) fn next_deadline(&self) -> Option<Instant> {
        let stage = match self.stage {
            CallStage::JoinSent { offer_due } | CallStage::AwaitingOffer { offer_due, .. } => {
                Some(offer_due)
            }
            CallStage::Offered { .. } => None,
        };
        let source = self.share.as_ref().and_then(|s| s.source.deadline());
        stage
            .into_iter()
            .chain(self.devices_due)
            .chain(self.path.deadline())
            .chain(source)
            .min()
    }
}

impl Session {
    /// A Join, mute or deafen refused before it reached the Lobby is one Issue here. A media
    /// fault is Voice ended, with its own words.
    fn raise_refusal(&mut self, result: Result<(), VoiceError>) {
        if let Err(e) = &result {
            let (kind, message) = match e {
                VoiceError::Media(media) => copy::media_failure(media),
                e => (IssueKind::VoiceRefused, e.to_string()),
            };
            self.raise(kind, message);
        }
    }

    pub(super) fn on_voice_event(&mut self, call: VoiceCallId, ev: VoiceEvent, now: Instant) {
        let Some(current) = self.call.as_mut().filter(|c| c.id == call) else {
            debug!(event = ?ev, "event from an ended voice session");
            return;
        };
        let vsid = current.vsid();
        match ev {
            VoiceEvent::DevicesReady => {
                current.devices_due = None;
            }
            VoiceEvent::DeviceFellBack { kind, wanted } => {
                warn!(?kind, %wanted, "the chosen audio device is gone; using the default");
                let (kind, message) = copy::device_fell_back(kind, &wanted);
                self.raise(kind, message);
            }
            VoiceEvent::Answer {
                vsid: answered,
                rev,
                sdp,
            } if Some(answered) == vsid => {
                let answer = ClientMsg::RtcAnswer(RtcAnswer {
                    voice_session_id: answered,
                    rev,
                    sdp,
                });
                if let Err(e) = self.command(answer, Command::RtcAnswer { call }, now) {
                    warn!(error = %e, "could not send the voice answer");
                }
            }
            VoiceEvent::Answer { .. } => debug!("answer for an older voice session"),
            VoiceEvent::Speaking(speaking) => {
                current.speaking = speaking;
                self.show_speaking();
            }
            VoiceEvent::Speakers(heard) => {
                current.heard = heard;
                self.show_speaking();
            }
            VoiceEvent::TalkingMuted => {
                // The Member may have unmuted in the frames the event was in flight, and a
                // deafened Member chose to be unreachable.
                let voice = &mut self.state.voice;
                if voice.muted && !voice.deafened {
                    voice.mute_nudges += 1;
                }
            }
            VoiceEvent::MicSilent => {
                if !self.state.voice.muted {
                    self.raise(IssueKind::MicSilent, copy::MIC_SILENT);
                }
            }
            VoiceEvent::Path(state) => self.path_step(
                |path| match state {
                    PathState::Down => path.path_down(now),
                    PathState::Up => path.path_up(),
                },
                now,
            ),
            VoiceEvent::NetworkChanged(change) => {
                info!(?change, "the network changed");
                self.path_step(|path| path.network_changed(change, now), now);
            }
            VoiceEvent::Stats(stats) => {
                if let Some(share) = current.share.as_mut() {
                    share.strain.on_stats(now, &stats);
                }
                if let Some(watch) = current.watch.as_mut() {
                    watch.strain.on_stats(now, &stats);
                }
                self.state.voice.stats = Some(stats);
                self.publish_strain();
            }
            VoiceEvent::Failed(e) => {
                warn!(error = %e, "voice media failed");
                self.leave_with(Ended::media(&e), now);
            }
            VoiceEvent::ShareFailed { share, error } => {
                // A capture of an earlier share that ends late never stops the current one.
                if self.is_current_share(call, share) {
                    warn!(%error, "screen share ended");
                    self.send_share_stop(now);
                    self.end_share(Some(error.to_string()));
                }
            }
            VoiceEvent::ShareSource { share, size } => {
                let local = current.share.as_mut().filter(|s| s.id == share);
                if let Some(size) = local.and_then(|s| s.source.heard(size, now)) {
                    self.send_share_source(size);
                }
            }
            VoiceEvent::WatchShows { watch, sharer } => {
                if let Some(w) = self.watch_mut().filter(|w| w.id == watch) {
                    info!(%sharer, "the Watch shows a share");
                    w.shows(sharer);
                    self.settle_watch();
                }
            }
            VoiceEvent::WatchFailed { watch, error } => {
                if self.watch_mut().is_some_and(|w| w.id == watch) {
                    warn!(%error, "the Watch's media failed");
                    self.unwatch(now);
                    self.raise(IssueKind::ShareFailed, copy::WATCH_FAILED);
                }
            }
            VoiceEvent::WatchLoad { watch, load } => {
                if let Some(w) = self.watch_mut().filter(|w| w.id == watch) {
                    w.strain.on_watch_load(now, &load);
                    self.publish_strain();
                }
                #[cfg(feature = "test-support")]
                self.watch_loads.push((now, load));
            }
            VoiceEvent::ShareLoad { share, load } => {
                let local = self
                    .call
                    .as_mut()
                    .and_then(|c| c.share.as_mut())
                    .filter(|s| s.id == share);
                if let Some(local) = local {
                    local.strain.on_share_load(now, &load);
                    let stalled = local.strain.take_exclusive_fullscreen();
                    self.publish_strain();
                    if stalled {
                        self.raise(IssueKind::ShareFailed, copy::SHARE_EXCLUSIVE_FULLSCREEN);
                    }
                }
            }
        }
    }

    fn publish_strain(&mut self) {
        let Some(call) = self.call.as_ref() else {
            return;
        };
        self.state.share.strain = call.share.as_ref().and_then(|s| s.strain.warning());
        if let Some(ui) = self.state.share.watch.as_mut() {
            ui.strain = call.watch.as_ref().and_then(|w| w.strain.warning());
        }
    }

    /// The size of what this Member shares. The server moves the drawing epoch with it, so a
    /// stroke drawn on the old picture ends and the boards clear. A report: nothing answers it.
    fn send_share_source(&mut self, size: SourceSize) {
        let (width, height) = (size.width.get(), size.height.get());
        let frame = ClientFrame::new(ClientMsg::ScreenShareSource(ShareSource { size }));
        let went = self.conn.send(&frame).is_ok();
        info!(width, height, went, "share source size sent");
        if let Some(share) = self.call.as_mut().and_then(|c| c.share.as_mut()) {
            share.source.sent(size, went);
        }
    }

    /// Every change to the call's path recovery goes through here, and so does what the
    /// controls show of it.
    pub(super) fn path_step(
        &mut self,
        step: impl FnOnce(&mut PathRecovery) -> Option<Step>,
        now: Instant,
    ) {
        let Some(call) = self.call.as_mut() else {
            return;
        };
        match step(&mut call.path) {
            None => {}
            Some(Step::Attempt(attempt)) => self.restart_path(attempt, now),
            Some(Step::Recovered) => info!("voice media path back"),
            Some(Step::GiveUp) => {
                warn!("voice media path not recovered");
                let lost = copy::voice_ended(VoiceEndReason::ConnectionLost);
                self.leave_with(Ended::voice(lost), now);
            }
        }
        if let Some(call) = &self.call {
            self.state.voice.phase = call.phase();
        }
    }

    pub(super) fn tick_call(&mut self, now: Instant) {
        let devices_due = self.call.as_ref().and_then(|c| c.devices_due);
        if devices_due.is_some_and(|t| t <= now) {
            warn!("the audio devices did not start within 3 s");
            self.leave_with(Ended::media(&MediaError::DeviceTimeout), now);
        }
        match self.call.as_ref().map(|c| c.stage) {
            Some(
                CallStage::JoinSent { offer_due } | CallStage::AwaitingOffer { offer_due, .. },
            ) if offer_due <= now => {
                warn!("no voice offer within 10 s");
                self.leave_with(Ended::voice(copy::VOICE_TIMED_OUT), now);
            }
            _ => {}
        }
        let held = self
            .call
            .as_mut()
            .and_then(|c| c.share.as_mut())
            .and_then(|s| s.source.due(now));
        if let Some(size) = held {
            self.send_share_source(size);
        }
        self.path_step(|path| path.tick(now), now);
    }

    /// The SFU answers with an offer that restarts ICE; the transport moves to a new socket
    /// first if the route to the SFU changed.
    fn restart_path(&mut self, attempt: u32, now: Instant) {
        let Some(vsid) = self.call.as_ref().and_then(VoiceCall::vsid) else {
            return;
        };
        info!(attempt, "restarting the voice media path");
        let restart = ClientMsg::RtcRestart(RtcRestart {
            voice_session_id: vsid,
        });
        if let Err(e) = self.command(restart, Command::RtcRestart, now) {
            warn!(error = %e, "could not send RTC_RESTART");
        }
    }

    pub(super) fn is_current(&self, call: VoiceCallId) -> bool {
        self.call.as_ref().is_some_and(|c| c.id == call)
    }

    pub(super) fn is_current_share(&self, call: VoiceCallId, share: ShareOpId) -> bool {
        self.call
            .as_ref()
            .is_some_and(|c| c.id == call && c.share.as_ref().is_some_and(|s| s.id == share))
    }

    fn next_share_op(&mut self) -> ShareOpId {
        self.share_ops += 1;
        ShareOpId(self.share_ops)
    }

    pub fn voice_call(&self) -> Option<VoiceCallId> {
        self.call.as_ref().map(|c| c.id)
    }

    pub(super) fn voice_active(&self) -> bool {
        self.call.is_some() && self.state.me_voice().is_some_and(|v| !v.joining)
    }

    /// The call's speaking rings: this Member by its own VAD, the others by what plays.
    fn show_speaking(&mut self) {
        let Some(call) = &self.call else {
            return;
        };
        let mut speaking: BTreeSet<UserId> = call.heard.iter().cloned().collect();
        if call.speaking
            && let Some(me) = &self.state.me
        {
            speaking.insert(me.clone());
        }
        self.state.voice.speaking = speaking;
    }

    /// Joins Voice: the media threads start, the devices open on their own thread, and
    /// `VOICE_JOIN` goes out at once, so opening them costs the join no time and a slow device
    /// never blocks the caller. Devices that fail leave Voice again, which the others see as a
    /// short visit. The controls show Joining until the ACK. `volumes` are how loud the person
    /// saved each other Member.
    pub(super) fn join_voice(
        &mut self,
        media: MediaConfig,
        volumes: impl IntoIterator<Item = (UserId, Volume)>,
        now: Instant,
    ) {
        let joined = self.try_join_voice(media, volumes, now);
        self.raise_refusal(joined);
    }

    fn try_join_voice(
        &mut self,
        media: MediaConfig,
        volumes: impl IntoIterator<Item = (UserId, Volume)>,
        now: Instant,
    ) -> Result<(), VoiceError> {
        if self.state.phase != Phase::Connected {
            return Err(VoiceError::NotConnected);
        }
        if self.call.is_some() {
            return Err(VoiceError::AlreadyInVoice);
        }
        let me = self.state.me.clone().ok_or(VoiceError::NotConnected)?;
        self.voice_calls += 1;
        let id = VoiceCallId(self.voice_calls);
        let path = PathRecovery::new(media.restart);
        self.io
            .start_voice(id, me, media, volumes.into_iter().collect())
            .map_err(|e| {
                warn!(error = %e, "voice media did not start");
                VoiceError::Media(e)
            })?;
        let call = VoiceCall {
            id,
            stage: CallStage::JoinSent {
                offer_due: now + OFFER_TIMEOUT,
            },
            devices_due: Some(now + DEVICE_START_TIMEOUT),
            path,
            speaking: false,
            heard: Vec::new(),
            muted_before_deafen: false,
            share: None,
            watch: None,
            unanswered: Vec::new(),
        };
        self.state.voice.phase = call.phase();
        self.call = Some(call);
        let join = ClientMsg::VoiceJoin(self.voice_flags());
        if let Err(e) = self.command(join, Command::VoiceJoin { call: id }, now) {
            warn!(error = %e, "could not send VOICE_JOIN");
            self.end_call(None);
            return Err(e.into());
        }
        info!("joining voice while the audio devices open");
        Ok(())
    }

    /// Leaves Voice at once: the media stops without waiting for the ACK.
    pub(super) fn leave_voice(&mut self, now: Instant) {
        if self.call.is_none() {
            return;
        }
        info!("leaving voice");
        self.send_leave(now);
        self.end_call(None);
    }

    /// What the controls show and the server is asked to hold.
    fn voice_flags(&self) -> VoiceFlags {
        VoiceFlags {
            muted: self.state.voice.muted,
            deafened: self.state.voice.deafened,
        }
    }

    /// The microphone and the speakers follow `flags` at once, before the server confirms.
    pub(super) fn apply_flags(&mut self, flags: VoiceFlags) {
        let shown = self.voice_flags();
        if let Some(v) = self.io.voice() {
            if flags.muted != shown.muted {
                v.set_muted(flags.muted);
            }
            if flags.deafened != shown.deafened {
                v.set_deafened(flags.deafened);
            }
        }
        self.state.voice.muted = flags.muted;
        self.state.voice.deafened = flags.deafened;
    }

    /// Unmuting while Deafened is an Undeafen too (the server couples them).
    pub(super) fn toggle_mute(&mut self, now: Instant) {
        let flags = VoiceFlags {
            muted: !self.state.voice.muted,
            deafened: false,
        };
        let set = self.send_flags(flags, now);
        self.raise_refusal(set);
    }

    /// Deafen mutes with it; Undeafen goes back to the mute the Member had before.
    pub(super) fn toggle_deafen(&mut self, now: Instant) {
        let set = self.try_set_deafened(!self.state.voice.deafened, now);
        self.raise_refusal(set);
    }

    fn try_set_deafened(&mut self, deafened: bool, now: Instant) -> Result<(), VoiceError> {
        let flags = if deafened {
            if let Some(call) = self.call.as_mut() {
                call.muted_before_deafen = self.state.voice.muted;
            }
            VoiceFlags {
                muted: true,
                deafened: true,
            }
        } else {
            VoiceFlags {
                muted: self.call.as_ref().is_some_and(|c| c.muted_before_deafen),
                deafened: false,
            }
        };
        self.send_flags(flags, now)
    }

    fn send_flags(&mut self, flags: VoiceFlags, now: Instant) -> Result<(), VoiceError> {
        let call = self.voice_call().ok_or(VoiceError::NotInVoice)?;
        if self.state.me_voice().is_none_or(|v| v.joining) {
            return Err(VoiceError::NotActive);
        }
        let previous = self.voice_flags();
        self.command(
            ClientMsg::VoiceStateSet(flags),
            Command::VoiceStateSet { call, previous },
            now,
        )?;
        self.apply_flags(flags);
        // The sound is the click's answer, like the audio and the buttons: it does not wait for
        // the Lobby. Deafen and Undeafen change the mute too, but are one sound.
        let sound = if flags.deafened != previous.deafened {
            if flags.deafened {
                Sound::Deafen
            } else {
                Sound::Undeafen
            }
        } else if flags.muted {
            Sound::Mute
        } else {
            Sound::Unmute
        };
        self.queued.push(UiEffect::Sound(sound));
        Ok(())
    }

    /// How loud `user` plays in the running call. The next call takes the volumes it joins with.
    pub(super) fn set_member_volume(&mut self, user: UserId, volume: Volume) {
        if let Some(v) = self.io.voice() {
            v.set_volume(user, volume);
        }
    }

    pub(super) fn choose_device(&mut self, kind: DeviceKind, choice: DeviceChoice) {
        if let Some(v) = self.io.voice() {
            v.choose_device(kind, choice);
        }
    }

    fn send_leave(&mut self, now: Instant) {
        let _ = self.command(ClientMsg::VoiceLeave, Command::VoiceLeave, now);
    }

    pub(super) fn leave_with(&mut self, why: Ended, now: Instant) {
        if self.call.is_none() {
            return;
        }
        self.send_leave(now);
        self.end_call(Some(why));
    }

    pub(super) fn end_call(&mut self, why: Option<Ended>) {
        let Some(call) = self.call.take() else {
            return;
        };
        self.io.stop_voice();
        self.watch_feed = None;
        self.end_ink();
        self.state.voice = VoiceUi {
            join_after: self.state.voice.join_after,
            ..VoiceUi::default()
        };
        self.state.share = ShareUi::default();
        // The own-share feed closes after Voice is told to stop.
        drop(call);
        if let Some(Ended { kind, message }) = why {
            self.raise(kind, message);
        }
    }

    pub(super) fn start_share(&mut self, source: ScreenSource, quality: Quality, now: Instant) {
        if let Err(e) = self.try_start_share(source, quality, now) {
            self.raise(IssueKind::ShareFailed, share_refusal(&e));
        }
    }

    fn try_start_share(
        &mut self,
        source: ScreenSource,
        quality: Quality,
        now: Instant,
    ) -> Result<(), VoiceError> {
        let call = self.voice_call().ok_or(VoiceError::NotInVoice)?;
        if !self.voice_active() {
            return Err(VoiceError::NotActive);
        }
        if let Some(blocked) = crate::media::share_blocked() {
            return Err(VoiceError::Media(blocked));
        }
        let share = self.next_share_op();
        let own = Arc::new(OwnShareFeed::default());
        self.io
            .voice_mut()
            .ok_or_else(not_running)
            .and_then(|v| v.start_share(source.clone(), quality, share, own.clone()))
            .map_err(|e| {
                warn!(error = %e, "screen share did not start");
                VoiceError::Media(e)
            })?;
        let start = ClientMsg::ScreenShareStart(ShareStart {
            audio: false,
            quality,
        });
        if let Err(e) = self.command(start, Command::ShareStart { call, share }, now) {
            self.stop_share_media();
            return Err(e.into());
        }
        info!(source = %source.name, "sharing");
        self.state.share.sharing = Some(source);
        if let Some(call) = self.call.as_mut() {
            call.share = Some(LocalShare {
                id: share,
                own,
                source: SourceReports::default(),
                strain: Strain::sharer(),
            });
        }
        Ok(())
    }

    pub(super) fn stop_share(&mut self, now: Instant) {
        if self.state.share.sharing.is_none() {
            return;
        }
        info!("stopping the screen share");
        self.send_share_stop(now);
        self.end_share(None);
    }

    pub(super) fn send_share_stop(&mut self, now: Instant) {
        let _ = self.command(ClientMsg::ScreenShareStop, Command::ShareStop, now);
    }

    fn stop_share_media(&mut self) {
        if let Some(v) = self.io.voice_mut() {
            v.stop_share();
        }
    }

    pub(super) fn end_share(&mut self, error: Option<String>) {
        self.stop_share_media();
        self.state.share.sharing = None;
        self.state.share.strain = None;
        if let Some(call) = self.call.as_mut() {
            call.share = None;
        }
        if let Some(error) = error {
            self.raise(IssueKind::ShareFailed, error);
        }
    }

    /// A Watch of `sharer`. The first starts the decoder, whose feed waits for
    /// [`Session::take_watch_feed`]; one while a Watch runs moves it once the server switched.
    pub(super) fn watch(&mut self, sharer: &UserId, now: Instant) {
        if let Err(e) = self.try_watch(sharer, now) {
            self.raise(IssueKind::ShareFailed, share_refusal(&e));
        }
    }

    /// The feed of the Watch just started, once.
    pub fn take_watch_feed(&mut self) -> Option<VideoFeed> {
        self.watch_feed.take()
    }

    fn try_watch(&mut self, sharer: &UserId, now: Instant) -> Result<(), VoiceError> {
        if cfg!(not(windows)) {
            return Err(VoiceError::Media(MediaError::ShareUnsupported));
        }
        let call = self.voice_call().ok_or(VoiceError::NotInVoice)?;
        if !self.voice_active() {
            return Err(VoiceError::NotActive);
        }
        // Known to be gone already: the server would say the same.
        if self.state.share_of(sharer).is_none() {
            return Err(VoiceError::ShareNotAvailable);
        }
        let running = self.call.as_ref().and_then(|c| c.watch.as_ref());
        if running.is_some_and(|w| w.has(sharer)) {
            return Ok(());
        }
        let first = running.is_none();
        let op = self.next_share_op();
        let request = ClientMsg::ScreenShareWatch(ShareWatch {
            sharer_id: sharer.clone(),
        });
        if first {
            let id = self.next_share_op();
            let (tx, paint, feed) = VideoFeed::channel();
            self.io
                .voice_mut()
                .ok_or_else(not_running)
                .and_then(|v| v.start_watch((tx, paint), id, sharer.clone()))
                .map_err(|e| {
                    warn!(error = %e, "watch did not start");
                    VoiceError::Media(e)
                })?;
            if let Err(e) = self.command(request, Command::Watch { call, watch: op }, now) {
                self.stop_watch_media();
                return Err(e.into());
            }
            self.watch_feed = Some(feed);
            if let Some(c) = self.call.as_mut() {
                c.watch = Some(Watch::new(id, op, sharer.clone()));
            }
        } else {
            self.command(request, Command::Watch { call, watch: op }, now)?;
            if let Some(w) = self.watch_mut() {
                w.ask(op, sharer.clone());
            }
        }
        info!(%sharer, first, "watch asked");
        self.settle_watch();
        Ok(())
    }

    fn watch_mut(&mut self) -> Option<&mut Watch> {
        self.call.as_mut()?.watch.as_mut()
    }

    /// After every change to the Watch or the roster: what ended drops out, the media moves to
    /// whom it wants, the snapshot says what the viewer shows, and a Watch with nothing asked or
    /// held is over.
    pub(super) fn settle_watch(&mut self) {
        let state = &self.state;
        if let Some(call) = self.call.as_mut()
            && let Some(w) = call.watch.as_mut()
        {
            let gone = w.keep_streaming(|sharer| state.share_of(sharer).is_some());
            call.unanswered.extend(gone);
        }
        match self.watch_mut().map(|w| w.wanted().cloned()) {
            Some(None) => {
                info!("the Watch is over");
                self.end_watch();
            }
            Some(Some(sharer)) => {
                if let Some(v) = self.io.voice() {
                    v.want_watch(sharer);
                }
            }
            None => {}
        }
        let watch = self.call.as_ref().and_then(|c| c.watch.as_ref());
        self.state.share.watch = watch.and_then(Watch::ui);
        self.follow_ink();
    }

    /// Stop Watching: the server is told, and the decoder stops with its feed.
    pub(super) fn unwatch(&mut self, now: Instant) {
        if self.call.as_ref().is_none_or(|c| c.watch.is_none()) {
            return;
        }
        let _ = self.command(ClientMsg::ScreenShareUnwatch, Command::Unwatch, now);
        self.end_watch();
    }

    /// Whether `op` was an ask of a share that ended first; it is forgotten either way.
    fn forget_ask(&mut self, op: ShareOpId) -> bool {
        let Some(call) = self.call.as_mut() else {
            return false;
        };
        let before = call.unanswered.len();
        call.unanswered.retain(|o| *o != op);
        call.unanswered.len() < before
    }

    fn end_watch(&mut self) {
        self.stop_watch_media();
        if let Some(call) = self.call.as_mut() {
            call.watch = None;
        }
        self.state.share.watch = None;
    }

    fn stop_watch_media(&mut self) {
        self.watch_feed = None;
        if let Some(v) = self.io.voice_mut() {
            v.stop_watch();
        }
    }

    /// The server took Watch `op` of `call`.
    pub(super) fn watch_taken(&mut self, call: VoiceCallId, op: ShareOpId) {
        if !self.is_current(call) || !self.watch_mut().is_some_and(|w| w.accepted(op)) {
            self.forget_ask(op);
            return;
        }
        self.settle_watch();
    }

    /// No answer to Watch `op` of `call` came in time. The answer still counts when it comes;
    /// meanwhile the picture follows the media.
    pub(super) fn watch_timed_out(&mut self, call: VoiceCallId, op: ShareOpId) {
        if self.is_current(call) && self.watch_mut().is_some_and(|w| w.timed_out(op)) {
            warn!("no answer to a Watch in time; it may yet show");
            self.raise(IssueKind::ShareFailed, copy::SHARE_NOT_CONFIRMED);
        }
    }

    /// Where the running share's own feed opens, for the one view that shows it, at up to 720p
    /// and about 30 fps.
    pub(super) fn own_share(&self) -> Option<Arc<OwnShareFeed>> {
        Some(self.call.as_ref()?.share.as_ref()?.own.clone())
    }

    /// Every `VOICE_ENDED` the Lobby sent, the ones for a call already left included.
    #[cfg(feature = "test-support")]
    pub fn voice_ended_seen(&self) -> &[VoiceEnded] {
        &self.voice_ended_seen
    }

    /// Whether this Session's Voice media is running.
    #[cfg(feature = "test-support")]
    pub fn voice_media_running(&self) -> bool {
        self.io.voice().is_some()
    }

    #[cfg(feature = "test-support")]
    pub fn voice_media_addr(&self) -> Option<std::net::SocketAddr> {
        self.io.voice()?.local_addr()
    }

    #[cfg(feature = "test-support")]
    pub fn share_video_sent(&self) -> Option<(u64, u64)> {
        Some(self.io.voice()?.share_video_sent())
    }

    /// Bytes of every datagram the transport sent by UDP, all of Voice and Screen Share.
    #[cfg(feature = "test-support")]
    pub fn udp_bytes_sent(&self) -> Option<u64> {
        Some(self.io.voice()?.udp_bytes_sent())
    }

    #[cfg(feature = "test-support")]
    pub fn watch_loads(&self) -> &[(Instant, crate::media::WatchLoad)] {
        &self.watch_loads
    }

    /// The H.264 profile and level of the watched share, from the SPS the decoder was given.
    #[cfg(feature = "test-support")]
    pub fn watched_profile_level(&self) -> Option<u32> {
        self.io.voice()?.watched_profile_level()
    }

    /// What a roster change means for this call: a watched share that ended (stopped, its Sharer
    /// left Voice or the Lobby) stops its decoder, since the server already forgot the watch; and
    /// the media path's recovery starts once the session is Active.
    pub(super) fn follow_roster(&mut self, msg: &ServerMsg, was_active: bool, now: Instant) {
        if matches!(
            msg,
            ServerMsg::MemberUpsert { .. } | ServerMsg::MemberRemoved(_) | ServerMsg::Ready(_)
        ) {
            self.settle_watch();
        }
        if !was_active && self.voice_active() {
            self.path_step(|path| path.activate(now), now);
        }
    }

    pub(super) fn voice_acked(&mut self, command: Command, ack: &Ack) {
        match command {
            Command::VoiceJoin { call } if self.is_current(call) => {
                if let (Some(call), Some(vsid)) = (self.call.as_mut(), ack.voice_session_id)
                    && let CallStage::JoinSent { offer_due } = call.stage
                {
                    info!(vsid = vsid.get(), "voice session started");
                    call.stage = CallStage::AwaitingOffer { vsid, offer_due };
                    self.state.voice.phase = call.phase();
                    self.queued.push(UiEffect::Sound(Sound::Join));
                }
            }
            Command::VoiceStateSet { call, .. } if self.is_current(call) => {
                self.apply_flags(VoiceFlags {
                    muted: ack.muted.unwrap_or(self.state.voice.muted),
                    deafened: ack.deafened.unwrap_or(self.state.voice.deafened),
                });
            }
            Command::VoiceLeave => self.queued.push(UiEffect::Sound(Sound::Leave)),
            Command::Watch { call, watch } => self.watch_taken(call, watch),
            command => debug!(?command, "ack for an ended voice session"),
        }
    }

    /// A refused Voice command; `retry_at` when the refusal names when to try again.
    pub(super) fn voice_refused(
        &mut self,
        command: Command,
        e: &ErrorBody,
        retry_at: Option<Instant>,
        now: Instant,
    ) {
        match command {
            Command::VoiceJoin { call } if self.is_current(call) => {
                warn!(code = ?e.code, "voice join refused");
                if e.code == ErrorCode::VoiceJoinCooldown && retry_at.is_some() {
                    // The Join waits the cooldown out.
                    self.state.voice.join_after = retry_at;
                }
                self.end_call(Some(Ended::voice(copy::voice_join_error(
                    e.code, &e.message,
                ))));
            }
            Command::RtcAnswer { call } if self.is_current(call) => {
                warn!(code = ?e.code, "voice answer refused");
                self.leave_with(Ended::voice(copy::server_error(e.code, &e.message)), now);
            }
            Command::VoiceStateSet { call, previous } if self.is_current(call) => {
                warn!(code = ?e.code, "voice state refused");
                self.apply_flags(previous);
                // A cooldown keeps the control disabled.
                if retry_at.is_some() {
                    self.state.voice.unmute_after = retry_at;
                }
                self.raise(
                    IssueKind::VoiceRefused,
                    copy::voice_state_error(e.code, &e.message),
                );
            }
            Command::ShareStart { call, share } if self.is_current_share(call, share) => {
                warn!(code = ?e.code, "screen share refused");
                self.end_share(Some(copy::share_error(e.code, &e.message)));
            }
            Command::Watch { call, watch } => {
                if self.is_current(call)
                    && (self.watch_mut().is_some_and(|w| w.refused(watch))
                        || self.forget_ask(watch))
                {
                    warn!(code = ?e.code, "watch refused");
                    self.raise(
                        IssueKind::ShareFailed,
                        copy::watch_error(e.code, &e.message),
                    );
                    self.settle_watch();
                }
            }
            // A refused restart leaves the attempt to its deadline.
            Command::RtcRestart => warn!(code = ?e.code, "voice restart refused"),
            Command::VoiceLeave | Command::ShareStop | Command::Unwatch => {}
            command => debug!(?command, code = ?e.code, "error for an ended voice session"),
        }
    }

    pub(super) fn on_offer(&mut self, offer: &RtcOffer, now: Instant) {
        let vsid = offer.voice_session_id;
        let Some(call) = self.call.as_mut().filter(|c| c.vsid() == Some(vsid)) else {
            debug!(vsid = vsid.get(), "offer for another voice session");
            return;
        };
        call.stage = CallStage::Offered { vsid };
        debug!(rev = offer.rev, "offer");
        let handed = self
            .io
            .voice()
            .ok_or_else(not_running)
            .and_then(|v| v.offer(vsid, offer.rev, offer.sdp.clone(), offer.tracks.clone()));
        if let Err(e) = handed {
            self.leave_with(Ended::media(&e), now);
        }
    }

    pub(super) fn on_watch_dropped(&mut self, dropped: &WatchDropped) {
        if let Some(w) = self.watch_mut() {
            w.strain.on_dropped(dropped);
        }
    }

    pub(super) fn on_voice_ended(&mut self, ended: &VoiceEnded) {
        if self
            .call
            .as_ref()
            .is_some_and(|c| c.vsid() == Some(ended.voice_session_id))
        {
            info!(reason = ?ended.reason, "voice ended by the server");
            self.end_call(Some(Ended::voice(copy::voice_ended(ended.reason))));
            self.queued.push(UiEffect::Sound(Sound::Leave));
        }
    }

    /// The sound for a Member joining or leaving the Voice session this client is in, read before
    /// the state takes the frame in. A Member who is still joining is not in it yet.
    pub(super) fn roster_sound(&self, msg: &ServerMsg) -> Option<UiEffect> {
        if !self.voice_active() {
            return None;
        }
        let (user, now_in) = match msg {
            ServerMsg::MemberUpsert { member } => {
                (&member.id, member.voice.is_some_and(|v| !v.joining))
            }
            ServerMsg::MemberRemoved(removed) => (&removed.user_id, false),
            _ => return None,
        };
        if self.state.me.as_ref() == Some(user) {
            return None;
        }
        let was_in = self
            .state
            .members
            .get(user)
            .and_then(|m| m.voice)
            .is_some_and(|v| !v.joining);
        match (was_in, now_in) {
            (false, true) => Some(UiEffect::Sound(Sound::Join)),
            (true, false) => Some(UiEffect::Sound(Sound::Leave)),
            _ => None,
        }
    }
}
