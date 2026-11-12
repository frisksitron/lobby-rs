//! Which audio devices Voice uses: what a person chose, what the OS lists, and how a running
//! stream follows both. The pure parts (choosing a device from a list, deciding when a stream must
//! move) are here so they run against a fake list; the cpal streams that act on them are in
//! `audio`.
//!
//! The OS lists change without notice (a headset is plugged in, the default moves). Windows can
//! say so through `IMMNotificationClient`, but that is COM callbacks and `unsafe` for a list that
//! changes once in a while, so [`DeviceWatch`] and the Voice call poll every [`DEVICE_POLL`].

use std::{
    sync::{Arc, Mutex, mpsc as std_mpsc},
    time::Duration,
};

use cpal::traits::{DeviceTrait as _, HostTrait as _};
use serde::{Deserialize, Deserializer, Serialize};

use super::MediaError;

/// How often the lists are read again, in a call and in Settings.
pub const DEVICE_POLL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeviceKind {
    Input,
    Output,
}

/// A device by the id the OS gives it, which survives a restart and a replug, and the name it had
/// when it was chosen, so a choice that is unplugged still has a name to show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedDevice {
    pub id: String,
    pub name: String,
}

/// What a person picked for the microphone or for the speakers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceChoice {
    /// Whatever the OS uses for everything, and follows it when that changes.
    #[default]
    SystemDefault,
    /// What the OS uses for calls (Windows keeps a second default for it).
    Communications,
    Named(NamedDevice),
}

impl<'de> Deserialize<'de> for DeviceChoice {
    /// A file from another version, or one edited by hand, loads as the system default.
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Stored {
            Text(String),
            Named { named: NamedDevice },
            Other(serde::de::IgnoredAny),
        }
        Ok(match Stored::deserialize(d)? {
            Stored::Text(t) if t == "communications" => DeviceChoice::Communications,
            Stored::Named { named } => DeviceChoice::Named(named),
            Stored::Text(_) | Stored::Other(_) => DeviceChoice::SystemDefault,
        })
    }
}

/// The devices of one direction, as the OS lists them now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Endpoints {
    pub devices: Vec<NamedDevice>,
    /// The id of the system default, if there is one.
    pub default: Option<String>,
    /// The id of the communications default, where the OS has one.
    pub communications: Option<String>,
}

impl Endpoints {
    pub fn get(&self, id: &str) -> Option<&NamedDevice> {
        self.devices.iter().find(|d| d.id == id)
    }

    pub fn has_communications(&self) -> bool {
        self.communications.is_some()
    }

    /// What `choice` opens now, and the named device it could not find, if any.
    pub(crate) fn resolve(&self, choice: &DeviceChoice) -> Resolved {
        match choice {
            DeviceChoice::SystemDefault => Resolved::to(Target::Default),
            DeviceChoice::Communications => Resolved::to(
                self.communications
                    .as_deref()
                    .filter(|id| self.get(id).is_some())
                    .map_or(Target::Default, |id| Target::Id(id.to_owned())),
            ),
            DeviceChoice::Named(wanted) => match self.get(&wanted.id) {
                Some(device) => Resolved::to(Target::Id(device.id.clone())),
                None => Resolved {
                    target: Target::Default,
                    fell_back: Some(wanted.clone()),
                },
            },
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceList {
    pub inputs: Endpoints,
    pub outputs: Endpoints,
}

impl DeviceList {
    pub fn of(&self, kind: DeviceKind) -> &Endpoints {
        match kind {
            DeviceKind::Input => &self.inputs,
            DeviceKind::Output => &self.outputs,
        }
    }
}

/// Where the lists come from: the OS in the app, a fixed list in tests.
pub trait DeviceSource: Send + Sync + 'static {
    fn list(&self) -> DeviceList;
}

/// The devices cpal sees, and on Windows the communications defaults.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemDevices;

impl DeviceSource for SystemDevices {
    fn list(&self) -> DeviceList {
        let host = cpal::default_host();
        DeviceList {
            inputs: endpoints(&host, DeviceKind::Input),
            outputs: endpoints(&host, DeviceKind::Output),
        }
    }
}

fn endpoints(host: &cpal::Host, kind: DeviceKind) -> Endpoints {
    let listed = match kind {
        DeviceKind::Input => host.input_devices().map(|d| d.collect::<Vec<_>>()),
        DeviceKind::Output => host.output_devices().map(|d| d.collect::<Vec<_>>()),
    };
    let devices = listed
        .unwrap_or_default()
        .iter()
        .filter_map(|device| {
            let id = device.id().ok()?.to_string();
            let name = device
                .description()
                .map_or_else(|_| id.clone(), |d| d.name().to_owned());
            Some(NamedDevice { id, name })
        })
        .collect();
    let default = match kind {
        DeviceKind::Input => host.default_input_device(),
        DeviceKind::Output => host.default_output_device(),
    }
    .and_then(|d| d.id().ok())
    .map(|id| id.to_string());
    Endpoints {
        devices,
        default,
        communications: communications_default(kind),
    }
}

#[cfg(windows)]
fn communications_default(kind: DeviceKind) -> Option<String> {
    let id = super::win::communications_endpoint(kind == DeviceKind::Input)?;
    Some(cpal::DeviceId::new(cpal::HostId::Wasapi, id).to_string())
}

/// A list that tests and `LOBBY_FAKE_MEDIA=1` change by hand.
#[derive(Debug, Default)]
pub struct FakeDevices {
    list: Mutex<DeviceList>,
}

impl FakeDevices {
    pub fn new(list: DeviceList) -> Self {
        Self {
            list: Mutex::new(list),
        }
    }

    /// A built-in microphone and a headset, speakers and the same headset, with the built-in
    /// pair as the system default.
    pub fn sample() -> DeviceList {
        let device = |id: &str, name: &str| NamedDevice {
            id: id.into(),
            name: name.into(),
        };
        let endpoints = |devices: Vec<NamedDevice>| Endpoints {
            default: Some(devices[0].id.clone()),
            communications: Some(devices[1].id.clone()),
            devices,
        };
        DeviceList {
            inputs: endpoints(vec![
                device("fake:mic", "Built-in Microphone"),
                device("fake:headset-mic", "Headset Microphone"),
            ]),
            outputs: endpoints(vec![
                device("fake:speakers", "Built-in Speakers"),
                device("fake:headset", "Headset Earphones"),
            ]),
        }
    }

    pub fn set(&self, list: DeviceList) {
        *self.list.lock().expect("fake device list lock") = list;
    }
}

impl DeviceSource for FakeDevices {
    fn list(&self) -> DeviceList {
        self.list.lock().expect("fake device list lock").clone()
    }
}

/// Which device to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Target {
    /// The OS default, which the stream follows.
    Default,
    Id(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Resolved {
    pub target: Target,
    /// The named device that is not listed, which `target` stands in for.
    pub fell_back: Option<NamedDevice>,
}

impl Resolved {
    fn to(target: Target) -> Self {
        Self {
            target,
            fell_back: None,
        }
    }
}

/// Why a running stream stopped being right.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Fault {
    /// The default it follows moved to another device.
    Moved = 1,
    /// Its device was unplugged or disabled.
    Gone,
    Busy,
    Denied,
    Other,
}

impl Fault {
    pub fn from_kind(kind: cpal::ErrorKind) -> Option<Fault> {
        use cpal::ErrorKind as K;
        match kind {
            K::DeviceChanged | K::StreamInvalidated => Some(Fault::Moved),
            K::DeviceNotAvailable => Some(Fault::Gone),
            K::DeviceBusy => Some(Fault::Busy),
            K::PermissionDenied => Some(Fault::Denied),
            // A glitch in the audio, not a stream that is gone.
            K::Xrun | K::RealtimeDenied => None,
            _ => Some(Fault::Other),
        }
    }

    pub fn from_code(code: u8) -> Option<Fault> {
        [
            Fault::Moved,
            Fault::Gone,
            Fault::Busy,
            Fault::Denied,
            Fault::Other,
        ]
        .into_iter()
        .find(|f| *f as u8 == code)
    }

    /// What a fault that moving to another device cannot cure ends the call with.
    pub fn error(self) -> MediaError {
        match self {
            Fault::Moved | Fault::Gone => MediaError::DeviceNotFound,
            Fault::Busy => MediaError::DeviceBusy,
            Fault::Denied => MediaError::PermissionDenied,
            Fault::Other => MediaError::Device("the audio stream stopped".into()),
        }
    }
}

/// What a stream should do about a change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Change {
    Stay,
    /// Close the stream and open it on [`Follower::target`]. `fell_back` is a named device the
    /// stream just left because it is gone.
    Reopen {
        fell_back: Option<NamedDevice>,
    },
    /// The fault is not one that another device cures.
    Fail(MediaError),
}

/// Tracks one stream against what the person chose and what the OS lists.
#[derive(Debug, Clone)]
pub(crate) struct Follower {
    choice: DeviceChoice,
    running: Target,
}

impl Follower {
    /// For a stream about to open.
    pub fn start(choice: DeviceChoice, endpoints: &Endpoints) -> (Follower, Option<NamedDevice>) {
        let Resolved { target, fell_back } = endpoints.resolve(&choice);
        (
            Follower {
                choice,
                running: target,
            },
            fell_back,
        )
    }

    pub fn target(&self) -> &Target {
        &self.running
    }

    /// The stream could not open on its target and opens on the default instead.
    pub fn use_default(&mut self) -> Option<NamedDevice> {
        self.running = Target::Default;
        match &self.choice {
            DeviceChoice::Named(wanted) => Some(wanted.clone()),
            _ => None,
        }
    }

    /// The person chose another device for the running stream.
    pub fn choose(&mut self, choice: DeviceChoice, endpoints: &Endpoints) -> Change {
        self.choice = choice;
        self.observe(endpoints)
    }

    /// The list was read again.
    pub fn observe(&mut self, endpoints: &Endpoints) -> Change {
        let Resolved { target, fell_back } = endpoints.resolve(&self.choice);
        if target == self.running {
            return Change::Stay;
        }
        self.running = target;
        Change::Reopen { fell_back }
    }

    /// The stream reported `fault`. `endpoints` is a fresh read.
    pub fn fault(&mut self, fault: Fault, endpoints: &Endpoints) -> Change {
        match fault {
            Fault::Moved => {
                let Resolved { target, fell_back } = endpoints.resolve(&self.choice);
                self.running = target;
                Change::Reopen { fell_back }
            }
            Fault::Gone => match (&self.choice, &self.running) {
                // The list can still show a device the stream already lost.
                (DeviceChoice::Named(wanted), Target::Id(_)) => {
                    let wanted = wanted.clone();
                    self.running = Target::Default;
                    Change::Reopen {
                        fell_back: Some(wanted),
                    }
                }
                _ => {
                    let Resolved { target, fell_back } = endpoints.resolve(&self.choice);
                    self.running = target;
                    Change::Reopen { fell_back }
                }
            },
            Fault::Busy | Fault::Denied | Fault::Other => Change::Fail(fault.error()),
        }
    }
}

/// While it lives, the lists are read every [`DEVICE_POLL`] on a thread of its own. Dropping it
/// stops the thread.
///
/// The reader takes a new list when it looks ([`DeviceWatch::take`]); the thread never wakes it.
/// A GPUI test executor fails a test that another thread wakes, and the Settings page already
/// redraws its meter on a timer.
pub struct DeviceWatch {
    fresh: Arc<Mutex<Option<DeviceList>>>,
    _stop: std_mpsc::Sender<()>,
}

impl DeviceWatch {
    pub fn start(source: Arc<dyn DeviceSource>, every: Duration) -> DeviceWatch {
        let fresh = Arc::new(Mutex::new(None));
        let (stop, stopped) = std_mpsc::channel::<()>();
        let slot = fresh.clone();
        let _ = std::thread::Builder::new()
            .name("lobby-devices".into())
            .spawn(move || {
                let mut shown = None;
                loop {
                    let latest = source.list();
                    if shown.as_ref() != Some(&latest) {
                        *slot.lock().expect("device list lock") = Some(latest.clone());
                        shown = Some(latest);
                    }
                    // Dropping the watch closes the channel, which ends the wait at once.
                    if !matches!(
                        stopped.recv_timeout(every),
                        Err(std_mpsc::RecvTimeoutError::Timeout)
                    ) {
                        return;
                    }
                }
            });
        DeviceWatch { fresh, _stop: stop }
    }

    /// The lists, if they were read for the first time or changed since the last `take`.
    pub fn take(&self) -> Option<DeviceList> {
        self.fresh.lock().expect("device list lock").take()
    }
}
