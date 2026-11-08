//! Voice on the transport: inbound Opus routed by mid to a playout slot, and the encoded
//! microphone frames sent on the Member's own m-line.

use std::{
    collections::HashMap,
    sync::{Arc, atomic::Ordering},
    time::Instant,
};

use lobby_protocol::{RtcTrack, TrackPurpose, UserId};
use str0m::{
    Rtc,
    format::Codec,
    media::{Frequency, MediaData, MediaTime, Mid, Pt},
};
use tokio::sync::Notify;
use tracing::{debug, info, warn};

use super::{EncodedFrame, payload_type};
use crate::{
    Volume,
    media::{
        VoiceEvent, VoiceEvents,
        jitter::Packet,
        playout::{PlayoutFeed, SlotCmd, VOICE_SLOTS},
    },
};

#[derive(Debug)]
struct Route {
    user: UserId,
    slot: usize,
    generation: u32,
}

#[derive(Debug, Default, Clone, Copy)]
struct Baseline {
    decoded: u64,
    concealed: u64,
    energy: u64,
    samples: u64,
}

#[derive(Debug, Default)]
pub(super) struct VoiceCounters {
    pub sent: u64,
    pub unsent: u64,
    pub overflow: u64,
}

pub(super) struct VoiceRoutes {
    frames: rtrb::Consumer<EncodedFrame>,
    /// Notified after each frame `lobby-voice-tx` queues.
    pub frames_ready: Arc<Notify>,
    feed: PlayoutFeed,
    pub uplink: Option<Mid>,
    opus_pt: Option<Pt>,
    routes: HashMap<Mid, Route>,
    /// Members not at 100 %; the slot a Member is bound to plays at their volume.
    volumes: HashMap<UserId, Volume>,
    free: [bool; VOICE_SLOTS],
    next_generation: u32,
    pub counters: VoiceCounters,
    baselines: [Baseline; VOICE_SLOTS],
    /// The Members speaking in what plays, as the Session last heard it.
    speakers: Vec<UserId>,
}

impl VoiceRoutes {
    pub fn new(
        frames: rtrb::Consumer<EncodedFrame>,
        frames_ready: Arc<Notify>,
        feed: PlayoutFeed,
        volumes: HashMap<UserId, Volume>,
    ) -> Self {
        Self {
            frames,
            frames_ready,
            feed,
            uplink: None,
            opus_pt: None,
            routes: HashMap::new(),
            volumes,
            free: [true; VOICE_SLOTS],
            next_generation: 0,
            counters: VoiceCounters::default(),
            baselines: [Baseline::default(); VOICE_SLOTS],
            speakers: Vec::new(),
        }
    }

    /// Tells the Session who speaks in what plays now, when that changed. A full queue keeps
    /// the change for the next look.
    pub fn tell_speakers(&mut self, events: &VoiceEvents) {
        let mut now: Vec<UserId> = self
            .routes
            .values()
            .filter(|r| self.feed.speech[r.slot].load(Ordering::Relaxed))
            .map(|r| r.user.clone())
            .collect();
        now.sort();
        if now != self.speakers && events.try_send(VoiceEvent::Speakers(now.clone())) {
            self.speakers = now;
        }
    }

    pub fn audio_in(&self) -> Vec<Mid> {
        self.routes.keys().copied().collect()
    }

    pub fn on_media(&mut self, data: &MediaData) {
        let Some(route) = self.routes.get(&data.mid) else {
            return;
        };
        let seq = **data.seq_range.end();
        let voiced = data.ext_vals.voice_activity == Some(true);
        let Some(packet) =
            Packet::new(route.generation, seq, data.time.numer(), voiced, &data.data)
        else {
            return;
        };
        if self.feed.packets[route.slot].push(packet).is_err() {
            self.counters.overflow += 1;
        }
    }

    pub fn set_volume(&mut self, user: UserId, volume: Volume) {
        for route in self.routes.values().filter(|r| r.user == user) {
            self.feed.gains.set_slot(route.slot, volume.gain());
        }
        self.volumes.insert(user, volume);
    }

    pub fn route(&mut self, me: &UserId, tracks: &[RtcTrack]) {
        self.uplink = tracks
            .iter()
            .find(|t| t.user_id == *me && t.purpose == TrackPurpose::VoiceAudio)
            .map(|t| Mid::from(t.mid.as_str()));
        let wanted: HashMap<Mid, &UserId> = tracks
            .iter()
            .filter(|t| t.user_id != *me && t.purpose == TrackPurpose::VoiceAudio)
            .map(|t| (Mid::from(t.mid.as_str()), &t.user_id))
            .collect();
        let stale: Vec<Mid> = self
            .routes
            .iter()
            .filter(|(mid, r)| wanted.get(*mid) != Some(&&r.user))
            .map(|(mid, _)| *mid)
            .collect();
        for mid in stale {
            let route = self.routes.remove(&mid).expect("listed above");
            debug!(user = %route.user, slot = route.slot, "slot unbound");
            let _ = self.feed.cmds.push(SlotCmd::Unbind { slot: route.slot });
            self.free[route.slot] = true;
        }
        for (mid, user) in wanted {
            if self.routes.contains_key(&mid) {
                continue;
            }
            let Some(slot) = self.free.iter().position(|f| *f) else {
                warn!(%user, "no free playout slot");
                continue;
            };
            self.next_generation += 1;
            let generation = self.next_generation;
            let volume = self.volumes.get(user).copied().unwrap_or_default();
            self.feed.gains.set_slot(slot, volume.gain());
            if self
                .feed
                .cmds
                .push(SlotCmd::Bind { slot, generation })
                .is_err()
            {
                warn!(%user, "playout command ring full");
                continue;
            }
            self.free[slot] = false;
            self.baselines[slot] = self.baseline(slot);
            debug!(%user, slot, generation, "slot bound");
            self.routes.insert(
                mid,
                Route {
                    user: user.clone(),
                    slot,
                    generation,
                },
            );
        }
    }

    pub fn send_frames(&mut self, mut rtc: Option<&mut Rtc>) {
        while let Ok(frame) = self.frames.pop() {
            self.send_frame(rtc.as_deref_mut(), &frame);
        }
    }

    fn send_frame(&mut self, rtc: Option<&mut Rtc>, frame: &EncodedFrame) {
        let (Some(rtc), Some(mid)) = (rtc, self.uplink) else {
            self.counters.unsent += 1;
            return;
        };
        let Some(writer) = rtc.writer(mid) else {
            self.counters.unsent += 1;
            return;
        };
        let Some(pt) = payload_type(&writer, Codec::Opus, &mut self.opus_pt) else {
            self.counters.unsent += 1;
            return;
        };
        let time = MediaTime::new(frame.ts, Frequency::FORTY_EIGHT_KHZ);
        let writer = writer.audio_level(-(frame.dbov as i8), frame.voiced);
        match writer.write(pt, Instant::now(), time, frame.payload()) {
            Ok(()) => self.counters.sent += 1,
            Err(e) => {
                self.counters.unsent += 1;
                debug!(error = %e, "voice frame not written");
            }
        }
    }

    fn baseline(&self, slot: usize) -> Baseline {
        let s = &self.feed.stats[slot];
        Baseline {
            decoded: s.decoded.load(Ordering::Relaxed),
            concealed: s.concealed.load(Ordering::Relaxed),
            energy: s.energy.load(Ordering::Relaxed),
            samples: s.samples.load(Ordering::Relaxed),
        }
    }

    pub fn log_slots(&mut self) {
        for route in self.routes.values() {
            let now = self.baseline(route.slot);
            let before = self.baselines[route.slot];
            let stats = &self.feed.stats[route.slot];
            let rms = stats.rms_since(before.energy, before.samples);
            info!(
                user = %route.user,
                slot = route.slot,
                decoded = now.decoded.wrapping_sub(before.decoded),
                concealed = now.concealed.wrapping_sub(before.concealed),
                rms = format_args!("{rms:.4}"),
                late = stats.late.load(Ordering::Relaxed),
                dropped = stats.dropped.load(Ordering::Relaxed),
                "voice slot"
            );
            self.baselines[route.slot] = now;
        }
    }
}
