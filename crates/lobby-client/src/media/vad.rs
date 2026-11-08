//! Speaking detection on each 20 ms frame the call sends: the frame's RMS level in dBFS against a
//! noise floor that follows the room slowly. Speech starts on the first frame well above the floor
//! and above an absolute minimum, holds while frames stay a smaller margin above it, and ends
//! [`HANGOVER_FRAMES`] after the last such frame. Time is counted in frames, so it is
//! deterministic.

use lobby_protocol::SPEAKING_HANGOVER;

use super::FRAME;

/// Below this RMS a frame is never speech (−50 dBFS): converter noise, breath, a quiet room.
pub(crate) const MIN_DBFS: f32 = -50.0;
/// [`MIN_DBFS`] as an RMS amplitude, for the playout's quiet points.
pub(crate) const MIN_RMS: f32 = 0.003_16;
/// A frame this far above the noise floor starts speech...
const ON_MARGIN_DB: f32 = 12.0;
/// ...and one this far above it keeps speech going.
const OFF_MARGIN_DB: f32 = 6.0;
const FRAME_MS: u128 = 20;
/// Frames that speech outlasts its last voiced frame: the gaps between words (200 ms).
pub(crate) const HANGOVER_FRAMES: u32 = (SPEAKING_HANGOVER.as_millis() / FRAME_MS) as u32;
/// The floor drops to a quieter frame at once and rises toward a louder one by this much per
/// frame (2.5 dB/s), so a pause between words finds the room again and a voice never becomes it.
const FLOOR_RISE_DB: f32 = 0.05;
/// The floor stays in this range: digital silence is no room, and a sound louder than −40 dBFS
/// for seconds is a voice, so a steady tone keeps speaking.
const FLOOR_MIN_DBFS: f32 = -70.0;
const FLOOR_MAX_DBFS: f32 = -40.0;
/// The lowest level RFC 6464 carries: 127 dB below full scale.
const SILENCE_DBFS: f32 = -127.0;

pub(crate) struct Vad {
    min_dbfs: f32,
    /// Unset until the first frame, which starts it.
    floor: Option<f32>,
    level: f32,
    /// Frames left before speech ends; 0 when silent.
    hangover: u32,
}

impl Vad {
    pub fn new() -> Self {
        Self::with_minimum(MIN_DBFS)
    }

    /// A gate with a higher absolute minimum: a keyboard or a fan must not pass it.
    pub fn with_minimum(min_dbfs: f32) -> Self {
        Self {
            min_dbfs,
            floor: None,
            level: SILENCE_DBFS,
            hangover: 0,
        }
    }

    pub fn speaking(&self) -> bool {
        self.hangover > 0
    }

    /// The last frame's level as RFC 6464 carries it: dB below full scale, 0..=127.
    pub fn dbov(&self) -> u8 {
        (-self.level).round().clamp(0.0, 127.0) as u8
    }

    /// One frame; the verdict if it changed.
    pub fn process(&mut self, frame: &[f32; FRAME]) -> Option<bool> {
        let before = self.speaking();
        let energy: f32 = frame.iter().map(|s| s * s).sum();
        let rms = (energy / FRAME as f32).sqrt();
        self.level = if rms > 0.0 {
            (20.0 * rms.log10()).max(SILENCE_DBFS)
        } else {
            SILENCE_DBFS
        };
        let floor = match self.floor {
            None => self.level,
            Some(floor) if self.level < floor => self.level,
            Some(floor) => (floor + FLOOR_RISE_DB).min(self.level),
        }
        .clamp(FLOOR_MIN_DBFS, FLOOR_MAX_DBFS);
        self.floor = Some(floor);
        let margin = if before { OFF_MARGIN_DB } else { ON_MARGIN_DB };
        if self.level >= self.min_dbfs && self.level >= floor + margin {
            self.hangover = HANGOVER_FRAMES;
        } else {
            self.hangover = self.hangover.saturating_sub(1);
        }
        (self.speaking() != before).then_some(self.speaking())
    }

    /// Nothing is sent (muted): speech ends at once. The floor stays, the room has not changed.
    pub fn silence(&mut self) -> Option<bool> {
        let was = self.speaking();
        self.hangover = 0;
        self.level = SILENCE_DBFS;
        was.then_some(false)
    }
}
