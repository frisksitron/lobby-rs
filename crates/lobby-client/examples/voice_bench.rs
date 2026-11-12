//! Measures Voice's mouth-to-ear latency: an in-process Lobby, Alice's microphone a pulse train
//! and Bob's speakers a sink that notes when it played each 20 ms block. Each pulse is timed from
//! the instant its first sample entered Alice's microphone ring to the instant Bob's sink played
//! it, on this process's one clock. Prints one JSON object on stdout and a summary on stderr.
//!
//! ```text
//! cargo run --release -p lobby-client --example voice_bench -- --noise rnnoise --seconds 30
//! ```
//!
//! Flags, all optional:
//! - `--noise rnnoise|webrtc|dpdfnet|off` (default rnnoise, the Settings' default) and `--aec`:
//!   Alice's capture DSP, with the compressor as Settings start it. `--noise off` without `--aec`
//!   is the microphone through the compressor and the limiter alone. DPDFNet takes the pulses for
//!   noise and removes them, here and under `--calibrate`: with it the bench times nothing, and
//!   shows only that the call carries it.
//! - `--studio`: echo cancellation, noise suppression and the compressor all out, so the
//!   microphone goes raw into the low-delay encoder; it overrides the two.
//! - `--seconds N`: seconds measured, after `--warmup N` (defaults 30 and 3).
//! - `--pulse buzz|tone`: a harmonic glide like a voiced vowel (default), or a 1 kHz tone.
//! - `--loss PERCENT`, `--delay-ms N`, `--jitter-ms N`: Bob's downlink through a relay that loses
//!   and holds datagrams.
//! - `--bob-after-ms N`: Bob joins N ms after Alice. Their media clocks start at their joins, so
//!   this moves Bob's playout pops against Alice's frames: the jitter buffer's wait for the next
//!   pop, which in a real call falls anywhere in 0-20 ms.
//! - `--calibrate`: no Lobby. The pulses go through each DSP setting and Opus offline, delayed by
//!   known amounts first, and the detector reports what it finds.
//! - `--move-output`: no timing. Alice sends a steady tone, and Bob plays it, deafened so the room
//!   hears only silence, on the real default output. A third of the way in Bob's speakers move to
//!   another real output, and two thirds in back to the default; the report is Bob's slot log of
//!   Alice, a line a second, around the moves. `--noise off` keeps every frame of the tone sent.
//! - `--monitor`: Alice hears herself. The timed sink is Alice's own and Bob plays nothing, so the
//!   latency is the monitor's: the DSP, Opus and the playout's queue, no network.
//!
//! Bob's sink plays a block from the instant it takes it from the playout, so a sample's play
//! instant is its block's plus its place in the block. A real output device adds its own buffer
//! after that, and a real microphone its own before the ring: `audio_device_probe` measures both.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use lobby_client::{
    AudioSink, AudioSource, Compressor, DeviceChoice, DeviceKind, DeviceSetup, DeviceSource as _,
    DspConfig, DspControl, Intent, MediaConfig, MemoryTokenStore, MonitorControl, NamedDevice,
    NoiseEngine, NoiseSuppression, SystemDevices, Tone,
    testing::{CaptureChain, CodecLoop, Onset, PlayedBlock, PulseShape, Pulses, test_client},
};
use lobby_server::{
    MediaPath,
    testing::{Direction, Impairment, MediaRelay, TestServer},
};
use serde_json::{Value, json};
use tracing_subscriber::{Layer as _, layer::SubscriberExt as _, util::SubscriberInitExt as _};

#[path = "../tests/integration/common/mod.rs"]
#[allow(dead_code)]
mod common;

use common::{Member, WAIT, active, origin, until};

const USAGE: &str = "usage: voice_bench [--noise rnnoise|webrtc|dpdfnet|off] [--aec] [--studio] \
[--seconds N] [--warmup N] [--pulse buzz|tone] [--loss PERCENT] [--delay-ms N] [--jitter-ms N] \
[--bob-after-ms N] [--calibrate] [--move-output] [--monitor]";

const RATE: f64 = 48_000.0;
const FRAME: usize = 960;
/// About 438 ms between pulses: prime, so the pulses fall at every place in a 20 ms frame.
const EVERY: u64 = 21_011;
/// 150 ms pulses.
const LENGTH: u64 = 7_200;
const AMPLITUDE: f32 = 0.5;
/// The part of a pulse matched against what Bob heard: 100 ms.
const TEMPLATE: usize = 4_800;
/// 1 ms envelope bins for the coarse onset.
const BIN: usize = 48;
/// The coarse onset is the first bin at this share of the window's loud level.
const ONSET_SHARE: f32 = 0.3;
/// Quieter than this, a window holds no pulse.
const SILENT_RMS: f32 = 1e-3;
/// The match searches this far before and after the coarse onset.
const SEARCH_BEFORE: usize = 1_440;
const SEARCH_AFTER: usize = 480;
/// A match below this correlation is a missed pulse.
const MIN_NCC: f32 = 0.5;
/// Bob's sink ring: 40 s of blocks, drained every few ms.
const SINK_BLOCKS: usize = 2_000;
const DRAIN_EVERY: Duration = Duration::from_millis(5);
/// Pulses this close to the end may still be on their way.
const TAIL: Duration = Duration::from_secs(1);
/// Delays the calibration puts in front of the chain, in samples.
const CALIBRATION_DELAYS: [usize; 4] = [0, 333, 960, 4_321];
const CALIBRATION_PULSES: u64 = 16;

#[derive(Debug, Clone, Copy)]
struct Dsp {
    /// None: the noise module out.
    noise: Option<NoiseEngine>,
    aec: bool,
    studio: bool,
}

impl Dsp {
    fn name(self) -> String {
        if self.studio {
            return "studio".into();
        }
        let noise = match self.noise {
            Some(NoiseEngine::Rnnoise) => "rnnoise",
            Some(NoiseEngine::Webrtc) => "webrtc",
            Some(NoiseEngine::Dpdfnet) => "dpdfnet",
            None => "off",
        };
        format!("noise={noise} aec={}", self.aec)
    }

    /// The compressor as Settings start it, or out with the other two for `studio`.
    fn config(self) -> DspConfig {
        if self.studio {
            return DspConfig {
                noise: NoiseSuppression {
                    on: false,
                    ..NoiseSuppression::default()
                },
                echo_cancellation: false,
                compressor: Compressor {
                    on: false,
                    ..Compressor::default()
                },
            };
        }
        DspConfig {
            noise: NoiseSuppression {
                on: self.noise.is_some(),
                engine: self.noise.unwrap_or_default(),
            },
            echo_cancellation: self.aec,
            ..DspConfig::default()
        }
    }
}

struct Args {
    dsp: Dsp,
    seconds: u64,
    warmup: u64,
    shape: PulseShape,
    impairment: Option<Impairment>,
    bob_after: Duration,
    calibrate: bool,
    move_output: bool,
    monitor: bool,
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut out = Args {
        dsp: Dsp {
            noise: Some(NoiseEngine::Rnnoise),
            aec: false,
            studio: false,
        },
        seconds: 30,
        warmup: 3,
        shape: buzz(),
        impairment: None,
        bob_after: Duration::ZERO,
        calibrate: false,
        move_output: false,
        monitor: false,
    };
    let mut impairment = Impairment::default();
    let mut impaired = false;
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or(format!("{flag} needs a value"));
        let number = |text: String| {
            text.parse::<u64>()
                .map_err(|_| format!("{flag}: `{text}` is not a number"))
        };
        match flag.as_str() {
            "--noise" => {
                out.dsp.noise = match value()?.as_str() {
                    "rnnoise" => Some(NoiseEngine::Rnnoise),
                    "webrtc" => Some(NoiseEngine::Webrtc),
                    "dpdfnet" => Some(NoiseEngine::Dpdfnet),
                    "off" => None,
                    other => return Err(format!("unknown noise suppression {other}")),
                };
            }
            "--aec" => out.dsp.aec = true,
            "--studio" => out.dsp.studio = true,
            "--seconds" => out.seconds = number(value()?)?,
            "--warmup" => out.warmup = number(value()?)?,
            "--pulse" => {
                out.shape = match value()?.as_str() {
                    "buzz" => buzz(),
                    "tone" => PulseShape::Tone { hz: 1000.0 },
                    other => return Err(format!("unknown pulse {other}")),
                };
            }
            "--loss" => {
                let text = value()?;
                impairment.loss_percent = text
                    .parse()
                    .map_err(|_| format!("--loss: `{text}` is not a number"))?;
                impaired = true;
            }
            "--delay-ms" => {
                impairment.delay = Duration::from_millis(number(value()?)?);
                impaired = true;
            }
            "--jitter-ms" => {
                impairment.jitter = Duration::from_millis(number(value()?)?);
                impaired = true;
            }
            "--bob-after-ms" => out.bob_after = Duration::from_millis(number(value()?)?),
            "--calibrate" => out.calibrate = true,
            "--move-output" => out.move_output = true,
            "--monitor" => out.monitor = true,
            other => return Err(format!("unknown flag {other}")),
        }
    }
    if out.seconds == 0 {
        return Err("--seconds must be at least 1".into());
    }
    out.impairment = impaired.then_some(impairment);
    Ok(out)
}

fn buzz() -> PulseShape {
    PulseShape::Buzz {
        from_hz: 120.0,
        to_hz: 200.0,
    }
}

fn pulses(shape: PulseShape) -> Pulses {
    Pulses::new(shape, EVERY, LENGTH, AMPLITUDE)
}

/// Where a pulse was found in a stream of samples.
#[derive(Debug, Clone, Copy)]
struct Found {
    /// The sample the pulse's first sample became.
    at: usize,
    ncc: f32,
    /// The first loud bin, which a suppressor that opens late moves later than `at`.
    envelope_at: usize,
}

struct Detector {
    template: Vec<f32>,
    template_energy: f64,
}

impl Detector {
    fn new(pulses: &Pulses) -> Detector {
        let template: Vec<f32> = (0..TEMPLATE as u64).map(|n| pulses.sample(n)).collect();
        let template_energy = template.iter().map(|x| f64::from(*x).powi(2)).sum();
        Detector {
            template,
            template_energy,
        }
    }

    /// The pulse in `signal[from..to]`: the loud part's first bin, then the lag around it where
    /// the signal best matches the pulse.
    fn find(&self, signal: &[f32], from: usize, to: usize) -> Option<Found> {
        let to = to.min(signal.len());
        if to <= from + TEMPLATE {
            return None;
        }
        let rms: Vec<f32> = signal[from..to]
            .chunks(BIN)
            .map(|bin| (bin.iter().map(|x| x * x).sum::<f32>() / bin.len() as f32).sqrt())
            .collect();
        let mut sorted = rms.clone();
        sorted.sort_by(f32::total_cmp);
        let loud = sorted[sorted.len() * 9 / 10];
        if loud < SILENT_RMS {
            return None;
        }
        let first = rms.iter().position(|r| *r >= loud * ONSET_SHARE)?;
        let envelope_at = from + first * BIN;
        let lo = envelope_at.saturating_sub(SEARCH_BEFORE).max(from);
        let hi = (envelope_at + SEARCH_AFTER).min(to - TEMPLATE);
        let mut best: Option<(usize, f32)> = None;
        for lag in lo..=hi {
            let window = &signal[lag..lag + TEMPLATE];
            let (mut dot, mut energy) = (0.0f64, 0.0f64);
            for (s, t) in window.iter().zip(&self.template) {
                dot += f64::from(*s) * f64::from(*t);
                energy += f64::from(*s).powi(2);
            }
            if energy == 0.0 {
                continue;
            }
            let ncc = (dot / (energy * self.template_energy).sqrt()) as f32;
            if best.is_none_or(|(_, b)| ncc > b) {
                best = Some((lag, ncc));
            }
        }
        best.map(|(at, ncc)| Found {
            at,
            ncc,
            envelope_at,
        })
    }
}

/// What a chain did to a signal, and the compute time per 20 ms frame, in µs.
struct Chained {
    signal: Vec<f32>,
    dsp_us: f64,
    opus_us: f64,
}

/// `pulses` from sample 0, `delay` samples late, through the DSP (if any) and then Opus (if
/// `opus`), in 20 ms frames as `lobby-voice-tx` runs them.
fn through_chain(
    pulses: &Pulses,
    count: u64,
    delay: usize,
    dsp: Option<Dsp>,
    opus: bool,
) -> Chained {
    let total = (count * EVERY) as usize + delay;
    let frames = total.div_ceil(FRAME) + 1;
    let mut signal = vec![0.0f32; frames * FRAME];
    for (i, s) in signal.iter_mut().enumerate().skip(delay) {
        *s = pulses.sample((i - delay) as u64);
    }
    let mut chain = dsp.map(|d| CaptureChain::new(d.config()));
    let studio = dsp.is_some_and(|d| d.studio);
    let mut codec = opus.then(|| CodecLoop::new(studio).expect("opus"));
    let (mut dsp_time, mut opus_time) = (Duration::ZERO, Duration::ZERO);
    for frame in signal.chunks_exact_mut(FRAME) {
        if let Some(chain) = &mut chain {
            let started = Instant::now();
            chain.process(frame);
            dsp_time += started.elapsed();
        }
        if let Some(codec) = &mut codec {
            let started = Instant::now();
            codec.round_trip(frame.try_into().expect("a whole frame"));
            opus_time += started.elapsed();
        }
    }
    let per_frame = |d: Duration| d.as_secs_f64() * 1e6 / frames as f64;
    Chained {
        signal,
        dsp_us: per_frame(dsp_time),
        opus_us: per_frame(opus_time),
    }
}

/// How late the chain delivers each pulse, in samples, once the known delay is taken off.
fn chain_delays(detector: &Detector, signal: &[f32], delay: usize, count: u64) -> Vec<(i64, f32)> {
    (0..count)
        .filter_map(|p| {
            let start = (p * EVERY) as usize + delay;
            let found = detector.find(signal, start, start + EVERY as usize)?;
            Some((found.at as i64 - start as i64, found.ncc))
        })
        .collect()
}

/// The chain's delay as the detector finds it at no added delay: the median, in samples.
fn chain_delay(shape: PulseShape, dsp: Option<Dsp>, opus: bool) -> Option<i64> {
    let pulses = pulses(shape);
    let detector = Detector::new(&pulses);
    let chained = through_chain(&pulses, CALIBRATION_PULSES, 0, dsp, opus);
    let mut found: Vec<i64> = chain_delays(&detector, &chained.signal, 0, CALIBRATION_PULSES)
        .into_iter()
        .filter(|(_, ncc)| *ncc >= MIN_NCC)
        .map(|(d, _)| d)
        .collect();
    found.sort_unstable();
    found.get(found.len() / 2).copied()
}

fn calibrate(shape: PulseShape) -> (Value, String) {
    let pulses = pulses(shape);
    let detector = Detector::new(&pulses);
    let lookahead = CodecLoop::new(false).expect("opus").lookahead();
    let studio_lookahead = CodecLoop::new(true).expect("opus").lookahead();
    let noises = std::iter::once(None).chain(NoiseEngine::ALL.map(Some));
    let settings = [false, true]
        .into_iter()
        .flat_map(|aec| noises.clone().map(move |noise| (noise, aec)));
    let mut chains: Vec<(String, Option<Dsp>, bool)> =
        vec![("direct".into(), None, false), ("opus".into(), None, true)];
    for (noise, aec) in settings {
        let dsp = Dsp {
            noise,
            aec,
            studio: false,
        };
        chains.push((format!("dsp {}", dsp.name()), Some(dsp), false));
        chains.push((format!("dsp {} + opus", dsp.name()), Some(dsp), true));
    }
    let studio = Dsp {
        noise: None,
        aec: false,
        studio: true,
    };
    chains.push((format!("dsp {}", studio.name()), Some(studio), false));
    chains.push((format!("dsp {} + opus", studio.name()), Some(studio), true));
    let mut report = Vec::new();
    let mut summary =
        format!("opus lookahead {lookahead:?} samples, studio {studio_lookahead:?} samples\n");
    for (name, dsp, opus) in chains {
        let mut per_delay = Vec::new();
        for delay in CALIBRATION_DELAYS {
            let chained = through_chain(&pulses, CALIBRATION_PULSES, delay, dsp, opus);
            let found = chain_delays(&detector, &chained.signal, delay, CALIBRATION_PULSES);
            let kept: Vec<i64> = found
                .iter()
                .filter(|(_, ncc)| *ncc >= MIN_NCC)
                .map(|(d, _)| *d)
                .collect();
            let mut nccs: Vec<f64> = found.iter().map(|(_, ncc)| f64::from(*ncc)).collect();
            nccs.sort_by(f64::total_cmp);
            let mut sorted = kept.clone();
            sorted.sort_unstable();
            per_delay.push(json!({
                "known_delay_samples": delay,
                "pulses": CALIBRATION_PULSES,
                "detected": kept.len(),
                "chain_delay_samples": {
                    "median": sorted.get(sorted.len() / 2),
                    "min": sorted.first(),
                    "max": sorted.last(),
                },
                "ncc": { "min": nccs.first().map(|v| round(*v, 3)),
                         "median": percentile(&nccs, 50.0).map(|v| round(v, 3)) },
                "compute_us_per_frame": {
                    "dsp": round(chained.dsp_us, 1),
                    "opus": round(chained.opus_us, 1),
                },
            }));
            summary.push_str(&format!(
                "{name:<36} +{delay:>5}: {}/{} found, chain delay median {:?} min {:?} max {:?} \
                 samples, ncc min {:.2}, us/frame dsp {:.0} opus {:.0}\n",
                kept.len(),
                CALIBRATION_PULSES,
                sorted.get(sorted.len() / 2),
                sorted.first(),
                sorted.last(),
                nccs.first().copied().unwrap_or(f64::NAN),
                chained.dsp_us,
                chained.opus_us,
            ));
        }
        report.push(json!({ "chain": name, "runs": per_delay }));
    }
    (
        json!({ "opus_lookahead_samples": lookahead, "chains": report }),
        summary,
    )
}

/// One line of `lobby-rtc`'s once-a-second "voice slot" log.
#[derive(Debug, Default, Clone)]
struct SlotLine {
    at: Option<Instant>,
    user: String,
    message: String,
    decoded: u64,
    concealed: u64,
    late: u64,
    dropped: u64,
}

impl tracing::field::Visit for SlotLine {
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        match field.name() {
            "decoded" => self.decoded = value,
            "concealed" => self.concealed = value,
            "late" => self.late = value,
            "dropped" => self.dropped = value,
            _ => {}
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        match field.name() {
            "user" => self.user = format!("{value:?}"),
            "message" => self.message = format!("{value:?}"),
            _ => {}
        }
    }
}

#[derive(Clone, Default)]
struct SlotLog(Arc<Mutex<Vec<SlotLine>>>);

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for SlotLog {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        let mut line = SlotLine::default();
        event.record(&mut line);
        if line.message == "voice slot" {
            line.at = Some(Instant::now());
            self.0.lock().expect("slot log").push(line);
        }
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let slots = SlotLog::default();
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_filter(tracing_subscriber::EnvFilter::from_default_env()),
        )
        .with(
            slots.clone().with_filter(
                tracing_subscriber::filter::Targets::new()
                    .with_target("lobby_client::media::rtc", tracing::Level::INFO),
            ),
        )
        .init();
    let args = match parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(e) => {
            eprintln!("{e}\n{USAGE}");
            std::process::exit(2);
        }
    };
    let (report, summary) = if args.calibrate {
        calibrate(args.shape)
    } else {
        run(&args, &slots).await
    };
    eprintln!("{summary}");
    println!("{report}");
}

/// Bob's sink, drained into memory on a thread of its own until `stop`.
fn drain_sink(mut ring: rtrb::Consumer<PlayedBlock>, stop: Arc<AtomicBool>) -> Vec<PlayedBlock> {
    let mut blocks = Vec::with_capacity(SINK_BLOCKS);
    while !stop.load(Ordering::SeqCst) {
        while let Ok(block) = ring.pop() {
            blocks.push(block);
        }
        std::thread::sleep(DRAIN_EVERY);
    }
    while let Ok(block) = ring.pop() {
        blocks.push(block);
    }
    blocks
}

async fn run(args: &Args, slots: &SlotLog) -> (Value, String) {
    let (server, relay) = match args.impairment {
        Some(_) => {
            let (server, relay) = TestServer::start_behind_relay(&["alice", "bob"], |_| {}).await;
            (server, Some(relay))
        }
        None => (
            TestServer::start_with_members(&["alice", "bob"], |_| {}).await,
            None,
        ),
    };
    let origin = origin(&server);
    let (store_a, store_b) = (MemoryTokenStore::new(), MemoryTokenStore::new());
    let (client_a, client_b) = (test_client(&store_a), test_client(&store_b));
    let mut a = Member::sign_in(&client_a, &origin, "alice", &server.mail)
        .await
        .unwrap();
    let mut b = Member::sign_in(&client_b, &origin, "bob", &server.mail)
        .await
        .unwrap();

    let mic = pulses(args.shape);
    let onsets = mic.onsets.clone();
    let (sink, heard) = rtrb::RingBuffer::new(SINK_BLOCKS);
    let (alice_sink, bob_sink) = if args.move_output {
        (
            AudioSink::Null,
            AudioSink::Device(DeviceSetup::new(
                DeviceChoice::SystemDefault,
                Arc::new(SystemDevices),
            )),
        )
    } else if args.monitor {
        (AudioSink::Timed(sink), AudioSink::Null)
    } else {
        (AudioSink::Null, AudioSink::Timed(sink))
    };
    let monitor = MonitorControl::default();
    monitor.set_on(args.monitor);
    a.join_voice(MediaConfig {
        source: if args.move_output {
            AudioSource::Synthetic(Tone::hz(440.0))
        } else {
            AudioSource::Pulses(mic.clone())
        },
        sink: alice_sink,
        dsp: DspControl::new(args.dsp.config()),
        monitor,
        ..MediaConfig::fake()
    })
    .await
    .unwrap();
    a.run_for(args.bob_after).await;
    b.join_voice(MediaConfig {
        source: AudioSource::Synthetic(Tone {
            hz: 440.0,
            amplitude: 0.0,
        }),
        sink: bob_sink,
        ..MediaConfig::fake()
    })
    .await
    .unwrap();
    if args.move_output {
        return move_output(args, a, b, server, slots).await;
    }
    let stop = Arc::new(AtomicBool::new(false));
    let drainer = {
        let stop = stop.clone();
        std::thread::spawn(move || drain_sink(heard, stop))
    };
    until(&mut a, &mut b, "both In Voice on both", |sa, sb| {
        ["alice", "bob"]
            .iter()
            .all(|n| active(sa, n) && active(sb, n))
    })
    .await;
    if let (Some(impairment), Some(relay)) = (args.impairment, &relay) {
        impair_downlink(&server, relay, &mut a, &mut b, impairment).await;
    }
    let alice = a.id_of("alice").to_string();

    let started = Instant::now();
    let measured_from = started + Duration::from_secs(args.warmup);
    let measured_to = measured_from + Duration::from_secs(args.seconds);
    let mut seconds = Vec::new();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    tick.tick().await;
    while Instant::now() < measured_to + TAIL {
        tokio::select! {
            _ = tick.tick() => {
                if Instant::now() >= measured_from {
                    seconds.push(b.state().voice.stats.clone());
                }
            }
            more = a.step() => assert!(more, "alice's session ended"),
            more = b.step() => assert!(more, "bob's session ended"),
        }
    }
    stop.store(true, Ordering::SeqCst);
    let blocks = drainer.join().expect("sink drainer");
    a.shutdown().await;
    b.shutdown().await;
    let clean = server.shutdown().await.is_clean();

    let onsets: Vec<Onset> = onsets
        .lock()
        .expect("onsets")
        .iter()
        .filter(|o| o.entered >= measured_from && o.entered < measured_to)
        .copied()
        .collect();
    let hint = chain_delay(args.shape, Some(args.dsp), true);
    let timed = time_pulses(&mic, &onsets, &blocks, hint);
    let lines: Vec<SlotLine> = slots
        .0
        .lock()
        .expect("slot log")
        .iter()
        .filter(|l| {
            l.user == alice
                && l.at
                    .is_some_and(|at| at >= measured_from && at < measured_to)
        })
        .cloned()
        .collect();
    let report = report(
        args, &timed, &onsets, hint, &lines, &seconds, clean, &blocks,
    );
    let summary = summary(args, &timed, onsets.len(), &lines);
    (report, summary)
}

/// Bob, deafened on the real default output, moves to another output and back mid-call.
async fn move_output(
    args: &Args,
    mut a: Member,
    mut b: Member,
    server: TestServer,
    slots: &SlotLog,
) -> (Value, String) {
    until(&mut a, &mut b, "both In Voice on both", |sa, sb| {
        ["alice", "bob"]
            .iter()
            .all(|n| active(sa, n) && active(sb, n))
    })
    .await;
    b.act(Intent::ToggleDeafen).await.expect("bob deafened");
    let outputs = SystemDevices.list().outputs;
    let other: NamedDevice = outputs
        .devices
        .iter()
        .find(|d| Some(&d.id) != outputs.default.as_ref())
        .cloned()
        .expect("a second output device to move to");
    let alice = a.id_of("alice").to_string();

    let started = Instant::now();
    let measured = Duration::from_secs(args.seconds);
    let measured_from = started + Duration::from_secs(args.warmup);
    let end = measured_from + measured;
    let mut moves = vec![
        (
            measured_from + measured / 3,
            DeviceChoice::Named(other.clone()),
        ),
        (
            measured_from + measured * 2 / 3,
            DeviceChoice::SystemDefault,
        ),
    ]
    .into_iter()
    .peekable();
    let mut moved = Vec::new();
    while Instant::now() < end {
        tokio::select! {
            _ = tokio::time::sleep(DRAIN_EVERY) => {
                if let Some((_, choice)) = moves.next_if(|(at, _)| Instant::now() >= *at) {
                    let at = Instant::now();
                    b.act(Intent::ChooseDevice {
                        kind: DeviceKind::Output,
                        choice: choice.clone(),
                    })
                    .await
                    .expect("bob's speakers moved");
                    moved.push((at, choice));
                }
            }
            more = a.step() => assert!(more, "alice's session ended"),
            more = b.step() => assert!(more, "bob's session ended"),
        }
    }
    a.shutdown().await;
    b.shutdown().await;
    let clean = server.shutdown().await.is_clean();

    let since = |at: Instant| round(ms_signed(at, measured_from) / 1000.0, 2);
    let lines: Vec<SlotLine> = slots
        .0
        .lock()
        .expect("slot log")
        .iter()
        .filter(|l| l.user == alice && l.at.is_some_and(|at| at >= measured_from))
        .cloned()
        .collect();
    let mut summary = format!(
        "bob deafened on the default output; moves to {:?} and back\n",
        other.name
    );
    for (at, choice) in &moved {
        summary.push_str(&format!("move at {:.2} s to {choice:?}\n", since(*at)));
    }
    for l in &lines {
        summary.push_str(&format!(
            "{:>6.2} s  decoded {:>3}  concealed {:>3}\n",
            since(l.at.expect("stamped")),
            l.decoded,
            l.concealed
        ));
    }
    let report = json!({
        "mode": "move-output",
        "dsp": args.dsp.name(),
        "moved_to": other.name,
        "moves": moved.iter().map(|(at, choice)| json!({
            "at_s": since(*at),
            "to": format!("{choice:?}"),
        })).collect::<Vec<_>>(),
        "bob_slot_log": lines.iter().map(|l| json!({
            "at_s": since(l.at.expect("stamped")),
            "decoded": l.decoded,
            "concealed": l.concealed,
        })).collect::<Vec<_>>(),
        "server_shutdown_clean": clean,
    });
    (report, summary)
}

async fn impair_downlink(
    server: &TestServer,
    relay: &MediaRelay,
    a: &mut Member,
    b: &mut Member,
    impairment: Impairment,
) {
    let user = server.user_id_of("bob").await;
    let on_udp = tokio::time::timeout(WAIT, async {
        while server.server.sfu_media_path(&user) != Some(MediaPath::Udp) {
            a.run_for(Duration::from_millis(10)).await;
            b.run_for(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(on_udp.is_ok(), "bob's media over UDP");
    relay.impair_client(
        b.voice_media_addr().await.expect("bob's media socket"),
        Direction::Down,
        impairment,
    );
}

/// One pulse as Bob heard it, in ms.
#[derive(Debug, Clone, Copy)]
struct Timed {
    latency: f64,
    /// From the pulse's first sample entering the ring to its frame's last, plus the pulse's
    /// place in the frame: a frame, plus however late the synthetic microphone pushed it.
    frame: f64,
    /// What the DSP and Opus delay the pulse by within the stream.
    algorithmic: f64,
    /// From the frame's last sample entering Alice's ring to Bob's sink taking that frame:
    /// encoding, transport, the SFU and the wait in the jitter buffer.
    frame_done_to_played: f64,
    ncc: f32,
    /// How much later the loud part came than the match: a suppressor opening late.
    onset_clipped: f64,
}

/// Each onset found in what Bob played. `hint` is the chain's delay from the offline run, which
/// says how many whole frames the in-stream delay spans.
fn time_pulses(
    mic: &Pulses,
    onsets: &[Onset],
    blocks: &[PlayedBlock],
    hint: Option<i64>,
) -> Vec<Option<Timed>> {
    let detector = Detector::new(mic);
    let signal: Vec<f32> = blocks.iter().flat_map(|b| b.pcm).collect();
    let played = |i: usize| blocks[i / FRAME].at + secs(((i % FRAME) as f64) / RATE);
    onsets
        .iter()
        .map(|o| {
            let first = blocks
                .iter()
                .position(|b| b.at + secs(FRAME as f64 / RATE) > o.entered)?;
            let from = first * FRAME;
            let found = detector.find(&signal, from, from + EVERY as usize)?;
            if found.ncc < MIN_NCC {
                return None;
            }
            let k = (o.sample % FRAME as u64) as i64;
            let j = (found.at % FRAME) as i64;
            let within = (j - k).rem_euclid(FRAME as i64);
            let wraps = hint.map_or(0, |h| ((h - within) as f64 / FRAME as f64).round() as i64);
            let delay = within + wraps * FRAME as i64;
            let frame_start = (found.at as i64 - delay - k).max(0) as usize;
            let frame_done = o.frame_done?;
            Some(Timed {
                latency: ms(played(found.at) - o.entered),
                frame: ms(frame_done - o.entered) + k as f64 / RATE * 1000.0,
                algorithmic: delay as f64 / RATE * 1000.0,
                frame_done_to_played: ms_signed(blocks[frame_start / FRAME].at, frame_done),
                ncc: found.ncc,
                onset_clipped: (found.envelope_at as f64 - found.at as f64) / RATE * 1000.0,
            })
        })
        .collect()
}

fn secs(s: f64) -> Duration {
    Duration::from_secs_f64(s)
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn ms_signed(later: Instant, earlier: Instant) -> f64 {
    match later.checked_duration_since(earlier) {
        Some(d) => ms(d),
        None => -ms(earlier - later),
    }
}

fn round(v: f64, places: i32) -> f64 {
    let scale = 10f64.powi(places);
    (v * scale).round() / scale
}

/// Nearest-rank percentile of an ascending list.
fn percentile(sorted: &[f64], p: f64) -> Option<f64> {
    let n = sorted.len();
    (n > 0).then(|| sorted[((p / 100.0 * n as f64).ceil() as usize).clamp(1, n) - 1])
}

fn spread(values: impl Iterator<Item = f64>) -> Value {
    let mut v: Vec<f64> = values.collect();
    v.sort_by(f64::total_cmp);
    let at = |p| percentile(&v, p).map(|x| round(x, 2));
    json!({
        "p50": at(50.0),
        "p95": at(95.0),
        "min": v.first().map(|x| round(*x, 2)),
        "max": v.last().map(|x| round(*x, 2)),
    })
}

#[allow(clippy::too_many_arguments)]
fn report(
    args: &Args,
    timed: &[Option<Timed>],
    onsets: &[Onset],
    hint: Option<i64>,
    lines: &[SlotLine],
    seconds: &[Option<lobby_client::VoiceStats>],
    clean: bool,
    blocks: &[PlayedBlock],
) -> Value {
    let found: Vec<Timed> = timed.iter().flatten().copied().collect();
    let of = |f: fn(&Timed) -> f64| spread(found.iter().map(f));
    let stats: Vec<&lobby_client::VoiceStats> = seconds.iter().flatten().collect();
    let in_ms = |d: Option<Duration>| d.map(ms);
    let gaps: Vec<f64> = blocks.windows(2).map(|w| ms(w[1].at - w[0].at)).collect();
    json!({
        "dsp": args.dsp.name(),
        "heard_by": if args.monitor { "alice, her own monitor" } else { "bob" },
        "pulse": format!("{:?}", args.shape),
        "link": args.impairment.map(|i| json!({
            "loss_percent": i.loss_percent,
            "delay_ms": ms(i.delay),
            "jitter_ms": ms(i.jitter),
        })),
        "bob_after_ms": ms(args.bob_after),
        "warmup_seconds": args.warmup,
        "measured_seconds": args.seconds,
        "pulses": {
            "sent": onsets.len(),
            "detected": found.len(),
            "missed": onsets.len() - found.len(),
        },
        "latency_ms": of(|t| t.latency),
        "stages_ms": {
            "frame": of(|t| t.frame),
            "algorithmic": of(|t| t.algorithmic),
            "frame_done_to_played": of(|t| t.frame_done_to_played),
            "onset_clipped": of(|t| t.onset_clipped),
        },
        "offline_chain_delay_ms": hint.map(|h| round(h as f64 / RATE * 1000.0, 2)),
        "ncc": spread(found.iter().map(|t| f64::from(t.ncc))),
        "latencies_ms": found.iter().map(|t| round(t.latency, 2)).collect::<Vec<_>>(),
        "bob": {
            "decoded": lines.iter().map(|l| l.decoded).sum::<u64>(),
            "concealed": lines.iter().map(|l| l.concealed).sum::<u64>(),
            "late_total": lines.last().map(|l| l.late),
            "dropped_total": lines.last().map(|l| l.dropped),
            "slot_log_seconds": lines.len(),
            "rtcp_jitter_ms": spread(stats.iter().filter_map(|s| in_ms(s.jitter))),
            "rtt_ms": spread(stats.iter().filter_map(|s| in_ms(s.rtt))),
            "loss_in_tenths_percent": spread(stats.iter().filter_map(|s| s.loss_in.map(f64::from))),
            "sink_block_gap_ms": spread(gaps.into_iter()),
        },
        "server_shutdown_clean": clean,
    })
}

fn summary(args: &Args, timed: &[Option<Timed>], sent: usize, lines: &[SlotLine]) -> String {
    let mut latency: Vec<f64> = timed.iter().flatten().map(|t| t.latency).collect();
    latency.sort_by(f64::total_cmp);
    let at = |p| percentile(&latency, p).unwrap_or(f64::NAN);
    let median = |f: fn(&Timed) -> f64| {
        let mut v: Vec<f64> = timed.iter().flatten().map(f).collect();
        v.sort_by(f64::total_cmp);
        percentile(&v, 50.0).unwrap_or(f64::NAN)
    };
    format!(
        "{}{} {:?} link {:?}, {} s\n\
         pulses: {} sent, {} detected\n\
         latency ms: p50 {:.1}, p95 {:.1}, max {:.1}\n\
         stages ms (p50): frame {:.1}, algorithmic {:.1}, frame done to played {:.1}\n\
         bob: decoded {}, concealed {}",
        if args.monitor { "monitor " } else { "" },
        args.dsp.name(),
        args.shape,
        args.impairment,
        args.seconds,
        sent,
        latency.len(),
        at(50.0),
        at(95.0),
        latency.last().copied().unwrap_or(f64::NAN),
        median(|t| t.frame),
        median(|t| t.algorithmic),
        median(|t| t.frame_done_to_played),
        lines.iter().map(|l| l.decoded).sum::<u64>(),
        lines.iter().map(|l| l.concealed).sum::<u64>(),
    )
}
