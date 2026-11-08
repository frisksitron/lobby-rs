//! Screen Share on the transport: the encoder's H.264 frames sent on the `screen-video` m-line,
//! and the watched share's frames from `watch-video` handed to the decoder, from a keyframe on.

use std::{
    sync::{Arc, Mutex, OnceLock, atomic::Ordering, mpsc::TrySendError},
    time::{Duration, Instant},
};

use lobby_protocol::{RtcTrack, TrackPurpose, UserId};
use str0m::{
    Rtc,
    format::Codec,
    media::{Frequency, KeyframeRequestKind, MediaData, MediaTime, Mid, Pt},
};
use tokio::sync::mpsc;
use tracing::{debug, info};

use super::{TransportView, payload_type, stats::ShareOut};
use crate::media::watch::{KeyframeGate, Lost, VideoIn, WatchInput};
#[cfg(windows)]
use crate::media::win::FineTimers;

const VIDEO_STATS_EVERY: Duration = Duration::from_secs(5);

pub(crate) struct EncodedVideo {
    pub data: Vec<u8>,
    pub keyframe: bool,
    pub at: Instant,
    pub rtp_time: u64,
    pub size: (u32, u32),
    /// The encoder that made it, by name, for the stats.
    pub encoder: Arc<str>,
}

#[derive(Default)]
pub(crate) struct KeyframeRequests {
    asked: Mutex<Option<Instant>>,
    /// Wakes the share's encoder, which would otherwise see a request at its next deadline.
    wake: OnceLock<Box<dyn Fn() + Send + Sync>>,
}

impl KeyframeRequests {
    pub fn request(&self) {
        let mut asked = self.asked.lock().unwrap_or_else(|e| e.into_inner());
        asked.get_or_insert_with(Instant::now);
        drop(asked);
        if let Some(wake) = self.wake.get() {
            wake();
        }
    }

    #[cfg(windows)]
    pub fn wake_with(&self, wake: impl Fn() + Send + Sync + 'static) {
        let _ = self.wake.set(Box::new(wake));
    }

    #[cfg(windows)]
    pub fn pending(&self) -> Option<Instant> {
        *self.asked.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[cfg(windows)]
    pub fn served(&self) {
        *self.asked.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

pub(super) struct Sharing {
    pub frames: mpsc::Receiver<EncodedVideo>,
    keyframes: Arc<KeyframeRequests>,
    #[cfg(windows)]
    _timers: FineTimers,
}

/// A watch m-line, with the Sharer the latest offer named for it. The SFU points a line at
/// another Sharer only after an offer made it inactive, so its frames are always theirs.
struct WatchLine {
    mid: Mid,
    sharer: UserId,
    gate: KeyframeGate,
}

struct Watching {
    input: WatchInput,
    /// The Sharer whose frames the decoder is fed: the one wanted, from its first keyframe on.
    /// Until then the one fed before goes on, so the picture stays live.
    fed: Option<UserId>,
}

#[derive(Debug, Default)]
struct VideoCounters {
    sent: u64,
    sent_keyframes: u64,
    unsent: u64,
    keyframe_requests_in: u64,
    received: u64,
    discarded: u64,
    decoder_overflow: u64,
    keyframe_requests_out: u64,
    network_lost: u64,
    network_lost_taken: u64,
}

#[derive(Default)]
pub(super) struct VideoPath {
    share_mid: Option<Mid>,
    watch_lines: Vec<WatchLine>,
    h264_pt: Option<Pt>,
    pub sharing: Option<Sharing>,
    watching: Option<Watching>,
    counters: VideoCounters,
    last_stats: Option<Instant>,
    pub share_out: ShareOut,
}

impl VideoPath {
    pub fn share(
        &mut self,
        frames: mpsc::Receiver<EncodedVideo>,
        keyframes: Arc<KeyframeRequests>,
    ) {
        // A share m-line from an earlier share is reused: start with a keyframe.
        if self.share_mid.is_some() {
            keyframes.request();
        }
        self.sharing = Some(Sharing {
            frames,
            keyframes,
            #[cfg(windows)]
            _timers: FineTimers::hold(),
        });
        self.share_out = None;
    }

    pub fn watch(&mut self, input: WatchInput) {
        for line in &mut self.watch_lines {
            line.gate = KeyframeGate::new();
        }
        self.watching = Some(Watching { input, fed: None });
    }

    pub fn unwatch(&mut self) {
        self.watching = None;
    }

    /// The share and watch m-lines in use, for the stats.
    pub fn live_mids(&self) -> (Option<Mid>, Option<Mid>) {
        let share = self.share_mid.filter(|_| self.sharing.is_some());
        let fed = self.watching.as_ref().and_then(|w| w.fed.as_ref());
        let watch = self
            .watch_lines
            .iter()
            .find(|l| Some(&l.sharer) == fed)
            .map(|l| l.mid);
        (share, watch)
    }

    /// The latest watched picture's size, and whether the GPU decoded it.
    pub fn watched(&self) -> Option<((u32, u32), bool)> {
        self.watching.as_ref().and_then(|w| w.input.shown.get())
    }

    pub fn take_lost(&mut self) -> u32 {
        let c = &mut self.counters;
        let taken = std::mem::replace(&mut c.network_lost_taken, c.network_lost);
        (c.network_lost - taken) as u32
    }

    pub fn watches(&self, mid: Mid) -> bool {
        self.watch_lines.iter().any(|l| l.mid == mid)
    }

    pub fn on_keyframe_request(&mut self, mid: Mid) {
        if Some(mid) != self.share_mid {
            return;
        }
        self.counters.keyframe_requests_in += 1;
        debug!(
            sharing = self.sharing.is_some(),
            "keyframe request from the SFU"
        );
        if let Some(s) = &self.sharing {
            s.keyframes.request();
        }
    }

    /// A frame on a watch line: to the decoder, from a keyframe on, asking the SFU for a
    /// keyframe when a gap, the decoder or a full queue makes delta frames useless. Only the
    /// Sharer fed and the one wanted count; the decoder moves to the wanted one at its keyframe.
    pub fn on_media(&mut self, data: MediaData, rtc: Option<&mut Rtc>, view: &TransportView) {
        let (Some(w), Some(i)) = (
            &mut self.watching,
            self.watch_lines.iter().position(|l| l.mid == data.mid),
        ) else {
            return;
        };
        self.counters.received += 1;
        let line = &mut self.watch_lines[i];
        let fed = w.fed.as_ref() == Some(&line.sharer);
        if !fed && *w.input.wanted.borrow() != line.sharer {
            self.counters.discarded += 1;
            return;
        }
        let now = Instant::now();
        // The decoder lost the picture of the share it is fed.
        let lost = fed && w.input.needs_keyframe.swap(false, Ordering::SeqCst);
        let keyframe = data.is_keyframe();
        let gate = line.gate.on_frame(data.contiguous, keyframe, lost, now);
        match gate.lost {
            Some(Lost::Network) => self.counters.network_lost += 1,
            Some(Lost::Decoder) => {
                w.input.lost_total.fetch_add(1, Ordering::Relaxed);
            }
            None => {}
        }
        let mut request = gate.request_keyframe;
        let (mid, sharer) = (line.mid, line.sharer.clone());
        if gate.forward && (fed || keyframe) {
            if !fed {
                debug!(%sharer, "the watch moves to another share");
                w.fed = Some(sharer.clone());
                for other in self.watch_lines.iter_mut().filter(|l| l.mid != mid) {
                    other.gate.reset();
                }
            }
            #[cfg(feature = "test-support")]
            if let Some(id) = crate::media::watch::sps_profile_level(&data.data) {
                view.watched_profile_level.store(id, Ordering::Relaxed);
            }
            #[cfg(not(feature = "test-support"))]
            let _ = view;
            if keyframe {
                debug!(bytes = data.data.len(), "watch keyframe to the decoder");
            }
            let frame = VideoIn {
                from: sharer,
                data: data.data,
                keyframe,
            };
            match w.input.frames.try_send(frame) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    self.counters.decoder_overflow += 1;
                    w.input.lost_total.fetch_add(1, Ordering::Relaxed);
                    request |= self.watch_lines[i].gate.overflowed(now);
                }
                Err(TrySendError::Disconnected(_)) => {}
            }
        } else {
            debug!(
                contiguous = data.contiguous,
                lost, "watch frame discarded until a keyframe"
            );
            self.counters.discarded += 1;
        }
        if request {
            self.request_keyframe(rtc, mid);
        }
    }

    fn request_keyframe(&mut self, rtc: Option<&mut Rtc>, mid: Mid) {
        let Some(rtc) = rtc else {
            return;
        };
        if let Some(mut writer) = rtc.writer(mid) {
            match writer.request_keyframe(None, KeyframeRequestKind::Pli) {
                Ok(()) => {
                    debug!("watch asked the SFU for a keyframe");
                    self.counters.keyframe_requests_out += 1;
                }
                Err(e) => debug!(error = %e, "keyframe request not possible"),
            }
        }
    }

    pub fn send(&mut self, frame: EncodedVideo, rtc: Option<&mut Rtc>, view: &TransportView) {
        let (Some(rtc), Some(mid)) = (rtc, self.share_mid) else {
            self.counters.unsent += 1;
            return;
        };
        let Some(writer) = rtc.writer(mid) else {
            self.counters.unsent += 1;
            return;
        };
        let Some(pt) = payload_type(&writer, Codec::H264, &mut self.h264_pt) else {
            self.counters.unsent += 1;
            return;
        };
        let time = MediaTime::new(frame.rtp_time, Frequency::NINETY_KHZ);
        let len = frame.data.len() as u64;
        match writer.write(pt, frame.at, time, frame.data) {
            Ok(()) => {
                view.video_frames.fetch_add(1, Ordering::Relaxed);
                view.video_bytes.fetch_add(len, Ordering::Relaxed);
                self.share_out = Some((frame.size, frame.encoder));
                self.counters.sent += 1;
                self.counters.sent_keyframes += u64::from(frame.keyframe);
                if frame.keyframe {
                    debug!(bytes = len, "share keyframe sent");
                }
            }
            Err(e) => {
                self.counters.unsent += 1;
                debug!(error = %e, "video frame not written");
            }
        }
    }

    pub fn route(&mut self, me: &UserId, tracks: &[RtcTrack]) {
        let share_mid = tracks
            .iter()
            .find(|t| t.user_id == *me && t.purpose == TrackPurpose::ScreenVideo)
            .map(|t| Mid::from(t.mid.as_str()));
        if share_mid.is_some() && share_mid != self.share_mid {
            debug!("share m-line negotiated");
            // The stream starts with a keyframe, so a Viewer can decode from its first frame.
            if let Some(s) = &self.sharing {
                s.keyframes.request();
            }
        }
        self.share_mid = share_mid;
        // A line that still carries the same Sharer keeps its gate; one pointed anew waits for
        // a keyframe.
        let mut before = std::mem::take(&mut self.watch_lines);
        for t in tracks
            .iter()
            .filter(|t| t.purpose == TrackPurpose::WatchVideo)
        {
            let mid = Mid::from(t.mid.as_str());
            let kept = before
                .iter()
                .position(|l| l.mid == mid && l.sharer == t.user_id)
                .map(|i| before.swap_remove(i));
            let line = kept.unwrap_or_else(|| {
                debug!(sharer = %t.user_id, "a watch m-line pointed at a share");
                WatchLine {
                    mid,
                    sharer: t.user_id.clone(),
                    gate: KeyframeGate::new(),
                }
            });
            self.watch_lines.push(line);
        }
    }

    pub fn log_stats(&mut self) {
        if (self.sharing.is_some() || self.watching.is_some())
            && self
                .last_stats
                .is_none_or(|t| t.elapsed() >= VIDEO_STATS_EVERY)
        {
            self.last_stats = Some(Instant::now());
            let v = &self.counters;
            info!(
                sharing = self.sharing.is_some(),
                watching = ?self.watching.as_ref().and_then(|w| w.fed.as_ref()),
                sent = v.sent,
                sent_keyframes = v.sent_keyframes,
                unsent = v.unsent,
                keyframe_requests_in = v.keyframe_requests_in,
                received = v.received,
                discarded = v.discarded,
                decoder_overflow = v.decoder_overflow,
                keyframe_requests_out = v.keyframe_requests_out,
                "screen share transport"
            );
        }
    }
}
