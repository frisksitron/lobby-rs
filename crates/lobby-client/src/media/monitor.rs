//! Monitor: the person hears their own voice as the call sends it, or with no call on, as one
//! would ([`MicTest::monitored`]). The encoder thread decodes each packet it has just encoded and
//! hands the 20 ms block to the playout, which adds it to the speakers' mix after the echo
//! reference is taken, so the echo canceller never removes the person's own voice. Muted sends
//! nothing, so the monitor is silent; Deafen silences it with the rest of the mix.
//!
//! [`MicTest::monitored`]: super::MicTest::monitored

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU32, Ordering},
};

use super::{
    FRAME, MediaError, mixer,
    opus_codec::{DTX_PACKET, Pcm, VoiceDecoder},
};

/// Blocks between the encoder thread and the playout. Their clocks drift apart, so the playout
/// lets no more than [`MONITOR_QUEUE`] wait.
const MONITOR_RING: usize = 4;
const MONITOR_QUEUE: usize = 2;

/// Whether the person hears themselves and how loud: the desktop writes it, the encoder thread
/// and the output callback read it. Off at full level to begin with, and never saved.
#[derive(Clone)]
pub struct MonitorControl(Arc<Shared>);

struct Shared {
    on: AtomicBool,
    /// `f32` bits.
    level: AtomicU32,
}

impl Default for MonitorControl {
    fn default() -> Self {
        Self(Arc::new(Shared {
            on: AtomicBool::new(false),
            level: AtomicU32::new(1.0f32.to_bits()),
        }))
    }
}

impl std::fmt::Debug for MonitorControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "MonitorControl(on: {}, level: {})",
            self.is_on(),
            self.level()
        )
    }
}

impl MonitorControl {
    pub fn set_on(&self, on: bool) {
        self.0.on.store(on, Ordering::Relaxed);
    }

    /// From 0 to 1, as a gain on the decoded voice.
    pub fn set_level(&self, level: f32) {
        self.0
            .level
            .store(level.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    pub fn is_on(&self) -> bool {
        self.0.on.load(Ordering::Relaxed)
    }

    pub fn level(&self) -> f32 {
        f32::from_bits(self.0.level.load(Ordering::Relaxed))
    }
}

/// The encoder thread's side: decodes what was just encoded.
pub(crate) struct MonitorTap {
    control: MonitorControl,
    decoder: VoiceDecoder,
    /// Whether the last frame was monitored: a decoder that sat out frames starts over.
    was_on: bool,
    pcm: Pcm,
    ring: rtrb::Producer<[f32; FRAME]>,
}

impl MonitorTap {
    /// `packet` is what the encoder made of the frame: `None` if it failed. A DTX packet is not
    /// sent, so the decoder fills the frame as a receiver would.
    pub fn frame(&mut self, packet: Option<&[u8]>) {
        let on = self.control.is_on();
        if !on {
            self.was_on = false;
            return;
        }
        if !self.was_on {
            self.decoder.reset();
            self.was_on = true;
        }
        match packet {
            Some(p) if p.len() > DTX_PACKET => {
                self.decoder.decode(p, &mut self.pcm);
            }
            Some(_) => self.decoder.conceal(&mut self.pcm),
            None => return,
        }
        let mut block = [0.0; FRAME];
        block.copy_from_slice(&self.pcm[..FRAME]);
        let _ = self.ring.push(block);
    }
}

/// The playout's side: adds the person's voice to the mix.
pub(crate) struct MonitorMix {
    control: MonitorControl,
    ring: rtrb::Consumer<[f32; FRAME]>,
    /// The gain the last block ended on; the next slides from it.
    gain: f32,
}

impl MonitorMix {
    /// After the echo reference took `mix`: the next block of the person's voice at the
    /// monitor's level times `master`, then the mix kept in full scale again.
    pub fn add(&mut self, mix: &mut [f32; FRAME], master: f32) {
        while self.ring.slots() > MONITOR_QUEUE {
            let _ = self.ring.pop();
        }
        let Ok(block) = self.ring.pop() else {
            self.gain = 0.0;
            return;
        };
        let to = if self.control.is_on() {
            self.control.level() * master
        } else {
            0.0
        };
        mixer::add_gain(mix, &block, self.gain, to);
        self.gain = to;
        mixer::clamp(mix);
    }
}

/// Both ends of one monitor.
pub(crate) fn monitor(control: MonitorControl) -> Result<(MonitorTap, MonitorMix), MediaError> {
    let (tx, rx) = rtrb::RingBuffer::new(MONITOR_RING);
    Ok((
        MonitorTap {
            control: control.clone(),
            decoder: VoiceDecoder::mono()?,
            was_on: false,
            pcm: [0.0; FRAME * 2],
            ring: tx,
        },
        MonitorMix {
            control,
            ring: rx,
            gain: 0.0,
        },
    ))
}
