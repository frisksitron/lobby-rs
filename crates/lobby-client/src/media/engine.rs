//! `VoiceHandle`: owns every media thread of one Voice session. Starting it
//! spawns the threads and returns at once; the `lobby-audio` thread opens the devices and
//! reports `DevicesReady` (or `Failed`) while the Session's `VOICE_JOIN` is on its way, so a
//! slow microphone never blocks the GPUI thread or delays the join.
//! Offers, the mute flag, Deafen, the Members' volumes and a newly chosen device go in through
//! it, and so do the Screen
//! Share's capture and encoder (`start_share`) and the watched share's decoder
//! (`start_watch`); none of them waits for a thread. `shutdown` stops and joins everything
//! within 1.5 s, the share first. Dropping it signals the same stop and hands the threads to a
//! short-lived reaper, so a drop never blocks and never needs a runtime.

#[cfg(feature = "test-support")]
use std::net::SocketAddr;
use std::{
    collections::HashMap,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc as std_mpsc,
    },
    thread::{JoinHandle, Thread},
    time::Duration,
};

use lobby_protocol::{Quality, RtcTrack, Sdp, UserId, VoiceSessionId};
use tokio::{
    runtime::Handle,
    sync::{Notify, mpsc},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::{
    DeviceChoice, DeviceKind, FRAME, MediaConfig, MediaError, SAMPLE_RATE, ShareOpId, VoiceEvent,
    VoiceEvents,
    audio::{self, AudioCounters, DeviceCmd, MicTx, Reports},
    dsp::{self, CaptureDsp},
    meter::DspMeter,
    mic_watch::{MicNotice, MicWatch},
    monitor::{self, MonitorTap},
    opus_codec::{DTX_PACKET, EncoderProfile, MAX_PACKET, VoiceEncoder},
    own_share::OwnShareFeed,
    playout::{Gains, Playout, PlayoutFeed},
    rtc::{self, EncodedFrame, RtcCmd, RtcTask, TransportView},
    screen::ScreenSource,
    vad::Vad,
    video::{PaintSize, VideoFrame},
    watch::{WatchHandle, WatchOut},
};
#[cfg(windows)]
use super::{
    capture::CaptureSetup,
    rtc::KeyframeRequests,
    share::{ENCODED_QUEUE, ShareHandle, ShareLink},
};
use crate::{ClientTask, Volume};

const TEARDOWN_BUDGET: Duration = Duration::from_millis(1500);
/// 200 ms of microphone samples between the capture callback and the encoder.
const MIC_RING: usize = SAMPLE_RATE as usize / 5;
/// 1 s of encoded 20 ms frames queued for the transport.
const TX_QUEUE: usize = 50;
const OFFER_QUEUE: usize = 8;
/// `lobby-voice-tx` is woken by each frame the microphone completes, and looks anyway this
/// often, one frame's time.
const ENCODER_IDLE: Duration = Duration::from_millis(20);

pub(crate) struct VoiceHandle {
    offers: mpsc::Sender<RtcCmd>,
    muted: Arc<AtomicBool>,
    gains: Arc<Gains>,
    stop: Arc<AtomicBool>,
    cancel: CancellationToken,
    /// `None` once stopping: the device thread stops when its channel closes.
    devices: Option<std_mpsc::Sender<DeviceCmd>>,
    threads: Vec<JoinHandle<()>>,
    events: VoiceEvents,
    #[cfg(windows)]
    capture: CaptureSetup,
    #[cfg(windows)]
    share: Option<ShareHandle>,
    watch: Option<WatchHandle>,
    stopped: Vec<JoinHandle<()>>,
    _network: Option<crate::netwatch::NetworkWatch>,
    #[cfg(feature = "test-support")]
    view: Arc<TransportView>,
    #[cfg(feature = "test-support")]
    decoder_fails_after: Option<u64>,
}

impl VoiceHandle {
    pub fn start(
        me: UserId,
        media: MediaConfig,
        volumes: HashMap<UserId, Volume>,
        events: VoiceEvents,
    ) -> Result<VoiceHandle, MediaError> {
        #[cfg(feature = "test-support")]
        if let Some(fault) = media.start_fails.clone() {
            return Err(fault);
        }
        let (frames_tx, frames_rx) = rtrb::RingBuffer::new(TX_QUEUE);
        let frames_ready = Arc::new(Notify::new());
        #[cfg_attr(not(feature = "test-support"), allow(unused_mut))]
        let Chain {
            mic,
            reader: mic_reader,
            mut tx,
            playout,
            feed,
        } = Chain::new(&media, frames_tx, frames_ready.clone())?;
        #[cfg(feature = "test-support")]
        if let Some(frames) = media.mic_silent_after {
            tx.watch.silent_after(frames);
        }
        let stop = Arc::new(AtomicBool::new(false));
        let counters = Arc::new(AudioCounters::default());
        let audio::AudioThreads { threads, devices } = audio::start(
            media.source,
            media.sink,
            mic,
            playout,
            stop.clone(),
            counters.clone(),
            Reports::Call(events.clone()),
        )?;
        let muted = Arc::new(AtomicBool::new(false));
        let cancel = CancellationToken::new();
        let (offers_tx, offers_rx) = mpsc::channel(OFFER_QUEUE);
        let view = Arc::new(TransportView::default());
        // From here an early return drops the handle, whose Drop stops what already runs.
        let mut handle = VoiceHandle {
            offers: offers_tx.clone(),
            muted: muted.clone(),
            gains: feed.gains.clone(),
            stop: stop.clone(),
            cancel: cancel.clone(),
            devices: Some(devices),
            threads,
            events: events.clone(),
            #[cfg(windows)]
            capture: CaptureSetup {
                #[cfg(feature = "test-support")]
                faults: media.capture_faults,
            },
            #[cfg(windows)]
            share: None,
            watch: None,
            stopped: Vec::new(),
            _network: media
                .watch_network
                .then(|| {
                    let cmds = offers_tx.clone();
                    // A full queue drops it: a burst of changes needs only one to arrive.
                    crate::netwatch::NetworkWatch::start(move || {
                        let _ = cmds.try_send(RtcCmd::NetworkChanged);
                    })
                })
                .flatten(),
            #[cfg(feature = "test-support")]
            view: view.clone(),
            #[cfg(feature = "test-support")]
            decoder_fails_after: media.decoder_fails_after,
        };
        let tx_events = events.clone();
        let encoder_thread = std::thread::Builder::new()
            .name("lobby-voice-tx".into())
            .spawn(move || encode(tx, tx_events, muted, stop, counters))
            .map_err(|e| MediaError::Transport(e.to_string()))?;
        let _ = mic_reader.set(encoder_thread.thread().clone());
        handle.threads.push(encoder_thread);
        let task = RtcTask::new(
            me,
            offers_rx,
            frames_rx,
            frames_ready,
            feed,
            events,
            cancel,
            view,
            media.stun,
            media.route,
            volumes,
        );
        let rtc_thread = rtc::spawn(task).map_err(|e| MediaError::Transport(e.to_string()))?;
        handle.threads.push(rtc_thread);
        info!("voice media started");
        Ok(handle)
    }

    pub fn offer(
        &self,
        vsid: VoiceSessionId,
        rev: u32,
        sdp: Sdp,
        tracks: Vec<RtcTrack>,
    ) -> Result<(), MediaError> {
        self.send(RtcCmd::Offer {
            vsid,
            rev,
            sdp,
            tracks,
        })
    }

    fn send(&self, cmd: RtcCmd) -> Result<(), MediaError> {
        self.offers
            .try_send(cmd)
            .map_err(|_| MediaError::Transport("the voice transport is not running".into()))
    }

    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::SeqCst);
    }

    /// Deafened plays nothing: the master gain is 0, and decoding goes on.
    pub fn set_deafened(&self, deafened: bool) {
        self.gains.set_master(if deafened { 0.0 } else { 1.0 });
    }

    /// `lobby-rtc` drains its queue at once, so a full one means Voice is already failing.
    pub fn set_volume(&self, user: UserId, volume: Volume) {
        let _ = self.offers.try_send(RtcCmd::Volume { user, volume });
    }

    /// The microphone or the speakers move to `choice` now, mid-call.
    pub fn choose_device(&self, kind: DeviceKind, choice: DeviceChoice) {
        if let Some(devices) = &self.devices {
            let _ = devices.send(DeviceCmd::Choose(kind, choice));
        }
    }

    #[cfg(windows)]
    pub fn start_share(
        &mut self,
        source: ScreenSource,
        quality: Quality,
        share: ShareOpId,
        own: Arc<OwnShareFeed>,
    ) -> Result<(), MediaError> {
        self.stop_share();
        let (frames_tx, frames_rx) = mpsc::channel(ENCODED_QUEUE);
        let keyframes = Arc::new(KeyframeRequests::default());
        self.send(RtcCmd::Share {
            frames: frames_rx,
            keyframes: keyframes.clone(),
        })?;
        let link = ShareLink {
            frames: frames_tx,
            keyframes,
            own,
            capture: self.capture,
            quality,
        };
        let share = ShareHandle::start(source, link, self.events.clone(), share)?;
        self.share = Some(share);
        Ok(())
    }

    pub fn stop_share(&mut self) {
        #[cfg(windows)]
        if let Some(share) = self.share.take() {
            let _ = self.offers.try_send(RtcCmd::StopShare);
            self.keep_stopped(share.stop());
        }
    }

    /// The decoder of Watch `watch` starts, and the transport feeds it `sharer`'s share once a
    /// keyframe of it comes.
    pub fn start_watch(
        &mut self,
        (pictures, paint): (mpsc::Sender<VideoFrame>, PaintSize),
        watch: ShareOpId,
        sharer: UserId,
    ) -> Result<(), MediaError> {
        self.stop_watch();
        let out = WatchOut {
            pictures,
            paint,
            events: self.events.clone(),
            watch,
            #[cfg(feature = "test-support")]
            fails_after: self.decoder_fails_after,
        };
        let (handle, input) = WatchHandle::start(out, sharer)?;
        self.send(RtcCmd::Watch(input))?;
        self.watch = Some(handle);
        Ok(())
    }

    /// The running Watch moves to `sharer` at their next keyframe; the share fed until then
    /// goes on meanwhile.
    pub fn want_watch(&self, sharer: UserId) {
        if let Some(watch) = &self.watch {
            watch.want(sharer);
        }
    }

    pub fn stop_watch(&mut self) {
        if let Some(watch) = self.watch.take() {
            let _ = self.offers.try_send(RtcCmd::Unwatch);
            self.keep_stopped(watch.stop());
        }
    }

    fn keep_stopped(&mut self, thread: JoinHandle<()>) {
        // Finished ones are detached; nothing is left to wait for.
        self.stopped.retain(|t| !t.is_finished());
        self.stopped.push(thread);
    }

    #[cfg(feature = "test-support")]
    pub fn local_addr(&self) -> Option<SocketAddr> {
        *self.view.local.lock().expect("transport view lock")
    }

    #[cfg(feature = "test-support")]
    pub fn share_video_sent(&self) -> (u64, u64) {
        (
            self.view.video_frames.load(Ordering::Relaxed),
            self.view.video_bytes.load(Ordering::Relaxed),
        )
    }

    #[cfg(feature = "test-support")]
    pub fn udp_bytes_sent(&self) -> u64 {
        self.view.udp_bytes_sent.load(Ordering::Relaxed)
    }

    #[cfg(feature = "test-support")]
    pub fn watched_profile_level(&self) -> Option<u32> {
        Some(self.view.watched_profile_level.load(Ordering::Relaxed)).filter(|id| *id != 0)
    }

    /// Every thread to wait for, the share's first (capture, encoder, decoder, then voice).
    fn all_threads(&mut self) -> Vec<JoinHandle<()>> {
        let mut threads: Vec<JoinHandle<()>> = Vec::new();
        #[cfg(windows)]
        if let Some(share) = self.share.take() {
            threads.push(share.stop());
        }
        if let Some(watch) = self.watch.take() {
            threads.push(watch.stop());
        }
        threads.append(&mut self.stopped);
        threads.append(&mut self.threads);
        threads
    }

    fn signal_stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.cancel.cancel();
        self.devices = None;
    }

    pub fn shutdown(mut self, rt: &Handle) -> ClientTask<()> {
        self.signal_stop();
        let threads = self.all_threads();
        ClientTask::spawn(rt, async move {
            let (done_tx, done_rx) = tokio::sync::oneshot::channel();
            let joiner = std::thread::Builder::new()
                .name("lobby-voice-join".into())
                .spawn(move || {
                    join(threads);
                    let _ = done_tx.send(());
                });
            if joiner.is_err() {
                warn!("could not start the voice teardown thread");
                return;
            }
            match tokio::time::timeout(TEARDOWN_BUDGET, done_rx).await {
                Ok(_) => info!("voice media stopped"),
                Err(_) => warn!("voice threads did not stop within 1.5 s; abandoned"),
            }
        })
    }
}

impl Drop for VoiceHandle {
    fn drop(&mut self) {
        self.signal_stop();
        let threads = self.all_threads();
        if threads.is_empty() {
            return;
        }
        // Never blocks the dropping thread (the GPUI thread, a test's teardown).
        let reaper = std::thread::Builder::new()
            .name("lobby-voice-reaper".into())
            .spawn(move || join(threads));
        if reaper.is_err() {
            warn!("could not start the voice reaper thread");
        }
    }
}

/// The microphone's way to the speakers as a call and Settings' MONITOR both build it from a
/// [`MediaConfig`]: the cleaning, the encoder and the monitor's tap on the encoder thread's side,
/// the playout on the speakers'.
struct Chain {
    mic: MicTx,
    /// Set to the thread that runs `tx`, which each whole frame of the microphone wakes.
    reader: Arc<OnceLock<Thread>>,
    tx: VoiceTx,
    playout: Playout,
    feed: PlayoutFeed,
}

impl Chain {
    fn new(
        media: &MediaConfig,
        frames: rtrb::Producer<EncodedFrame>,
        frames_ready: Arc<Notify>,
    ) -> Result<Chain, MediaError> {
        let (mic, mic_rx) = rtrb::RingBuffer::new(MIC_RING);
        let reader = Arc::new(OnceLock::new());
        let (tap, reference) = dsp::render_ring(media.dsp.clone());
        let (heard, monitored) = monitor::monitor(media.monitor.clone())?;
        let (playout, feed) = Playout::new(tap, monitored)?;
        let dsp = CaptureDsp::new(
            media.dsp.clone(),
            reference,
            DspMeter::new(media.levels.clone()),
        );
        Ok(Chain {
            mic: MicTx::new(mic, reader.clone()),
            reader,
            tx: VoiceTx::new(mic_rx, frames, frames_ready, dsp, heard)?,
            playout,
            feed,
        })
    }
}

/// Settings' MONITOR while no call is on: [`Chain`] with no transport and no Session. Its
/// threads end with `stop` and with the returned sender.
pub(super) fn monitor_alone(
    media: MediaConfig,
    stop: Arc<AtomicBool>,
    reports: Reports,
) -> Result<std_mpsc::Sender<DeviceCmd>, MediaError> {
    // Nothing reads what it encodes: the monitor's tap has decoded it by then.
    let (frames, _unsent) = rtrb::RingBuffer::new(1);
    let Chain {
        mic,
        reader,
        mut tx,
        playout,
        feed: _,
    } = Chain::new(&media, frames, Arc::new(Notify::new()))?;
    let counters = Arc::new(AudioCounters::default());
    let audio::AudioThreads { devices, .. } = audio::start(
        media.source,
        media.sink,
        mic,
        playout,
        stop.clone(),
        counters,
        reports,
    )?;
    let encoder = std::thread::Builder::new()
        .name("lobby-monitor-tx".into())
        .spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                if let Captured::Short = tx.frame(false) {
                    std::thread::park_timeout(ENCODER_IDLE);
                }
            }
            info!("monitor without a call stopped");
        })
        .map_err(|e| MediaError::Transport(e.to_string()))?;
    let _ = reader.set(encoder.thread().clone());
    info!("monitor without a call started");
    Ok(devices)
}

fn join(threads: Vec<JoinHandle<()>>) {
    for t in threads {
        let name = t.thread().name().unwrap_or("voice").to_owned();
        if t.join().is_err() {
            warn!(thread = %name, "voice thread panicked");
        }
    }
}

/// `lobby-voice-tx`: 20 ms frames from the microphone ring through the VAD and Opus to the
/// transport.
fn encode(
    mut tx: VoiceTx,
    events: VoiceEvents,
    muted: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    counters: Arc<AudioCounters>,
) {
    // The latest VAD verdict the Session has not taken yet: a full queue delays it by a frame
    // or more, but never loses it (a lost `false` would leave the ring lit while silent).
    let mut unsent: Option<bool> = None;
    // A notice is said once, so a full queue holds it back rather than losing it.
    let mut unsent_notice: Option<MicNotice> = None;
    while !stop.load(Ordering::SeqCst) {
        let Captured::Frame { speaking, notice } = tx.frame(muted.load(Ordering::SeqCst)) else {
            std::thread::park_timeout(ENCODER_IDLE);
            continue;
        };
        if let Some(speaking) = speaking {
            debug!(speaking, "vad");
            unsent = Some(speaking);
        }
        if let Some(speaking) = unsent
            && events.try_send(VoiceEvent::Speaking(speaking))
        {
            unsent = None;
        }
        unsent_notice = unsent_notice.or(notice);
        if let Some(n) = unsent_notice
            && events.try_send(match n {
                MicNotice::TalkingMuted => VoiceEvent::TalkingMuted,
                MicNotice::Silent => VoiceEvent::MicSilent,
            })
        {
            unsent_notice = None;
        }
    }
    if tx.failed > 0 {
        warn!(frames = tx.failed, "opus encode failed");
    }
    debug!(
        dropped = tx.dropped,
        mic_overflow = counters.mic_overflow.load(Ordering::Relaxed),
        "encoder stopped"
    );
}

pub(crate) enum Captured {
    /// The microphone ring holds less than a frame.
    Short,
    Frame {
        speaking: Option<bool>,
        notice: Option<MicNotice>,
    },
}

/// One 20 ms frame of `lobby-voice-tx` at a time: the DSP, the VAD, then Opus, then the
/// transport's queue. Muted sends nothing, and neither does DTX. The RTP clock advances with
/// every captured frame, sent or not, so a receiver sees the gap for what it is. The DSP runs
/// while muted too, so the echo canceller stays adapted, and so does the [`MicWatch`].
pub(crate) struct VoiceTx {
    mic: rtrb::Consumer<f32>,
    dsp: CaptureDsp,
    encoder: VoiceEncoder,
    profile: EncoderProfile,
    vad: Vad,
    watch: MicWatch,
    /// The frame as the device delivered it: the DSP rewrites `pcm` in place.
    raw: [f32; FRAME],
    frames: rtrb::Producer<EncodedFrame>,
    frames_ready: Arc<Notify>,
    monitor: MonitorTap,
    pcm: [f32; FRAME],
    filled: usize,
    ts: u64,
    /// Frames the full queue turned away, and frames libopus failed on.
    dropped: u64,
    failed: u64,
}

impl VoiceTx {
    pub fn new(
        mic: rtrb::Consumer<f32>,
        frames: rtrb::Producer<EncodedFrame>,
        frames_ready: Arc<Notify>,
        dsp: CaptureDsp,
        monitor: MonitorTap,
    ) -> Result<Self, MediaError> {
        let profile = EncoderProfile::studio(dsp.studio());
        Ok(Self {
            mic,
            encoder: VoiceEncoder::new(profile)?,
            profile,
            dsp,
            vad: Vad::new(),
            watch: MicWatch::new(),
            raw: [0.0; FRAME],
            frames,
            frames_ready,
            monitor,
            pcm: [0.0; FRAME],
            filled: 0,
            ts: 0,
            dropped: 0,
            failed: 0,
        })
    }

    pub fn frame(&mut self, muted: bool) -> Captured {
        self.dsp.sync();
        self.sync_encoder();
        while self.filled < FRAME {
            let Ok(s) = self.mic.pop() else {
                return Captured::Short;
            };
            self.pcm[self.filled] = s;
            self.filled += 1;
        }
        self.filled = 0;
        self.raw = self.pcm;
        self.dsp.process(&mut self.pcm);
        let speaking = if muted {
            self.vad.silence()
        } else {
            self.vad.process(&self.pcm)
        };
        let notice = self.watch.frame(&self.raw, &self.pcm, muted);
        if !muted {
            self.send();
        }
        self.ts += FRAME as u64;
        Captured::Frame { speaking, notice }
    }

    /// A new profile builds a new encoder, which allocates: between frames, like the DSP. If
    /// libopus refuses, the old encoder stays until the next change.
    fn sync_encoder(&mut self) {
        let wanted = EncoderProfile::studio(self.dsp.studio());
        if wanted == self.profile {
            return;
        }
        self.profile = wanted;
        match VoiceEncoder::new(wanted) {
            Ok(encoder) => self.encoder = encoder,
            Err(e) => warn!(%e, ?wanted, "opus encoder not rebuilt"),
        }
    }

    fn send(&mut self) {
        let mut frame = EncodedFrame {
            ts: self.ts,
            dbov: self.vad.dbov(),
            voiced: self.vad.speaking(),
            len: 0,
            data: [0; MAX_PACKET],
        };
        let encoded = self.encoder.encode(&self.pcm, &mut frame.data);
        self.monitor.frame(encoded.map(|n| &frame.data[..n]));
        match encoded {
            Some(n) if n <= DTX_PACKET => {}
            Some(n) => {
                frame.len = n as u16;
                if self.frames.push(frame).is_ok() {
                    self.frames_ready.notify_one();
                } else {
                    self.dropped += 1;
                }
            }
            None => self.failed += 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        DspConfig, DspControl, MonitorControl, NoiseSuppression,
        media::{
            Tone,
            meter::{DspLevels, DspMeter},
            mic_watch::MicNotice,
            monitor::MonitorMix,
        },
    };

    /// The encoder thread's frame path alone, with a microphone ring the test fills.
    struct Rig {
        mic: rtrb::Producer<f32>,
        tx: VoiceTx,
        _sent: rtrb::Consumer<EncodedFrame>,
        tone: Tone,
        produced: u64,
    }

    impl Rig {
        fn new() -> Rig {
            let control = DspControl::new(DspConfig {
                noise: NoiseSuppression {
                    on: false,
                    ..NoiseSuppression::default()
                },
                ..DspConfig::default()
            });
            Rig::with(control, MonitorControl::default()).0
        }

        /// The rig, and the playout's end of its monitor.
        fn with(control: DspControl, monitor: MonitorControl) -> (Rig, MonitorMix) {
            let (mic, mic_rx) = rtrb::RingBuffer::new(FRAME * 2);
            let (frames_tx, sent) = rtrb::RingBuffer::new(4);
            let (_tap, reference) = dsp::render_ring(control.clone());
            let (heard, monitored) = monitor::monitor(monitor).expect("decoder");
            let tx = VoiceTx::new(
                mic_rx,
                frames_tx,
                Arc::new(Notify::new()),
                CaptureDsp::new(control, reference, DspMeter::new(DspLevels::default())),
                heard,
            )
            .expect("encoder");
            let rig = Rig {
                mic,
                tx,
                _sent: sent,
                tone: Tone::hz(440.0),
                produced: 0,
            };
            (rig, monitored)
        }

        /// `frames` of 20 ms: a tone, or zeros as a switched-off microphone sends. Returns the
        /// notices with the frame they came on.
        fn run(&mut self, frames: u32, tone: bool, muted: bool) -> Vec<(u32, MicNotice)> {
            let mut told = Vec::new();
            for i in 0..frames {
                for _ in 0..FRAME {
                    let s = if tone {
                        self.tone.sample(self.produced)
                    } else {
                        0.0
                    };
                    self.produced += 1;
                    self.mic.push(s).expect("room in the ring");
                }
                let Captured::Frame { notice, .. } = self.tx.frame(muted) else {
                    panic!("a whole frame was pushed");
                };
                told.extend(notice.map(|n| (i, n)));
            }
            told
        }
    }

    #[test]
    fn talking_while_muted_reaches_the_events_once() {
        let mut rig = Rig::new();
        let told = rig.run(200, true, true);
        assert_eq!(told.len(), 1, "{told:?}");
        assert_eq!(told[0].1, MicNotice::TalkingMuted);
        assert!(
            rig.run(200, true, false).is_empty(),
            "talking unmuted is the point of Voice"
        );
    }

    #[test]
    fn a_microphone_of_zeros_is_told_after_five_seconds_and_again_after_sound_returns() {
        let mut rig = Rig::new();
        assert!(
            rig.run(crate::media::mic_watch::SILENT_FRAMES - 1, false, false)
                .is_empty()
        );
        assert_eq!(rig.run(1, false, false), vec![(0, MicNotice::Silent)]);
        assert!(
            rig.run(1000, false, false).is_empty(),
            "once per stretch of silence"
        );
        assert!(rig.run(5, true, false).is_empty());
        let again = rig.run(1000, false, false);
        assert_eq!(again.len(), 1, "{again:?}");
    }

    #[test]
    fn the_monitored_voice_reaches_the_speakers_and_never_the_echo_reference() {
        let control = DspControl::new(DspConfig {
            echo_cancellation: true,
            noise: NoiseSuppression {
                on: false,
                ..NoiseSuppression::default()
            },
            ..DspConfig::default()
        });
        let monitor = MonitorControl::default();
        monitor.set_on(true);
        let (mut rig, monitored) = Rig::with(control.clone(), monitor);
        let (tap, mut reference) = dsp::render_ring(control);
        let (mut playout, _feed) = Playout::new(tap, monitored).expect("decoders");

        let mut played = Vec::new();
        let mut block = [0.0; FRAME];
        let mut references = 0;
        for _ in 0..25 {
            rig.run(1, true, false);
            playout.render(&mut block);
            played.extend_from_slice(&block);
            while let Ok(frame) = reference.pop() {
                assert!(
                    frame.iter().all(|s| *s == 0.0),
                    "the echo reference holds no voice"
                );
                references += 1;
            }
        }
        assert_eq!(
            references, 50,
            "the playout fed the echo canceller every 10 ms"
        );
        let tail = &played[played.len() - FRAME * 10..];
        let rms = (tail.iter().map(|s| s * s).sum::<f32>() / tail.len() as f32).sqrt();
        assert!(rms > 0.1, "the 0.25 tone through Opus is heard: rms {rms}");
    }
}
