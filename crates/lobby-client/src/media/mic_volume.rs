//! The microphone's own volume, the input level Windows' Sound settings show: read, followed as
//! it changes, and set. It is the microphone's, not Lobby's, so it works in and out of a call and
//! every app hears the change. A `lobby-mic-volume` thread holds the endpoint's volume control and
//! finds the chosen microphone again every [`DEVICE_POLL`], as a call does, so it follows a
//! moved default or a replugged headset.

use std::sync::{Arc, Mutex, mpsc as std_mpsc};

use super::DeviceSetup;

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

/// While it lives, the chosen microphone's volume is followed. Dropping it stops its thread.
pub struct MicVolume {
    gain: Arc<Mutex<MicGain>>,
    /// None for a fake, which holds the level it is set to and touches no device.
    sets: Option<std_mpsc::Sender<f32>>,
}

impl MicVolume {
    /// The volume of fake media's microphone, which is no device.
    pub fn fake(level: f32) -> MicVolume {
        MicVolume {
            gain: Arc::new(Mutex::new(MicGain::Level(level.clamp(0.0, 1.0)))),
            sets: None,
        }
    }

    pub fn start(setup: DeviceSetup) -> MicVolume {
        let gain = Arc::new(Mutex::new(MicGain::Reading));
        let (sets, wanted) = std_mpsc::channel();
        #[cfg(windows)]
        {
            let shared = gain.clone();
            let started = std::thread::Builder::new()
                .name("lobby-mic-volume".into())
                .spawn(move || os::follow(&setup, &shared, &wanted));
            if started.is_err() {
                *lock(&gain) = MicGain::Unavailable(GainUnavailable::NoControl);
            }
        }
        #[cfg(not(windows))]
        {
            let _ = (setup, wanted);
            *lock(&gain) = MicGain::Unavailable(GainUnavailable::Unsupported);
        }
        MicVolume {
            gain,
            sets: Some(sets),
        }
    }

    pub fn read(&self) -> MicGain {
        lock(&self.gain).clone()
    }

    /// From 0 to 1. Ignored while the volume is not [`MicGain::Level`].
    pub fn set(&self, level: f32) {
        let level = level.clamp(0.0, 1.0);
        match &self.sets {
            Some(sets) => {
                let _ = sets.send(level);
            }
            None => *lock(&self.gain) = MicGain::Level(level),
        }
    }
}

fn lock(gain: &Mutex<MicGain>) -> std::sync::MutexGuard<'_, MicGain> {
    gain.lock().expect("mic volume lock")
}

#[cfg(windows)]
mod os {
    use std::{
        sync::{Arc, Mutex, mpsc as std_mpsc},
        time::Instant,
    };

    use tracing::debug;

    use super::{GainUnavailable, MicGain, lock};
    use crate::media::{DEVICE_POLL, DeviceSetup, devices::Target, win::EndpointVolume};

    /// Until the `MicVolume` is dropped: the chosen microphone's control, opened again when the
    /// choice resolves to another endpoint, and the levels asked for set on it.
    pub(super) fn follow(
        setup: &DeviceSetup,
        gain: &Arc<Mutex<MicGain>>,
        wanted: &std_mpsc::Receiver<f32>,
    ) {
        let mut open: Option<(String, EndpointVolume)> = None;
        let mut polled = None::<Instant>;
        loop {
            if polled.is_none_or(|at| at.elapsed() >= DEVICE_POLL) {
                polled = Some(Instant::now());
                let id = endpoint_id(setup);
                if open.as_ref().map(|(open, _)| open) != id.as_ref() {
                    open = None;
                    *lock(gain) = match id {
                        None => MicGain::Unavailable(GainUnavailable::NoDevice),
                        Some(id) => match opened(&id, gain) {
                            Some((volume, level)) => {
                                open = Some((id, volume));
                                MicGain::Level(level)
                            }
                            None => MicGain::Unavailable(GainUnavailable::NoControl),
                        },
                    };
                }
            }
            let wait = DEVICE_POLL.saturating_sub(polled.map_or(DEVICE_POLL, |at| at.elapsed()));
            let mut level = match wanted.recv_timeout(wait) {
                Ok(level) => level,
                Err(std_mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std_mpsc::RecvTimeoutError::Disconnected) => return,
            };
            // A knob that is turning asks faster than this thread sets: only the latest counts.
            while let Ok(newer) = wanted.try_recv() {
                level = newer;
            }
            if let Some((_, volume)) = &open
                && volume.set(level).is_ok()
                && let Ok(level) = volume.level()
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

    /// The IMMDevice id of the microphone the choice names now: cpal's id without its host.
    fn endpoint_id(setup: &DeviceSetup) -> Option<String> {
        let list = setup.source.list();
        let inputs = &list.inputs;
        let id = match inputs.resolve(&setup.choice).target {
            Target::Default => inputs.default.clone()?,
            Target::Id(id) => id,
        };
        let id = id.parse::<cpal::DeviceId>().ok()?;
        Some(id.id().to_owned())
    }
}
