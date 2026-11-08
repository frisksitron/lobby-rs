//! One Member's connection to the SFU: its `Rtc` and the m-lines negotiated on it.

use std::time::Instant;

use lobby_protocol::{
    Quality, RtcTrack, SPEAKING_HANGOVER, SourceSize, TrackPurpose, UserId, VoiceSessionId,
};
use str0m::{Rtc, change::SdpPendingOffer, media::Mid};
use tracing::info;

use super::{
    MediaPath,
    bitrate::AudioRate,
    restart::{DeadPeer, IceRestart},
    timeline::Timeline,
};

#[derive(Debug)]
pub(super) enum DownlinkSource {
    Inactive,
    /// In the outstanding offer's tracks, but nothing is written to the m-line until the peer
    /// answers: before that the client's mid table cannot route its media.
    Pending(UserId),
    Committed(UserId),
}

impl DownlinkSource {
    pub(super) fn assigned(&self) -> Option<&UserId> {
        match self {
            Self::Inactive => None,
            Self::Pending(user) | Self::Committed(user) => Some(user),
        }
    }

    pub(super) fn committed(&self) -> Option<&UserId> {
        match self {
            Self::Committed(user) => Some(user),
            Self::Inactive | Self::Pending(_) => None,
        }
    }

    pub(super) fn assign(&mut self, user: UserId) {
        *self = Self::Pending(user);
    }

    pub(super) fn deactivate(&mut self) {
        *self = Self::Inactive;
    }

    pub(super) fn commit(&mut self) {
        *self = match std::mem::replace(self, Self::Inactive) {
            Self::Pending(user) => Self::Committed(user),
            other => other,
        };
    }
}

#[derive(Debug)]
pub(super) struct Downlink {
    pub(super) mid: Mid,
    pub(super) source: DownlinkSource,
}

/// A Viewer's watch m-lines: the share on screen, and the next one getting ready.
pub(super) const WATCH_LINES: usize = 2;

#[derive(Debug, Clone, Copy)]
pub(super) struct ShareWanted {
    pub(super) quality: Quality,
    pub(super) source: Option<SourceSize>,
}

impl ShareWanted {
    pub(super) fn bps(&self) -> Option<u32> {
        let s = self.source?;
        Some(self.quality.bps_from((s.width.get(), s.height.get())))
    }
}

/// Stays receiving between shares: an inactive m-line drops what the Sharer sent before hearing
/// of the stop, and the next share's first frame would wait on that hole.
#[derive(Debug)]
pub(super) struct ShareUplink {
    pub(super) mid: Mid,
    pub(super) video_seen: bool,
}

impl ShareUplink {
    pub(super) fn new(mid: Mid) -> Self {
        Self {
            mid,
            video_seen: false,
        }
    }
}

/// What a watch line sends its Viewer. At most one line of a Viewer is on screen (`Live`,
/// `Recovering` or `Behind`): another line only goes live once it carries the share asked for,
/// and then every other line stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Shows {
    /// Nothing until a keyframe, since a decoder cannot start from a delta frame: the line was
    /// just pointed at its Sharer, or a Watch resumed it. It waits only while its Sharer is the
    /// one asked for.
    NotYet,
    /// Its Sharer's picture is on screen, and goes on while the next share gets ready.
    Live,
    Recovering,
    Behind,
    /// Nothing: its Watch ended, its share ended, or another line took over.
    Stopped,
}

impl Shows {
    pub(super) fn on_screen(self) -> bool {
        matches!(self, Self::Live | Self::Recovering | Self::Behind)
    }
}

#[derive(Debug)]
pub(super) struct WatchDownlink {
    pub(super) mid: Mid,
    pub(super) source: DownlinkSource,
    pub(super) shows: Shows,
    pub(super) asked_at: Option<Instant>,
    /// The last keyframe written to the line, in bytes: its time on the downlink is no backlog.
    pub(super) keyframe_bytes: usize,
    pub(super) dropped: u32,
    /// The downlink's own media timeline: whatever Sharer (or new encoder of the same one) it
    /// carries, its RTP time only moves forward. A receiver drops frames whose time went back
    /// after its stream paused, which a re-pointed downlink otherwise does.
    pub(super) timeline: Timeline,
}

impl WatchDownlink {
    pub(super) fn new(mid: Mid, source: UserId) -> Self {
        Self {
            mid,
            source: DownlinkSource::Pending(source),
            shows: Shows::NotYet,
            asked_at: None,
            keyframe_bytes: 0,
            dropped: 0,
            timeline: Timeline::default(),
        }
    }

    pub(super) fn repoint(&mut self, source: UserId) {
        self.source.assign(source);
        self.shows = Shows::NotYet;
        self.asked_at = None;
        self.timeline.reanchor();
    }

    pub(super) fn admits(&mut self, keyframe: bool) -> bool {
        match self.shows {
            Shows::Live => true,
            Shows::Behind | Shows::Recovering if !keyframe => {
                self.dropped += 1;
                false
            }
            _ => keyframe,
        }
    }

    pub(super) fn keyframe_sent(&mut self, bytes: usize) {
        self.shows = Shows::Live;
        self.keyframe_bytes = bytes;
    }

    pub(super) fn lost(&mut self) {
        if self.shows == Shows::Live {
            self.shows = Shows::Recovering;
        }
    }

    pub(super) fn fall_behind_newly(&mut self) -> bool {
        if !self.shows.on_screen() {
            return false;
        }
        self.dropped += 1;
        std::mem::replace(&mut self.shows, Shows::Behind) != Shows::Behind
    }
}

pub(super) struct Peer {
    pub(super) user: UserId,
    pub(super) vsid: VoiceSessionId,
    pub(super) rtc: Rtc,
    pub(super) pending: Option<(u32, SdpPendingOffer)>,
    pub(super) rev: u32,
    pub(super) answered: bool,
    pub(super) connected: bool,
    /// Joined while another peer still had a line for this Member. The others are offered it
    /// only once it connects, so their old lines go inactive first and a client never plays the
    /// new stream in the old one's slot. An old peer may be reaped before anyone looks.
    pub(super) rejoined: bool,
    pub(super) muted: bool,
    /// Set while the Member speaks: the server's one verdict, which the Lobby broadcasts.
    pub(super) speaking_until: Option<Instant>,
    pub(super) closing: bool,
    pub(super) uplink: Mid,
    pub(super) downlinks: Vec<Downlink>,
    pub(super) share_wanted: Option<ShareWanted>,
    pub(super) share: Option<ShareUplink>,
    pub(super) watch_wanted: Option<UserId>,
    /// At most [`WATCH_LINES`]: on a switch the share on screen keeps its line while the next
    /// one gets ready on the other.
    pub(super) watch: Vec<WatchDownlink>,
    /// The latest BWE estimate of the SFU's sending to this peer, its downlink.
    pub(super) downlink_bps: Option<u64>,
    pub(super) audio_out: AudioRate,
    pub(super) need_bps: u64,
    pub(super) next_timeout: Instant,
    /// The last datagram from the client: STUN, DTLS, RTP or RTCP.
    pub(super) last_heard: Option<Instant>,
    /// How DTLS and media go to the client: ICE's nominated pair.
    pub(super) path: Option<MediaPath>,
    pub(super) restart: IceRestart,
    pub(super) received: u64,
    pub(super) forwarded: u64,
    pub(super) muted_dropped: u64,
    pub(super) video_received: u64,
    pub(super) video_forwarded: u64,
    pub(super) keyframe_requests: u64,
}

impl Peer {
    /// Graceful close: the DTLS close_notify goes out on the next drain. str0m closes locally
    /// without awaiting the peer's reply, so the `Rtc` dies within one loop turn.
    pub(super) fn close(&mut self) {
        self.closing = true;
        let _ = self.rtc.close();
    }

    /// ICE-lite reports `Disconnected` only after 15 s without a binding request, and str0m keeps
    /// the `Rtc` alive after it, so silence is timed here. The clock starts at the first
    /// datagram; until then the join watchdog bounds the peer.
    pub(super) fn dead_at(&self, limits: DeadPeer) -> Option<Instant> {
        if self.closing || !self.rtc.is_alive() {
            return None;
        }
        Some(self.last_heard? + self.restart.dead_after(limits))
    }

    /// A voice packet's V flag: a voiced one holds speaking for the hangover, an unvoiced one
    /// ends it. Returns the change, if any.
    pub(super) fn heard(&mut self, voiced: bool, now: Instant) -> Option<bool> {
        let was = self.speaking_until.is_some();
        self.speaking_until = voiced.then(|| now + SPEAKING_HANGOVER);
        (was != voiced).then_some(voiced)
    }

    /// Whether the hangover after the last voiced packet ran out with nothing heard since.
    pub(super) fn speech_expired(&mut self, now: Instant) -> bool {
        let expired = self.speaking_until.is_some_and(|t| t <= now);
        if expired {
            self.speaking_until = None;
        }
        expired
    }

    pub(super) fn restart_wanted(&self, why: &str) {
        info!(user = %self.user, vsid = self.vsid.get(), why, "ice restart wanted");
    }

    pub(super) fn sharing(&self) -> bool {
        self.share_wanted.is_some() && self.share.is_some()
    }

    /// The Sharer whose video goes out on watch line `i` now: the share on screen, and the share
    /// asked for from its first keyframe on. The share on screen carries on until that one
    /// shows, so the Viewer is never left without a picture.
    pub(super) fn forwards(&self, i: usize) -> Option<&UserId> {
        let line = self.watch.get(i)?;
        let source = line.source.committed()?;
        match line.shows {
            Shows::Live | Shows::Recovering | Shows::Behind => Some(source),
            Shows::NotYet => (self.watch_wanted.as_ref() == Some(source)).then_some(source),
            Shows::Stopped => None,
        }
    }

    pub(super) fn watch_asked(&mut self, sharer: &UserId, now: Instant) -> bool {
        self.watch_wanted = Some(sharer.clone());
        let mut resumes = false;
        for w in &mut self.watch {
            if w.source.assigned() != Some(sharer) {
                if w.shows == Shows::NotYet {
                    w.shows = Shows::Stopped;
                }
                continue;
            }
            if w.shows == Shows::Stopped {
                w.shows = Shows::NotYet;
            }
            if w.source.committed().is_some() && w.shows != Shows::Live {
                w.asked_at = Some(now);
                resumes = true;
            }
        }
        resumes
    }

    /// Stop Watching: every line stops, and keeps its m-line for the next Watch.
    pub(super) fn unwatch(&mut self) {
        self.watch_wanted = None;
        for w in &mut self.watch {
            w.shows = Shows::Stopped;
        }
    }

    /// `sharer`'s share ended: their line stops, and so does a Watch of them, which the server
    /// forgets.
    pub(super) fn share_ended(&mut self, sharer: &UserId) {
        if self.watch_wanted.as_ref() == Some(sharer) {
            self.unwatch();
        }
        for w in &mut self.watch {
            if w.source.assigned() == Some(sharer) {
                w.shows = Shows::Stopped;
            }
        }
    }

    /// Line `i` showed its first picture of the share asked for: it is on screen now, and every
    /// other line stops.
    pub(super) fn shown(&mut self, i: usize) {
        for (j, w) in self.watch.iter_mut().enumerate() {
            if j != i {
                w.shows = Shows::Stopped;
            }
        }
    }

    pub(super) fn line_of(&self, source: &UserId) -> Option<usize> {
        (0..self.watch.len()).find(|&i| self.forwards(i) == Some(source))
    }

    pub(super) fn watched(&self) -> Vec<&UserId> {
        (0..self.watch.len())
            .filter_map(|i| self.forwards(i))
            .collect()
    }

    pub(super) fn mlines(&self) -> usize {
        1 + self.downlinks.len() + usize::from(self.share.is_some()) + self.watch.len()
    }

    pub(super) fn tracks(&self) -> Vec<RtcTrack> {
        let track = |mid: Mid, user: &UserId, purpose| RtcTrack {
            mid: mid.to_string(),
            user_id: user.clone(),
            purpose,
        };
        let mut tracks = vec![track(self.uplink, &self.user, TrackPurpose::VoiceAudio)];
        tracks.extend(self.downlinks.iter().filter_map(|d| {
            let source = d.source.assigned()?;
            Some(track(d.mid, source, TrackPurpose::VoiceAudio))
        }));
        if let Some(s) = &self.share {
            tracks.push(track(s.mid, &self.user, TrackPurpose::ScreenVideo));
        }
        tracks.extend(self.watch.iter().filter_map(|w| {
            let source = w.source.assigned()?;
            Some(track(w.mid, source, TrackPurpose::WatchVideo))
        }));
        tracks
    }
}
