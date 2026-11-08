//! The microphone's own volume, the input level Windows' Sound settings show: read, followed as
//! it changes, and set. It is the microphone's, not Lobby's, so it works in and out of a call and
//! every app hears the change. A `lobby-mic-volume` thread holds the endpoint's volume control.
//! It reads no device list of its own: its owner hands it each list it reads
//! ([`MicVolume::follow`]), so it follows a moved default or a replugged headset with them.

use std::sync::{Arc, Mutex, mpsc as std_mpsc};

use super::{DeviceChoice, Endpoints, devices::Target};

/// What the volume control says.
#[derive(Debug, Clone, PartialEq)]
pub enum MicGain {
    /// The control is being opened.
    Reading,
    /// From 0 to 1, as Windows shows it from 0 to 100.
    Level(f32),
    Unavailable(GainUnavailable),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GainUnavailable {
    #[error("No microphone found.")]
    NoDevice,
    #[error("This microphone's volume cannot be changed.")]
    NoControl,
    #[error("The microphone's volume can be changed on Windows only.")]
    Unsupported,
}

/// What the owner asks the thread.
enum Ask {
    /// Hold this endpoint's control; None when no microphone is listed.
    Follow(Option<String>),
    Set(f32),
}

/// While it lives, the chosen microphone's volume is followed. Dropping it stops its thread.
pub struct MicVolume {
    gain: Arc<Mutex<MicGain>>,
    /// None for a fake, which holds the level it is set to and touches no device.
    asks: Option<std_mpsc::Sender<Ask>>,
}

impl MicVolume {
    /// The volume of fake media's microphone, which is no device.
    pub fn fake(level: f32) -> MicVolume {
        MicVolume {
            gain: Arc::new(Mutex::new(MicGain::Level(level.clamp(0.0, 1.0)))),
            asks: None,
        }
    }

    /// Reads [`MicGain::Reading`] until the first [`MicVolume::follow`] names a microphone.
    pub fn start() -> MicVolume {
        let gain = Arc::new(Mutex::new(MicGain::Reading));
        let (asks, asked) = std_mpsc::channel();
        #[cfg(windows)]
        {
            let shared = gain.clone();
            let started = std::thread::Builder::new()
                .name("lobby-mic-volume".into())
                .spawn(move || os::follow(&shared, &asked));
            if started.is_err() {
                *lock(&gain) = MicGain::Unavailable(GainUnavailable::NoControl);
            }
        }
        #[cfg(not(windows))]
        {
            let _ = asked;
            *lock(&gain) = MicGain::Unavailable(GainUnavailable::Unsupported);
        }
        MicVolume {
            gain,
            asks: Some(asks),
        }
    }

    /// The microphone `choice` names in `inputs`, a list just read. The thread opens its control
    /// when that is another endpoint than the one it holds.
    pub fn follow(&self, choice: &DeviceChoice, inputs: &Endpoints) {
        if let Some(asks) = &self.asks {
            let _ = asks.send(Ask::Follow(endpoint_id(choice, inputs)));
        }
    }

    pub fn read(&self) -> MicGain {
        lock(&self.gain).clone()
    }

    /// From 0 to 1. Ignored while the volume is not [`MicGain::Level`].
    pub fn set(&self, level: f32) {
        let level = level.clamp(0.0, 1.0);
        match &self.asks {
            Some(asks) => {
                let _ = asks.send(Ask::Set(level));
            }
            None => *lock(&self.gain) = MicGain::Level(level),
        }
    }
}

/// The IMMDevice id of the microphone `choice` names: cpal's id without its host.
fn endpoint_id(choice: &DeviceChoice, inputs: &Endpoints) -> Option<String> {
    let id = match inputs.resolve(choice).target {
        Target::Default => inputs.default.clone()?,
        Target::Id(id) => id,
    };
    let id = id.parse::<cpal::DeviceId>().ok()?;
    Some(id.id().to_owned())
}

fn lock(gain: &Mutex<MicGain>) -> std::sync::MutexGuard<'_, MicGain> {
    gain.lock().expect("mic volume lock")
}

#[cfg(windows)]
mod os {
    use std::sync::{Arc, Mutex, mpsc as std_mpsc};

    use tracing::debug;

    use super::{Ask, GainUnavailable, MicGain, lock};
    use crate::media::win::EndpointVolume;

    /// Until the `MicVolume` is dropped: the control of the endpoint last asked for, opened again
    /// when another is asked for, and the levels asked for set on it.
    pub(super) fn follow(gain: &Arc<Mutex<MicGain>>, asked: &std_mpsc::Receiver<Ask>) {
        let mut held: Option<Held> = None;
        while let Ok(first) = asked.recv() {
            // A slider that is moving asks faster than this thread sets: only the latest level
            // counts, set on the microphone that was held when it was asked.
            let mut level = None;
            for ask in std::iter::once(first).chain(asked.try_iter()) {
                match ask {
                    Ask::Set(newer) => level = Some(newer),
                    Ask::Follow(id) => {
                        if let (Some(held), Some(level)) = (&held, level.take()) {
                            held.set(level, gain);
                        }
                        if held.as_ref().is_none_or(|held| held.id != id) {
                            // The old control stops writing the level before the next opens.
                            drop(held.take());
                            held = Some(Held::open(id, gain));
                        }
                    }
                }
            }
            if let (Some(held), Some(level)) = (&held, level) {
                held.set(level, gain);
            }
        }
    }

    /// The endpoint asked for last (None: no microphone is listed), and its control if it opened.
    struct Held {
        id: Option<String>,
        control: Option<EndpointVolume>,
    }

    impl Held {
        fn open(id: Option<String>, gain: &Arc<Mutex<MicGain>>) -> Held {
            let (control, shown) = match id.as_deref().map(|id| opened(id, gain)) {
                None => (None, MicGain::Unavailable(GainUnavailable::NoDevice)),
                Some(None) => (None, MicGain::Unavailable(GainUnavailable::NoControl)),
                Some(Some((control, level))) => (Some(control), MicGain::Level(level)),
            };
            *lock(gain) = shown;
            Held { id, control }
        }

        fn set(&self, level: f32, gain: &Arc<Mutex<MicGain>>) {
            if let Some(control) = &self.control
                && control.set(level).is_ok()
                && let Ok(level) = control.level()
            {
                *lock(gain) = MicGain::Level(level);
            }
        }
    }

    /// The control and its level now, its changes written to `gain` as they come.
    fn opened(id: &str, gain: &Arc<Mutex<MicGain>>) -> Option<(EndpointVolume, f32)> {
        let heard = gain.clone();
        let volume = EndpointVolume::open(id, move |level, ours| {
            debug!(level, ours, "microphone volume changed");
            *lock(&heard) = MicGain::Level(level);
        })
        .ok()?;
        let level = volume.level().ok()?;
        Some((volume, level))
    }
}
