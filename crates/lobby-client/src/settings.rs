//! What a person chose in Settings, kept in `state.json` with the Lobbies. Plain data: the desktop
//! reads it and writes it back through its `Lobbies` entity.

use std::collections::BTreeMap;

use lobby_protocol::{Quality, UserId};
use serde::{Deserialize, Deserializer, Serialize};

use crate::{DeviceChoice, LobbyOrigin, Volume};

/// Every field has a default, so a file from before a field existed loads with it set, and
/// `Persisted` leaves the whole object out of the file while nothing was changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Whether the window's close button keeps Lobby running in the tray instead of quitting.
    pub close_to_tray: bool,
    /// How big the whole UI draws, in whole percent: [`UI_SCALE_MIN`] to [`UI_SCALE_MAX`] in
    /// steps of [`UI_SCALE_STEP`]. [`Settings::normalize`] brings any other value to one of them.
    #[serde(deserialize_with = "lenient_percent")]
    pub ui_scale: u16,
    /// The short sounds for joining and leaving Voice, mute and the like.
    pub sounds: bool,
    pub input_device: DeviceChoice,
    pub output_device: DeviceChoice,
    /// How loud each other Member plays for this person, by Lobby (its origin) and Member. Only
    /// the Members moved off 100 % are kept.
    pub member_volumes: BTreeMap<String, BTreeMap<UserId, Volume>>,
    /// What takes steady noise out of the microphone before it is sent.
    pub noise_suppression: NoiseSuppression,
    /// Takes the speakers' sound out of the microphone. Off: headphones do it better.
    pub echo_cancellation: bool,
    /// Evens out loud and quiet speech, after the two above.
    pub compressor: Compressor,
    #[serde(deserialize_with = "lenient_quality")]
    pub share_quality: Quality,
}

pub const UI_SCALE_DEFAULT: u16 = 100;
pub const UI_SCALE_MIN: u16 = 80;
/// Past this the small window cannot hold the layout.
pub const UI_SCALE_MAX: u16 = 150;
const UI_SCALE_STEP: u16 = 10;

/// `percent` clamped to the range and snapped to the nearest step (halves go up).
fn snap_ui_scale(percent: u16) -> u16 {
    let steps = (percent.clamp(UI_SCALE_MIN, UI_SCALE_MAX) - UI_SCALE_MIN + UI_SCALE_STEP / 2)
        / UI_SCALE_STEP;
    UI_SCALE_MIN + steps * UI_SCALE_STEP
}

/// One step bigger or smaller, stopping at the ends.
pub fn step_ui_scale(percent: u16, bigger: bool) -> u16 {
    let now = snap_ui_scale(percent);
    if bigger {
        snap_ui_scale(now + UI_SCALE_STEP)
    } else {
        snap_ui_scale(now.saturating_sub(UI_SCALE_STEP))
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            close_to_tray: true,
            ui_scale: UI_SCALE_DEFAULT,
            sounds: true,
            input_device: DeviceChoice::default(),
            output_device: DeviceChoice::default(),
            member_volumes: BTreeMap::new(),
            noise_suppression: NoiseSuppression::default(),
            echo_cancellation: false,
            compressor: Compressor::default(),
            share_quality: Quality::UpTo2160,
        }
    }
}

impl Settings {
    pub fn member_volume(&self, lobby: &LobbyOrigin, user: &UserId) -> Volume {
        self.member_volumes
            .get(lobby.as_str())
            .and_then(|members| members.get(user))
            .copied()
            .unwrap_or_default()
    }

    /// Every Member of `lobby` who is not at 100 %.
    pub fn volumes_of(&self, lobby: &LobbyOrigin) -> BTreeMap<UserId, Volume> {
        self.member_volumes
            .get(lobby.as_str())
            .cloned()
            .unwrap_or_default()
    }

    pub fn set_member_volume(&mut self, lobby: &LobbyOrigin, user: &UserId, volume: Volume) {
        if volume != Volume::DEFAULT {
            self.member_volumes
                .entry(lobby.as_str().to_owned())
                .or_default()
                .insert(user.clone(), volume);
            return;
        }
        if let Some(members) = self.member_volumes.get_mut(lobby.as_str()) {
            members.remove(user);
            if members.is_empty() {
                self.member_volumes.remove(lobby.as_str());
            }
        }
    }

    /// Brings what a hand-edited or newer file holds to what this version shows.
    pub fn normalize(&mut self) {
        self.ui_scale = snap_ui_scale(self.ui_scale);
    }

    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// The noise module: whether it is in the microphone's path, and the suppressor it runs, kept
/// while the module is out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct NoiseSuppression {
    pub on: bool,
    pub engine: NoiseEngine,
}

impl Default for NoiseSuppression {
    fn default() -> Self {
        Self {
            on: true,
            engine: NoiseEngine::default(),
        }
    }
}

/// What takes steady noise (fans, hiss, keys) out of the microphone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum NoiseEngine {
    /// A small neural network trained on speech: the best on most microphones.
    #[default]
    Rnnoise,
    /// The classic WebRTC suppressor: lighter on the CPU.
    Webrtc,
}

impl NoiseEngine {
    /// Named for what a person hears, not for the library: RNNoise is "Strong", WebRTC's
    /// suppressor "Gentle".
    pub fn label(self) -> &'static str {
        match self {
            NoiseEngine::Rnnoise => "Strong",
            NoiseEngine::Webrtc => "Gentle",
        }
    }

    fn from_stored(text: &str) -> Self {
        match text {
            "webrtc" => NoiseEngine::Webrtc,
            _ => NoiseEngine::default(),
        }
    }
}

/// What a later or earlier version stored is not worth refusing the file for: any text, or any
/// other value, loads as one of ours.
impl<'de> Deserialize<'de> for NoiseEngine {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(stored_text(d)?.map_or_else(Self::default, |t| Self::from_stored(&t)))
    }
}

impl<'de> Deserialize<'de> for NoiseSuppression {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(default)]
        struct Module {
            on: bool,
            engine: NoiseEngine,
        }
        impl Default for Module {
            fn default() -> Self {
                let NoiseSuppression { on, engine } = NoiseSuppression::default();
                Module { on, engine }
            }
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Stored {
            /// Before the rack the setting was one choice, "off" among the engines.
            Text(String),
            Module(Module),
            Other(serde::de::IgnoredAny),
        }
        Ok(match Stored::deserialize(d)? {
            Stored::Text(text) => NoiseSuppression {
                on: text != "off",
                engine: NoiseEngine::from_stored(&text),
            },
            Stored::Module(Module { on, engine }) => NoiseSuppression { on, engine },
            Stored::Other(_) => NoiseSuppression::default(),
        })
    }
}

/// The compressor module. The limiter under it stays on whichever way this is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Compressor {
    pub on: bool,
    pub amount: CompressorAmount,
}

impl Default for Compressor {
    fn default() -> Self {
        Self {
            on: true,
            amount: CompressorAmount::DEFAULT,
        }
    }
}

impl<'de> Deserialize<'de> for Compressor {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(default)]
        struct Module {
            on: bool,
            amount: CompressorAmount,
        }
        impl Default for Module {
            fn default() -> Self {
                let Compressor { on, amount } = Compressor::default();
                Module { on, amount }
            }
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Stored {
            Module(Module),
            Other(serde::de::IgnoredAny),
        }
        Ok(match Stored::deserialize(d)? {
            Stored::Module(Module { on, amount }) => Compressor { on, amount },
            Stored::Other(_) => Compressor::default(),
        })
    }
}

/// How hard the compressor works, from 0 (gentle) to [`CompressorAmount::MAX`] (heavy). The DSP
/// turns it into a threshold and a ratio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct CompressorAmount(u8);

impl CompressorAmount {
    pub const MAX: u8 = 100;
    /// Where the compressor stood before it had a knob.
    pub const DEFAULT: CompressorAmount = CompressorAmount(50);
    /// The "Strong" choice in Settings.
    pub const STRONG: CompressorAmount = CompressorAmount(80);

    /// `amount`, held to [`CompressorAmount::MAX`].
    pub fn new(amount: u8) -> Self {
        Self(amount.min(Self::MAX))
    }

    pub const fn get(self) -> u8 {
        self.0
    }
}

impl Default for CompressorAmount {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl<'de> Deserialize<'de> for CompressorAmount {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Stored {
            Number(f64),
            Other(serde::de::IgnoredAny),
        }
        Ok(match Stored::deserialize(d)? {
            Stored::Number(n) if n.is_finite() => {
                Self::new(n.round().clamp(0., f64::from(Self::MAX)) as u8)
            }
            _ => Self::DEFAULT,
        })
    }
}

fn lenient_percent<'de, D: Deserializer<'de>>(d: D) -> Result<u16, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Stored {
        Number(f64),
        Other(serde::de::IgnoredAny),
    }
    Ok(match Stored::deserialize(d)? {
        Stored::Number(n) if n.is_finite() => n.round().clamp(0., f64::from(u16::MAX)) as u16,
        _ => UI_SCALE_DEFAULT,
    })
}

fn lenient_quality<'de, D: Deserializer<'de>>(d: D) -> Result<Quality, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Stored {
        Known(Quality),
        Other(serde::de::IgnoredAny),
    }
    Ok(match Stored::deserialize(d)? {
        Stored::Known(quality) => quality,
        Stored::Other(_) => Settings::default().share_quality,
    })
}

/// The text of a stored choice, or `None` for any other kind of value.
fn stored_text<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Stored {
        Text(String),
        Other(serde::de::IgnoredAny),
    }
    Ok(match Stored::deserialize(d)? {
        Stored::Text(text) => Some(text),
        Stored::Other(_) => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Persisted;

    fn load(json: &str) -> Persisted {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn a_file_from_before_settings_loads_with_the_defaults() {
        let state = load(r#"{"lobbies": [], "sidebar_width": 240}"#);
        assert_eq!(state.settings, Settings::default());
        assert!(state.settings.close_to_tray);
        assert!(state.settings.sounds, "sounds are on until turned off");
    }

    #[test]
    fn untouched_settings_are_not_written_and_changed_ones_round_trip() {
        let plain = serde_json::to_string(&Persisted::default()).unwrap();
        assert!(!plain.contains("settings"), "{plain}");

        let state = Persisted {
            settings: Settings {
                close_to_tray: false,
                ..Settings::default()
            },
            ..Persisted::default()
        };
        let json = serde_json::to_string(&state).unwrap();
        assert_eq!(load(&json), state);
    }

    #[test]
    fn a_missing_field_takes_its_default_and_unknown_choices_fall_back() {
        let state = load(r#"{"lobbies": [], "settings": {"close_to_tray": false}}"#);
        assert!(!state.settings.close_to_tray);

        for value in [r#""neon""#, "7", "null", r#"{"x": 1}"#, "[]"] {
            let state = load(&format!(
                r#"{{"lobbies": [], "settings": {{"share_quality": {value}}}}}"#
            ));
            assert_eq!(
                state.settings,
                Settings::default(),
                "share_quality: {value}"
            );
        }
    }

    #[test]
    fn the_choices_of_removed_settings_are_ignored() {
        let state = load(
            r#"{"lobbies": [], "settings": {"theme": "gruvbox", "appearance": "light",
                "last_settings_tab": "voice", "close_to_tray": false}}"#,
        );
        assert!(!state.settings.close_to_tray);
        assert_eq!(
            Settings {
                close_to_tray: true,
                ..state.settings
            },
            Settings::default()
        );
    }

    #[test]
    fn every_choice_round_trips() {
        let state = Persisted {
            settings: Settings {
                close_to_tray: false,
                ui_scale: 120,
                sounds: false,
                input_device: DeviceChoice::Communications,
                output_device: DeviceChoice::Named(crate::NamedDevice {
                    id: "wasapi:{0.0.0.00000000}.{abc}".into(),
                    name: "Headset (Arctis)".into(),
                }),
                member_volumes: Settings::default().member_volumes,
                noise_suppression: NoiseSuppression {
                    on: false,
                    engine: NoiseEngine::Webrtc,
                },
                echo_cancellation: true,
                compressor: Compressor {
                    on: false,
                    amount: CompressorAmount::new(85),
                },
                share_quality: Quality::UpTo1440,
            },
            ..Persisted::default()
        };
        let json = serde_json::to_string(&state).unwrap();
        assert_eq!(load(&json), state, "{json}");
    }

    #[test]
    fn member_volumes_are_kept_per_lobby_and_member_and_100_percent_is_forgotten() {
        let policy = crate::HttpPolicy::LoopbackOnly;
        let one = LobbyOrigin::parse("https://one.example", policy).unwrap();
        let two = LobbyOrigin::parse("https://two.example", policy).unwrap();
        let (ann, bob): (UserId, UserId) = (
            "usr_000000000000000000000001".parse().unwrap(),
            "usr_000000000000000000000002".parse().unwrap(),
        );
        let mut settings = Settings::default();
        settings.set_member_volume(&one, &ann, Volume::new(150));
        settings.set_member_volume(&one, &bob, Volume::new(0));
        assert_eq!(settings.member_volume(&one, &ann).percent(), 150);
        assert_eq!(settings.member_volume(&one, &bob).percent(), 0);
        assert_eq!(settings.member_volume(&two, &ann), Volume::DEFAULT);

        let state = Persisted {
            settings: settings.clone(),
            ..Persisted::default()
        };
        let json = serde_json::to_string(&state).unwrap();
        assert_eq!(load(&json), state, "{json}");

        settings.set_member_volume(&one, &ann, Volume::DEFAULT);
        settings.set_member_volume(&one, &bob, Volume::DEFAULT);
        assert!(settings.is_default(), "a reset leaves nothing behind");

        let state = load(&json.replace("150", "900"));
        assert_eq!(state.settings.member_volume(&one, &ann).percent(), 200);
    }

    #[test]
    fn device_choices_persist_and_an_old_or_odd_file_means_the_system_default() {
        let chosen = Settings {
            input_device: DeviceChoice::Named(crate::NamedDevice {
                id: "mic-1".into(),
                name: "USB Microphone".into(),
            }),
            output_device: DeviceChoice::Communications,
            ..Settings::default()
        };
        let dir = tempfile::tempdir().unwrap();
        let file = crate::StateFile::at(dir.path().join("state.json"));
        let saved = Persisted {
            settings: chosen.clone(),
            ..Persisted::default()
        };
        assert!(file.write(1, &saved).unwrap());
        let loaded = file.load();
        assert!(loaded.problem.is_none());
        assert_eq!(loaded.state.settings, chosen);

        let old = load(r#"{"lobbies": [], "settings": {"close_to_tray": false}}"#);
        assert_eq!(old.settings.input_device, DeviceChoice::SystemDefault);
        assert_eq!(old.settings.output_device, DeviceChoice::SystemDefault);
        for value in [r#""loud""#, "7", "null", r#"{"named": {"id": 1}}"#] {
            let state = load(&format!(
                r#"{{"lobbies": [], "settings": {{"input_device": {value}, "output_device": {value}}}}}"#
            ));
            assert_eq!(state.settings, Settings::default(), "{value}");
        }
    }

    #[test]
    fn the_rack_starts_with_rnnoise_and_the_compressor_in_and_echo_cancellation_out() {
        let old = load(r#"{"lobbies": [], "settings": {"close_to_tray": false}}"#);
        assert_eq!(
            old.settings.noise_suppression,
            NoiseSuppression {
                on: true,
                engine: NoiseEngine::Rnnoise
            }
        );
        assert_eq!(
            old.settings.compressor,
            Compressor {
                on: true,
                amount: CompressorAmount::new(50)
            }
        );
        assert!(!old.settings.echo_cancellation);
        for value in ["3", r#"{"on": "yes"}"#, "[1]", r#""loud""#] {
            let odd = load(&format!(
                r#"{{"lobbies": [], "settings": {{"noise_suppression": {value}, "compressor": {value}}}}}"#
            ));
            assert_eq!(odd.settings, Settings::default(), "{value}");
        }
        for (stored, held) in [("72", 72), ("240", 100), ("-4", 0), (r#""lots""#, 50)] {
            let state = load(&format!(
                r#"{{"lobbies": [], "settings": {{"compressor": {{"amount": {stored}}}}}}}"#
            ));
            assert_eq!(state.settings.compressor.amount.get(), held, "{stored}");
            assert!(
                state.settings.compressor.on,
                "a missing field takes its default"
            );
        }
    }

    #[test]
    fn a_file_that_still_has_studio_mode_loads_and_forgets_it() {
        let old = load(r#"{"lobbies": [], "settings": {"studio_mode": true, "sounds": false}}"#);
        assert!(!old.settings.sounds);
        assert_eq!(
            Settings {
                sounds: true,
                ..old.settings
            },
            Settings::default()
        );
    }

    #[test]
    fn a_file_from_before_the_rack_keeps_its_noise_choice() {
        let noise = |stored: &str| {
            load(&format!(
                r#"{{"lobbies": [], "settings": {{"noise_suppression": "{stored}"}}}}"#
            ))
            .settings
            .noise_suppression
        };
        let rnnoise = NoiseSuppression::default();
        assert_eq!(
            noise("off"),
            NoiseSuppression {
                on: false,
                ..rnnoise
            },
            "taken out, with the default engine to go back in with"
        );
        assert_eq!(
            noise("webrtc"),
            NoiseSuppression {
                on: true,
                engine: NoiseEngine::Webrtc
            }
        );
        assert_eq!(noise("rnnoise"), rnnoise);
    }

    #[test]
    fn the_scale_snaps_to_a_step_inside_the_range() {
        for (stored, shown) in [
            (0, 80),
            (79, 80),
            (80, 80),
            (84, 80),
            (85, 90),
            (100, 100),
            (104, 100),
            (105, 110),
            (149, 150),
            (150, 150),
            (151, 150),
            (u16::MAX, 150),
        ] {
            let mut settings = Settings {
                ui_scale: stored,
                ..Settings::default()
            };
            settings.normalize();
            assert_eq!(settings.ui_scale, shown, "{stored}");
            assert_eq!(snap_ui_scale(shown), shown, "a step stays");
        }
    }

    #[test]
    fn stepping_stops_at_the_ends() {
        assert_eq!(step_ui_scale(100, true), 110);
        assert_eq!(step_ui_scale(100, false), 90);
        assert_eq!(step_ui_scale(150, true), 150);
        assert_eq!(step_ui_scale(80, false), 80);
        assert_eq!(step_ui_scale(0, false), 80, "a stray value lands on a step");
        assert_eq!(step_ui_scale(133, true), 140);
    }

    #[test]
    fn the_scale_is_default_in_an_old_file_and_snapped_on_load() {
        let state = load(r#"{"lobbies": [], "settings": {"close_to_tray": false}}"#);
        assert_eq!(state.settings.ui_scale, 100);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, br#"{"lobbies": [], "settings": {"ui_scale": 173}}"#).unwrap();
        let loaded = crate::StateFile::at(path).load();
        assert!(loaded.problem.is_none());
        assert_eq!(loaded.state.settings.ui_scale, 150);

        for value in [r#""big""#, "null", "[]", r#"{"x": 1}"#, "-3"] {
            let state = load(&format!(
                r#"{{"lobbies": [], "settings": {{"ui_scale": {value}}}}}"#
            ));
            let mut settings = state.settings;
            settings.normalize();
            assert!(
                [100, 80].contains(&settings.ui_scale),
                "{value}: {}",
                settings.ui_scale
            );
        }
    }
}
