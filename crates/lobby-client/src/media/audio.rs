//! Audio in and out: the microphone and speakers through cpal, or the test and fake-media
//! stand-ins (a synthetic tone, a clocked sink). Which device each uses is a [`DeviceChoice`];
//! `devices` decides what that means for the OS's lists, and this module opens the streams and
//! moves them when the lists change or the person chooses another device.
//!
//! The cpal streams live on one `lobby-audio` thread, which builds them, reports whether that
//! worked ([`Reports`]), keeps them on the right devices until Voice stops
//! (its [`DeviceCmd`] sender is dropped), and drops them then, output first. The callbacks are
//! real-time: no allocation, no blocking lock, no log. A stream that is rebuilt needs its ring
//! or playout back, so they sit behind a mutex the callback only `try_lock`s (nothing else takes
//! it while a stream runs).

use std::{
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
        mpsc as std_mpsc,
    },
    thread::{JoinHandle, Thread},
    time::{Duration, Instant},
};

use cpal::{
    SampleFormat, StreamConfig,
    traits::{DeviceTrait as _, HostTrait as _, StreamTrait as _},
};
use tracing::{info, warn};

use super::{
    AudioSink, AudioSource, DEVICE_POLL, DEVICE_START_TIMEOUT, DeviceChoice, DeviceKind,
    DeviceSetup, FRAME, MediaConfig, MediaError, MicLevel, MicMeter, NamedDevice, SAMPLE_RATE,
    VoiceEvent, VoiceEvents,
    devices::{Change, Fault, Follower, Target},
    engine,
    playout::Playout,
};

const TICK: Duration = Duration::from_millis(5);
/// How often the `lobby-audio` thread looks at its streams' faults.
const WATCH_TICK: Duration = Duration::from_millis(250);
/// Output callback scratch: 100 ms of mono, far beyond any WASAPI period.
const OUTPUT_SCRATCH: usize = SAMPLE_RATE as usize / 10;
/// A stream that keeps being moved is not the device's choice to make: past this the call ends.
const MOVES_PER_CALL: u32 = 8;

#[derive(Debug, Default)]
pub(crate) struct AudioCounters {
    pub mic_overflow: AtomicU64,
}

pub(crate) struct AudioThreads {
    pub threads: Vec<JoinHandle<()>>,
    /// Dropping it stops the devices.
    pub devices: std_mpsc::Sender<DeviceCmd>,
}

/// What the `lobby-audio` thread is told while the streams run.
#[derive(Debug)]
pub(crate) enum DeviceCmd {
    /// The person chose another device: the stream of that kind moves to it now.
    Choose(DeviceKind, DeviceChoice),
}

/// The write end of the microphone ring. Each whole 20 ms frame in it wakes `reader`, which
/// takes the ring a frame at a time, so the frame is encoded as soon as it is complete.
pub(crate) struct MicTx {
    ring: rtrb::Producer<f32>,
    written: usize,
    reader: Arc<OnceLock<Thread>>,
}

impl MicTx {
    pub fn new(ring: rtrb::Producer<f32>, reader: Arc<OnceLock<Thread>>) -> MicTx {
        MicTx {
            ring,
            written: 0,
            reader,
        }
    }

    /// `false` when the ring is full and the sample is dropped.
    fn push(&mut self, sample: f32) -> bool {
        if self.ring.push(sample).is_err() {
            return false;
        }
        self.written += 1;
        if self.written == FRAME {
            self.written = 0;
            if let Some(reader) = self.reader.get() {
                reader.unpark();
            }
        }
        true
    }
}

/// Who hears how the devices are doing: a call's Session, or the Settings page's microphone
/// test, which keeps only why it failed.
pub(crate) enum Reports {
    Call(VoiceEvents),
    Test(Arc<Mutex<Option<MediaError>>>),
}

impl Reports {
    fn ready(&self) {
        if let Reports::Call(events) = self {
            events.blocking_send(VoiceEvent::DevicesReady);
        }
    }

    fn report(&self, report: Report) {
        match (self, report) {
            (Reports::Call(events), Report::FellBack(kind, wanted)) => {
                events.blocking_send(VoiceEvent::DeviceFellBack {
                    kind,
                    wanted: wanted.name,
                });
            }
            (Reports::Call(events), Report::Failed(e)) => {
                events.blocking_send(VoiceEvent::Failed(e));
            }
            (Reports::Test(failure), Report::Failed(e)) => {
                *failure.lock().expect("mic test failure lock") = Some(e);
            }
            (Reports::Test(_), Report::FellBack(..)) => {}
        }
    }
}

pub(crate) fn start(
    source: AudioSource,
    sink: AudioSink,
    mut mic: MicTx,
    mut playout: Playout,
    stop: Arc<AtomicBool>,
    counters: Arc<AudioCounters>,
    reports: Reports,
) -> Result<AudioThreads, MediaError> {
    let mut threads = Vec::new();
    let mut slots = Vec::new();
    #[cfg(feature = "test-support")]
    let mut broken = None;

    let startup_stop = stop.clone();
    let started = (|| {
        match source {
            AudioSource::Synthetic(tone) => {
                let stop = stop.clone();
                threads.push(thread("lobby-synth", move || {
                    synthesize(|n| tone.sample(n), &mut mic, &stop, &counters)
                })?);
            }
            #[cfg(feature = "test-support")]
            AudioSource::Pulses(pulses) => {
                let stop = stop.clone();
                threads.push(thread("lobby-synth", move || {
                    synthesize(|n| pulses.next(n), &mut mic, &stop, &counters)
                })?);
            }
            AudioSource::Device { setup, level } => {
                slots.push(Slot::input(setup, mic, counters, level));
            }
            #[cfg(feature = "test-support")]
            AudioSource::Broken(e) => broken = Some(e),
        }
        match sink {
            AudioSink::Device(setup) => slots.push(Slot::output(setup, playout)),
            AudioSink::Null => {
                threads.push(thread("lobby-playout", move || {
                    clocked_sink(&mut playout, |_| {}, &stop)
                })?);
            }
            #[cfg(feature = "test-support")]
            AudioSink::Capture(mut out) => {
                threads.push(thread("lobby-playout", move || {
                    clocked_sink(
                        &mut playout,
                        |block| {
                            for s in block {
                                let _ = out.push(*s);
                            }
                        },
                        &stop,
                    )
                })?);
            }
            #[cfg(feature = "test-support")]
            AudioSink::Timed(mut out) => {
                threads.push(thread("lobby-playout", move || {
                    clocked_sink(
                        &mut playout,
                        |block| {
                            let _ = out.push(super::PlayedBlock {
                                at: Instant::now(),
                                pcm: *block,
                            });
                        },
                        &stop,
                    )
                })?);
            }
        }

        let (cmds_tx, cmds) = std_mpsc::channel::<DeviceCmd>();
        threads.push(thread("lobby-audio", move || {
            let mut report = |report: Report| reports.report(report);
            let mut running = Running::new(slots);
            let opened = running.open_all(&mut report);
            #[cfg(feature = "test-support")]
            let opened = opened.and(broken.map_or(Ok(()), Err));
            if let Err(e) = opened {
                warn!(error = %e, "audio devices did not start");
                report(Report::Failed(e));
                return;
            }
            reports.ready();
            running.keep_open(&cmds, &mut report);
            drop(running);
        })?);
        Ok(cmds_tx)
    })();
    match started {
        Ok(devices) => Ok(AudioThreads { threads, devices }),
        Err(e) => {
            startup_stop.store(true, Ordering::SeqCst);
            for thread in threads {
                let _ = thread.join();
            }
            Err(e)
        }
    }
}

fn thread(name: &str, f: impl FnOnce() + Send + 'static) -> Result<JoinHandle<()>, MediaError> {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(f)
        .map_err(|e| MediaError::Device(e.to_string()))
}

/// While it lives, Settings listens to the microphone on streams of its own, the ones Voice would
/// open for the same choices. They follow the devices like Voice does and say nothing about it.
/// Dropping it stops them.
pub struct MicTest {
    /// Dropping it ends the `lobby-audio` thread, which drops the streams.
    _devices: Option<std_mpsc::Sender<DeviceCmd>>,
    stop: Arc<AtomicBool>,
    failure: Arc<Mutex<Option<MediaError>>>,
}

impl MicTest {
    /// The microphone's level alone, measured into `level`.
    pub fn start(setup: DeviceSetup, level: MicLevel) -> MicTest {
        MicTest::running(|_, reports| {
            let (devices, cmds) = std_mpsc::channel::<DeviceCmd>();
            thread("lobby-mic-test", move || {
                // The ring is never read: a sample that has nowhere to go is dropped.
                let (ring, _unread) = rtrb::RingBuffer::new(1);
                let mut running = Running::new(vec![Slot::input(
                    setup,
                    MicTx::new(ring, Arc::default()),
                    Arc::new(AudioCounters::default()),
                    level,
                )]);
                let mut report = |report: Report| reports.report(report);
                match running.open_all(&mut report) {
                    Ok(()) => running.keep_open(&cmds, &mut report),
                    Err(e) => report(Report::Failed(e)),
                }
            })?;
            Ok(devices)
        })
    }

    /// MONITOR with no call on: the microphone through the chain a call builds from `media` to
    /// its speakers, measured along the way as a call is, and sent nowhere.
    pub fn monitored(media: MediaConfig) -> MicTest {
        MicTest::running(|stop, reports| engine::monitor_alone(media, stop, reports))
    }

    fn running<F>(start: F) -> MicTest
    where
        F: FnOnce(Arc<AtomicBool>, Reports) -> Result<std_mpsc::Sender<DeviceCmd>, MediaError>,
    {
        let failure = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let devices = match start(stop.clone(), Reports::Test(failure.clone())) {
            Ok(devices) => Some(devices),
            Err(e) => {
                stop.store(true, Ordering::SeqCst);
                *failure.lock().expect("mic test failure lock") = Some(e);
                None
            }
        };
        MicTest {
            _devices: devices,
            stop,
            failure,
        }
    }

    /// Why the microphone cannot be listened to, once it is known.
    pub fn failure(&self) -> Option<MediaError> {
        self.failure.lock().expect("mic test failure lock").clone()
    }
}

impl Drop for MicTest {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

enum Report {
    /// A named device was left for the system default.
    FellBack(DeviceKind, NamedDevice),
    Failed(MediaError),
}

/// A fault a stream's error callback left for the `lobby-audio` thread.
#[derive(Debug, Default)]
struct FaultFlag(AtomicU8);

impl FaultFlag {
    fn set(&self, fault: Fault) {
        self.0.store(fault as u8, Ordering::Relaxed);
    }

    fn take(&self) -> Option<Fault> {
        Fault::from_code(self.0.swap(0, Ordering::Relaxed))
    }
}

type Build = Box<dyn Fn(&cpal::Device, &Arc<FaultFlag>) -> Result<cpal::Stream, MediaError> + Send>;

pub(super) fn device(
    host: &cpal::Host,
    kind: DeviceKind,
    target: &Target,
) -> Result<cpal::Device, MediaError> {
    match (target, kind) {
        (Target::Default, DeviceKind::Input) => {
            host.default_input_device().ok_or(MediaError::NoDevice)
        }
        (Target::Default, DeviceKind::Output) => host
            .default_output_device()
            .ok_or(MediaError::NoOutputDevice),
        (Target::Id(id), _) => id
            .parse::<cpal::DeviceId>()
            .ok()
            .and_then(|id| host.device_by_id(&id))
            .ok_or(MediaError::DeviceNotFound),
    }
}

/// One stream: what it is following and how to build it on a device.
struct Slot {
    kind: DeviceKind,
    setup: DeviceSetup,
    build: Build,
    follower: Option<Follower>,
    stream: Option<cpal::Stream>,
    fault: Arc<FaultFlag>,
    moves: u32,
}

impl Slot {
    fn input(
        setup: DeviceSetup,
        mic: MicTx,
        counters: Arc<AudioCounters>,
        level: MicLevel,
    ) -> Slot {
        let mic = Arc::new(Mutex::new(mic));
        Slot::new(
            DeviceKind::Input,
            setup,
            Box::new(move |device, fault| {
                let mic = mic.clone();
                let counters = counters.clone();
                input_stream(device, level.clone(), fault, move |sample| {
                    let Ok(mut mic) = mic.try_lock() else {
                        return;
                    };
                    if !mic.push(sample) {
                        counters.mic_overflow.fetch_add(1, Ordering::Relaxed);
                    }
                })
            }),
        )
    }

    fn output(setup: DeviceSetup, playout: Playout) -> Slot {
        let playout = Arc::new(Mutex::new(playout));
        Slot::new(
            DeviceKind::Output,
            setup,
            Box::new(move |device, fault| output_stream(device, playout.clone(), fault)),
        )
    }

    fn new(kind: DeviceKind, setup: DeviceSetup, build: Build) -> Slot {
        Slot {
            kind,
            setup,
            build,
            follower: None,
            stream: None,
            fault: Arc::new(FaultFlag::default()),
            moves: 0,
        }
    }

    /// Closes the stream and opens one on the follower's target. A device that cannot be opened
    /// is left for the default; only a default that cannot be opened is an error.
    fn open(&mut self, host: &cpal::Host) -> Result<Option<NamedDevice>, MediaError> {
        self.stream = None;
        self.fault = Arc::new(FaultFlag::default());
        let target = self.follower().target().clone();
        match self.open_on(host, &target) {
            Ok(()) => Ok(None),
            Err(e) if target != Target::Default => {
                warn!(error = %e, "device did not open; using the system default");
                let fell_back = self.follower_mut().use_default();
                self.open_on(host, &Target::Default)?;
                Ok(fell_back)
            }
            Err(e) => Err(e),
        }
    }

    fn open_on(&mut self, host: &cpal::Host, target: &Target) -> Result<(), MediaError> {
        let device = device(host, self.kind, target)?;
        self.stream = Some((self.build)(&device, &self.fault)?);
        Ok(())
    }

    /// Acts on what the follower decided. Only the moves the person did not ask for count
    /// toward [`MOVES_PER_CALL`]. Returns the named device the stream fell back from, if any.
    fn apply(
        &mut self,
        host: &cpal::Host,
        change: Change,
        asked: bool,
    ) -> Result<Option<NamedDevice>, MediaError> {
        let fell_back = match change {
            Change::Stay => return Ok(None),
            Change::Fail(e) => return Err(e),
            Change::Reopen { fell_back } => fell_back,
        };
        if !asked {
            self.moves += 1;
            if self.moves > MOVES_PER_CALL {
                return Err(MediaError::Device("the audio device keeps changing".into()));
            }
        }
        let also = self.open(host)?;
        log_opened(self.kind);
        Ok(fell_back.or(also))
    }

    fn follower(&self) -> &Follower {
        self.follower.as_ref().expect("a stream follows something")
    }

    fn follower_mut(&mut self) -> &mut Follower {
        self.follower.as_mut().expect("a stream follows something")
    }
}

/// The streams of one call (or one test of the microphone).
struct Running {
    slots: Vec<Slot>,
}

impl Running {
    fn new(slots: Vec<Slot>) -> Running {
        Running { slots }
    }

    fn open_all(&mut self, report: &mut impl FnMut(Report)) -> Result<(), MediaError> {
        if self.slots.is_empty() {
            return Ok(());
        }
        let host = cpal::default_host();
        for slot in &mut self.slots {
            let list = slot.setup.source.list();
            let (follower, mut fell_back) =
                Follower::start(slot.setup.choice.clone(), list.of(slot.kind));
            slot.follower = Some(follower);
            fell_back = slot.open(&host)?.or(fell_back);
            if let Some(wanted) = fell_back {
                report(Report::FellBack(slot.kind, wanted));
            }
            log_opened(slot.kind);
        }
        Ok(())
    }

    /// Until Voice stops (the sender is dropped): a stream the person moves to another device, that
    /// reports a fault, or whose device the list no longer shows, is moved.
    fn keep_open(&mut self, cmds: &std_mpsc::Receiver<DeviceCmd>, report: &mut impl FnMut(Report)) {
        if self.slots.is_empty() {
            while cmds.recv().is_ok() {}
            return;
        }
        let host = cpal::default_host();
        let mut polled = Instant::now();
        loop {
            let mut asked = match cmds.recv_timeout(WATCH_TICK) {
                Ok(DeviceCmd::Choose(kind, choice)) => Some((kind, choice)),
                Err(std_mpsc::RecvTimeoutError::Timeout) => None,
                Err(std_mpsc::RecvTimeoutError::Disconnected) => return,
            };
            let due = polled.elapsed() >= DEVICE_POLL;
            if due {
                polled = Instant::now();
            }
            for slot in &mut self.slots {
                let choice = asked
                    .take_if(|(kind, _)| *kind == slot.kind)
                    .map(|(_, choice)| choice);
                let fault = choice.is_none().then(|| slot.fault.take()).flatten();
                if choice.is_none() && fault.is_none() && !due {
                    continue;
                }
                let list = slot.setup.source.list();
                let endpoints = list.of(slot.kind);
                let follower = slot.follower_mut();
                let (change, chosen) = match (choice, fault) {
                    (Some(choice), _) => (follower.choose(choice, endpoints), true),
                    (None, Some(fault)) => (follower.fault(fault, endpoints), false),
                    (None, None) => (follower.observe(endpoints), false),
                };
                match slot.apply(&host, change, chosen) {
                    Ok(Some(wanted)) => report(Report::FellBack(slot.kind, wanted)),
                    Ok(None) => {}
                    Err(e) => return report(Report::Failed(e)),
                }
            }
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        // Output first: the playout's slots outlive the stream that pulls from them.
        for slot in self.slots.iter_mut().rev() {
            slot.stream = None;
        }
    }
}

fn log_opened(kind: DeviceKind) {
    info!(?kind, "audio stream opened");
}

pub(crate) fn device_error(e: cpal::Error) -> MediaError {
    match e.kind() {
        cpal::ErrorKind::DeviceNotAvailable => MediaError::DeviceNotFound,
        cpal::ErrorKind::DeviceBusy => MediaError::DeviceBusy,
        cpal::ErrorKind::PermissionDenied => MediaError::PermissionDenied,
        _ => MediaError::Device(e.to_string()),
    }
}

fn error_callback(what: &'static str, fault: Arc<FaultFlag>) -> impl FnMut(cpal::Error) {
    move |e| {
        warn!(error = %e, "{what} stream error");
        if let Some(f) = Fault::from_kind(e.kind()) {
            fault.set(f);
        }
    }
}

/// The device at 48 kHz if it offers it, else at its own rate (resampled here), as f32 or i16,
/// downmixed to mono and handed to `on_sample` one sample at a time. Each sample is measured into
/// `level` first, at the device's own rate and before any DSP.
fn input_stream(
    device: &cpal::Device,
    level: MicLevel,
    fault: &Arc<FaultFlag>,
    on_sample: impl FnMut(f32) + Send + 'static,
) -> Result<cpal::Stream, MediaError> {
    let name = device_name(device, "input");
    let default = device.default_input_config().map_err(device_error)?;
    let format = default.sample_format();
    let mut config = default.config();
    if config.sample_rate != SAMPLE_RATE
        && let Ok(mut configs) = device.supported_input_configs()
        && let Some(c) = configs.find(|c| {
            c.channels() == config.channels
                && c.sample_format() == format
                && (c.min_sample_rate()..=c.max_sample_rate()).contains(&SAMPLE_RATE)
        })
    {
        config = c.with_sample_rate(SAMPLE_RATE).config();
    }
    let channels = usize::from(config.channels.max(1));
    let resampler = Resampler::new(config.sample_rate, SAMPLE_RATE);
    let meter = MicMeter::new(config.sample_rate, level);
    info!(device = %name, rate = config.sample_rate, channels, ?format, "microphone");
    let on_error = error_callback("microphone", fault.clone());
    let stream = match format {
        SampleFormat::F32 => device.build_input_stream(
            config,
            mono_input::<f32>(channels, meter, resampler, on_sample),
            on_error,
            Some(DEVICE_START_TIMEOUT),
        ),
        SampleFormat::I16 => device.build_input_stream(
            config,
            mono_input::<i16>(channels, meter, resampler, on_sample),
            on_error,
            Some(DEVICE_START_TIMEOUT),
        ),
        other => {
            return Err(MediaError::Device(format!(
                "unsupported microphone sample format {other:?}"
            )));
        }
    }
    .map_err(device_error)?;
    stream.play().map_err(device_error)?;
    Ok(stream)
}

/// The input callback: each frame of `T` samples downmixed to one f32, measured, then resampled.
fn mono_input<T>(
    channels: usize,
    mut meter: MicMeter,
    mut resampler: Resampler,
    mut on_sample: impl FnMut(f32) + Send + 'static,
) -> impl FnMut(&[T], &cpal::InputCallbackInfo) + Send + 'static
where
    T: cpal::Sample,
    f32: cpal::FromSample<T>,
{
    move |data, _| {
        for frame in data.chunks(channels) {
            let s = frame.iter().map(|v| v.to_sample::<f32>()).sum::<f32>() / frame.len() as f32;
            meter.push(s);
            resampler.push(s, &mut on_sample);
        }
    }
}

fn device_name(device: &cpal::Device, fallback: &str) -> String {
    device
        .description()
        .map(|d| d.name().to_owned())
        .unwrap_or_else(|_| fallback.into())
}

fn output_stream(
    device: &cpal::Device,
    playout: Arc<Mutex<Playout>>,
    fault: &Arc<FaultFlag>,
) -> Result<cpal::Stream, MediaError> {
    let name = device_name(device, "output");
    let config = output_config(device)?;
    let channels = usize::from(config.channels.max(1));
    info!(device = %name, channels, "speakers");
    mono_output(
        device,
        config,
        error_callback("speaker", fault.clone()),
        move |mono| match playout.try_lock() {
            Ok(mut playout) => playout.render(mono),
            Err(_) => mono.fill(0.0),
        },
    )
}

/// The device at 48 kHz f32: WASAPI converts to the mix format.
pub(super) fn output_config(device: &cpal::Device) -> Result<StreamConfig, MediaError> {
    let default = device.default_output_config().map_err(device_error)?;
    Ok(StreamConfig {
        channels: default.channels(),
        sample_rate: SAMPLE_RATE,
        buffer_size: cpal::BufferSize::Default,
    })
}

/// A playing stream: `render` fills mono samples, copied to every channel.
pub(super) fn mono_output(
    device: &cpal::Device,
    config: StreamConfig,
    on_error: impl FnMut(cpal::Error) + Send + 'static,
    mut render: impl FnMut(&mut [f32]) + Send + 'static,
) -> Result<cpal::Stream, MediaError> {
    let channels = usize::from(config.channels.max(1));
    let mut scratch = vec![0f32; OUTPUT_SCRATCH];
    let stream = device
        .build_output_stream::<f32, _, _>(
            config,
            move |data, _| {
                for chunk in data.chunks_mut(OUTPUT_SCRATCH * channels) {
                    let frames = chunk.len() / channels;
                    let mono = &mut scratch[..frames];
                    render(mono);
                    for (out, s) in chunk.chunks_mut(channels).zip(mono.iter()) {
                        out.fill(*s);
                    }
                }
            },
            on_error,
            Some(DEVICE_START_TIMEOUT),
        )
        .map_err(device_error)?;
    stream.play().map_err(device_error)?;
    Ok(stream)
}

fn synthesize(
    mut sample: impl FnMut(u64) -> f32,
    mic: &mut MicTx,
    stop: &AtomicBool,
    counters: &AudioCounters,
) {
    let start = Instant::now();
    let mut produced: u64 = 0;
    while !stop.load(Ordering::SeqCst) {
        let due = (start.elapsed().as_secs_f64() * f64::from(SAMPLE_RATE)) as u64;
        while produced < due {
            if !mic.push(sample(produced)) {
                counters.mic_overflow.fetch_add(1, Ordering::Relaxed);
            }
            produced += 1;
        }
        std::thread::sleep(TICK);
    }
}

fn clocked_sink(playout: &mut Playout, mut deliver: impl FnMut(&[f32; FRAME]), stop: &AtomicBool) {
    let start = Instant::now();
    let mut rendered: u64 = 0;
    let mut block = [0f32; FRAME];
    while !stop.load(Ordering::SeqCst) {
        let due = (start.elapsed().as_secs_f64() * f64::from(SAMPLE_RATE)) as u64;
        while rendered + FRAME as u64 <= due {
            playout.render(&mut block);
            deliver(&block);
            rendered += FRAME as u64;
        }
        std::thread::sleep(TICK);
    }
}

/// Linear interpolation from the device rate to 48 kHz, for microphones that do not offer
/// 48 kHz. Allocation-free and stateful across callbacks.
struct Resampler {
    ratio: f64,
    phase: f64,
    prev: f32,
    passthrough: bool,
}

impl Resampler {
    fn new(from: u32, to: u32) -> Self {
        Self {
            ratio: f64::from(from) / f64::from(to),
            phase: 0.0,
            prev: 0.0,
            passthrough: from == to,
        }
    }

    fn push(&mut self, x: f32, mut emit: impl FnMut(f32)) {
        if self.passthrough {
            emit(x);
            return;
        }
        while self.phase < 1.0 {
            emit(self.prev + (x - self.prev) * self.phase as f32);
            self.phase += self.ratio;
        }
        self.phase -= 1.0;
        self.prev = x;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{IssueKind, copy};

    /// Each way a device can fail ends a call with the Issue that tells the person what to do.
    #[test]
    fn every_device_error_has_its_own_issue() {
        use cpal::ErrorKind as K;
        for (kind, issue) in [
            (K::DeviceNotAvailable, IssueKind::DeviceNotFound),
            (K::DeviceBusy, IssueKind::DeviceInUse),
            (K::PermissionDenied, IssueKind::MediaPermissionDenied),
            (K::BackendError, IssueKind::VoiceEnded),
            (K::UnsupportedConfig, IssueKind::VoiceEnded),
        ] {
            let (code, message) = copy::media_failure(&device_error(cpal::Error::new(kind)));
            assert_eq!(code, issue, "{kind:?}");
            assert!(!message.is_empty());
        }
    }
}
