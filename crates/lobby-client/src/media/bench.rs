//! What `voice_bench` times Voice with (feature `test-support`): a pulse train for a microphone
//! that says when each pulse entered the microphone ring, a sink that says when it played each
//! block, and the capture DSP and the Opus round trip alone, to time them at their boundary.

use std::{
    sync::{Arc, Mutex},
    time::Instant,
};

use super::{
    FRAME, MediaError, SAMPLE_RATE,
    dsp::{DSP_FRAME, Dsp, DspConfig},
    meter::{DspLevels, DspMeter},
    opus_codec::{DTX_PACKET, EncoderProfile, MAX_PACKET, Pcm, VoiceDecoder, VoiceEncoder},
};

/// What each pulse is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PulseShape {
    Tone {
        hz: f32,
    },
    /// Harmonics of a fundamental that glides from `from_hz` to `to_hz`, as a voiced vowel does,
    /// up to 4 kHz. The glide keeps the pulse from matching itself one period later.
    Buzz {
        from_hz: f32,
        to_hz: f32,
    },
}

/// One pulse as the microphone ring took it.
#[derive(Debug, Clone, Copy)]
pub struct Onset {
    /// The pulse's first sample, counted from the start of the microphone.
    pub sample: u64,
    /// When that sample entered the ring.
    pub entered: Instant,
    /// When the last sample of the 20 ms frame holding it entered the ring.
    pub frame_done: Option<Instant>,
}

/// A pulse of `length` samples every `every` samples, the first at sample 0. Each starts at its
/// peak, so its onset is one sample.
#[derive(Debug, Clone)]
pub struct Pulses {
    pub shape: PulseShape,
    pub every: u64,
    pub length: u64,
    pub amplitude: f32,
    pub onsets: Arc<Mutex<Vec<Onset>>>,
}

const BUZZ_TOP_HZ: f32 = 4000.0;

impl Pulses {
    pub fn new(shape: PulseShape, every: u64, length: u64, amplitude: f32) -> Pulses {
        Pulses {
            shape,
            every,
            length,
            amplitude,
            onsets: Arc::default(),
        }
    }

    /// The `n`th sample at 48 kHz.
    pub fn sample(&self, n: u64) -> f32 {
        let p = n % self.every;
        if p >= self.length {
            return 0.0;
        }
        let t = p as f64 / f64::from(SAMPLE_RATE);
        let tau = std::f64::consts::TAU;
        let x = match self.shape {
            PulseShape::Tone { hz } => (tau * f64::from(hz) * t).cos(),
            PulseShape::Buzz { from_hz, to_hz } => {
                let (from, to) = (f64::from(from_hz), f64::from(to_hz));
                let duration = self.length as f64 / f64::from(SAMPLE_RATE);
                let phase = tau * (from * t + (to - from) * t * t / (2.0 * duration));
                let harmonics = (f64::from(BUZZ_TOP_HZ) / from.max(to)).floor().max(1.0) as u32;
                let (mut sum, mut norm) = (0.0, 0.0);
                for h in 1..=harmonics {
                    let h = f64::from(h);
                    sum += (h * phase).cos() / h;
                    norm += 1.0 / h;
                }
                sum / norm
            }
        };
        (f64::from(self.amplitude) * x) as f32
    }

    /// The `n`th sample, noting the instant when `n` starts a pulse or ends a frame that holds
    /// the latest one. Called just before the sample enters the ring.
    pub(crate) fn next(&self, n: u64) -> f32 {
        let starts = n.is_multiple_of(self.every);
        let ends_frame = (n + 1).is_multiple_of(FRAME as u64);
        if starts || ends_frame {
            let now = Instant::now();
            let mut onsets = self.onsets.lock().expect("onsets");
            if starts {
                onsets.push(Onset {
                    sample: n,
                    entered: now,
                    frame_done: None,
                });
            }
            if ends_frame
                && let Some(last) = onsets.last_mut()
                && last.frame_done.is_none()
            {
                last.frame_done = Some(now);
            }
        }
        self.sample(n)
    }
}

/// A 20 ms block the sink played, and when it took it from the playout.
pub struct PlayedBlock {
    pub at: Instant,
    pub pcm: [f32; FRAME],
}

/// The capture DSP alone, as `lobby-voice-tx` runs it, meter and all. With echo cancellation the
/// reference is silence, as it is when nothing plays.
pub struct CaptureChain {
    dsp: Dsp,
    meter: DspMeter,
}

impl CaptureChain {
    pub fn new(config: DspConfig) -> CaptureChain {
        CaptureChain {
            dsp: Dsp::new(config),
            meter: DspMeter::new(DspLevels::default()),
        }
    }

    /// `pcm` in place, a whole number of 10 ms frames.
    pub fn process(&mut self, pcm: &mut [f32]) {
        for frame in pcm.chunks_exact_mut(DSP_FRAME) {
            self.dsp.render(&[0.0; DSP_FRAME]);
            self.dsp.process(frame, Some(&mut self.meter));
        }
    }
}

/// The Voice encoder and a decoder, back to back.
pub struct CodecLoop {
    encoder: VoiceEncoder,
    decoder: VoiceDecoder,
    packet: [u8; MAX_PACKET],
    pcm: Pcm,
}

impl CodecLoop {
    /// The encoder as the studio profile sets it up, or as Voice does.
    pub fn new(studio: bool) -> Result<CodecLoop, MediaError> {
        Ok(CodecLoop {
            encoder: VoiceEncoder::new(EncoderProfile::studio(studio))?,
            decoder: VoiceDecoder::mono()?,
            packet: [0; MAX_PACKET],
            pcm: [0.0; FRAME * 2],
        })
    }

    /// libopus' own figure for the encoder's lookahead, in samples.
    pub fn lookahead(&mut self) -> Option<usize> {
        self.encoder.lookahead()
    }

    /// A frame through Opus and back. A frame DTX would not send plays as silence.
    pub fn round_trip(&mut self, frame: &mut [f32; FRAME]) {
        match self.encoder.encode(frame, &mut self.packet) {
            Some(n) if n > DTX_PACKET => {
                self.decoder.decode(&self.packet[..n], &mut self.pcm);
                frame.copy_from_slice(&self.pcm[..FRAME]);
            }
            _ => frame.fill(0.0),
        }
    }
}
