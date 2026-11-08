//! SDP negotiation: a new `Rtc`, the first offer, applying answers, and the follow-up offers that
//! re-point m-lines as Members join, leave, share and watch.

use std::{
    net::SocketAddr,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

use lobby_protocol::{
    Rid, Sdp, UserId, VoiceSessionId,
    encoder::{BWE_START_BPS, H264_LEVEL},
};
use str0m::{
    Candidate, Rtc,
    bwe::Bitrate,
    change::SdpAnswer,
    media::{Direction, MediaKind},
    net::TcpType,
};
use tracing::{debug, error, info, trace, warn};

use super::{
    SfuEvent, SfuEventKind,
    bitrate::AudioRate,
    event_loop::Sfu,
    peer::{Downlink, DownlinkSource, Peer, ShareUplink, Shows, WATCH_LINES, WatchDownlink},
    restart::IceRestart,
};

/// Frame-mode reordering on every `Rtc`: str0m's defaults would stall forwarding behind every lost
/// uplink packet.
const REORDER_AUDIO_PACKETS: usize = 1;
const REORDER_AUDIO_TIMEOUT: Duration = Duration::from_millis(40);
/// How long a video frame may take to assemble, from its first packet. A keyframe paced at a low
/// bandwidth estimate (146 kbps was measured on an idle link) spreads 10-20 KB over up to ~1.1 s
/// and must still arrive whole. A complete frame leaves at once, so only a frame missing a packet
/// that is never repaired, or the frame after such a gap, waits this long. That is shorter than
/// str0m's 2 s default, and RTX repairs most losses well inside it.
const REORDER_VIDEO_TIMEOUT: Duration = Duration::from_millis(1500);
const VOICE_TRACK: &str = "voice-audio";
const SHARE_TRACK: &str = "screen-video";
const WATCH_TRACK: &str = "watch-video";
/// H.264 constrained baseline, packetization-mode 1.
pub(crate) const H264_PT: u8 = 108;
pub(super) const H264_RTX_PT: u8 = 109;

impl Sfu {
    fn new_rtc(&self) -> Rtc {
        let mut config = Rtc::builder()
            .set_ice_lite(true)
            .clear_codecs()
            .enable_opus(true, false)
            .set_reordering_size_audio(REORDER_AUDIO_PACKETS)
            .set_reordering_timeout_audio(Some(REORDER_AUDIO_TIMEOUT))
            .set_reordering_timeout_video(Some(REORDER_VIDEO_TIMEOUT))
            // BWE can only be turned on here, so every peer gets it though only a Viewer uses it.
            // Without a desired bitrate it never probes, and its pacer never holds back audio.
            .enable_bwe(Some(Bitrate::bps(BWE_START_BPS)));
        config.codec_config().add_h264(
            H264_PT.into(),
            Some(H264_RTX_PT.into()),
            true,
            H264_LEVEL.profile_level_id(),
        );
        let mut rtc = config.build(Instant::now());
        for ip in &self.advertise {
            let udp = SocketAddr::new(*ip, self.port);
            let tcp = SocketAddr::new(*ip, self.tcp_port);
            let passive = Candidate::builder()
                .tcp()
                .host(tcp)
                .tcptype(TcpType::Passive)
                .build();
            for (addr, candidate) in [(udp, Candidate::host(udp, "udp")), (tcp, passive)] {
                match candidate {
                    Ok(c) => {
                        rtc.add_local_candidate(c);
                    }
                    Err(e) => {
                        warn!(%addr, error = %e, "advertised address rejected as a candidate");
                    }
                }
            }
        }
        rtc
    }

    pub(super) fn join(&mut self, user: UserId, vsid: VoiceSessionId, muted: bool) {
        let rejoined = self.peers.iter().any(|p| {
            p.user == user
                || p.downlinks
                    .iter()
                    .any(|d| d.source.assigned() == Some(&user))
        });
        for old in self
            .peers
            .iter_mut()
            .filter(|p| p.user == user && !p.closing)
        {
            old.close();
        }
        let others: Vec<UserId> = self
            .peers
            .iter()
            .filter(|p| !p.closing)
            .map(|p| p.user.clone())
            .collect();
        let mut rtc = self.new_rtc();
        let mut api = rtc.sdp_api();
        let uplink = api.add_media(
            MediaKind::Audio,
            Direction::SendRecv,
            Some(user.to_string()),
            Some(VOICE_TRACK.into()),
            None,
        );
        // The Members already in Voice are in the first offer, so their audio flows as soon as
        // the transport is up, with no second offer.
        let downlinks = others
            .into_iter()
            .map(|other| Downlink {
                mid: api.add_media(
                    MediaKind::Audio,
                    Direction::SendOnly,
                    Some(other.to_string()),
                    Some(VOICE_TRACK.into()),
                    None,
                ),
                source: DownlinkSource::Pending(other),
            })
            .collect();
        let Some((offer, pending)) = api.apply() else {
            error!(%user, "the initial offer had no changes");
            self.events.push(SfuEvent {
                user,
                vsid,
                kind: SfuEventKind::NegotiationFailed {
                    rid: None,
                    fatal: true,
                },
            });
            return;
        };
        let peer = Peer {
            user: user.clone(),
            vsid,
            rtc,
            pending: Some((1, pending)),
            rev: 1,
            answered: false,
            connected: false,
            rejoined,
            muted,
            speaking_until: None,
            closing: false,
            uplink,
            downlinks,
            share_wanted: None,
            share: None,
            watch_wanted: None,
            watch: Vec::new(),
            downlink_bps: None,
            audio_out: AudioRate::new(Instant::now()),
            need_bps: 0,
            next_timeout: Instant::now(),
            last_heard: None,
            path: None,
            restart: IceRestart::new(),
            received: 0,
            forwarded: 0,
            muted_dropped: 0,
            video_received: 0,
            video_forwarded: 0,
            keyframe_requests: 0,
        };
        info!(%user, vsid = vsid.get(), "peer joined");
        trace!(sdp = offer.to_sdp_string(), "initial offer");
        self.events.push(SfuEvent {
            user,
            vsid,
            kind: SfuEventKind::Offer {
                rev: 1,
                sdp: Sdp::new(offer.to_sdp_string()),
                tracks: peer.tracks(),
            },
        });
        self.peers.push(peer);
    }

    pub(super) fn answer(
        &mut self,
        user: UserId,
        vsid: VoiceSessionId,
        rev: u32,
        sdp: Sdp,
        rid: Option<Rid>,
    ) {
        let Some(p) = self
            .peers
            .iter_mut()
            .find(|p| p.user == user && p.vsid == vsid && !p.closing)
        else {
            debug!(%user, vsid = vsid.get(), "answer for no live peer");
            return;
        };
        let kind = match p.pending.take() {
            None => {
                warn!(%user, rev, "answer without an outstanding offer");
                SfuEventKind::NegotiationFailed { rid, fatal: false }
            }
            Some((pending_rev, pending)) if pending_rev != rev => {
                warn!(%user, rev, pending_rev, "answer to another offer");
                p.pending = Some((pending_rev, pending));
                SfuEventKind::NegotiationFailed { rid, fatal: false }
            }
            Some((_, pending)) => {
                trace!(sdp = sdp.as_str(), "answer");
                let applied = SdpAnswer::from_sdp_string(sdp.as_str())
                    .map_err(str0m::RtcError::from)
                    .and_then(|answer| p.rtc.sdp_api().accept_answer(pending, answer));
                match applied {
                    Ok(()) => {
                        p.answered = true;
                        p.restart.answered();
                        for d in &mut p.downlinks {
                            d.source.commit();
                        }
                        for w in &mut p.watch {
                            w.source.commit();
                            if let Some(source) = w.source.committed()
                                && w.shows == Shows::NotYet
                                && p.watch_wanted.as_ref() == Some(source)
                            {
                                debug!(viewer = %user, sharer = %source, "watch downlink answered");
                                w.asked_at = Some(Instant::now());
                                self.sharers_asked_for_keyframes.push(source.clone());
                            }
                        }
                        debug!(%user, rev, "answer applied");
                        SfuEventKind::AnswerApplied { rid }
                    }
                    Err(e) => {
                        // The `Rtc` may be half-changed now; the session cannot continue.
                        warn!(%user, rev, error = %e, "answer rejected");
                        p.closing = true;
                        p.rtc.disconnect();
                        SfuEventKind::NegotiationFailed { rid, fatal: true }
                    }
                }
            }
        };
        self.events.push(SfuEvent { user, vsid, kind });
    }

    pub(super) fn renegotiate(&mut self) {
        // A Member still connecting is offered too, so the others have answered by the time
        // their audio comes.
        let live: Vec<UserId> = self
            .peers
            .iter()
            .filter(|p| !p.closing && (p.connected || !p.rejoined))
            .map(|p| p.user.clone())
            .collect();
        let sharing: Vec<UserId> = self
            .peers
            .iter()
            .filter(|p| p.connected && !p.closing && p.sharing())
            .map(|p| p.user.clone())
            .collect();
        for p in &mut self.peers {
            if !p.connected || p.closing || p.pending.is_some() || !p.answered {
                continue;
            }
            let wanted: Vec<&UserId> = live.iter().filter(|u| **u != p.user).collect();
            let mut api = p.rtc.sdp_api();
            for d in &mut p.downlinks {
                if d.source.assigned().is_some_and(|s| !wanted.contains(&s)) {
                    api.set_direction(d.mid, Direction::Inactive);
                    d.source.deactivate();
                }
            }
            for user in wanted {
                if p.downlinks
                    .iter()
                    .any(|d| d.source.assigned() == Some(user))
                {
                    continue;
                }
                match p
                    .downlinks
                    .iter_mut()
                    .find(|d| d.source.assigned().is_none())
                {
                    Some(free) => {
                        api.set_direction(free.mid, Direction::SendOnly);
                        free.source.assign(user.clone());
                    }
                    None => {
                        let mid = api.add_media(
                            MediaKind::Audio,
                            Direction::SendOnly,
                            Some(user.to_string()),
                            Some(VOICE_TRACK.into()),
                            None,
                        );
                        p.downlinks.push(Downlink {
                            mid,
                            source: DownlinkSource::Pending(user.clone()),
                        });
                    }
                }
            }
            if p.share_wanted.is_some() && p.share.is_none() {
                let mid = api.add_media(
                    MediaKind::Video,
                    Direction::RecvOnly,
                    Some(p.user.to_string()),
                    Some(SHARE_TRACK.into()),
                    None,
                );
                p.share = Some(ShareUplink::new(mid));
            }
            // With no share asked for, or its Sharer gone, the lines stay: a pause keeps the
            // m-line, since inactive it would drop what is queued or on its way and the Viewer
            // would wait on that hole when watching again. Only forwarding stops.
            if let Some(target) = p.watch_wanted.as_ref().filter(|s| sharing.contains(s)) {
                let free: Vec<bool> = p
                    .watch
                    .iter()
                    .map(|w| w.source.assigned().is_none())
                    .collect();
                // At most one line is on screen, so with the share asked for on no line, the
                // other goes inactive here and is free for it in the next offer.
                for w in &mut p.watch {
                    if w.source.assigned().is_some_and(|s| s != target) && !w.shows.on_screen() {
                        api.set_direction(w.mid, Direction::Inactive);
                        w.source.deactivate();
                    }
                }
                if !p.watch.iter().any(|w| w.source.assigned() == Some(target)) {
                    // A line made inactive in this offer cannot be pointed elsewhere in it: the
                    // SDP would not change, so str0m would make no offer and the Viewer's mid
                    // table would never learn the new source. Such a line waits a round trip.
                    let reuse = (0..p.watch.len()).find(|&i| free[i]);
                    match reuse {
                        Some(i) => {
                            api.set_direction(p.watch[i].mid, Direction::SendOnly);
                            p.watch[i].repoint(target.clone());
                        }
                        None if p.watch.len() < WATCH_LINES => {
                            let mid = api.add_media(
                                MediaKind::Video,
                                Direction::SendOnly,
                                Some(target.to_string()),
                                Some(WATCH_TRACK.into()),
                                None,
                            );
                            p.watch.push(WatchDownlink::new(mid, target.clone()));
                        }
                        None => {}
                    }
                }
            }
            let restart = p.restart.wanted();
            if restart {
                api.ice_restart(true);
            }
            let Some((offer, pending)) = api.apply() else {
                continue;
            };
            if restart {
                p.restart.offered();
                self.probe.restarts_offered.fetch_add(1, Ordering::Relaxed);
            }
            p.rev += 1;
            p.pending = Some((p.rev, pending));
            debug!(user = %p.user, rev = p.rev, mlines = p.mlines(), "renegotiating");
            self.events.push(SfuEvent {
                user: p.user.clone(),
                vsid: p.vsid,
                kind: SfuEventKind::Offer {
                    rev: p.rev,
                    sdp: Sdp::new(offer.to_sdp_string()),
                    tracks: p.tracks(),
                },
            });
        }
    }
}
