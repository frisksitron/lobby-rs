//! Playout: the remote slots, each with its own packet ring, jitter buffer
//! and Opus decoder, all created when Voice starts, and the mix of their 20 ms blocks. It runs
//! inside the audio output callback (or the clocked thread of a non-device sink), so `render`
//! never allocates, locks or logs. Slots are bound and unbound through a command ring; a
//! decoder or ring never moves into or out of the callback.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
};

use super::{
    FRAME, MediaError,
    dsp::RenderTap,
    jitter::{JitterBuffer, Packet, Playout as Next},
    mixer,
    monitor::MonitorMix,
    opus_codec::{Pcm, VoiceDecoder},
    vad,
};

/// Mono Voice slots, the first ones: more than the 24 other Members a Voice holds.
pub(crate) const VOICE_SLOTS: usize = 28;
/// Stereo slots for Screen Share Audio, one per Sharer, after the Voice slots.
pub(crate) const SHARE_AUDIO_SLOTS: usize = 4;
pub(crate) const SLOTS: usize = VOICE_SLOTS + SHARE_AUDIO_SLOTS;
pub(crate) const PACKET_RING: usize = 32;
const COMMAND_RING: usize = 64;
/// Energy is published as fixed point: Σ x² · 2^24.
const ENERGY_SCALE: f32 = (1u32 << 24) as f32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SlotCmd {
    Bind { slot: usize, generation: u32 },
    Unbind { slot: usize },
}

#[derive(Debug, Default)]
pub(crate) struct SlotStats {
    pub decoded: AtomicU64,
    pub concealed: AtomicU64,
    pub energy: AtomicU64,
    pub samples: AtomicU64,
    pub late: AtomicU64,
    pub dropped: AtomicU64,
}

impl SlotStats {
    pub fn rms_since(&self, energy0: u64, samples0: u64) -> f32 {
        let samples = self.samples.load(Ordering::Relaxed).wrapping_sub(samples0);
        if samples == 0 {
            return 0.0;
        }
        let energy = self.energy.load(Ordering::Relaxed).wrapping_sub(energy0);
        ((energy as f64 / f64::from(ENERGY_SCALE)) / samples as f64).sqrt() as f32
    }
}

pub(crate) type StatsTable = Arc<[SlotStats; SLOTS]>;

/// Whether each Voice slot's Member speaks in what plays now: the V flag of the frame playing,
/// held [`vad::HANGOVER_FRAMES`] after the last voiced one, so a mute or a gap ends it. Written
/// by `render`, read by `lobby-rtc`, which knows who owns the slot.
pub(crate) type SpeechTable = Arc<[AtomicBool; VOICE_SLOTS]>;

/// What the person chose, written by the Session's side and read by `render`: a gain per slot
/// (the Member's volume, written by `lobby-rtc`, which knows who owns the slot) and a master
/// (0 while Deafened). `f32` bits in atomics, so neither side waits for the other.
pub(crate) struct Gains {
    master: AtomicU32,
    slots: [AtomicU32; SLOTS],
}

impl Gains {
    fn new() -> Self {
        Self {
            master: AtomicU32::new(1.0f32.to_bits()),
            slots: std::array::from_fn(|_| AtomicU32::new(1.0f32.to_bits())),
        }
    }

    pub fn set_master(&self, gain: f32) {
        self.master.store(gain.to_bits(), Ordering::Relaxed);
    }

    pub fn set_slot(&self, slot: usize, gain: f32) {
        self.slots[slot].store(gain.to_bits(), Ordering::Relaxed);
    }

    pub(super) fn master(&self) -> f32 {
        f32::from_bits(self.master.load(Ordering::Relaxed))
    }

    fn slot(&self, slot: usize) -> f32 {
        f32::from_bits(self.slots[slot].load(Ordering::Relaxed))
    }
}

struct Slot {
    generation: Option<u32>,
    /// The gain the last block ended on; the next slides from it to the table's.
    gain: f32,
    packets: rtrb::Consumer<Packet>,
    jitter: JitterBuffer,
    decoder: VoiceDecoder,
    /// The last block it played was below the speaking threshold.
    quiet: bool,
    /// Blocks its Member still speaks for; 0 when not speaking.
    hangover: u32,
}

pub(crate) struct PlayoutFeed {
    pub packets: Vec<rtrb::Producer<Packet>>,
    pub cmds: rtrb::Producer<SlotCmd>,
    pub stats: StatsTable,
    pub speech: SpeechTable,
    pub gains: Arc<Gains>,
}

pub(crate) struct Playout {
    slots: Vec<Slot>,
    cmds: rtrb::Consumer<SlotCmd>,
    stats: StatsTable,
    speech: SpeechTable,
    gains: Arc<Gains>,
    master: f32,
    mix: [f32; FRAME],
    block: Pcm,
    pos: usize,
    tap: RenderTap,
    monitor: MonitorMix,
}

impl Playout {
    pub fn new(tap: RenderTap, monitor: MonitorMix) -> Result<(Playout, PlayoutFeed), MediaError> {
        let mut slots = Vec::with_capacity(SLOTS);
        let mut producers = Vec::with_capacity(SLOTS);
        for i in 0..SLOTS {
            let (tx, rx) = rtrb::RingBuffer::new(PACKET_RING);
            producers.push(tx);
            slots.push(Slot {
                generation: None,
                gain: 1.0,
                packets: rx,
                jitter: JitterBuffer::new(),
                decoder: if i < VOICE_SLOTS {
                    VoiceDecoder::mono()?
                } else {
                    VoiceDecoder::stereo()?
                },
                quiet: true,
                hangover: 0,
            });
        }
        let (cmd_tx, cmd_rx) = rtrb::RingBuffer::new(COMMAND_RING);
        let stats: StatsTable = Arc::new(std::array::from_fn(|_| SlotStats::default()));
        let speech: SpeechTable = Arc::new(std::array::from_fn(|_| AtomicBool::new(false)));
        let gains = Arc::new(Gains::new());
        Ok((
            Playout {
                slots,
                cmds: cmd_rx,
                stats: stats.clone(),
                speech: speech.clone(),
                gains: gains.clone(),
                master: 1.0,
                mix: [0.0; FRAME],
                block: [0.0; FRAME * 2],
                pos: FRAME,
                tap,
                monitor,
            },
            PlayoutFeed {
                packets: producers,
                cmds: cmd_tx,
                stats,
                speech,
                gains,
            },
        ))
    }

    pub fn render(&mut self, out: &mut [f32]) {
        let mut written = 0;
        while written < out.len() {
            if self.pos == FRAME {
                self.next_block();
                self.pos = 0;
            }
            let n = (FRAME - self.pos).min(out.len() - written);
            out[written..written + n].copy_from_slice(&self.mix[self.pos..self.pos + n]);
            self.pos += n;
            written += n;
        }
    }

    fn next_block(&mut self) {
        while let Ok(cmd) = self.cmds.pop() {
            let slot = match cmd {
                SlotCmd::Bind { slot, generation } => {
                    let s = &mut self.slots[slot];
                    s.generation = Some(generation);
                    s.gain = self.gains.slot(slot);
                    s.jitter.reset();
                    s.decoder.reset();
                    slot
                }
                SlotCmd::Unbind { slot } => {
                    let s = &mut self.slots[slot];
                    s.generation = None;
                    s.jitter.reset();
                    slot
                }
            };
            self.slots[slot].hangover = 0;
            if let Some(speech) = self.speech.get(slot) {
                speech.store(false, Ordering::Relaxed);
            }
        }
        self.mix.fill(0.0);
        for (i, slot) in self.slots.iter_mut().enumerate() {
            while let Ok(packet) = slot.packets.pop() {
                if Some(packet.generation) == slot.generation {
                    slot.jitter.push(packet);
                }
            }
            if slot.generation.is_none() {
                continue;
            }
            let stats = &self.stats[i];
            let next = slot.jitter.pop(slot.quiet);
            slot.hangover = match next {
                Next::Frame(p) if p.voiced => vad::HANGOVER_FRAMES,
                Next::Frame(_) => 0,
                _ => slot.hangover.saturating_sub(1),
            };
            if let Some(speech) = self.speech.get(i) {
                speech.store(slot.hangover > 0, Ordering::Relaxed);
            }
            // FEC and PLC are reconstructed, not received: they count as concealed. Comfort
            // fills a DTX gap or a drift insert, which is no loss.
            let counter = match next {
                Next::Frame(p) if slot.decoder.decode(p.payload(), &mut self.block) => {
                    Some(&stats.decoded)
                }
                Next::Frame(_) => Some(&stats.concealed),
                Next::Fec(next) => {
                    slot.decoder.decode_fec(next, &mut self.block);
                    Some(&stats.concealed)
                }
                Next::Conceal => {
                    slot.decoder.conceal(&mut self.block);
                    Some(&stats.concealed)
                }
                Next::Comfort => {
                    slot.decoder.conceal(&mut self.block);
                    None
                }
                Next::Silence => {
                    slot.quiet = true;
                    continue;
                }
            };
            if let Some(counter) = counter {
                counter.fetch_add(1, Ordering::Relaxed);
            }
            let block = &self.block[..FRAME];
            let energy: f32 = block.iter().map(|s| s * s).sum();
            slot.quiet = energy < vad::MIN_RMS * vad::MIN_RMS * FRAME as f32;
            stats
                .energy
                .fetch_add((energy * ENERGY_SCALE) as u64, Ordering::Relaxed);
            stats.samples.fetch_add(FRAME as u64, Ordering::Relaxed);
            stats.late.store(slot.jitter.counts.late, Ordering::Relaxed);
            stats
                .dropped
                .store(slot.jitter.counts.dropped, Ordering::Relaxed);
            let gain = self.gains.slot(i);
            mixer::add_gain(&mut self.mix, block, slot.gain, gain);
            slot.gain = gain;
        }
        let master = self.gains.master();
        mixer::scale(&mut self.mix, self.master, master);
        self.master = master;
        mixer::clamp(&mut self.mix);
        self.tap.push(&self.mix);
        self.monitor.add(&mut self.mix, master);
    }
}
