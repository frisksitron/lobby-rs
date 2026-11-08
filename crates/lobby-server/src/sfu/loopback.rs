//! The SFU over real loopback UDP against in-process str0m clients; the test plays the voice
//! machine, carrying offers and answers between the two.

use std::{
    cell::Cell,
    collections::HashMap,
    net::{Ipv4Addr, SocketAddr},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use lobby_protocol::{
    Quality, RtcTrack, Sdp, TrackPurpose, UserId, VoiceSessionId, encoder::H264_LEVEL,
};
use str0m::{
    Candidate, Event, Input, Output, Rtc,
    change::SdpOffer,
    format::Codec,
    media::{Frequency, MediaTime, Mid},
    net::{Protocol, Receive},
};
use tokio::{
    net::UdpSocket,
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use super::{
    negotiate::{H264_PT, H264_RTX_PT},
    *,
};
use crate::{Config, config::SfuConfig};

const WAIT: Duration = Duration::from_secs(10);
/// A closed peer's `Rtc` is gone within this (str0m completes a close without the peer).
const REAP_WITHIN: Duration = Duration::from_secs(2);
const VIDEO_EVERY: Duration = Duration::from_millis(33);
/// Frames of another stream that arrive while a stopped one stays quiet.
const FLOWING_WHILE_QUIET: usize = 20;
/// A delta frame of the test pattern.
const DELTA_BYTES: usize = 600;
/// A Voice frame, sent every 20 ms.
const VOICE_BYTES: usize = 40;

fn user(n: u8) -> UserId {
    format!("usr_{:024x}", u128::from(n) + 1).parse().unwrap()
}

fn vsid(n: u64) -> VoiceSessionId {
    VoiceSessionId::new(n).unwrap()
}

enum PeerCmd {
    Offer {
        sdp: Sdp,
        tracks: Vec<RtcTrack>,
        reply: oneshot::Sender<Sdp>,
    },
    /// The Sharer sends a keyframe unasked, as its encoder does on a scene cut.
    SendKeyframe,
}

/// A share frame as a Viewer got it.
#[derive(Debug, Clone)]
struct Frame {
    at: Instant,
    keyframe: bool,
    /// The Sharer the offer named for the m-line it came on.
    from: UserId,
    /// The Sharer whose encoder made it, by the [`tag`] its payload ends with.
    made_by: u8,
}

#[derive(Debug, Default, Clone)]
struct VideoSeen {
    frames: HashMap<UserId, usize>,
    first_was_keyframe: HashMap<UserId, bool>,
    keyframe_requests: Vec<Instant>,
    log: Vec<Frame>,
}

impl VideoSeen {
    fn since(&self, t: Instant) -> Vec<Frame> {
        self.log.iter().filter(|f| f.at >= t).cloned().collect()
    }
}

struct TestPeer {
    cmds: mpsc::Sender<PeerCmd>,
    /// Audio frames received per sending Member, routed by the offer's mid table.
    received: Arc<Mutex<HashMap<UserId, usize>>>,
    video: Arc<Mutex<VideoSeen>>,
    sending: Arc<AtomicBool>,
    /// Sends a delta frame every 33 ms on its share uplink, and a keyframe when asked.
    sharing: Arc<AtomicBool>,
    /// Sends a keyframe only by `send_keyframe`: the SFU's requests are not heard.
    holds_keyframes: Arc<AtomicBool>,
    stop: CancellationToken,
    task: JoinHandle<()>,
}

impl TestPeer {
    fn spawn(me: UserId) -> TestPeer {
        let (cmds, rx) = mpsc::channel(8);
        let received = Arc::new(Mutex::new(HashMap::new()));
        let video = Arc::new(Mutex::new(VideoSeen::default()));
        let flags = Flags {
            sending: Arc::new(AtomicBool::new(false)),
            sharing: Arc::new(AtomicBool::new(false)),
            holds_keyframes: Arc::new(AtomicBool::new(false)),
            stop: CancellationToken::new(),
        };
        let task = tokio::spawn(peer_loop(
            me,
            rx,
            received.clone(),
            video.clone(),
            flags.clone(),
        ));
        TestPeer {
            cmds,
            received,
            video,
            sending: flags.sending,
            sharing: flags.sharing,
            holds_keyframes: flags.holds_keyframes,
            stop: flags.stop,
            task,
        }
    }

    async fn send_keyframe(&self) {
        self.cmds.send(PeerCmd::SendKeyframe).await.unwrap();
    }

    fn received_from(&self, u: &UserId) -> usize {
        self.received.lock().unwrap().get(u).copied().unwrap_or(0)
    }

    fn video(&self) -> VideoSeen {
        self.video.lock().unwrap().clone()
    }

    fn video_from(&self, u: &UserId) -> usize {
        self.video
            .lock()
            .unwrap()
            .frames
            .get(u)
            .copied()
            .unwrap_or(0)
    }

    async fn answer(&self, sdp: Sdp, tracks: Vec<RtcTrack>) -> Sdp {
        let (reply, rx) = oneshot::channel();
        self.cmds
            .send(PeerCmd::Offer { sdp, tracks, reply })
            .await
            .unwrap();
        rx.await.unwrap()
    }

    async fn stop(self) {
        self.stop.cancel();
        self.task.await.unwrap();
    }

    /// The client is gone without a DTLS close_notify (killed, unplugged).
    fn vanish(self) {
        self.task.abort();
    }
}

#[derive(Clone)]
struct Flags {
    sending: Arc<AtomicBool>,
    sharing: Arc<AtomicBool>,
    holds_keyframes: Arc<AtomicBool>,
    stop: CancellationToken,
}

/// What names a Sharer in their share's payload.
fn tag(u: &UserId) -> u8 {
    *u.to_string().as_bytes().last().expect("a user id")
}

fn test_rtc() -> Rtc {
    let mut config = Rtc::builder().clear_codecs().enable_opus(true, false);
    config.codec_config().add_h264(
        H264_PT.into(),
        Some(H264_RTX_PT.into()),
        true,
        H264_LEVEL.profile_level_id(),
    );
    config.build(Instant::now())
}

/// Annex B frames: a keyframe is SPS, PPS and an IDR slice (several packets); a delta frame is
/// one non-IDR slice. The last byte names the Sharer, so a Viewer can tell whose share it got.
fn h264_frame(keyframe: bool, sharer: u8) -> Vec<u8> {
    let mut frame = Vec::new();
    if keyframe {
        frame.extend_from_slice(&[0, 0, 0, 1, 0x67, 0x42, 0xe0, 0x1f, 0x8d, 0x68, 0x05]);
        frame.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xce, 0x3c, 0x80]);
    }
    frame.extend_from_slice(&[0, 0, 0, 1, if keyframe { 0x65 } else { 0x41 }]);
    frame.extend(std::iter::repeat_n(
        0x5a,
        if keyframe { 6000 } else { DELTA_BYTES },
    ));
    frame.push(sharer);
    frame
}

async fn peer_loop(
    me: UserId,
    mut cmds: mpsc::Receiver<PeerCmd>,
    received: Arc<Mutex<HashMap<UserId, usize>>>,
    video: Arc<Mutex<VideoSeen>>,
    flags: Flags,
) {
    let socket = UdpSocket::bind(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0))
        .await
        .unwrap();
    let local = socket.local_addr().unwrap();
    let mut rtc = test_rtc();
    rtc.add_local_candidate(Candidate::host(local, "udp").unwrap());
    let mut mids: HashMap<Mid, UserId> = HashMap::new();
    let mut uplink: Option<Mid> = None;
    let mut share_mid: Option<Mid> = None;
    let mut watch: HashMap<Mid, UserId> = HashMap::new();
    let mut buf = vec![0u8; 2000];
    let mut next_frame = Instant::now();
    let mut next_video = Instant::now();
    let mut keyframe_due = false;
    let mut connected = false;
    let (mut ts, mut vts) = (0u64, 0u64);
    loop {
        let timeout = loop {
            match rtc.poll_output().unwrap() {
                Output::Timeout(t) => break t,
                Output::Transmit(t) => {
                    let _ = socket.send_to(&t.contents, t.destination).await;
                }
                Output::Event(Event::Connected) => connected = true,
                Output::Event(Event::MediaData(d)) => {
                    if let Some(from) = mids.get(&d.mid) {
                        *received.lock().unwrap().entry(from.clone()).or_default() += 1;
                    } else if let Some(from) = watch.get(&d.mid) {
                        let mut v = video.lock().unwrap();
                        v.first_was_keyframe
                            .entry(from.clone())
                            .or_insert(d.is_keyframe());
                        *v.frames.entry(from.clone()).or_default() += 1;
                        let Some(&made_by) = d.data.last() else {
                            panic!("an empty share frame");
                        };
                        v.log.push(Frame {
                            at: Instant::now(),
                            keyframe: d.is_keyframe(),
                            from: from.clone(),
                            made_by,
                        });
                    }
                }
                Output::Event(Event::KeyframeRequest(req)) if Some(req.mid) == share_mid => {
                    video.lock().unwrap().keyframe_requests.push(Instant::now());
                    if !flags.holds_keyframes.load(Ordering::SeqCst) {
                        keyframe_due = true;
                    }
                }
                Output::Event(_) => {}
            }
        };
        let send = flags.sending.load(Ordering::SeqCst) && connected;
        let sharing = flags.sharing.load(Ordering::SeqCst);
        let share = sharing && connected && share_mid.is_some();
        if !sharing {
            // A new share starts a new encoder, whose clock starts again.
            vts = 0;
        }
        let mut wake = timeout;
        if send {
            wake = wake.min(next_frame);
        }
        if share {
            wake = wake.min(next_video);
        }
        tokio::select! {
            _ = flags.stop.cancelled() => {
                let _ = rtc.close();
                while let Ok(out) = rtc.poll_output() {
                    match out {
                        Output::Transmit(t) => {
                            let _ = socket.send_to(&t.contents, t.destination).await;
                        }
                        Output::Timeout(_) if !rtc.is_alive() => break,
                        Output::Timeout(t) => rtc.handle_input(Input::Timeout(t)).unwrap(),
                        Output::Event(_) => {}
                    }
                }
                break;
            }
            Some(cmd) = cmds.recv() => match cmd {
                PeerCmd::SendKeyframe => keyframe_due = true,
                PeerCmd::Offer { sdp, tracks, reply } => {
                    let offer = SdpOffer::from_sdp_string(sdp.as_str()).unwrap();
                    let answer = rtc.sdp_api().accept_offer(offer).unwrap();
                    let mid = |t: &RtcTrack| Mid::from(t.mid.as_str());
                    mids = tracks
                        .iter()
                        .filter(|t| t.user_id != me && t.purpose == TrackPurpose::VoiceAudio)
                        .map(|t| (mid(t), t.user_id.clone()))
                        .collect();
                    uplink = tracks
                        .iter()
                        .find(|t| t.user_id == me && t.purpose == TrackPurpose::VoiceAudio)
                        .map(mid);
                    share_mid = tracks
                        .iter()
                        .find(|t| t.purpose == TrackPurpose::ScreenVideo)
                        .map(mid);
                    watch = tracks
                        .iter()
                        .filter(|t| t.purpose == TrackPurpose::WatchVideo)
                        .map(|t| (mid(t), t.user_id.clone()))
                        .collect();
                    let _ = reply.send(Sdp::new(answer.to_sdp_string()));
                }
            },
            r = socket.recv_from(&mut buf) => {
                let Ok((n, source)) = r else { continue };
                let Ok(contents) = buf[..n].try_into() else {
                    continue;
                };
                rtc.handle_input(Input::Receive(
                    Instant::now(),
                    Receive {
                        proto: Protocol::Udp,
                        source,
                        destination: local,
                        contents,
                    },
                ))
                .unwrap();
            }
            _ = tokio::time::sleep_until(wake.into()) => {}
        }
        let now = Instant::now();
        if send
            && now >= next_frame
            && let Some(mid) = uplink
            && let Some(writer) = rtc.writer(mid)
        {
            let pt = writer
                .payload_params()
                .find(|p| p.spec().codec == Codec::Opus)
                .map(|p| p.pt())
                .unwrap();
            // Not a real Opus frame: the SFU forwards payloads without decoding them.
            let frame = vec![0xF8u8; VOICE_BYTES];
            writer
                .write(
                    pt,
                    now,
                    MediaTime::new(ts, Frequency::FORTY_EIGHT_KHZ),
                    frame,
                )
                .unwrap();
            ts += 960;
            next_frame = now + Duration::from_millis(20);
        }
        if share
            && (now >= next_video || keyframe_due)
            && let Some(mid) = share_mid
            && let Some(writer) = rtc.writer(mid)
        {
            let pt = writer
                .payload_params()
                .find(|p| p.spec().codec == Codec::H264)
                .map(|p| p.pt())
                .unwrap();
            let frame = h264_frame(keyframe_due, tag(&me));
            keyframe_due = false;
            let time = MediaTime::new(vts, Frequency::NINETY_KHZ);
            writer.write(pt, now, time, frame).unwrap();
            vts += 3000;
            next_video = now + VIDEO_EVERY;
        }
        rtc.handle_input(Input::Timeout(Instant::now())).unwrap();
    }
}

struct Harness {
    sfu: SfuHandle,
    events: mpsc::UnboundedReceiver<SfuEvent>,
    thread: SfuThread,
    stop: CancellationToken,
    offers: HashMap<UserId, Vec<(u32, usize, Vec<RtcTrack>)>>,
    connected: Vec<(UserId, VoiceSessionId)>,
    lost: Vec<(UserId, VoiceSessionId)>,
    streaming: Vec<(UserId, VoiceSessionId)>,
}

impl Harness {
    async fn start() -> Harness {
        let loopback = Ipv4Addr::LOCALHOST.into();
        let cfg = SfuConfig {
            udp_bind: SocketAddr::new(loopback, 0),
            tcp_bind: SocketAddr::new(loopback, 0),
            advertise_ips: vec![loopback],
            ..Config::default().sfu
        };
        let udp = bind_udp(cfg.udp_bind).unwrap();
        let tcp = bind_tcp(cfg.tcp_bind).unwrap();
        let (sfu, inbox) = channel();
        let (tx, events) = mpsc::unbounded_channel();
        let sink: EventSink = Box::new(move |ev| {
            let _ = tx.send(ev);
        });
        let stop = CancellationToken::new();
        let thread = SfuThread::spawn(
            udp,
            tcp,
            cfg,
            inbox,
            sink,
            stop.clone(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        Harness {
            sfu,
            events,
            thread,
            stop,
            offers: HashMap::new(),
            connected: Vec::new(),
            lost: Vec::new(),
            streaming: Vec::new(),
        }
    }

    fn send(&self, cmd: SfuCmd) {
        self.sfu.try_send(cmd).unwrap();
    }

    async fn drive(
        &mut self,
        peers: &HashMap<UserId, TestPeer>,
        what: &str,
        done: impl Fn(&Harness) -> bool,
    ) {
        let deadline = tokio::time::Instant::now() + WAIT;
        while !done(self) {
            let ev = tokio::time::timeout_at(deadline, self.events.recv())
                .await
                .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
                .unwrap();
            self.handle(peers, ev).await;
        }
    }

    async fn idle(&mut self, peers: &HashMap<UserId, TestPeer>, d: Duration) {
        let until = tokio::time::Instant::now() + d;
        while let Ok(ev) = tokio::time::timeout_at(until, self.events.recv()).await {
            self.handle(peers, ev.unwrap()).await;
        }
    }

    async fn handle(&mut self, peers: &HashMap<UserId, TestPeer>, ev: SfuEvent) {
        let SfuEvent { user, vsid, kind } = ev;
        match kind {
            SfuEventKind::Offer { rev, sdp, tracks } => {
                let mlines = sdp.as_str().lines().filter(|l| l.starts_with("m=")).count();
                self.offers
                    .entry(user.clone())
                    .or_default()
                    .push((rev, mlines, tracks.clone()));
                let Some(peer) = peers.get(&user) else {
                    return;
                };
                let answer = peer.answer(sdp, tracks).await;
                let op = SfuOp::Answer {
                    rev,
                    sdp: answer,
                    rid: None,
                };
                self.send(SfuCmd { user, vsid, op });
            }
            SfuEventKind::Connected => self.connected.push((user, vsid)),
            SfuEventKind::Lost => self.lost.push((user, vsid)),
            SfuEventKind::ShareVideoStarted => self.streaming.push((user, vsid)),
            SfuEventKind::AnswerApplied { .. }
            | SfuEventKind::WatchDropped { .. }
            | SfuEventKind::Speaking(_) => {}
            SfuEventKind::NegotiationFailed { fatal, .. } => {
                assert!(!fatal, "negotiation failed for {user}");
            }
        }
    }

    fn last_offer(&self, u: &UserId) -> Option<&(u32, usize, Vec<RtcTrack>)> {
        self.offers.get(u).and_then(|o| o.last())
    }

    fn routes(&self, u: &UserId, from: &UserId) -> bool {
        self.has_track(u, TrackPurpose::VoiceAudio, Some(from))
    }

    fn has_track(&self, u: &UserId, purpose: TrackPurpose, of: Option<&UserId>) -> bool {
        self.last_offer(u).is_some_and(|(_, _, tracks)| {
            tracks
                .iter()
                .any(|t| t.purpose == purpose && of.is_none_or(|o| *o == t.user_id))
        })
    }

    async fn shutdown(mut self) {
        self.stop.cancel();
        tokio::time::timeout(WAIT, self.thread.join())
            .await
            .expect("the SFU thread stops");
    }
}

fn cmd(u: &UserId, v: u64, op: SfuOp) -> SfuCmd {
    SfuCmd {
        user: u.clone(),
        vsid: vsid(v),
        op,
    }
}

async fn wait_live(h: &Harness, n: usize, within: Duration) {
    let deadline = tokio::time::Instant::now() + within;
    while h.sfu.live_peers() != n {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{} live peers after {within:?}, expected {n}",
            h.sfu.live_peers()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Each joins in order, so the first gets vsid 1.
async fn voice_room(h: &mut Harness, members: &[UserId]) -> HashMap<UserId, TestPeer> {
    for (i, u) in members.iter().enumerate() {
        h.send(cmd(u, i as u64 + 1, SfuOp::Join { muted: false }));
    }
    let peers: HashMap<UserId, TestPeer> = members
        .iter()
        .map(|u| (u.clone(), TestPeer::spawn(u.clone())))
        .collect();
    h.drive(&peers, "everyone connected and routed", |h| {
        h.connected.len() == members.len()
            && members
                .iter()
                .all(|a| members.iter().all(|b| a == b || h.routes(a, b)))
    })
    .await;
    peers
}

async fn start_share(h: &mut Harness, peers: &HashMap<UserId, TestPeer>, sharer: &UserId, v: u64) {
    let quality = Quality::UpTo2160;
    h.send(cmd(sharer, v, SfuOp::StartShare { quality }));
    peers[sharer].sharing.store(true, Ordering::SeqCst);
    let s = sharer.clone();
    h.drive(peers, "the share streaming", |h| {
        h.streaming.iter().any(|(u, id)| *u == s && *id == vsid(v))
    })
    .await;
    assert!(h.has_track(sharer, TrackPurpose::ScreenVideo, Some(sharer)));
}

async fn wait_for(
    h: &mut Harness,
    peers: &HashMap<UserId, TestPeer>,
    what: &str,
    done: impl Fn(&HashMap<UserId, TestPeer>) -> bool,
) {
    let deadline = Instant::now() + WAIT;
    while !done(peers) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        h.idle(peers, Duration::from_millis(20)).await;
    }
}

async fn wait_quiet(
    h: &mut Harness,
    peers: &HashMap<UserId, TestPeer>,
    what: &str,
    quiet: impl Fn(&HashMap<UserId, TestPeer>) -> usize,
    flowing: impl Fn(&HashMap<UserId, TestPeer>) -> usize,
) {
    let mark = Cell::new((quiet(peers), flowing(peers)));
    wait_for(h, peers, what, |p| {
        let (q, f) = (quiet(p), flowing(p));
        if q != mark.get().0 {
            mark.set((q, f));
        }
        f >= mark.get().1 + FLOWING_WHILE_QUIET
    })
    .await;
}

async fn wait_heard(h: &mut Harness, peers: &HashMap<UserId, TestPeer>, n: usize) {
    let heard = |peers: &HashMap<UserId, TestPeer>| {
        peers.iter().all(|(to, p)| {
            peers
                .keys()
                .all(|from| from == to || p.received_from(from) >= n)
        })
    };
    wait_for(h, peers, "audio flowing both ways", heard).await;
}

async fn watch(
    h: &mut Harness,
    peers: &HashMap<UserId, TestPeer>,
    viewer: &UserId,
    v: u64,
    sharer: &UserId,
) {
    h.send(cmd(
        viewer,
        v,
        SfuOp::Watch {
            sharer: sharer.clone(),
        },
    ));
    let (viewer, sharer) = (viewer.clone(), sharer.clone());
    h.drive(peers, "the watch downlink", |h| {
        h.has_track(&viewer, TrackPurpose::WatchVideo, Some(&sharer))
    })
    .await;
}

#[tokio::test]
async fn two_peers_hear_each_other_and_mute_stops_forwarding() {
    let mut h = Harness::start().await;
    let (a, b) = (user(1), user(2));
    let peers = voice_room(&mut h, &[a.clone(), b.clone()]).await;
    for p in peers.values() {
        p.sending.store(true, Ordering::SeqCst);
    }
    wait_heard(&mut h, &peers, 20).await;

    h.send(cmd(&a, 1, SfuOp::SetMuted { muted: true }));
    wait_quiet(
        &mut h,
        &peers,
        "nothing from a while Muted, and b still heard",
        |p| p[&b].received_from(&a),
        |p| p[&a].received_from(&b),
    )
    .await;

    h.send(cmd(&b, 2, SfuOp::Leave));
    h.drive(&peers, "a's downlink for b goes away", |h| {
        !h.routes(&a, &b)
    })
    .await;
    assert!(h.lost.is_empty(), "{:?}", h.lost);
    for (_, p) in peers {
        p.stop().await;
    }
    h.shutdown().await;
}

#[tokio::test]
async fn leaving_peers_are_reaped_even_when_their_client_never_answers() {
    let mut h = Harness::start().await;
    let (a, b) = (user(1), user(2));
    let mut peers = voice_room(&mut h, &[a.clone(), b.clone()]).await;
    wait_live(&h, 2, Duration::from_secs(1)).await;
    peers.remove(&b).unwrap().vanish();
    h.send(cmd(&b, 2, SfuOp::Leave));
    wait_live(&h, 1, REAP_WITHIN).await;
    assert!(h.lost.is_empty(), "{:?}", h.lost);
    for (_, p) in peers {
        p.stop().await;
    }
    h.shutdown().await;
}

/// A shares, B watches: B's downlink starts at the keyframe the SFU asked A for once B's answer
/// was applied, and video keeps flowing. Once the share stops, B gets none of what A still sends.
#[tokio::test]
async fn a_viewer_gets_video_from_a_requested_keyframe_and_stop_share_ends_it() {
    let mut h = Harness::start().await;
    let (a, b) = (user(1), user(2));
    let peers = voice_room(&mut h, &[a.clone(), b.clone()]).await;
    start_share(&mut h, &peers, &a, 1).await;
    assert!(
        peers[&a].video().keyframe_requests.is_empty(),
        "no Viewer, no keyframe request"
    );
    h.send(cmd(&b, 2, SfuOp::Watch { sharer: a.clone() }));
    h.drive(&peers, "b's watch downlink", |h| {
        h.has_track(&b, TrackPurpose::WatchVideo, Some(&a))
    })
    .await;
    wait_for(&mut h, &peers, "b's first frames", |p| {
        p[&b].video_from(&a) >= 5
    })
    .await;
    let seen = peers[&b].video();
    assert_eq!(
        seen.first_was_keyframe.get(&a),
        Some(&true),
        "the first frame a Viewer gets is a keyframe: {seen:?}"
    );
    let asked = &peers[&a].video().keyframe_requests;
    assert_eq!(asked.len(), 1, "one request for the new Viewer: {asked:?}");
    assert!(!h.has_track(&b, TrackPurpose::ScreenVideo, None));

    peers[&a].sending.store(true, Ordering::SeqCst);
    h.send(cmd(&a, 1, SfuOp::StopShare));
    wait_quiet(
        &mut h,
        &peers,
        "no video after the stop, while a's Voice goes on",
        |p| p[&b].video_from(&a),
        |p| p[&b].received_from(&a),
    )
    .await;
    peers[&a].sharing.store(false, Ordering::SeqCst);
    for (_, p) in peers {
        p.stop().await;
    }
    h.shutdown().await;
}

/// Whose frames a Viewer got, in runs, by what each frame carries: `(Sharer tag, frames, the run
/// starts at a keyframe)`.
fn sharer_runs(log: &[Frame]) -> Vec<(char, usize, bool)> {
    let mut runs: Vec<(char, usize, bool)> = Vec::new();
    for f in log {
        match runs.last_mut() {
            Some((who, n, _)) if *who == char::from(f.made_by) => *n += 1,
            _ => runs.push((char::from(f.made_by), 1, f.keyframe)),
        }
    }
    runs
}

/// Every frame came on a line the offers named for the Sharer whose frame it is.
fn assert_labelled_right(log: &[Frame], what: &str) {
    let wrong: Vec<_> = log
        .iter()
        .filter(|f| tag(&f.from) != f.made_by)
        .map(|f| (char::from(f.made_by), char::from(tag(&f.from))))
        .collect();
    assert!(
        wrong.is_empty(),
        "{what}: frames under another's line: {wrong:?}"
    );
}

/// The frames `p` got from `sharer` since `t`, by their payload.
fn made_by_since(p: &TestPeer, sharer: &UserId, t: Instant) -> Vec<Frame> {
    let log = p.video().since(t);
    log.into_iter()
        .filter(|f| f.made_by == tag(sharer))
        .collect()
}

#[tokio::test]
async fn a_switch_keeps_the_old_share_coming_until_the_new_one_shows() {
    let mut h = Harness::start().await;
    let (a, b, c, d) = (user(1), user(2), user(3), user(4));
    let peers = voice_room(&mut h, &[a.clone(), b.clone(), c.clone(), d.clone()]).await;
    start_share(&mut h, &peers, &a, 1).await;
    start_share(&mut h, &peers, &c, 3).await;
    start_share(&mut h, &peers, &d, 4).await;
    watch(&mut h, &peers, &b, 2, &a).await;
    h.idle(&peers, Duration::from_millis(500)).await;
    for (from, to) in [(&a, &c), (&c, &d), (&d, &a)] {
        let what = format!("{} to {}", char::from(tag(from)), char::from(tag(to)));
        // The new share's keyframe comes only once the old one has shown it goes on with the
        // new line ready: without the hold it can come within one frame of the old share.
        peers[to].holds_keyframes.store(true, Ordering::SeqCst);
        let switched = Instant::now();
        watch(&mut h, &peers, &b, 2, to).await;
        let ready = Instant::now();
        wait_for(
            &mut h,
            &peers,
            &format!("{what}: the old share at b while the new gets ready"),
            |p| made_by_since(&p[&b], from, ready).len() >= 5,
        )
        .await;
        peers[to].holds_keyframes.store(false, Ordering::SeqCst);
        peers[to].send_keyframe().await;
        wait_for(
            &mut h,
            &peers,
            &format!("{what}: the new share at b"),
            |p| {
                p[&b]
                    .video()
                    .since(switched)
                    .iter()
                    .any(|f| f.made_by == tag(to))
            },
        )
        .await;
        h.idle(&peers, Duration::from_millis(300)).await;
        let log = peers[&b].video().since(switched);
        let runs = sharer_runs(&log);
        assert_labelled_right(&log, &what);
        let first = log
            .iter()
            .position(|f| f.made_by == tag(to))
            .expect("the new share's first frame");
        assert!(log[first].keyframe, "{what}: from a keyframe: {runs:?}");
        assert!(
            log[first..].iter().all(|f| f.made_by == tag(to)),
            "{what}: nothing of the old share after the new one's first frame: {runs:?}"
        );
        let last_old = &log[first - 1];
        assert_eq!(last_old.made_by, tag(from), "{what}: {runs:?}");
        assert!(!h.has_track(&b, TrackPurpose::WatchVideo, Some(from)));
    }
    let watch_lines = h.offers[&b]
        .iter()
        .flat_map(|(_, _, tracks)| tracks)
        .filter(|t| t.purpose == TrackPurpose::WatchVideo)
        .map(|t| t.mid.clone())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(watch_lines.len(), 2, "two watch lines, never more");
    let (_, _, tracks) = h.last_offer(&b).expect("b's offers");
    let lines = tracks
        .iter()
        .filter(|t| t.purpose == TrackPurpose::WatchVideo);
    assert_eq!(lines.count(), 1, "one line carries the share on screen");
    for (_, p) in peers {
        p.sharing.store(false, Ordering::SeqCst);
        p.stop().await;
    }
    h.shutdown().await;
}
