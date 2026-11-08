use std::time::{Duration, Instant};

use lobby_protocol::encoder::desired_bps;
use str0m::{
    Rtc,
    bwe::Bitrate,
    media::{KeyframeRequestKind, MediaData, MediaTime, Mid},
};
use tracing::{debug, info};

use super::{
    SfuEvent, SfuEventKind,
    event_loop::Sfu,
    peer::{Peer, Shows},
};

/// A Viewer still waiting for a keyframe this long after the request asks again (the request or
/// the keyframe's packets were lost).
pub const KEYFRAME_RETRY: Duration = Duration::from_secs(1);
/// A Viewer's video that has waited this long in str0m's pacer, beyond what the line's last
/// keyframe takes at the rate the share needs, is more than the downlink carries. str0m sends a
/// queue whose packets wait 2 s on average in one burst: a 1 Mbps Viewer was sent 80-230 KB in
/// 100 ms every 2 s, and lost a third of its packets.
pub const WATCH_QUEUE_LIMIT: Duration = Duration::from_millis(150);

impl Sfu {
    pub(super) fn forward_media(&mut self) {
        let mut media = std::mem::take(&mut self.media);
        for (src, data) in media.drain(..) {
            let from = &self.peers[src];
            if from.closing {
                continue;
            }
            if data.mid == from.uplink {
                self.forward_voice(src, &data);
            } else if from.share.as_ref().is_some_and(|s| s.mid == data.mid) {
                self.forward_video(src, &data);
            }
        }
        self.media = media;
    }

    fn forward_voice(&mut self, src: usize, data: &MediaData) {
        let from = &mut self.peers[src];
        from.received += 1;
        if from.muted {
            from.muted_dropped += 1;
            return;
        }
        let voiced = data.ext_vals.voice_activity == Some(true);
        if let Some(speaking) = from.heard(voiced, Instant::now()) {
            self.events.push(SfuEvent {
                user: from.user.clone(),
                vsid: from.vsid,
                kind: SfuEventKind::Speaking(speaking),
            });
        }
        let source = from.user.clone();
        let mut forwarded = 0;
        for (j, to) in self.peers.iter_mut().enumerate() {
            if j == src || to.closing {
                continue;
            }
            let Some(mid) = to
                .downlinks
                .iter()
                .find(|d| d.source.committed() == Some(&source))
                .map(|d| d.mid)
            else {
                continue;
            };
            if write_at(&mut to.rtc, mid, data, data.time) {
                to.audio_out.sent(data.data.len());
                forwarded += 1;
            }
        }
        self.peers[src].forwarded += forwarded;
    }

    fn forward_video(&mut self, src: usize, data: &MediaData) {
        let now = Instant::now();
        let from = &mut self.peers[src];
        if from.share_wanted.is_none() {
            return;
        }
        let (user, vsid) = (from.user.clone(), from.vsid);
        let Some(share) = from.share.as_mut() else {
            return;
        };
        // str0m has no source for the m-line before its first packet, so a keyframe request
        // made earlier failed. A share's frames may start with deltas: its first keyframe was
        // lost, or came before the Sharer's answer was applied.
        let first = !share.video_seen;
        if first {
            share.video_seen = true;
            info!(%user, keyframe = data.is_keyframe(), "share video started");
            self.events.push(SfuEvent {
                user: user.clone(),
                vsid,
                kind: SfuEventKind::ShareVideoStarted,
            });
        }
        let source = user;
        let keyframe = data.is_keyframe();
        let gap = !data.contiguous && !keyframe;
        if keyframe {
            debug!(sharer = %source, bytes = data.data.len(), "share keyframe received");
        }
        self.peers[src].video_received += 1;
        let mut forwarded = 0;
        let mut asked = false;
        for (j, to) in self.peers.iter_mut().enumerate() {
            if j == src || to.closing {
                continue;
            }
            let Some(line) = to.line_of(&source) else {
                continue;
            };
            let w = &mut to.watch[line];
            if gap {
                w.lost();
                asked = true;
            } else if first && !keyframe && w.shows != Shows::Live {
                debug!(viewer = %to.user, sharer = %source, "keyframe asked again: the share arrived");
                w.asked_at = Some(now);
                asked = true;
            }
            let was_first = w.shows == Shows::NotYet;
            if !w.admits(keyframe) {
                continue;
            }
            let mid = w.mid;
            let time = MediaTime::new(
                w.timeline.map(data.time.numer(), now),
                data.time.frequency(),
            );
            if behind(to, line, now) {
                if to.watch[line].fall_behind_newly() {
                    restart_estimate(to);
                }
                continue;
            }
            if !write_at(&mut to.rtc, mid, data, time) {
                to.watch[line].lost();
                debug!(viewer = %to.user, sharer = %source, "frame rejected: the viewer waits for a keyframe");
                asked = true;
                continue;
            }
            forwarded += 1;
            if keyframe {
                to.watch[line].keyframe_sent(data.data.len());
                if was_first {
                    debug!(viewer = %to.user, sharer = %source, "first keyframe forwarded to a viewer");
                    to.shown(line);
                }
            }
        }
        if gap && asked {
            debug!(sharer = %source, "gap in a share uplink");
        }
        if asked {
            self.sharers_asked_for_keyframes.push(source);
        }
        self.peers[src].video_forwarded += forwarded;
    }

    /// Every request goes to the Sharer at once. The Sharer's encoder decides how often a
    /// keyframe is made, and serves the requests that come sooner with its next one.
    pub(super) fn ask_keyframes(&mut self) {
        let asks = &self.sharers_asked_for_keyframes;
        for p in &mut self.peers {
            if !p.closing && p.sharing() && asks.contains(&p.user) {
                request_keyframe(p);
            }
        }
        self.sharers_asked_for_keyframes.clear();
    }

    pub(super) fn report_dropped(&mut self) {
        for p in &mut self.peers {
            let frames: u32 = p
                .watch
                .iter_mut()
                .map(|w| std::mem::take(&mut w.dropped))
                .sum();
            if frames == 0 || p.closing {
                continue;
            }
            self.events.push(SfuEvent {
                user: p.user.clone(),
                vsid: p.vsid,
                kind: SfuEventKind::WatchDropped {
                    frames,
                    downlink_bps: p.downlink_bps,
                },
            });
        }
    }

    pub(super) fn retry_keyframes(&mut self, now: Instant) {
        for p in &mut self.peers {
            if p.closing {
                continue;
            }
            for line in 0..p.watch.len() {
                let Some(source) = p.forwards(line).cloned() else {
                    continue;
                };
                let behind = behind(p, line, now);
                let w = &mut p.watch[line];
                if w.shows != Shows::Live
                    && !behind
                    && w.asked_at
                        .is_none_or(|t| now.duration_since(t) >= KEYFRAME_RETRY)
                {
                    debug!(viewer = %p.user, sharer = %source, "viewer still awaits a keyframe");
                    w.asked_at = Some(now);
                    self.sharers_asked_for_keyframes.push(source);
                }
            }
        }
        self.ask_keyframes();
    }

    pub(super) fn set_desired_bitrates(&mut self, now: Instant) {
        let watched: Vec<u64> = self
            .peers
            .iter()
            .map(|p| {
                let watched = p.watch_wanted.as_ref().filter(|_| !p.closing);
                let bps = watched.and_then(|sharer| {
                    let s = self.peers.iter().find(|s| &s.user == sharer)?;
                    s.share_wanted?.bps()
                });
                bps.map_or(0, u64::from)
            })
            .collect();
        for (p, share_bps) in self.peers.iter_mut().zip(watched) {
            p.audio_out.roll(now);
            let need = match share_bps {
                0 => 0,
                bps => bps + p.audio_out.bps(),
            };
            if need != p.need_bps {
                p.rtc
                    .bwe()
                    .set_desired_bitrate(Bitrate::bps(desired_bps(need)));
                p.need_bps = need;
            }
        }
    }
}

fn request_keyframe(p: &mut Peer) {
    let Some(share) = p.share.as_ref() else {
        return;
    };
    let asked = p
        .rtc
        .writer(share.mid)
        .map(|mut w| w.request_keyframe(None, KeyframeRequestKind::Pli));
    match asked {
        Some(Ok(())) => {
            p.keyframe_requests += 1;
            debug!(sharer = %p.user, "keyframe requested");
        }
        Some(Err(e)) => {
            debug!(sharer = %p.user, error = %e, "keyframe request not possible yet");
        }
        None => {}
    }
}

/// Past losses hold str0m's estimate far below the link and stop its probes, so without a reset
/// a line that fell behind stays behind after the link has room again.
fn restart_estimate(p: &mut Peer) {
    let from = p.downlink_bps.unwrap_or(p.need_bps).min(p.need_bps);
    debug!(viewer = %p.user, from, "downlink behind: the estimate starts again");
    p.rtc.bwe().reset(Bitrate::bps(from));
}

/// Whether the video of `p`'s line `line` waits in str0m's pacer past [`WATCH_QUEUE_LIMIT`]
/// beyond its last keyframe's time at the rate the share needs. Not at the estimate: on a link
/// slower than the share the estimate falls towards zero, and the allowance would pass the 2 s
/// after which str0m's pacer sends its whole queue, so the line would never be behind.
fn behind(p: &mut Peer, line: usize, now: Instant) -> bool {
    let w = &p.watch[line];
    let keyframe_bits = w.keyframe_bytes as u64 * 8;
    let keyframe_time = match p.need_bps {
        0 => Duration::ZERO,
        bps => Duration::from_micros(keyframe_bits * 1_000_000 / bps),
    };
    let mid = w.mid;
    queued_since(&mut p.rtc, mid)
        .is_some_and(|t| now.saturating_duration_since(t) >= WATCH_QUEUE_LIMIT + keyframe_time)
}

fn queued_since(rtc: &mut Rtc, mid: Mid) -> Option<Instant> {
    rtc.direct_api()
        .stream_tx_by_mid(mid, None)?
        .queue_info()?
        .first_unsent()
}

fn write_at(rtc: &mut Rtc, mid: Mid, data: &MediaData, time: MediaTime) -> bool {
    let Some(mut writer) = rtc.writer(mid) else {
        return false;
    };
    let Some(pt) = writer.match_params(data.params) else {
        return false;
    };
    // The sender's RFC 6464 level and V flag go on to each receiver, which times the speaking
    // ring to its own playout.
    if let (Some(level), Some(voiced)) = (data.ext_vals.audio_level, data.ext_vals.voice_activity) {
        writer = writer.audio_level(level, voiced);
    }
    match writer.write(pt, data.network_time, time, data.data.clone()) {
        Ok(()) => true,
        Err(e) => {
            debug!(error = %e, "forward failed");
            false
        }
    }
}
