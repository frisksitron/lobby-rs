//! The microphone meter: what the raw signal, before any DSP, says about the person's level.
//!
//! The audio callback feeds every sample to a [`MicMeter`]. Each 10 ms frame it measures an RMS
//! level (loudness) and a sample peak (clipping), moves the displayed level with meter
//! ballistics, judges whether the microphone is too quiet, too loud or clipping, and publishes
//! all of it to a [`MicLevel`] the UI reads. Everything is in dBFS, and nothing locks or
//! allocates.
//!
//! In a call the encoder thread also meters the capture DSP along its length ([`DspMeter`] into
//! [`DspLevels`]), with the same ballistics.

use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU8, AtomicU32, Ordering},
    },
};

/// Frames per second: one measurement per 10 ms.
const FRAMES_PER_SECOND: u32 = 100;
/// The level of silence: nothing is reported below it.
const SILENCE_DB: f32 = -100.0;

/// A frame whose sample peak reaches this is clipping: the converter's range is used up, so the
/// top of the waves is flattened. Digital audio practice treats peaks within 1 dB of full scale
/// as clipped (EBU R128 and most DAW meters flag from about -1 dBFS).
const CLIP_PEAK_DB: f32 = -1.0;
/// Clipped frames within [`CLIP_WINDOW`] that make the microphone clipping, so one stray peak
/// (a cough, a knock) is not.
const CLIP_FRAMES: u32 = 2;
/// The span clipping is judged over: 128 frames, 1.28 s.
const CLIP_WINDOW: u32 = 128;
/// Speech RMS at or above this is too loud. Voice recommendations (broadcast and conferencing
/// guides) put speech at -30 to -12 dBFS RMS, and -10 leaves no headroom for the peaks, which
/// run 12 to 20 dB above the RMS.
const TOO_LOUD_RMS_DB: f32 = -10.0;
/// Speech whose peaks stay below this is too quiet: the usable range is 45 dB under full scale,
/// near where a cheap microphone's own noise (about -60 dBFS) starts to matter after gain.
const TOO_QUIET_PEAK_DB: f32 = -45.0;
/// Speech RMS from here up to [`TOO_LOUD_RMS_DB`] is good (the conferencing range).
const GOOD_RMS_DB: f32 = -40.0;
/// From here the meter is red: closer than this to full scale the next louder syllable clips.
const DANGER_FROM_DB: f32 = -6.0;

/// Frames of speech the verdict looks at: one second of talking, not of waiting.
const SPEECH_WINDOW: u32 = 100;
/// Share of those frames, out of 4, whose RMS must be over [`TOO_LOUD_RMS_DB`] for "too loud".
const TOO_LOUD_QUARTERS: u32 = 3;
/// A frame is speech when its RMS is this far over the noise floor...
const SPEECH_MARGIN_DB: f32 = 12.0;
/// ...and over this absolute level, so converter noise in a quiet room is not speech.
const SPEECH_MIN_RMS_DB: f32 = -70.0;
/// The noise floor is the quietest 100 ms block of the last three seconds.
const FLOOR_BLOCK_FRAMES: u32 = 10;
const FLOOR_BLOCKS: usize = 30;
/// Without speech for this long (5 s) a verdict is forgotten, so it is not shown stale.
const STALE_FRAMES: u32 = 500;

/// The displayed level moves this far toward a louder frame each frame (fast attack)...
const ATTACK: f32 = 0.6;
/// ...and falls at this many dB per second (slower release).
const RELEASE_DB_PER_SECOND: f32 = 60.0;
/// The peak mark stays at its top for 1 s...
const PEAK_HOLD_FRAMES: u32 = 100;
/// ...then falls at this many dB per second.
const PEAK_FALL_DB_PER_SECOND: f32 = 30.0;

/// What the level says about how the microphone is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MicQuality {
    /// Nothing wrong, or not enough speech yet to say.
    #[default]
    Good,
    TooQuiet,
    TooLoud,
    Clipping,
}

impl MicQuality {
    fn code(self) -> u8 {
        self as u8
    }

    fn from_code(code: u8) -> MicQuality {
        match code {
            1 => MicQuality::TooQuiet,
            2 => MicQuality::TooLoud,
            3 => MicQuality::Clipping,
            _ => MicQuality::Good,
        }
    }
}

/// How a level reads on the meter, for the colour of its segments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    Quiet,
    Good,
    Loud,
    Danger,
}

impl Zone {
    pub fn of(db: f32) -> Zone {
        if db < GOOD_RMS_DB {
            Zone::Quiet
        } else if db < TOO_LOUD_RMS_DB {
            Zone::Good
        } else if db < DANGER_FROM_DB {
            Zone::Loud
        } else {
            Zone::Danger
        }
    }
}

/// One look at the meter. Levels are dBFS, -100 for silence and 0 for full scale.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MicReading {
    /// The RMS level with meter ballistics: what the bar shows.
    pub level_db: f32,
    /// The sample peak, held for a moment: the mark above the bar.
    pub peak_db: f32,
    pub quality: MicQuality,
}

impl MicReading {
    pub const SILENT: MicReading = MicReading {
        level_db: SILENCE_DB,
        peak_db: SILENCE_DB,
        quality: MicQuality::Good,
    };
}

#[derive(Default)]
struct Published {
    level: AtomicU32,
    peak: AtomicU32,
    quality: AtomicU8,
}

/// What a [`MicMeter`] measured, written by an audio callback and read by the UI. Reads are three
/// relaxed atomics, so a reader never holds the callback up.
#[derive(Clone)]
pub struct MicLevel(Arc<Published>);

impl Default for MicLevel {
    fn default() -> Self {
        let level = MicLevel(Arc::default());
        level.publish(MicReading::SILENT);
        level
    }
}

impl fmt::Debug for MicLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MicLevel")
    }
}

impl MicLevel {
    pub fn read(&self) -> MicReading {
        MicReading {
            level_db: f32::from_bits(self.0.level.load(Ordering::Relaxed)),
            peak_db: f32::from_bits(self.0.peak.load(Ordering::Relaxed)),
            quality: MicQuality::from_code(self.0.quality.load(Ordering::Relaxed)),
        }
    }

    fn publish(&self, reading: MicReading) {
        self.0
            .level
            .store(reading.level_db.to_bits(), Ordering::Relaxed);
        self.0
            .peak
            .store(reading.peak_db.to_bits(), Ordering::Relaxed);
        self.0
            .quality
            .store(reading.quality.code(), Ordering::Relaxed);
    }
}

fn db(amplitude: f32) -> f32 {
    (20.0 * amplitude.max(1e-6).log10()).max(SILENCE_DB)
}

/// `db_per_second` as a step of one frame.
fn step(db_per_second: f32) -> f32 {
    db_per_second / FRAMES_PER_SECOND as f32
}

/// A displayed level one frame on: toward a louder `db` fast, down from it at the release rate.
fn follow(shown: f32, db: f32) -> f32 {
    if db > shown {
        shown + ATTACK * (db - shown)
    } else {
        (shown - step(RELEASE_DB_PER_SECOND)).max(db)
    }
}

fn window_mask(frames: u32) -> u128 {
    if frames >= 128 {
        u128::MAX
    } else {
        (1u128 << frames) - 1
    }
}

/// The audio callback's side. Feed it every sample of the raw microphone, mono, at the rate it
/// was built for. Dropping it publishes silence, so a stopped microphone does not leave a level
/// on the meter.
pub struct MicMeter {
    out: MicLevel,
    frame_len: u32,
    in_frame: u32,
    sum_squares: f32,
    frame_peak: f32,

    shown_db: f32,
    peak_db: f32,
    peak_hold_left: u32,

    blocks: [f32; FLOOR_BLOCKS],
    block: usize,
    block_frames: u32,
    block_min: f32,

    clipped: u128,
    /// One bit per speech frame, newest lowest: whether its RMS was over the loud line.
    loud: u128,
    /// The same, for whether its peak reached the quiet line.
    audible: u128,
    speech_frames: u32,
    since_speech: u32,
}

impl MicMeter {
    pub fn new(sample_rate: u32, out: MicLevel) -> MicMeter {
        MicMeter {
            out,
            frame_len: (sample_rate / FRAMES_PER_SECOND).max(1),
            in_frame: 0,
            sum_squares: 0.0,
            frame_peak: 0.0,
            shown_db: SILENCE_DB,
            peak_db: SILENCE_DB,
            peak_hold_left: 0,
            blocks: [f32::INFINITY; FLOOR_BLOCKS],
            block: 0,
            block_frames: 0,
            block_min: f32::INFINITY,
            clipped: 0,
            loud: 0,
            audible: 0,
            speech_frames: 0,
            since_speech: 0,
        }
    }

    pub fn push(&mut self, sample: f32) {
        if !sample.is_finite() {
            return;
        }
        self.sum_squares += sample * sample;
        self.frame_peak = self.frame_peak.max(sample.abs());
        self.in_frame += 1;
        if self.in_frame == self.frame_len {
            let rms = (self.sum_squares / self.frame_len as f32).sqrt();
            self.frame(db(rms), db(self.frame_peak));
            self.in_frame = 0;
            self.sum_squares = 0.0;
            self.frame_peak = 0.0;
        }
    }

    fn frame(&mut self, rms_db: f32, peak_db: f32) {
        self.shown_db = follow(self.shown_db, rms_db);
        if peak_db >= self.peak_db {
            self.peak_db = peak_db;
            self.peak_hold_left = PEAK_HOLD_FRAMES;
        } else if self.peak_hold_left > 0 {
            self.peak_hold_left -= 1;
        } else {
            self.peak_db = (self.peak_db - step(PEAK_FALL_DB_PER_SECOND)).max(peak_db);
        }

        let floor = self.noise_floor(rms_db);
        let speech = rms_db >= floor + SPEECH_MARGIN_DB && rms_db >= SPEECH_MIN_RMS_DB;
        self.clipped = self.clipped << 1 | u128::from(peak_db >= CLIP_PEAK_DB);
        if speech {
            self.since_speech = 0;
            self.speech_frames = (self.speech_frames + 1).min(SPEECH_WINDOW);
            self.loud = self.loud << 1 | u128::from(rms_db >= TOO_LOUD_RMS_DB);
            self.audible = self.audible << 1 | u128::from(peak_db >= TOO_QUIET_PEAK_DB);
        } else {
            self.since_speech = self.since_speech.saturating_add(1);
            if self.since_speech >= STALE_FRAMES {
                self.speech_frames = 0;
                self.loud = 0;
                self.audible = 0;
            }
        }

        self.out.publish(MicReading {
            level_db: self.shown_db,
            peak_db: self.peak_db,
            quality: self.quality(),
        });
    }

    /// The quietest block of the last three seconds, counting the one in progress.
    fn noise_floor(&mut self, rms_db: f32) -> f32 {
        self.block_min = self.block_min.min(rms_db);
        self.block_frames += 1;
        let floor = self.blocks.iter().fold(self.block_min, |a, b| a.min(*b));
        if self.block_frames == FLOOR_BLOCK_FRAMES {
            self.blocks[self.block] = self.block_min;
            self.block = (self.block + 1) % FLOOR_BLOCKS;
            self.block_frames = 0;
            self.block_min = f32::INFINITY;
        }
        floor
    }

    fn quality(&self) -> MicQuality {
        let clipped = (self.clipped & window_mask(CLIP_WINDOW)).count_ones();
        if clipped >= CLIP_FRAMES {
            return MicQuality::Clipping;
        }
        if self.speech_frames < SPEECH_WINDOW {
            return MicQuality::Good;
        }
        let window = window_mask(SPEECH_WINDOW);
        if (self.loud & window).count_ones() * 4 >= SPEECH_WINDOW * TOO_LOUD_QUARTERS {
            MicQuality::TooLoud
        } else if self.audible & window == 0 {
            MicQuality::TooQuiet
        } else {
            MicQuality::Good
        }
    }
}

impl Drop for MicMeter {
    fn drop(&mut self) {
        self.out.publish(MicReading::SILENT);
    }
}

/// The gain reduction shown falls back at this rate once the compressor lets go.
const REDUCTION_RELEASE_DB_PER_SECOND: f32 = 40.0;
/// The limiter acts for a few milliseconds at a time; it falls back slower than the compressor, so
/// a single peak stays in sight for a few frames of the UI.
const LIMIT_RELEASE_DB_PER_SECOND: f32 = 15.0;

/// One look along the capture DSP, in a call or MONITOR's: the RMS level at each point of the
/// chain with the microphone meter's ballistics, in dBFS, and how far the compressor and the
/// limiter are turning the voice down, in dB. A module out of the path passes its input on, so its
/// point reads the same as the one before it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DspReading {
    /// The microphone as captured.
    pub mic_db: f32,
    pub after_echo_db: f32,
    pub after_noise_db: f32,
    /// What Opus is given: after the compressor and the limiter.
    pub out_db: f32,
    pub reduction_db: f32,
    pub limit_db: f32,
}

impl DspReading {
    pub const SILENT: DspReading = DspReading {
        mic_db: SILENCE_DB,
        after_echo_db: SILENCE_DB,
        after_noise_db: SILENCE_DB,
        out_db: SILENCE_DB,
        reduction_db: 0.0,
        limit_db: 0.0,
    };
}

/// What a [`DspMeter`] measured, written by the encoder thread once per 10 ms frame and read by
/// the UI. Relaxed atomics, as [`MicLevel`]'s.
#[derive(Clone)]
pub struct DspLevels(Arc<[AtomicU32; 6]>);

impl Default for DspLevels {
    fn default() -> Self {
        let levels = DspLevels(Arc::default());
        levels.publish(DspReading::SILENT);
        levels
    }
}

impl fmt::Debug for DspLevels {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DspLevels")
    }
}

impl DspLevels {
    pub fn read(&self) -> DspReading {
        let [mic, echo, noise, out, reduction, limit] = self
            .0
            .each_ref()
            .map(|v| f32::from_bits(v.load(Ordering::Relaxed)));
        DspReading {
            mic_db: mic,
            after_echo_db: echo,
            after_noise_db: noise,
            out_db: out,
            reduction_db: reduction,
            limit_db: limit,
        }
    }

    fn publish(&self, r: DspReading) {
        let values = [
            r.mic_db,
            r.after_echo_db,
            r.after_noise_db,
            r.out_db,
            r.reduction_db,
            r.limit_db,
        ];
        for (slot, value) in self.0.iter().zip(values) {
            slot.store(value.to_bits(), Ordering::Relaxed);
        }
    }
}

/// The encoder thread's side: the DSP hands it each frame's RMS at the four points and the
/// compressor's and the limiter's reductions. Dropping it publishes silence, as [`MicMeter`] does.
pub(crate) struct DspMeter {
    out: DspLevels,
    shown: DspReading,
}

impl DspMeter {
    pub fn new(out: DspLevels) -> DspMeter {
        DspMeter {
            out,
            shown: DspReading::SILENT,
        }
    }

    /// One 10 ms frame: `rms` as amplitudes, at the microphone, after echo cancellation, after
    /// noise suppression and at the end.
    pub fn frame(&mut self, rms: [f32; 4], reduction_db: f32, limit_db: f32) {
        let s = &mut self.shown;
        s.mic_db = follow(s.mic_db, db(rms[0]));
        s.after_echo_db = follow(s.after_echo_db, db(rms[1]));
        s.after_noise_db = follow(s.after_noise_db, db(rms[2]));
        s.out_db = follow(s.out_db, db(rms[3]));
        s.reduction_db = reduction_db
            .max(s.reduction_db - step(REDUCTION_RELEASE_DB_PER_SECOND))
            .max(0.0);
        s.limit_db = limit_db
            .max(s.limit_db - step(LIMIT_RELEASE_DB_PER_SECOND))
            .max(0.0);
        self.out.publish(self.shown);
    }
}

impl Drop for DspMeter {
    fn drop(&mut self) {
        self.out.publish(DspReading::SILENT);
    }
}
