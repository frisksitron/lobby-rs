//! UI sounds: join, leave, mute, unmute, deafen and undeafen, each a pair of soft sine blips,
//! rising for on and join, falling for off and leave. They are synthesized here, so there are no
//! assets and no licences.
//!
//! The player has its own small output path, apart from Voice's: the leave sound plays after
//! Voice's stream is gone. A `lobby-sounds` thread opens the speakers the Settings chose when a
//! sound is due (the same choice Voice's output follows) and closes them again after
//! [`OUTPUT_IDLE`] of silence, so the device is not held between sounds and a changed default or
//! choice is picked up by the next one.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc as std_mpsc,
    },
    time::Duration,
};

use tracing::{debug, warn};

use super::{
    DeviceChoice, DeviceKind, DeviceSetup, MediaError, SAMPLE_RATE, SystemDevices, audio,
    devices::Target,
};

/// The loudest sample of every sound.
pub const SOUND_GAIN: f32 = 0.3;
/// How long one blip lasts, from its start to silence.
pub const BLIP: Duration = Duration::from_millis(80);
/// How long after the first blip starts the second does. Less than [`BLIP`]: they overlap softly.
pub const BLIP_STEP: Duration = Duration::from_millis(65);
/// The fade-in of a blip; the rest of it is a smooth release.
pub const BLIP_ATTACK: Duration = Duration::from_millis(8);
/// The two notes of each sound, in hertz, in the order they play.
pub const JOIN_NOTES: [f32; 2] = [523.25, 783.99];
pub const LEAVE_NOTES: [f32; 2] = [783.99, 523.25];
pub const UNMUTE_NOTES: [f32; 2] = [659.25, 880.0];
pub const MUTE_NOTES: [f32; 2] = [880.0, 659.25];
pub const UNDEAFEN_NOTES: [f32; 2] = [392.0, 587.33];
pub const DEAFEN_NOTES: [f32; 2] = [587.33, 392.0];

/// How long the output stays open with nothing playing.
const OUTPUT_IDLE: Duration = Duration::from_secs(2);
/// Sounds that can play at once; a further one is dropped.
const MAX_VOICES: usize = 4;
const START_QUEUE: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Sound {
    Join,
    Leave,
    Mute,
    Unmute,
    Deafen,
    Undeafen,
}

impl Sound {
    pub const ALL: [Sound; 6] = [
        Sound::Join,
        Sound::Leave,
        Sound::Mute,
        Sound::Unmute,
        Sound::Deafen,
        Sound::Undeafen,
    ];

    fn notes(self) -> [f32; 2] {
        match self {
            Sound::Join => JOIN_NOTES,
            Sound::Leave => LEAVE_NOTES,
            Sound::Mute => MUTE_NOTES,
            Sound::Unmute => UNMUTE_NOTES,
            Sound::Deafen => DEAFEN_NOTES,
            Sound::Undeafen => UNDEAFEN_NOTES,
        }
    }
}

/// Every sound, rendered once as mono 48 kHz samples.
#[derive(Debug)]
pub struct SoundBank {
    buffers: [Vec<f32>; Sound::ALL.len()],
}

impl SoundBank {
    pub fn render() -> SoundBank {
        SoundBank {
            buffers: Sound::ALL.map(render_sound),
        }
    }

    pub fn samples(&self, sound: Sound) -> &[f32] {
        &self.buffers[sound as usize]
    }
}

fn samples_of(d: Duration) -> usize {
    (d.as_secs_f64() * f64::from(SAMPLE_RATE)).round() as usize
}

/// A blip's level at `t` samples in: a linear rise, then half a cosine down to zero at the end,
/// so neither edge clicks.
fn envelope(t: usize, len: usize, attack: usize) -> f32 {
    if t < attack {
        t as f32 / attack as f32
    } else {
        let released = (t - attack) as f32 / (len - attack) as f32;
        0.5 * (1.0 + (std::f32::consts::PI * released).cos())
    }
}

fn render_sound(sound: Sound) -> Vec<f32> {
    let (len, step, attack) = (
        samples_of(BLIP),
        samples_of(BLIP_STEP),
        samples_of(BLIP_ATTACK),
    );
    let mut out = vec![0f32; step + len];
    for (n, hz) in sound.notes().into_iter().enumerate() {
        let turn = std::f64::consts::TAU * f64::from(hz) / f64::from(SAMPLE_RATE);
        for t in 0..len {
            out[n * step + t] += envelope(t, len, attack) * (turn * t as f64).sin() as f32;
        }
    }
    let peak = out.iter().fold(0f32, |peak, s| peak.max(s.abs()));
    for s in &mut out {
        *s *= SOUND_GAIN / peak;
    }
    out
}

/// The sounds playing now, summed into the output one sample at a time.
struct Voices {
    playing: [Option<(Sound, usize)>; MAX_VOICES],
}

impl Voices {
    fn new() -> Voices {
        Voices {
            playing: [None; MAX_VOICES],
        }
    }

    fn start(&mut self, sound: Sound) {
        if let Some(slot) = self.playing.iter_mut().find(|s| s.is_none()) {
            *slot = Some((sound, 0));
        }
    }

    fn next(&mut self, bank: &SoundBank) -> f32 {
        let mut sum = 0.0;
        for slot in &mut self.playing {
            let Some((sound, at)) = slot else {
                continue;
            };
            match bank.samples(*sound).get(*at).copied() {
                Some(s) => {
                    sum += s;
                    *at += 1;
                }
                None => *slot = None,
            }
        }
        sum.clamp(-1.0, 1.0)
    }
}

/// Plays UI sounds. Dropping it stops the thread and closes the output.
#[derive(Debug)]
pub struct SoundPlayer {
    tx: std_mpsc::Sender<Sound>,
    /// The speakers the next sound goes to.
    output: Arc<Mutex<DeviceSetup>>,
}

impl SoundPlayer {
    /// A player with its sounds going to the speakers `play` is given.
    pub fn start() -> Result<SoundPlayer, MediaError> {
        let (player, rx) = SoundPlayer::channel();
        let bank = Arc::new(SoundBank::render());
        let output = player.output.clone();
        std::thread::Builder::new()
            .name("lobby-sounds".into())
            .spawn(move || run(&rx, &bank, &output))
            .map_err(|e| MediaError::Device(e.to_string()))?;
        Ok(player)
    }

    /// A player that opens no device: what it is asked to play arrives on the receiver. For tests.
    pub fn channel() -> (SoundPlayer, std_mpsc::Receiver<Sound>) {
        let (tx, rx) = std_mpsc::channel();
        let output = DeviceSetup::new(DeviceChoice::SystemDefault, Arc::new(SystemDevices));
        (
            SoundPlayer {
                tx,
                output: Arc::new(Mutex::new(output)),
            },
            rx,
        )
    }

    pub fn play(&self, sound: Sound, output: &DeviceSetup) {
        {
            let mut current = self.output.lock().expect("sound output lock");
            if current.choice != output.choice {
                *current = output.clone();
            }
        }
        let _ = self.tx.send(sound);
    }
}

/// What `output` opens now: the default unless the person chose another device that is listed.
fn target_of(output: &DeviceSetup) -> Target {
    match output.choice {
        DeviceChoice::SystemDefault => Target::Default,
        _ => output.source.list().outputs.resolve(&output.choice).target,
    }
}

fn run(rx: &std_mpsc::Receiver<Sound>, bank: &Arc<SoundBank>, setup: &Mutex<DeviceSetup>) {
    let mut output: Option<Output> = None;
    loop {
        let sound = if output.is_some() {
            match rx.recv_timeout(OUTPUT_IDLE) {
                Ok(sound) => sound,
                Err(std_mpsc::RecvTimeoutError::Timeout) => {
                    output = None;
                    continue;
                }
                Err(std_mpsc::RecvTimeoutError::Disconnected) => return,
            }
        } else {
            let Ok(sound) = rx.recv() else { return };
            sound
        };
        let wanted = target_of(&setup.lock().expect("sound output lock"));
        if output
            .as_ref()
            .is_some_and(|o| o.broken() || o.target != wanted)
        {
            output = None;
        }
        if output.is_none() {
            output = Output::open(bank, wanted)
                .inspect_err(|e| warn!(error = %e, "UI sounds have no output"))
                .ok();
        }
        if let Some(output) = &mut output {
            output.start(sound);
        }
    }
}

struct Output {
    target: Target,
    _stream: cpal::Stream,
    starts: rtrb::Producer<Sound>,
    broken: Arc<AtomicBool>,
}

impl Output {
    fn open(bank: &Arc<SoundBank>, target: Target) -> Result<Output, MediaError> {
        let opened = Self::open_on(bank, target.clone());
        match target {
            Target::Id(_) => opened.or_else(|_| Self::open_on(bank, Target::Default)),
            Target::Default => opened,
        }
    }

    fn open_on(bank: &Arc<SoundBank>, target: Target) -> Result<Output, MediaError> {
        let host = cpal::default_host();
        let device = audio::device(&host, DeviceKind::Output, &target)?;
        let config = audio::output_config(&device)?;
        let (starts, mut queued) = rtrb::RingBuffer::new(START_QUEUE);
        let broken = Arc::new(AtomicBool::new(false));
        let (bank, mut voices) = (bank.clone(), Voices::new());
        let stream = audio::mono_output(
            &device,
            config,
            {
                let broken = broken.clone();
                move |e| {
                    warn!(error = %e, "UI sound stream error");
                    broken.store(true, Ordering::Relaxed);
                }
            },
            move |mono| {
                while let Ok(sound) = queued.pop() {
                    voices.start(sound);
                }
                for s in mono {
                    *s = voices.next(&bank);
                }
            },
        )?;
        debug!("UI sound output open");
        Ok(Output {
            target,
            _stream: stream,
            starts,
            broken,
        })
    }

    fn broken(&self) -> bool {
        self.broken.load(Ordering::Relaxed)
    }

    fn start(&mut self, sound: Sound) {
        let _ = self.starts.push(sound);
    }
}
