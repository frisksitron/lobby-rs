//! The per-slot jitter buffer: a ring of 20 ms frames keyed by RTP timestamp, and a state machine
//! over it. Time is counted in pops, one per 20 ms output block.
//!
//! - Buffering: no timeline yet. Playout starts once the target is buffered, or once the first
//!   frame has waited that long.
//! - Playing: a missing frame with later ones buffered is a loss: the next frame's in-band FEC,
//!   else PLC. Packets reorder within 320 ms; late and duplicate ones are dropped. Beyond the
//!   target + 60 ms (a burst after a stall) the oldest frames are dropped.
//! - Dry: nothing buffered, because the sender is in DTX, muted, or a packet is late. The timeline
//!   keeps running: 100 ms of comfort (decoder PLC), then silence, for up to 600 ms, so a DTX gap
//!   (libopus refreshes every 400 ms) is silence, never a loss or a restart. A late packet steps
//!   the timeline back to it: the comfort already played became extra delay.
//!
//! The target starts at 20 ms. A late packet raises it by a frame, a loss to 40 ms so the next
//! packet's FEC is there in time, and 30 s without either lowers it by a frame. A raise inserts
//! a frame where the buffer runs dry or at a quiet point; drift carries out a fall.
//!
//! Clock drift: every packet's lead over the playout is sampled. When even the latest packet of a
//! window came early, a frame is dropped; when even the earliest came late, a comfort frame is
//! inserted. Both wait for a quiet point (silence, DTX or a lost frame), so 100 ppm of skew moves
//! the delay by one frame every 200 s and nobody hears it.
//!
//! It runs inside the audio output callback: fixed storage, no allocation, no locks.

use super::{FRAME, opus_codec::MAX_PACKET};

/// 320 ms of 20 ms frames.
pub(crate) const CAPACITY: usize = 16;
/// The playout delay aimed for at first and at least: 20 ms, a frame with no lookahead.
pub(crate) const MIN_TARGET: u64 = 1;
/// The target after a loss: one frame of lookahead, so the next packet's FEC is there in time.
const FEC_TARGET: u64 = 2;
/// The most a late link raises the target to: 40 ms. A stall
/// beyond it still steps the timeline back, and drift brings the delay back down after.
pub(crate) const MAX_TARGET: u64 = 2;
/// Pops without a late or lost packet before the target falls by one: 30 s.
const LOWER_AFTER: u32 = 1500;
/// Beyond the target plus 60 ms the oldest frames are dropped.
pub(crate) const MAX_EXTRA_FRAMES: u64 = 3;
/// Decoder PLC at the start of a dry spell, so a sound fades instead of stopping dead.
pub(crate) const COMFORT_FRAMES: u64 = 5;
/// How long the timeline outlives the last packet: past libopus' 400 ms DTX refresh.
pub(crate) const DRY_HOLD: u64 = 30;
/// Packets per drift verdict: 2 s of speech.
const DRIFT_WINDOW: u32 = 100;
/// Verdicts in a row a drop waits for a quiet point before it happens anyway.
const FORCE_AFTER: u32 = 2;
const FRAME_TS: u64 = FRAME as u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Packet {
    pub generation: u32,
    /// The RTP sequence number, extended: contiguous across a DTX gap, not across a loss.
    pub seq: u64,
    pub ts: u64,
    /// The sender's V flag (RFC 6464): it was speaking in this frame.
    pub voiced: bool,
    pub len: u16,
    pub data: [u8; MAX_PACKET],
}

impl Packet {
    pub fn new(generation: u32, seq: u64, ts: u64, voiced: bool, payload: &[u8]) -> Option<Packet> {
        if payload.len() > MAX_PACKET {
            return None;
        }
        let mut data = [0u8; MAX_PACKET];
        data[..payload.len()].copy_from_slice(payload);
        Some(Packet {
            generation,
            seq,
            ts,
            voiced,
            len: payload.len() as u16,
            data,
        })
    }

    pub fn payload(&self) -> &[u8] {
        &self.data[..usize::from(self.len)]
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Playout<'a> {
    Frame(&'a Packet),
    /// A lost frame, rebuilt from the next packet's in-band FEC.
    Fec(&'a [u8]),
    /// A lost frame: decoder PLC.
    Conceal,
    /// A dry buffer or an inserted frame: decoder PLC, but not a loss.
    Comfort,
    Silence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// No timeline: `held` pops since a packet arrived.
    Buffering { held: u64 },
    /// Frame `next` plays at the next pop.
    Playing { next: u64 },
    /// Nothing to play since frame `missing`; `next` keeps time.
    Dry { next: u64, missing: u64, dry: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Adjust {
    Drop,
    Insert,
}

/// The drift verdicts: how far ahead of the playout each packet arrived, over a window.
#[derive(Debug, Clone, Copy)]
struct Drift {
    packets: u32,
    min_lead: i64,
    max_lead: i64,
    pending: Option<Adjust>,
    /// Verdicts in a row that asked for `pending`.
    age: u32,
}

impl Drift {
    const fn new() -> Self {
        Drift {
            packets: 0,
            min_lead: i64::MAX,
            max_lead: i64::MIN,
            pending: None,
            age: 0,
        }
    }

    fn observe(&mut self, lead: i64, target: u64) {
        self.min_lead = self.min_lead.min(lead);
        self.max_lead = self.max_lead.max(lead);
        self.packets += 1;
        if self.packets < DRIFT_WINDOW {
            return;
        }
        // At the target depth a packet arrives one pop before the pop that plays it.
        let aim = target as i64 - 1;
        let verdict = if self.min_lead > aim {
            Some(Adjust::Drop)
        } else if self.max_lead < aim {
            Some(Adjust::Insert)
        } else {
            None
        };
        let age = if verdict == self.pending {
            self.age + 1
        } else {
            0
        };
        *self = Drift {
            pending: verdict,
            age,
            ..Drift::new()
        };
    }

    fn forced(&self) -> bool {
        self.age >= FORCE_AFTER
    }

    /// The delay moved: the window so far measured the old one.
    fn done(&mut self) {
        *self = Drift::new();
    }

    /// The target rose above the delay: insert a frame at the next quiet point or dry pop.
    fn insert(&mut self) {
        *self = Drift {
            pending: Some(Adjust::Insert),
            ..Drift::new()
        };
    }
}

/// The playout delay aimed for, in frames: the one place that decides it. Drift moves the
/// timeline to it, at a quiet point.
#[derive(Debug, Clone, Copy)]
struct Target {
    frames: u64,
    /// Pops since a late or lost packet.
    clean: u32,
}

impl Target {
    const fn new() -> Self {
        Target {
            frames: MIN_TARGET,
            clean: 0,
        }
    }

    /// Whether it rose.
    fn raise(&mut self, to: u64) -> bool {
        self.clean = 0;
        let rose = to.min(MAX_TARGET) > self.frames;
        self.frames = self.frames.max(to.min(MAX_TARGET));
        rose
    }

    fn late(&mut self) -> bool {
        self.raise(self.frames + 1)
    }

    /// Waiting longer does not bring a lost packet back, but one frame of it brings the FEC.
    fn lost(&mut self) -> bool {
        self.raise(FEC_TARGET)
    }

    /// One pop. Whether a long clean run lowered it.
    fn tick(&mut self) -> bool {
        self.clean = self.clean.saturating_add(1);
        if self.clean < LOWER_AFTER || self.frames == MIN_TARGET {
            return false;
        }
        self.frames -= 1;
        self.clean = 0;
        true
    }
}

#[derive(Clone, Copy)]
struct Entry {
    frame: Option<u64>,
    packet: Packet,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct JitterCounts {
    /// Arrived after their frame's turn.
    pub late: u64,
    pub duplicate: u64,
    /// Dropped to bring a burst back to the target.
    pub dropped: u64,
    /// Frames skipped and inserted to follow clock drift and late packets.
    pub skipped: u64,
    pub inserted: u64,
    /// Times the target rose or fell.
    pub retargets: u64,
}

pub(crate) struct JitterBuffer {
    entries: [Entry; CAPACITY],
    state: State,
    drift: Drift,
    /// Outlives a new timeline: the link is the same.
    target: Target,
    /// Of the last frame played or passed over, to tell a DTX gap from a loss.
    last_seq: Option<u64>,
    pub counts: JitterCounts,
}

impl JitterBuffer {
    pub fn new() -> Self {
        let empty = Entry {
            frame: None,
            packet: Packet {
                generation: 0,
                seq: 0,
                ts: 0,
                voiced: false,
                len: 0,
                data: [0; MAX_PACKET],
            },
        };
        Self {
            entries: [empty; CAPACITY],
            state: State::Buffering { held: 0 },
            drift: Drift::new(),
            target: Target::new(),
            last_seq: None,
            counts: JitterCounts::default(),
        }
    }

    /// A new sender in this slot.
    pub fn reset(&mut self) {
        self.restart();
        self.target = Target::new();
    }

    /// A new timeline from the same sender.
    fn restart(&mut self) {
        for e in &mut self.entries {
            e.frame = None;
        }
        self.state = State::Buffering { held: 0 };
        self.drift = Drift::new();
        self.last_seq = None;
    }

    fn late(&mut self) -> bool {
        let rose = self.target.late();
        self.counts.retargets += u64::from(rose);
        rose
    }

    fn lost(&mut self) {
        if self.target.lost() {
            self.counts.retargets += 1;
            self.drift.insert();
        }
    }

    /// Takes a frame out without playing it.
    fn pass_over(&mut self, frame: u64) {
        let e = &mut self.entries[Self::slot(frame)];
        e.frame = None;
        self.last_seq = Some(e.packet.seq);
    }

    fn slot(frame: u64) -> usize {
        (frame % CAPACITY as u64) as usize
    }

    fn has(&self, frame: u64) -> bool {
        self.entries[Self::slot(frame)].frame == Some(frame)
    }

    fn packet(&self, frame: u64) -> &Packet {
        &self.entries[Self::slot(frame)].packet
    }

    /// The oldest and the newest frame buffered.
    fn span(&self) -> Option<(u64, u64)> {
        let mut frames = self.entries.iter().filter_map(|e| e.frame);
        let first = frames.next()?;
        Some(frames.fold((first, first), |(lo, hi), f| (lo.min(f), hi.max(f))))
    }

    /// Frames buffered from the playout point on, while there is a timeline.
    #[cfg(test)]
    pub fn depth(&self) -> Option<u64> {
        let (State::Playing { next } | State::Dry { next, .. }) = self.state else {
            return None;
        };
        Some(
            self.span()
                .map_or(0, |(_, newest)| (newest + 1).saturating_sub(next)),
        )
    }

    pub fn push(&mut self, packet: Packet) {
        let frame = packet.ts / FRAME_TS;
        match self.state {
            State::Playing { next } | State::Dry { next, .. }
                if frame >= next + CAPACITY as u64 =>
            {
                // Far ahead of the timeline: the sender's clock jumped, or we stalled.
                self.restart();
            }
            State::Dry { next, missing, dry } if frame < next && frame >= missing => {
                // Stepping back already raised the delay to at least the new target.
                self.late();
                self.counts.inserted += next - frame;
                self.drift.done();
                self.state = State::Dry {
                    next: frame,
                    missing,
                    dry,
                };
            }
            State::Playing { next } | State::Dry { next, .. } if frame < next => {
                self.counts.late += 1;
                self.drift
                    .observe(frame as i64 - next as i64, self.target.frames);
                if self.late() {
                    self.drift.insert();
                }
                return;
            }
            State::Playing { next } | State::Dry { next, .. } => {
                self.drift
                    .observe((frame - next) as i64, self.target.frames);
            }
            State::Buffering { .. } => {
                if let Some((oldest, newest)) = self.span()
                    && (frame + CAPACITY as u64 <= newest || frame >= oldest + CAPACITY as u64)
                {
                    self.restart();
                }
            }
        }
        let e = &mut self.entries[Self::slot(frame)];
        if e.frame == Some(frame) {
            self.counts.duplicate += 1;
            return;
        }
        e.frame = Some(frame);
        e.packet = packet;
    }

    /// `quiet`: the last block this slot played was below the speaking threshold, so a frame can
    /// be dropped or inserted there unheard.
    pub fn pop(&mut self, quiet: bool) -> Playout<'_> {
        match self.state {
            State::Buffering { held } => match self.span() {
                Some((oldest, newest)) if newest - oldest + 1 + held >= self.target.frames => {
                    self.play(oldest, quiet)
                }
                Some(_) => {
                    self.state = State::Buffering { held: held + 1 };
                    Playout::Silence
                }
                None => Playout::Silence,
            },
            State::Playing { next } => {
                self.tick();
                self.play(next, quiet)
            }
            // The stream resumes: its first frame is never the one skipped.
            State::Dry { next, .. } if self.has(next) => {
                self.tick();
                self.play(next, false)
            }
            State::Dry { next, missing, dry } => {
                self.tick();
                self.dry(next, missing, dry + 1)
            }
        }
    }

    fn tick(&mut self) {
        self.counts.retargets += u64::from(self.target.tick());
    }

    fn play(&mut self, mut next: u64, quiet: bool) -> Playout<'_> {
        let target = self.target.frames;
        if let Some((_, newest)) = self.span()
            && newest > next + target + MAX_EXTRA_FRAMES
        {
            let keep_from = newest - target;
            for f in next..keep_from {
                if self.has(f) {
                    self.pass_over(f);
                    self.counts.dropped += 1;
                }
            }
            next = keep_from;
            self.drift.done();
        }
        match self.drift.pending {
            Some(Adjust::Drop)
                if self.has(next + 1) && (quiet || !self.has(next) || self.drift.forced()) =>
            {
                if self.has(next) {
                    self.pass_over(next);
                }
                self.counts.skipped += 1;
                self.drift.done();
                next += 1;
            }
            Some(Adjust::Insert) if quiet => {
                self.counts.inserted += 1;
                self.drift.done();
                self.state = State::Playing { next };
                return Playout::Comfort;
            }
            _ => {}
        }
        self.state = State::Playing { next: next + 1 };
        if self.has(next) {
            // A gap in the sequence: the frame before was lost, and played as comfort.
            if self
                .last_seq
                .is_some_and(|seq| self.packet(next).seq > seq + 1)
            {
                self.lost();
            }
            self.pass_over(next);
            return Playout::Frame(self.packet(next));
        }
        if self.has(next + 1) {
            self.lost();
            return Playout::Fec(self.packet(next + 1).payload());
        }
        if self.span().is_some_and(|(_, newest)| newest > next) {
            self.lost();
            return Playout::Conceal;
        }
        // Raising the target: this frame may still come, so the timeline waits for it.
        let step = if self.drift.pending == Some(Adjust::Insert) {
            self.counts.inserted += 1;
            self.drift.done();
            0
        } else {
            1
        };
        self.state = State::Dry {
            next: next + step,
            missing: next,
            dry: 1,
        };
        Playout::Comfort
    }

    fn dry(&mut self, next: u64, missing: u64, dry: u64) -> Playout<'_> {
        let resumes = self.span().map(|(oldest, _)| oldest);
        if resumes.is_none() && dry > DRY_HOLD {
            // Muted, gone, or a long silence: the next packet starts a new timeline.
            self.restart();
            return Playout::Silence;
        }
        let step = match self.drift.pending {
            Some(Adjust::Drop) => {
                self.counts.skipped += 1;
                self.drift.done();
                if self.has(next + 1) {
                    return self.play(next + 1, false);
                }
                2
            }
            Some(Adjust::Insert) => {
                self.counts.inserted += 1;
                self.drift.done();
                0
            }
            _ => 1,
        };
        self.state = State::Dry {
            next: next + step,
            missing,
            dry,
        };
        // A sequence gap before the stream resumes means the gap ended in a loss, not in DTX.
        if let Some(first) = resumes
            && first == next + 1
            && self
                .last_seq
                .is_some_and(|seq| self.packet(first).seq > seq + 1)
        {
            self.lost();
            return Playout::Fec(self.packet(first).payload());
        }
        if dry <= COMFORT_FRAMES {
            Playout::Comfort
        } else {
            Playout::Silence
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: u64 = 123_456 * FRAME_TS;

    fn pkt(n: u64, seq: u64) -> Packet {
        Packet::new(1, seq, BASE + n * FRAME_TS, false, &[n as u8; 3]).unwrap()
    }

    fn kind(p: &Playout<'_>) -> String {
        match p {
            Playout::Frame(p) => format!("F{}", p.payload()[0]),
            Playout::Fec(d) => format!("fec{}", d[0]),
            Playout::Conceal => "plc".into(),
            Playout::Comfort => "cn".into(),
            Playout::Silence => "-".into(),
        }
    }

    enum Op {
        /// Frames whose sequence numbers follow their timestamps.
        Push(&'static [u64]),
        /// Frame and sequence number, for a DTX gap: the timestamp jumps, the sequence does not.
        PushSeq(&'static [(u64, u64)]),
        Pop(&'static [&'static str]),
        /// Pops that each play the frame before is quiet.
        PopQuiet(&'static [&'static str]),
    }
    use Op::{Pop, PopQuiet, Push, PushSeq};

    const CN5: [&str; 5] = ["cn"; 5];

    #[test]
    fn playout_table() {
        let cases: &[(&str, &[Op])] = &[
            (
                "at the 20 ms target a frame plays at the first pop, then in order",
                &[
                    Pop(&["-"]),
                    Push(&[0]),
                    Pop(&["F0"]),
                    Push(&[1, 2]),
                    Pop(&["F1", "F2"]),
                ],
            ),
            (
                "reorders within the window",
                &[Push(&[1, 0, 3, 2]), Pop(&["F0", "F1", "F2", "F3"])],
            ),
            (
                "a lost frame plays the next one's FEC, or PLC when that is missing too",
                &[
                    Push(&[0, 1]),
                    Pop(&["F0"]),
                    Pop(&["F1"]),
                    Push(&[3]),
                    Pop(&["fec3"]),
                    Push(&[4, 7]),
                    Pop(&["F3", "F4", "plc", "fec7", "F7"]),
                ],
            ),
            (
                "a buffer that runs dry on a loss conceals with the next frame's FEC",
                &[
                    Push(&[0, 1]),
                    Pop(&["F0", "F1", "cn"]),
                    Push(&[4]),
                    Pop(&["fec4", "F4"]),
                ],
            ),
            (
                "a frame older than the playout is late, and a duplicate plays once",
                &[
                    Push(&[0, 1, 1]),
                    Pop(&["F0", "F1"]),
                    Push(&[0, 1]),
                    Pop(&["cn"]),
                ],
            ),
            (
                "a late frame the buffer ran dry on steps the timeline back",
                &[
                    Push(&[0, 1]),
                    Pop(&["F0", "F1", "cn", "cn"]),
                    Push(&[2, 3]),
                    Pop(&["F2", "F3"]),
                ],
            ),
            (
                "a DTX gap keeps the timeline: comfort, silence, then the refresh frame on time",
                &[
                    Push(&[0, 1]),
                    Pop(&["F0", "F1"]),
                    Pop(&CN5),
                    Pop(&["-"; 14]),
                    PushSeq(&[(22, 2)]),
                    Pop(&["-", "F22"]),
                ],
            ),
            (
                "a sender quiet beyond 600 ms starts a new timeline at the target",
                &[
                    Push(&[0, 1]),
                    Pop(&["F0", "F1"]),
                    Pop(&CN5),
                    Pop(&["-"; 26]),
                    PushSeq(&[(500, 2)]),
                    Pop(&["F244"]),
                    PushSeq(&[(501, 3)]),
                    Pop(&["F245"]),
                ],
            ),
            (
                "a backlog beyond target + 60 ms drops the oldest",
                &[Push(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]), Pop(&["F8", "F9"])],
            ),
            (
                "a loss raises the 20 ms target a frame at the next dry pop, so the next loss plays FEC",
                &[
                    Push(&[0]),
                    Pop(&["F0", "cn"]),
                    Push(&[2]),
                    Pop(&["F2"]),
                    Push(&[3]),
                    Pop(&["F3", "cn"]),
                    Push(&[4, 6]),
                    Pop(&["F4", "fec6", "F6"]),
                ],
            ),
            (
                "a jump far ahead restarts there",
                &[
                    Push(&[0, 1]),
                    Pop(&["F0", "F1"]),
                    Push(&[40, 41]),
                    Pop(&["F40", "F41"]),
                ],
            ),
            (
                "quiet pops change nothing without drift",
                &[Push(&[0, 1, 2]), PopQuiet(&["F0", "F1", "F2"])],
            ),
        ];
        for (name, ops) in cases {
            let mut j = JitterBuffer::new();
            for op in *ops {
                match op {
                    Push(ns) => ns.iter().for_each(|&n| j.push(pkt(n, n))),
                    PushSeq(ps) => ps.iter().for_each(|&(n, seq)| j.push(pkt(n, seq))),
                    Pop(want) | PopQuiet(want) => {
                        let quiet = matches!(op, PopQuiet(_));
                        let got: Vec<String> = want.iter().map(|_| kind(&j.pop(quiet))).collect();
                        assert_eq!(got, *want, "{name}");
                    }
                }
            }
        }
        assert!(
            Packet::new(1, 0, 0, false, &[0; MAX_PACKET + 1]).is_none(),
            "oversized"
        );
        assert_eq!(
            Packet::new(1, 0, 0, false, &[7; 10]).unwrap().payload(),
            &[7; 10]
        );
    }

    #[test]
    fn late_and_duplicate_packets_are_counted() {
        let mut j = JitterBuffer::new();
        [0, 1, 1].into_iter().for_each(|n| j.push(pkt(n, n)));
        j.pop(false);
        j.pop(false);
        j.push(pkt(0, 0));
        assert_eq!(
            j.counts,
            JitterCounts {
                late: 1,
                duplicate: 1,
                retargets: 1,
                ..JitterCounts::default()
            }
        );
    }

    /// A sender and a network, played through the buffer one 20 ms pop at a time.
    #[derive(Clone, Copy)]
    struct Trace {
        seconds: u64,
        /// Positive: the sender's clock runs fast.
        skew_ppm: f64,
        loss_percent: u64,
        /// Talkspurts and pauses, in frames; no pauses is one long tone.
        talk: u64,
        pause: u64,
        /// Where between two pops the packets land, in frames.
        phase: f64,
        /// Network jitter, uniform within ± this.
        jitter_ms: f64,
        /// One packet in this many is held back by `SPIKE_MS`; 0 is never.
        spike_every: u64,
        /// Once in this many frames the link stalls for 60 to 120 ms, then delivers the backlog
        /// at once, as Wi-Fi does; 0 is never.
        stall_every: u64,
    }

    #[derive(Debug, Default)]
    struct Report {
        /// Frames buffered when a frame plays.
        depth: Option<(u64, u64)>,
        /// From arrival to playout, in frames.
        delay: Option<(f64, f64)>,
        /// From the link's base arrival, without jitter, to playout: the delay the buffer adds.
        added_delay: f64,
        played: u64,
        lost_in_talk: u64,
        /// Frames rebuilt in talk: FEC, PLC or comfort.
        filled_in_talk: u64,
        /// Of those, rebuilt from the next packet's FEC rather than guessed by PLC.
        fec_in_talk: u64,
        /// Talk frames that arrived and were never heard.
        unheard: u64,
        /// Between two heard talk frames: fillers beyond the frames lost, and frames passed over.
        talk_inserted: u64,
        talk_skipped: u64,
        counts: JitterCounts,
    }

    const SPIKE_MS: f64 = 30.0;
    const FRAME_MS: f64 = 20.0;

    /// A fixed pseudo-random number in 0..1 for `k`, one stream per `salt`.
    fn noise(k: u64, salt: u64) -> f64 {
        ((k ^ salt).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 40) as f64 / (1u64 << 24) as f64
    }

    impl Trace {
        fn frames(&self) -> u64 {
            self.seconds * 50
        }

        fn loud(&self, k: u64) -> bool {
            k < self.frames() && (self.pause == 0 || k % (self.talk + self.pause) < self.talk)
        }

        /// libopus DTX: 200 ms of quiet frames go out, then one every 400 ms.
        fn sent(&self, k: u64) -> bool {
            if self.loud(k) {
                return true;
            }
            let quiet_for = k % (self.talk + self.pause) - self.talk;
            quiet_for < 10 || (quiet_for - 10) % 21 == 20
        }

        /// Never the first: before it there is nothing to conceal from.
        fn lost(&self, k: u64) -> bool {
            k > 0 && (k.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 40) % 100 < self.loss_percent
        }

        fn base_arrival(&self, k: u64) -> f64 {
            k as f64 * (1.0 - self.skew_ppm * 1e-6) + self.phase
        }

        fn arrival(&self, k: u64) -> f64 {
            let mut at =
                self.base_arrival(k) + (noise(k, 0x5A5A) * 2.0 - 1.0) * self.jitter_ms / FRAME_MS;
            if self.spike_every > 0 && k % self.spike_every == self.spike_every / 2 {
                at += SPIKE_MS / FRAME_MS;
            }
            if let Some(n) = k.checked_div(self.stall_every) {
                let start = n * self.stall_every + self.stall_every / 3;
                let frames = 3 + (noise(n, 0xC3C3) * 4.0) as u64;
                if (start..start + frames).contains(&k) {
                    at = at.max(self.base_arrival(start + frames));
                }
            }
            at
        }

        fn run(&self) -> Report {
            let mut packets = Vec::new();
            let mut seq = 0;
            for k in 0..self.frames() {
                if !self.sent(k) {
                    continue;
                }
                seq += 1;
                if !self.lost(k) {
                    let p =
                        Packet::new(1, seq, BASE + k * FRAME_TS, false, &k.to_le_bytes()).unwrap();
                    packets.push((self.arrival(k), k, p));
                }
            }
            packets.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut report = Report::default();
            let mut heard = vec![false; self.frames() as usize];
            let mut j = JitterBuffer::new();
            let mut arrived = packets.iter().peekable();
            let mut quiet = true;
            let mut last: Option<u64> = None;
            let mut fillers: u64 = 0;
            for tick in 0..self.frames() + 50 {
                while let Some((_, _, p)) = arrived.next_if(|(at, _, _)| *at <= tick as f64) {
                    j.push(*p);
                }
                let depth = j.depth();
                match j.pop(quiet) {
                    Playout::Frame(p) => {
                        let k = u64::from_le_bytes(p.payload().try_into().unwrap());
                        heard[k as usize] = true;
                        report.played += 1;
                        if let Some(d) = depth {
                            report.depth = Some(
                                report
                                    .depth
                                    .map_or((d, d), |(lo, hi)| (lo.min(d), hi.max(d))),
                            );
                        }
                        report.added_delay += tick as f64 - self.base_arrival(k);
                        let delay = tick as f64 - self.arrival(k);
                        report.delay = Some(
                            report
                                .delay
                                .map_or((delay, delay), |(lo, hi)| (lo.min(delay), hi.max(delay))),
                        );
                        if let Some(prev) = last
                            && (prev..=k).all(|f| self.loud(f))
                        {
                            let gap = k - prev - 1;
                            report.talk_inserted += fillers.saturating_sub(gap);
                            report.talk_skipped += gap.saturating_sub(fillers);
                        }
                        last = Some(k);
                        fillers = 0;
                        quiet = !self.loud(k);
                    }
                    p @ (Playout::Fec(_) | Playout::Conceal | Playout::Comfort) => {
                        fillers += 1;
                        if last.is_some_and(|k| self.loud(k + fillers)) {
                            report.filled_in_talk += 1;
                            report.fec_in_talk += u64::from(matches!(p, Playout::Fec(_)));
                        }
                    }
                    Playout::Silence => {
                        fillers += 1;
                        quiet = true;
                    }
                }
            }
            for &(_, k, _) in &packets {
                if self.loud(k) && !heard[k as usize] {
                    report.unheard += 1;
                }
            }
            report.lost_in_talk = (0..self.frames())
                .filter(|&k| self.loud(k) && self.lost(k))
                .count() as u64;
            report.counts = j.counts;
            report.added_delay /= report.played.max(1) as f64;
            report
        }
    }

    const TEN_MINUTES: u64 = 600;
    /// 100 ppm over ten minutes is 60 ms: three frames.
    const DRIFT_FRAMES: u64 = 3;

    fn speech(skew_ppm: f64, loss_percent: u64) -> Trace {
        Trace {
            seconds: TEN_MINUTES,
            skew_ppm,
            loss_percent,
            talk: 150,
            pause: 50,
            phase: 0.5,
            jitter_ms: 0.0,
            spike_every: 0,
            stall_every: 0,
        }
    }

    fn assert_depth_bounded(r: &Report) {
        let (lo, hi) = r.depth.expect("played");
        assert!(lo >= 1 && hi <= FEC_TARGET + 1, "depth {lo}..={hi}: {r:?}");
    }

    #[test]
    fn dtx_gaps_are_silence_on_one_timeline() {
        let r = speech(0.0, 0).run();
        assert_eq!(r.unheard, 0, "{r:?}");
        assert_eq!(
            (r.filled_in_talk, r.talk_inserted, r.talk_skipped),
            (0, 0, 0),
            "{r:?}"
        );
        assert_eq!(r.counts, JitterCounts::default(), "{r:?}");
        let (lo, hi) = r.delay.expect("played");
        assert!(hi - lo < 1e-9, "one delay across every DTX gap: {r:?}");
        assert_depth_bounded(&r);
    }

    #[test]
    fn ten_percent_loss_is_concealed_frame_for_frame() {
        for trace in [
            Trace {
                pause: 0,
                ..speech(0.0, 10)
            },
            speech(0.0, 10),
        ] {
            let r = trace.run();
            assert!(r.lost_in_talk > 0);
            assert_eq!(r.unheard, 0, "{r:?}");
            // The first loss raises the target to the FEC lookahead: one inserted frame.
            assert!(r.talk_inserted <= 1 && r.talk_skipped == 0, "{r:?}");
            assert_eq!(r.counts.late, 0, "{r:?}");
            assert_depth_bounded(&r);
            if trace.pause == 0 {
                assert!(r.filled_in_talk <= r.lost_in_talk + 1, "{r:?}");
            } else {
                // A talkspurt whose first frame is lost after a long silence starts with the
                // next one: there is nothing yet to conceal from.
                assert!(r.filled_in_talk <= r.lost_in_talk, "{r:?}");
            }
        }
    }

    #[test]
    fn skew_both_ways_is_absorbed_in_the_pauses() {
        for (skew, loss) in [(100.0, 0), (-100.0, 0), (100.0, 10), (-100.0, 10)] {
            let r = speech(skew, loss).run();
            let what = format!("{skew} ppm, {loss} % loss: {r:?}");
            assert_depth_bounded(&r);
            assert_eq!((r.unheard, r.counts.late), (0, 0), "{what}");
            // At 20 ms a slow sender's frame of drift can first show as a late packet.
            assert!(r.talk_inserted <= DRIFT_FRAMES + 1, "{what}");
            let net = r.counts.skipped as i64 - r.counts.inserted as i64;
            let fixes = if skew > 0.0 { net } else { -net };
            let drift = DRIFT_FRAMES as i64;
            assert!((drift - 1..=drift + 1).contains(&fixes), "{what}");
            if loss == 0 {
                assert_eq!(r.talk_skipped, 0, "{what}");
            }
        }
    }

    #[derive(Debug, Default)]
    struct LinkReport {
        /// Averaged over the pop phases.
        added_delay_ms: f64,
        /// Summed over the pop phases.
        concealed: u64,
        fec: u64,
        unheard: u64,
        retargets: u64,
    }

    /// Ten minutes of speech over each link, at five pop phases.
    fn links() -> Vec<(&'static str, LinkReport)> {
        let link = |jitter_ms, spike_every, stall_every, skew_ppm, loss| Trace {
            jitter_ms,
            spike_every,
            stall_every,
            ..speech(skew_ppm, loss)
        };
        let schedules = [
            ("clean ±2 ms", link(2.0, 0, 0, 0.0, 0)),
            ("owner ±5 ms, 1/500 late", link(5.0, 500, 0, 0.0, 0)),
            ("Wi-Fi stall every 4 s", link(5.0, 0, 200, 0.0, 0)),
            ("Wi-Fi stall every 20 s", link(5.0, 0, 1000, 0.0, 0)),
            ("drift +0.1 %", link(2.0, 0, 0, 1000.0, 0)),
            ("drift -0.1 %", link(2.0, 0, 0, -1000.0, 0)),
            ("±5 ms, 2 % loss", link(5.0, 0, 0, 0.0, 2)),
        ];
        const PHASES: [f64; 5] = [0.1, 0.3, 0.5, 0.7, 0.9];
        schedules
            .into_iter()
            .map(|(name, trace)| {
                let mut total = LinkReport::default();
                for phase in PHASES {
                    let r = Trace { phase, ..trace }.run();
                    total.added_delay_ms += r.added_delay * FRAME_MS / PHASES.len() as f64;
                    total.concealed += r.filled_in_talk;
                    total.fec += r.fec_in_talk;
                    total.retargets += r.counts.retargets;
                    total.unheard += r.unheard;
                }
                (name, total)
            })
            .collect()
    }

    #[test]
    fn link_schedules() {
        let fixed = [
            (30.0, 0),
            (33.8, 42),
            (78.1, 1015),
            (57.3, 476),
            (35.4, 0),
            (24.6, 0),
            (26.0, 2491),
        ];
        let links = links();
        assert_eq!(links.len(), fixed.len());
        for ((name, r), (delay, concealed)) in links.into_iter().zip(fixed) {
            assert!(r.added_delay_ms <= delay + 0.5, "{name}: {r:?}");
            if name.starts_with("clean") {
                assert!(r.added_delay_ms <= delay - 19.0, "{name}: {r:?}");
            }
            // One more per run: the first stall comes before the first raise.
            if name.starts_with("Wi-Fi") {
                assert!(r.concealed <= concealed + 5, "{name}: {r:?}");
            }
        }
    }

    #[test]
    fn skew_in_a_tone_without_pauses_stays_bounded() {
        for skew in [100.0, -100.0] {
            let r = Trace {
                pause: 0,
                ..speech(skew, 0)
            }
            .run();
            let what = format!("{skew} ppm: {r:?}");
            assert_depth_bounded(&r);
            assert_eq!(r.counts.late, 0, "{what}");
            assert!(
                r.talk_inserted + r.talk_skipped <= DRIFT_FRAMES + 1,
                "{what}"
            );
        }
    }
}
