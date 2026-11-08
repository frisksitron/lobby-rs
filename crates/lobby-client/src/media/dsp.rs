//! The capture DSP: what the microphone's sound goes through between the capture ring and the
//! VAD, at 48 kHz mono in 10 ms frames. Echo cancellation first (it needs the speakers' sound
//! as its reference and the microphone as captured), then noise suppression, then a compressor
//! with a limiter. Each of the three can be taken out of the path; the limiter stays while any
//! processing does. With all three out the chain skips everything and leaves the frame as
//! captured. A [`Dsp`] is built from a [`DspConfig`] and then allocates nothing per frame; a
//! changed setting rebuilds only its own stage between frames ([`Dsp::set`]), and the
//! compressor's retunes it in place.
//!
//! The reference for echo cancellation is the playout's final mix, handed over in 10 ms frames
//! through an `rtrb` ring ([`RenderTap`]). The playout only writes to it while the setting is on.
//!
//! RNNoise and the compressor allocate nothing per frame. sonora's suppressor and AEC3 do
//! (measured: about 2 and 40 allocations per frame), inside
//! their crate; the encoder thread has 200 ms of slack, so that is the cost of choosing them.
//!
//! Each frame is metered at the four points of the chain into a [`DspMeter`].

use std::sync::{
    Arc,
    atomic::{AtomicU16, Ordering},
};

use nnnoiseless::DenoiseState;
use sonora::{
    AudioProcessing, Config, StreamConfig,
    config::{EchoCanceller, NoiseSuppression as WebrtcNs, NoiseSuppressionLevel},
};

use super::{SAMPLE_RATE, meter::DspMeter};
use crate::{Compressor, CompressorAmount, NoiseEngine, NoiseSuppression, Settings};

/// 10 ms at 48 kHz: the frame RNNoise and the WebRTC processing both work in.
pub(crate) const DSP_FRAME: usize = SAMPLE_RATE as usize / 100;
pub(crate) type Frame = [f32; DSP_FRAME];

/// 200 ms of reference between the output callback and the encoder thread.
const RENDER_RING: usize = 20;
/// RNNoise works on 16-bit sample values.
const I16_SCALE: f32 = 32768.0;
/// The WebRTC suppressor's strength (about 18 dB): speech sounds natural at this level.
const WEBRTC_LEVEL: NoiseSuppressionLevel = NoiseSuppressionLevel::High;

/// The compressor's Amount runs from gentle to heavy: the threshold falls linearly between these,
/// and the ratio rises geometrically, so the middle of the knob is -18 dB at 3:1.
const THRESHOLD_GENTLE_DB: f32 = -6.0;
const THRESHOLD_HEAVY_DB: f32 = -30.0;
const RATIO_GENTLE: f32 = 1.5;
const RATIO_HEAVY: f32 = 6.0;
/// The makeup gain gives back this share of what the compressor takes from a talking voice, an
/// envelope of [`TALKING_DB`], so a heavier Amount evens speech out without making it quieter.
/// At the middle of the knob that is 3 dB.
const MAKEUP_SHARE: f32 = 0.75;
const TALKING_DB: f32 = -12.0;
/// About -1 dBFS: nothing leaves the limiter louder.
pub(crate) const CEILING: f32 = 0.89;
const ATTACK_MS: f32 = 5.0;
const RELEASE_MS: f32 = 100.0;
const LIMITER_RELEASE_MS: f32 = 50.0;

/// What the chain does, as the Settings chose it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DspConfig {
    pub echo_cancellation: bool,
    pub noise: NoiseSuppression,
    pub compressor: Compressor,
}

impl DspConfig {
    fn cancels_echo(self) -> bool {
        self.echo_cancellation
    }

    /// The noise engine in the path, if any.
    fn noise_engine(self) -> Option<NoiseEngine> {
        self.noise.on.then_some(self.noise.engine)
    }

    /// All three steps are out: the microphone goes on as captured, the limiter skipped too,
    /// into the low-delay encoder.
    pub fn studio(self) -> bool {
        !self.echo_cancellation && !self.noise.on && !self.compressor.on
    }
}

impl From<&Settings> for DspConfig {
    fn from(settings: &Settings) -> Self {
        Self {
            echo_cancellation: settings.echo_cancellation,
            noise: settings.noise_suppression,
            compressor: settings.compressor,
        }
    }
}

/// The settings of the running call, shared with it: the desktop writes them when the person
/// changes one, and the encoder thread and the output callback read them.
#[derive(Clone, Default)]
pub struct DspControl(Arc<AtomicU16>);

impl std::fmt::Debug for DspControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DspControl({:?})", self.get())
    }
}

/// The compressor's Amount takes the low seven bits.
const AMOUNT_MASK: u16 = 0x7f;
const COMPRESSOR_BIT: u16 = 1 << 7;
const NOISE_BIT: u16 = 1 << 8;
const WEBRTC_BIT: u16 = 1 << 9;
const ECHO_BIT: u16 = 1 << 10;

impl DspControl {
    pub fn new(config: DspConfig) -> Self {
        let control = Self::default();
        control.set(config);
        control
    }

    pub fn set(&self, config: DspConfig) {
        let bit = |on: bool, bit: u16| if on { bit } else { 0 };
        let bits = u16::from(config.compressor.amount.get())
            | bit(config.compressor.on, COMPRESSOR_BIT)
            | bit(config.noise.on, NOISE_BIT)
            | bit(config.noise.engine == NoiseEngine::Webrtc, WEBRTC_BIT)
            | bit(config.echo_cancellation, ECHO_BIT);
        self.0.store(bits, Ordering::Relaxed);
    }

    pub(crate) fn get(&self) -> DspConfig {
        let bits = self.0.load(Ordering::Relaxed);
        DspConfig {
            echo_cancellation: bits & ECHO_BIT != 0,
            noise: NoiseSuppression {
                on: bits & NOISE_BIT != 0,
                engine: if bits & WEBRTC_BIT != 0 {
                    NoiseEngine::Webrtc
                } else {
                    NoiseEngine::Rnnoise
                },
            },
            compressor: Compressor {
                on: bits & COMPRESSOR_BIT != 0,
                amount: CompressorAmount::new((bits & AMOUNT_MASK) as u8),
            },
        }
    }
}

/// The playout's side of the echo reference.
pub(crate) struct RenderTap {
    control: DspControl,
    ring: rtrb::Producer<Frame>,
}

impl RenderTap {
    /// The mix that went to the speakers, in whole 10 ms frames. Nothing happens while echo
    /// cancellation is off; a full ring drops the frame.
    pub fn push(&mut self, mix: &[f32]) {
        if !self.control.get().cancels_echo() {
            return;
        }
        for chunk in mix.chunks_exact(DSP_FRAME) {
            let mut frame = [0.0; DSP_FRAME];
            frame.copy_from_slice(chunk);
            let _ = self.ring.push(frame);
        }
    }
}

/// The reference ring: the playout writes its mix to the tap, and [`CaptureDsp`] reads it.
pub(crate) fn render_ring(control: DspControl) -> (RenderTap, rtrb::Consumer<Frame>) {
    let (ring, reference) = rtrb::RingBuffer::new(RENDER_RING);
    (RenderTap { control, ring }, reference)
}

/// The noise module's engine, while it is in the path.
enum Suppressor {
    Rnnoise(Box<DenoiseState<'static>>),
    Webrtc(Box<AudioProcessing>),
}

impl Suppressor {
    fn new(engine: NoiseEngine) -> Suppressor {
        match engine {
            NoiseEngine::Rnnoise => {
                let mut rnnoise = DenoiseState::new();
                // RNNoise's FFT plans are made on its first frame: make them here, not in the
                // first 10 ms it is in the call.
                rnnoise.process_frame(&mut [0.0; DSP_FRAME], &[0.0; DSP_FRAME]);
                Suppressor::Rnnoise(rnnoise)
            }
            NoiseEngine::Webrtc => Suppressor::Webrtc(webrtc(Config {
                noise_suppression: Some(WebrtcNs {
                    level: WEBRTC_LEVEL,
                    ..WebrtcNs::default()
                }),
                ..Config::default()
            })),
        }
    }
}

/// sonora's processing with one of its stages. Echo cancellation and its suppressor are built
/// apart, so the level between them can be metered.
fn webrtc(config: Config) -> Box<AudioProcessing> {
    let stream = StreamConfig::new(SAMPLE_RATE, 1);
    Box::new(
        AudioProcessing::builder()
            .config(config)
            .capture_config(stream)
            .render_config(stream)
            .build(),
    )
}

fn echo_canceller() -> Box<AudioProcessing> {
    webrtc(Config {
        echo_canceller: Some(EchoCanceller::default()),
        ..Config::default()
    })
}

/// Each stage is built from the one setting that owns it: the echo canceller from
/// [`DspConfig::cancels_echo`], the suppressor from [`DspConfig::noise_engine`], the compressor
/// from [`DspConfig::compressor`]. With all three out ([`DspConfig::studio`]) the frame passes
/// untouched, the limiter skipped too.
pub(crate) struct Dsp {
    config: DspConfig,
    echo: Option<Box<AudioProcessing>>,
    noise: Option<Suppressor>,
    dynamics: Dynamics,
    input: Frame,
    output: Frame,
}

impl Dsp {
    pub fn new(config: DspConfig) -> Dsp {
        Dsp {
            config,
            echo: config.cancels_echo().then(echo_canceller),
            noise: config.noise_engine().map(Suppressor::new),
            dynamics: Dynamics::new(config.compressor),
            input: [0.0; DSP_FRAME],
            output: [0.0; DSP_FRAME],
        }
    }

    /// The chain as `wanted` has it, from the next frame. Only a stage whose setting changed is
    /// rebuilt, which allocates, so the echo canceller keeps what it has learnt through any other
    /// change; the compressor retunes in place and its envelope carries on. Returns whether the
    /// echo canceller is new.
    pub fn set(&mut self, wanted: DspConfig) -> bool {
        let was = std::mem::replace(&mut self.config, wanted);
        if wanted.noise_engine() != was.noise_engine() {
            self.noise = wanted.noise_engine().map(Suppressor::new);
        }
        if wanted.compressor != was.compressor {
            self.dynamics.tune(wanted.compressor);
        }
        if wanted.cancels_echo() == was.cancels_echo() {
            return false;
        }
        self.echo = wanted.cancels_echo().then(echo_canceller);
        self.echo.is_some()
    }

    /// What the speakers played, for the echo canceller to find in the microphone. Without one
    /// this does nothing.
    pub fn render(&mut self, frame: &Frame) {
        if let Some(echo) = &mut self.echo {
            let done = echo.process_render_f32(&[frame], &mut [&mut self.output]);
            debug_assert!(done.is_ok(), "{done:?}");
        }
    }

    /// Cleans `pcm` in place, a whole number of 10 ms frames, metering each into `meter`.
    pub fn process(&mut self, pcm: &mut [f32], mut meter: Option<&mut DspMeter>) {
        debug_assert_eq!(pcm.len() % DSP_FRAME, 0);
        for frame in pcm.chunks_exact_mut(DSP_FRAME) {
            let mic = rms(frame);
            if self.config.studio() {
                if let Some(meter) = meter.as_deref_mut() {
                    meter.frame([mic; 4], 0.0, 0.0);
                }
                continue;
            }
            let after_echo = match &mut self.echo {
                Some(echo) => {
                    let done = echo.process_capture_f32(&[&*frame], &mut [&mut self.output]);
                    debug_assert!(done.is_ok(), "{done:?}");
                    frame.copy_from_slice(&self.output);
                    rms(frame)
                }
                None => mic,
            };
            let after_noise = match &mut self.noise {
                Some(Suppressor::Rnnoise(rnnoise)) => {
                    for (scaled, s) in self.input.iter_mut().zip(&*frame) {
                        *scaled = s * I16_SCALE;
                    }
                    rnnoise.process_frame(&mut self.output, &self.input);
                    for (s, scaled) in frame.iter_mut().zip(&self.output) {
                        *s = scaled / I16_SCALE;
                    }
                    rms(frame)
                }
                Some(Suppressor::Webrtc(webrtc)) => {
                    let done = webrtc.process_capture_f32(&[&*frame], &mut [&mut self.output]);
                    debug_assert!(done.is_ok(), "{done:?}");
                    frame.copy_from_slice(&self.output);
                    rms(frame)
                }
                None => after_echo,
            };
            let (reduction, limit) = self.dynamics.process(frame);
            if let Some(meter) = meter.as_deref_mut() {
                meter.frame([mic, after_echo, after_noise, rms(frame)], reduction, limit);
            }
        }
    }
}

fn rms(frame: &[f32]) -> f32 {
    let sum: f32 = frame.iter().map(|s| s * s).sum();
    (sum / frame.len() as f32).sqrt()
}

/// Where the compressor starts and how hard it works, from its Amount. The one place the knob
/// turns into decibels.
struct Curve {
    threshold_db: f32,
    ratio: f32,
    makeup_db: f32,
}

impl Curve {
    fn of(amount: CompressorAmount) -> Curve {
        let t = f32::from(amount.get()) / f32::from(CompressorAmount::MAX);
        let threshold_db = THRESHOLD_GENTLE_DB + (THRESHOLD_HEAVY_DB - THRESHOLD_GENTLE_DB) * t;
        let ratio = RATIO_GENTLE * (RATIO_HEAVY / RATIO_GENTLE).powf(t);
        let mut curve = Curve {
            threshold_db,
            ratio,
            makeup_db: 0.0,
        };
        curve.makeup_db = MAKEUP_SHARE * curve.reduction_db(TALKING_DB);
        curve
    }

    /// How far an envelope of `envelope_db` is turned down.
    fn reduction_db(&self, envelope_db: f32) -> f32 {
        (envelope_db - self.threshold_db).max(0.0) * (1.0 - 1.0 / self.ratio)
    }
}

/// A feed-forward compressor over a limiter. The envelope follower tracks the signal's level; above
/// the threshold the gain falls by the ratio, then the makeup gain is added; and the limiter keeps
/// every sample under the ceiling, instantly on the way down and smoothly on the way back up. With
/// the compressor out the gain is one and the limiter works alone.
struct Dynamics {
    envelope: f32,
    limit: f32,
    attack: f32,
    release: f32,
    limit_release: f32,
    compressing: bool,
    threshold: f32,
    makeup: f32,
    slope: f32,
    curve: Curve,
}

/// The one-pole coefficient that closes 63% of the distance in `ms`.
fn smoothing(ms: f32) -> f32 {
    1.0 - (-1.0 / (ms * 0.001 * SAMPLE_RATE as f32)).exp()
}

fn from_db(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

impl Dynamics {
    fn new(compressor: Compressor) -> Dynamics {
        let mut dynamics = Dynamics {
            envelope: 0.0,
            limit: 1.0,
            attack: smoothing(ATTACK_MS),
            release: smoothing(RELEASE_MS),
            limit_release: smoothing(LIMITER_RELEASE_MS),
            compressing: false,
            threshold: 1.0,
            makeup: 1.0,
            slope: 0.0,
            curve: Curve::of(compressor.amount),
        };
        dynamics.tune(compressor);
        dynamics
    }

    fn tune(&mut self, compressor: Compressor) {
        let curve = Curve::of(compressor.amount);
        self.compressing = compressor.on;
        self.threshold = from_db(curve.threshold_db);
        self.makeup = if compressor.on {
            from_db(curve.makeup_db)
        } else {
            1.0
        };
        self.slope = 1.0 / curve.ratio - 1.0;
        self.curve = curve;
    }

    /// The frame through the compressor and the limiter. Returns how far the compressor turned
    /// the frame's loudest moment down, and how far the limiter did, in dB.
    fn process(&mut self, frame: &mut [f32]) -> (f32, f32) {
        let mut loudest = 0.0f32;
        let mut deepest_limit = 1.0f32;
        for s in frame {
            let x = if s.is_finite() { *s } else { 0.0 };
            let mut gain = 1.0;
            if self.compressing {
                let level = x.abs();
                let coefficient = if level > self.envelope {
                    self.attack
                } else {
                    self.release
                };
                self.envelope += coefficient * (level - self.envelope);
                loudest = loudest.max(self.envelope);
                gain = self.makeup;
                if self.envelope > self.threshold {
                    gain *= (self.envelope / self.threshold).powf(self.slope);
                }
            }
            let y = x * gain;
            let needed = if y.abs() > CEILING {
                CEILING / y.abs()
            } else {
                1.0
            };
            self.limit = (self.limit + (1.0 - self.limit) * self.limit_release).min(needed);
            deepest_limit = deepest_limit.min(self.limit);
            *s = (y * self.limit).clamp(-CEILING, CEILING);
        }
        let limit_db = -20.0 * deepest_limit.log10();
        if !self.compressing || loudest <= self.threshold {
            return (0.0, limit_db);
        }
        (self.curve.reduction_db(20.0 * loudest.log10()), limit_db)
    }
}

/// The chain as `lobby-voice-tx` holds it: the settings it follows, the chain built from them,
/// the echo reference and the meter.
pub(crate) struct CaptureDsp {
    control: DspControl,
    dsp: Dsp,
    reference: rtrb::Consumer<Frame>,
    meter: DspMeter,
}

impl CaptureDsp {
    pub fn new(
        control: DspControl,
        reference: rtrb::Consumer<Frame>,
        meter: DspMeter,
    ) -> CaptureDsp {
        CaptureDsp {
            dsp: Dsp::new(control.get()),
            control,
            reference,
            meter,
        }
    }

    /// Between frames, never inside a real-time section: a changed setting rebuilds its stage.
    pub fn sync(&mut self) {
        if self.dsp.set(self.control.get()) {
            // What the playout left while the echo canceller was off is old.
            while self.reference.pop().is_ok() {}
        }
    }

    pub fn studio(&self) -> bool {
        self.dsp.config.studio()
    }

    pub fn process(&mut self, pcm: &mut [f32]) {
        if self.dsp.config.cancels_echo() {
            // The speakers' sound first: the canceller looks for it in what follows.
            while let Ok(frame) = self.reference.pop() {
                self.dsp.render(&frame);
            }
        }
        self.dsp.process(pcm, Some(&mut self.meter));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::meter::{DspLevels, DspReading};

    /// The speakers playing white noise from a fixed seed, and a microphone that hears them a
    /// frame later at half the level, as a call through speakers does.
    struct Room {
        control: DspControl,
        tap: RenderTap,
        dsp: CaptureDsp,
        levels: DspLevels,
        seed: u32,
        echo: Frame,
    }

    impl Room {
        fn new(config: DspConfig) -> Room {
            let control = DspControl::new(config);
            let (tap, reference) = render_ring(control.clone());
            let levels = DspLevels::default();
            Room {
                dsp: CaptureDsp::new(control.clone(), reference, DspMeter::new(levels.clone())),
                control,
                tap,
                levels,
                seed: 1,
                echo: [0.0; DSP_FRAME],
            }
        }

        /// `frames` of 10 ms. Returns how far below the microphone the echo canceller's output
        /// read at its worst, in dB.
        fn run(&mut self, frames: u32) -> f32 {
            let mut least = f32::MAX;
            for _ in 0..frames {
                let mut played = [0.0; DSP_FRAME];
                for s in &mut played {
                    self.seed = self
                        .seed
                        .wrapping_mul(1_664_525)
                        .wrapping_add(1_013_904_223);
                    *s = (self.seed >> 8) as f32 / (1 << 24) as f32 * 0.6 - 0.3;
                }
                self.tap.push(&played);
                let mut mic = self.echo.map(|s| s * 0.5);
                self.echo = played;
                self.dsp.sync();
                self.dsp.process(&mut mic);
                let read = self.levels.read();
                least = least.min(read.mic_db - read.after_echo_db);
            }
            least
        }
    }

    #[test]
    fn changing_the_other_settings_keeps_what_the_echo_canceller_learnt() {
        let noise_off = NoiseSuppression {
            on: false,
            ..NoiseSuppression::default()
        };
        let mut config = DspConfig {
            echo_cancellation: true,
            noise: noise_off,
            ..DspConfig::default()
        };
        // A learnt canceller holds about 50 dB; a new one falls to about 15 within two frames.
        const REMOVED_DB: f32 = 30.0;
        let mut room = Room::new(config);
        room.run(300);
        let learnt = room.run(50);
        assert!(
            learnt > REMOVED_DB,
            "three seconds in, the echo is gone: {learnt} dB"
        );

        config.noise.on = true;
        room.control.set(config);
        let after = room.run(20);
        assert!(after > REMOVED_DB, "noise suppression on: {after} dB");
        config.noise.engine = NoiseEngine::Webrtc;
        room.control.set(config);
        let after = room.run(20);
        assert!(after > REMOVED_DB, "the other noise engine: {after} dB");
        config.compressor.on = false;
        room.control.set(config);
        let after = room.run(20);
        assert!(after > REMOVED_DB, "the compressor off: {after} dB");
    }

    #[test]
    fn with_all_three_steps_out_the_frame_comes_back_bit_exact() {
        let config = DspConfig {
            echo_cancellation: false,
            noise: NoiseSuppression {
                on: false,
                ..NoiseSuppression::default()
            },
            compressor: Compressor {
                on: false,
                ..Compressor::default()
            },
        };
        assert!(config.studio());
        for back in [
            DspConfig {
                echo_cancellation: true,
                ..config
            },
            DspConfig {
                noise: NoiseSuppression::default(),
                ..config
            },
            DspConfig {
                compressor: Compressor::default(),
                ..config
            },
        ] {
            assert!(!back.studio(), "{back:?}");
        }
        let mut dsp = Dsp::new(config);
        let mut pcm: Vec<f32> = (0..DSP_FRAME * 3)
            .map(|i| (i as f32 * 0.37).sin() * 1.2)
            .collect();
        let before = pcm.clone();
        dsp.render(&[0.5; DSP_FRAME]);
        dsp.process(&mut pcm, None);
        assert_eq!(pcm, before, "no suppression, no compressor, no limiter");
    }

    /// Half a second of a 200 Hz tone at `amplitude`.
    fn tone(amplitude: f32) -> Vec<f32> {
        (0..DSP_FRAME * 50)
            .map(|i| amplitude * (i as f32 * std::f32::consts::TAU * 200.0 / 48_000.0).sin())
            .collect()
    }

    /// The tone through the chain with only the compressor in question: what came out, the RMS
    /// of its last 100 ms, and what the meter read. The echo canceller stays in, with nothing to
    /// cancel, so that the compressor off is not the all-off chain, which skips the limiter.
    fn through(compressor: Compressor, amplitude: f32) -> (Vec<f32>, f32, DspReading) {
        let mut dsp = Dsp::new(DspConfig {
            noise: NoiseSuppression {
                on: false,
                ..NoiseSuppression::default()
            },
            echo_cancellation: true,
            compressor,
        });
        let levels = DspLevels::default();
        let mut meter = DspMeter::new(levels.clone());
        let mut pcm = tone(amplitude);
        dsp.process(&mut pcm, Some(&mut meter));
        let tail = rms(&pcm[pcm.len() - DSP_FRAME * 10..]);
        (pcm, tail, levels.read())
    }

    #[test]
    fn a_higher_amount_turns_a_loud_voice_down_further_and_off_leaves_only_the_limiter() {
        let at = |amount| Compressor {
            on: true,
            amount: CompressorAmount::new(amount),
        };
        let input = rms(&tone(0.5));
        let (_, gentle, gentle_read) = through(at(20), 0.5);
        let (_, heavy, heavy_read) = through(at(80), 0.5);
        let (gentle_reduction, heavy_reduction) =
            (gentle_read.reduction_db, heavy_read.reduction_db);
        assert!(
            heavy < gentle && gentle < input,
            "{heavy} < {gentle} < {input}"
        );
        assert!(
            heavy_reduction > gentle_reduction && gentle_reduction > 0.0,
            "the meter: {heavy_reduction} > {gentle_reduction} > 0"
        );

        let off = Compressor {
            on: false,
            ..at(80)
        };
        let (_, _, read) = through(off, 0.5);
        assert_eq!((read.reduction_db, read.limit_db), (0.0, 0.0));
        // The echo canceller clips at full scale, so the limiter has 1 dB to take at most.
        let (pcm, _, read) = through(off, 1.5);
        let peak = pcm.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak <= CEILING, "the limiter still caps: {peak}");
        assert!(
            read.limit_db > 0.5,
            "and the meter sees it: {}",
            read.limit_db
        );
    }
}
