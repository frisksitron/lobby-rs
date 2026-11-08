//! What the microphone's frames say that nothing else tells the Member: they talk while muted, or
//! the device sends only digital zeros (a headset's mute switch, or the OS's microphone privacy
//! switch; the device API reports neither). Every rule counts 20 ms frames, never wall time, so it
//! is deterministic. `lobby-voice-tx` feeds it each frame; the Session decides what to tell.

use super::{FRAME, vad::Vad};

/// RMS a muted frame must reach to count as the Member talking. Higher than the speaking ring's
/// gate: a keyboard or a fan should not nudge.
const TALK_MIN_DBFS: f32 = -40.0;
/// Continuous talking, hold included, before the Member is told they are muted (1 s).
pub(crate) const MUTED_TALK_FRAMES: u32 = 50;
/// Frames between two nudges, even across mute cycles (30 s).
pub(crate) const MUTED_NUDGE_COOLDOWN: u64 = 1500;
/// Consecutive all-zero frames before the Member is told the microphone is silent (5 s).
pub(crate) const SILENT_FRAMES: u32 = 250;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MicNotice {
    TalkingMuted,
    Silent,
}

pub(crate) struct MicWatch {
    talk: Vad,
    talking_for: u32,
    /// Off after a nudge until the Member has unmuted: one nudge per mute.
    armed: bool,
    since_nudge: u64,
    zeros_for: u32,
    silent_after: u32,
    /// Told once per call until a non-zero sample comes back.
    told_silent: bool,
}

impl MicWatch {
    pub fn new() -> Self {
        Self {
            talk: Vad::with_minimum(TALK_MIN_DBFS),
            talking_for: 0,
            armed: true,
            since_nudge: MUTED_NUDGE_COOLDOWN,
            zeros_for: 0,
            silent_after: SILENT_FRAMES,
            told_silent: false,
        }
    }

    #[cfg(feature = "test-support")]
    pub fn silent_after(&mut self, frames: u32) {
        self.silent_after = frames;
    }

    /// One frame as the device delivered it (`raw`) and after the DSP (`cleaned`, where an echo
    /// canceller has taken the speakers out of it).
    pub fn frame(
        &mut self,
        raw: &[f32; FRAME],
        cleaned: &[f32; FRAME],
        muted: bool,
    ) -> Option<MicNotice> {
        let silent = self.digital_silence(raw, muted);
        let talking = self.talking_muted(cleaned, muted);
        silent.or(talking)
    }

    fn digital_silence(&mut self, raw: &[f32; FRAME], muted: bool) -> Option<MicNotice> {
        if raw.iter().any(|s| *s != 0.0) {
            self.zeros_for = 0;
            self.told_silent = false;
            return None;
        }
        if muted {
            self.zeros_for = 0;
            return None;
        }
        self.zeros_for = self.zeros_for.saturating_add(1);
        if self.zeros_for >= self.silent_after && !self.told_silent {
            self.told_silent = true;
            return Some(MicNotice::Silent);
        }
        None
    }

    fn talking_muted(&mut self, cleaned: &[f32; FRAME], muted: bool) -> Option<MicNotice> {
        self.since_nudge = self.since_nudge.saturating_add(1);
        if !muted {
            self.armed = true;
            self.talking_for = 0;
            self.talk.silence();
            return None;
        }
        self.talk.process(cleaned);
        self.talking_for = if self.talk.speaking() {
            self.talking_for.saturating_add(1)
        } else {
            0
        };
        if self.armed
            && self.talking_for >= MUTED_TALK_FRAMES
            && self.since_nudge >= MUTED_NUDGE_COOLDOWN
        {
            self.armed = false;
            self.since_nudge = 0;
            return Some(MicNotice::TalkingMuted);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEECH: [f32; FRAME] = [0.2; FRAME];
    const HUM: [f32; FRAME] = [0.001; FRAME];
    const ZEROS: [f32; FRAME] = [0.0; FRAME];

    /// Feeds `frames` of `pcm` and returns the notices with the frame index they came on.
    fn feed(
        watch: &mut MicWatch,
        pcm: &[f32; FRAME],
        frames: u32,
        muted: bool,
    ) -> Vec<(u32, MicNotice)> {
        (0..frames)
            .filter_map(|i| watch.frame(pcm, pcm, muted).map(|n| (i, n)))
            .collect()
    }

    #[test]
    fn talking_muted_is_told_once_after_a_second_of_speech() {
        let mut watch = MicWatch::new();
        let told = feed(&mut watch, &SPEECH, 400, true);
        assert_eq!(told.len(), 1, "one nudge for as long as they talk muted");
        let (at, notice) = told[0];
        assert_eq!(notice, MicNotice::TalkingMuted);
        assert!(
            (MUTED_TALK_FRAMES - 1..MUTED_TALK_FRAMES + 3).contains(&at),
            "at {at}"
        );
    }

    #[test]
    fn room_noise_and_a_short_word_do_not_nudge() {
        let mut watch = MicWatch::new();
        assert!(feed(&mut watch, &HUM, 3000, true).is_empty());
        assert!(feed(&mut watch, &SPEECH, 20, true).is_empty());
        assert!(feed(&mut watch, &ZEROS, 100, true).is_empty());
        assert!(feed(&mut watch, &SPEECH, 20, true).is_empty());
    }

    #[test]
    fn talking_unmuted_never_nudges() {
        let mut watch = MicWatch::new();
        assert!(feed(&mut watch, &SPEECH, 500, false).is_empty());
    }

    #[test]
    fn a_nudge_waits_for_an_unmute_and_the_cooldown() {
        let mut watch = MicWatch::new();
        assert_eq!(feed(&mut watch, &SPEECH, 100, true).len(), 1);
        // Muted again at once: not armed until they unmute, and the cooldown has not run out.
        assert!(feed(&mut watch, &SPEECH, 100, true).is_empty());
        feed(&mut watch, &SPEECH, 10, false);
        assert!(
            feed(&mut watch, &SPEECH, 100, true).is_empty(),
            "cooling down"
        );
        // Past the cooldown an unmute and a mute arm it again.
        feed(&mut watch, &ZEROS, MUTED_NUDGE_COOLDOWN as u32, false);
        assert_eq!(feed(&mut watch, &SPEECH, 100, true).len(), 1);
    }

    #[test]
    fn zeros_for_five_seconds_are_told_once_until_audio_returns() {
        let mut watch = MicWatch::new();
        let told = feed(&mut watch, &ZEROS, 2000, false);
        assert_eq!(told, vec![(SILENT_FRAMES - 1, MicNotice::Silent)]);
        assert!(feed(&mut watch, &HUM, 10, false).is_empty());
        let again = feed(&mut watch, &ZEROS, 2000, false);
        assert_eq!(again.len(), 1, "told again after the audio came back");
    }

    #[test]
    fn a_quiet_room_is_not_digital_silence() {
        let mut watch = MicWatch::new();
        assert!(feed(&mut watch, &HUM, 3000, false).is_empty());
    }

    #[test]
    fn zeros_while_muted_do_not_count() {
        let mut watch = MicWatch::new();
        assert!(feed(&mut watch, &ZEROS, 1000, true).is_empty());
        assert!(feed(&mut watch, &ZEROS, SILENT_FRAMES - 1, false).is_empty());
    }
}
