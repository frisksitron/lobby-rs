use super::{FRAME, MediaError, SAMPLE_RATE};

/// The largest Opus packet (RFC 6716, section 3.4).
pub(crate) const MAX_PACKET: usize = 1275;
/// With DTX on, libopus returns a packet this short for a frame not worth sending.
pub(crate) const DTX_PACKET: usize = 2;
/// One decoded 20 ms block with room for stereo; the mono result is its first `FRAME` samples.
pub(crate) type Pcm = [f32; FRAME * 2];
const VOICE_BITRATE: i32 = 128_000;
/// Studio Mode's mono bitrate: CELT at this rate is close to transparent.
const STUDIO_BITRATE: i32 = 256_000;
/// The loss the encoder plans FEC for (no RTCP feedback drives it yet).
const EXPECTED_LOSS_PERCENT: i32 = 5;

/// How the encoder is set up: Voice for a cleaned microphone, Studio for the raw one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EncoderProfile {
    /// Voip: 6.5 ms of lookahead, and DTX leaves out frames that hold only silence.
    Voice,
    /// Restricted low delay: CELT only, 2.5 ms of lookahead, every frame sent. Its in-band FEC
    /// is left set; FEC is SILK-only, so it does nothing here.
    Studio,
}

impl EncoderProfile {
    pub fn studio(on: bool) -> Self {
        if on { Self::Studio } else { Self::Voice }
    }
}

pub(crate) struct VoiceEncoder(opus::Encoder);

impl VoiceEncoder {
    pub fn new(profile: EncoderProfile) -> Result<Self, MediaError> {
        let (application, bitrate) = match profile {
            EncoderProfile::Voice => (opus::Application::Voip, VOICE_BITRATE),
            EncoderProfile::Studio => (opus::Application::LowDelay, STUDIO_BITRATE),
        };
        let mut enc =
            opus::Encoder::new(SAMPLE_RATE, opus::Channels::Mono, application).map_err(codec)?;
        enc.set_bitrate(opus::Bitrate::Bits(bitrate))
            .map_err(codec)?;
        enc.set_vbr(true).map_err(codec)?;
        enc.set_inband_fec(true).map_err(codec)?;
        enc.set_packet_loss_perc(EXPECTED_LOSS_PERCENT)
            .map_err(codec)?;
        enc.set_dtx(profile == EncoderProfile::Voice)
            .map_err(codec)?;
        Ok(Self(enc))
    }

    /// The packet's length, or `None` if libopus failed. It runs in a real-time section, so the
    /// error is not formatted.
    pub fn encode(&mut self, pcm: &[f32; FRAME], out: &mut [u8; MAX_PACKET]) -> Option<usize> {
        self.0.encode_float(pcm, out).ok()
    }

    #[cfg(feature = "test-support")]
    pub fn lookahead(&mut self) -> Option<usize> {
        self.0.get_lookahead().ok().map(|n| n as usize)
    }
}

pub(crate) struct VoiceDecoder {
    decoder: opus::Decoder,
    stereo: bool,
}

impl VoiceDecoder {
    pub fn mono() -> Result<Self, MediaError> {
        Self::new(opus::Channels::Mono)
    }

    pub fn stereo() -> Result<Self, MediaError> {
        Self::new(opus::Channels::Stereo)
    }

    fn new(channels: opus::Channels) -> Result<Self, MediaError> {
        Ok(Self {
            decoder: opus::Decoder::new(SAMPLE_RATE, channels).map_err(codec)?,
            stereo: channels == opus::Channels::Stereo,
        })
    }

    pub fn decode(&mut self, packet: &[u8], out: &mut Pcm) -> bool {
        if self.run(packet, out, false) {
            return true;
        }
        self.conceal(out);
        false
    }

    pub fn decode_fec(&mut self, next: &[u8], out: &mut Pcm) {
        if !self.run(next, out, true) {
            self.conceal(out);
        }
    }

    pub fn conceal(&mut self, out: &mut Pcm) {
        if !self.run(&[], out, false) {
            out.fill(0.0);
        }
    }

    pub fn reset(&mut self) {
        let _ = self.decoder.reset_state();
    }

    /// One 20 ms frame into `out[..FRAME]`, as mono.
    fn run(&mut self, packet: &[u8], out: &mut Pcm, fec: bool) -> bool {
        let channels = if self.stereo { 2 } else { 1 };
        let decoded = self
            .decoder
            .decode_float(packet, &mut out[..FRAME * channels], fec);
        if !matches!(decoded, Ok(n) if n == FRAME) {
            return false;
        }
        if self.stereo {
            for i in 0..FRAME {
                out[i] = (out[2 * i] + out[2 * i + 1]) * 0.5;
            }
        }
        true
    }
}

fn codec(e: opus::Error) -> MediaError {
    MediaError::Codec(e.to_string())
}
