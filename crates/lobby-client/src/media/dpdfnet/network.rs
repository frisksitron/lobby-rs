//! The network: one spectrum in, the enhanced spectrum out, with what it keeps between frames.

use super::{
    layers::{
        C, Dense, DprnnBlock, DprnnScratch, Grouped, GruScratch, Pathway, SepConv, SqueezedGru,
        SubPixel, relu, sigmoid, tanh,
    },
    weights::Reader,
};

/// Bins in a spectrum: a 960-point real FFT.
pub(super) const BINS: usize = 481;
/// A spectrum as the network takes it: each bin's real and imaginary parts, in turn.
pub(super) type Spectrum = [f32; 2 * BINS];

/// Bins the magnitude path's convolutions see; the Nyquist bin is dropped and later mirrored.
const MAG: usize = 480;
/// Low bins the deep filter enhances.
const DF: usize = 96;
/// Frames the deep filter combines per bin.
const ORDER: usize = 5;
const COEFS: usize = 2 * ORDER;
/// Frequency rows after each of the magnitude encoder's strided convolutions.
const E1: usize = 160;
const E2: usize = 80;
const E3: usize = 40;
/// Frequency rows of the deep-filter encoder after its strided convolution.
const D1: usize = 48;
const HID: usize = 256;
const EMB: usize = 512;
/// The model scales the unnormalised FFT by this on the way in and undoes it on the way out.
const WNORM: f32 = 1.0 / 960.0;
/// The feature normalisations' smoothing, as the model's two constants: 1 - 0.98 in f32 is not
/// 0.02 in f32.
const ALPHA: f32 = 0.98;
const BETA: f32 = 0.02;

/// The last `T` frames of `N` values, oldest first.
struct History<const T: usize, const N: usize> {
    frames: [[f32; N]; T],
    next: usize,
}

impl<const T: usize, const N: usize> History<T, N> {
    fn new() -> History<T, N> {
        History {
            frames: [[0.0; N]; T],
            next: 0,
        }
    }

    /// Drops the oldest frame and returns the slot for the newest.
    fn push(&mut self) -> &mut [f32; N] {
        let slot = self.next;
        self.next = (slot + 1) % T;
        &mut self.frames[slot]
    }

    /// Frame `k`, counted from the oldest.
    fn get(&self, k: usize) -> &[f32; N] {
        &self.frames[(self.next + k) % T]
    }
}

/// Everything one frame leaves for the next.
struct State {
    erb_mu: [f32; BINS],
    spec_s: [f32; DF],
    mag_feat: History<3, BINS>,
    spec_feat: History<3, { 2 * DF }>,
    dprnn_erb: [[f32; E3 * C]; 2],
    dprnn_df: [[f32; D1 * C]; 2],
    enc_gru: [f32; HID],
    dec_gru: [f32; 2 * HID],
    df_gru: [f32; 2 * HID],
    c0: History<ORDER, { DF * C }>,
    spec: History<3, { 2 * BINS }>,
    coefs: History<3, { DF * COEFS }>,
    masked: History<ORDER, { 2 * BINS }>,
}

impl State {
    fn new(weights: &Weights) -> Box<State> {
        let mut state = Box::new(State {
            erb_mu: [0.0; BINS],
            spec_s: [0.0; DF],
            mag_feat: History::new(),
            spec_feat: History::new(),
            dprnn_erb: [[0.0; E3 * C]; 2],
            dprnn_df: [[0.0; D1 * C]; 2],
            enc_gru: [0.0; HID],
            dec_gru: [0.0; 2 * HID],
            df_gru: [0.0; 2 * HID],
            c0: History::new(),
            spec: History::new(),
            coefs: History::new(),
            masked: History::new(),
        });
        state.erb_mu.copy_from_slice(&weights.erb_mu0);
        state.spec_s.copy_from_slice(&weights.spec_s0);
        state
    }
}

/// What one frame computes on its way through the stages.
struct Scratch {
    spec: Spectrum,
    e0: Box<[f32]>,
    e1: Box<[f32]>,
    e2: Box<[f32]>,
    e3: Box<[f32]>,
    x3: Box<[f32]>,
    c1: Box<[f32]>,
    tmp: Box<[f32]>,
    emb_in: [f32; 2 * EMB],
    emb: [f32; EMB],
    dec_emb: [f32; EMB],
    d3: Box<[f32]>,
    d2: Box<[f32]>,
    d1: Box<[f32]>,
    d0: Box<[f32]>,
    mask: [f32; BINS],
    df_hidden: [f32; HID],
    df_skip: [f32; HID],
    df_out: [f32; DF * COEFS],
    convp_raw: [f32; DF * COEFS],
    convp: [f32; DF * COEFS],
    dprnn: DprnnScratch,
    gru: GruScratch,
}

impl Scratch {
    fn new() -> Box<Scratch> {
        let zeros = |n: usize| vec![0.0; n].into_boxed_slice();
        Box::new(Scratch {
            spec: [0.0; 2 * BINS],
            e0: zeros(MAG * C),
            e1: zeros(E1 * C),
            e2: zeros(E2 * C),
            e3: zeros(E3 * C),
            x3: zeros(E3 * C),
            c1: zeros(D1 * C),
            tmp: zeros(MAG * C),
            emb_in: [0.0; 2 * EMB],
            emb: [0.0; EMB],
            dec_emb: [0.0; EMB],
            d3: zeros(E3 * C),
            d2: zeros(E2 * C),
            d1: zeros(E1 * C),
            d0: zeros(MAG * C),
            mask: [0.0; BINS],
            df_hidden: [0.0; HID],
            df_skip: [0.0; HID],
            df_out: [0.0; DF * COEFS],
            convp_raw: [0.0; DF * COEFS],
            convp: [0.0; DF * COEFS],
            dprnn: DprnnScratch::new(D1),
            gru: GruScratch::new(),
        })
    }
}

/// The trained parameters.
struct Weights {
    erb_mu0: Box<[f32]>,
    spec_s0: Box<[f32]>,
    /// `[time][freq tap][channel]`
    erb_conv0_w: Box<[f32]>,
    erb_conv0_b: Box<[f32]>,
    erb_convs: [SepConv; 3],
    dprnn_erb: [DprnnBlock; 2],
    /// `[time][freq tap][channel]`; channels 0..32 read the real part, 32..64 the imaginary.
    df_conv0_w: Box<[f32]>,
    df_conv0_pw: Dense,
    df_conv1: SepConv,
    dprnn_df: [DprnnBlock; 2],
    erb_fc_emb: Grouped,
    df_fc_emb: Grouped,
    enc_gru: SqueezedGru,
    dec_gru: SqueezedGru,
    dec_fc_emb: Grouped,
    /// Outermost last: the skips from e3, e2, e1 and e0.
    pathways: [Pathway; 4],
    upsample: [SubPixel; 3],
    /// `[freq tap][channel]`
    mask_w: Box<[f32]>,
    mask_b: Box<[f32]>,
    df_gru: SqueezedGru,
    df_skip: Grouped,
    df_out: Grouped,
    /// `[time][group][in][out]`: two groups of 32 channels in, 5 out.
    convp_w: Box<[f32]>,
    convp_pw: Dense,
}

impl Weights {
    fn read(bytes: &[u8]) -> Weights {
        let mut reader = Reader::new(bytes);
        let r = &mut reader;
        let erb_mu0 = r.tensor("erb_norm.init", &[BINS]);
        let spec_s0 = r.tensor("spec_norm.init", &[DF]);
        let erb_conv0_w = r.tensor("enc.erb_conv0.w", &[3, 3, C]);
        let erb_conv0_b = r.tensor("enc.erb_conv0.b", &[C]);
        let erb_convs = [
            SepConv::read(r, "enc.erb_conv1", 3),
            SepConv::read(r, "enc.erb_conv2", 2),
            SepConv::read(r, "enc.erb_conv3", 2),
        ];
        let dprnn_erb = [
            DprnnBlock::read(r, "enc.dprnn_erb.0"),
            DprnnBlock::read(r, "enc.dprnn_erb.1"),
        ];
        let df_conv0_w = r.tensor("enc.df_conv0.w", &[3, 3, C]);
        let df_conv0_pw = Dense::read(r, "enc.df_conv0.pw", C, C);
        let df_conv1 = SepConv::read(r, "enc.df_conv1", 2);
        let dprnn_df = [
            DprnnBlock::read(r, "enc.dprnn_df.0"),
            DprnnBlock::read(r, "enc.dprnn_df.1"),
        ];
        let erb_fc_emb = Grouped::read(r, "enc.erb_fc_emb", 32, E3 * C, EMB);
        let df_fc_emb = Grouped::read(r, "enc.df_fc_emb", 32, D1 * C, EMB);
        let enc_gru = SqueezedGru::read(r, "enc.emb_gru", 2 * EMB, 16, 1, Some((16, EMB)));
        let dec_gru = SqueezedGru::read(r, "erb_dec.emb_gru", EMB, 16, 2, Some((16, EMB)));
        let dec_fc_emb = Grouped::read(r, "erb_dec.erb_fc_emb", 32, EMB, E3 * C);
        let p3 = Pathway::read(r, "erb_dec.conv3p");
        let t3 = SubPixel::read(r, "erb_dec.convt3", 2);
        let p2 = Pathway::read(r, "erb_dec.conv2p");
        let t2 = SubPixel::read(r, "erb_dec.convt2", 2);
        let p1 = Pathway::read(r, "erb_dec.conv1p");
        let t1 = SubPixel::read(r, "erb_dec.convt1", 3);
        let p0 = Pathway::read(r, "erb_dec.conv0p");
        let mask_w = r.tensor("erb_dec.conv0_out.w", &[3, C]);
        let mask_b = r.tensor("erb_dec.conv0_out.b", &[1]);
        let df_gru = SqueezedGru::read(r, "df_dec.df_gru", EMB, 8, 2, None);
        let df_skip = Grouped::read(r, "df_dec.df_skip", 16, EMB, HID);
        let df_out = Grouped::read(r, "df_dec.df_out", 16, HID, DF * COEFS);
        let convp_w = r.tensor("df_dec.df_convp.w", &[ORDER, 2, C / 2, ORDER]);
        let convp_pw = Dense::read(r, "df_dec.df_convp.pw", COEFS, COEFS);
        reader.finish();
        Weights {
            erb_mu0,
            spec_s0,
            erb_conv0_w,
            erb_conv0_b,
            erb_convs,
            dprnn_erb,
            df_conv0_w,
            df_conv0_pw,
            df_conv1,
            dprnn_df,
            erb_fc_emb,
            df_fc_emb,
            enc_gru,
            dec_gru,
            dec_fc_emb,
            pathways: [p3, p2, p1, p0],
            upsample: [t3, t2, t1],
            mask_w,
            mask_b,
            df_gru,
            df_skip,
            df_out,
            convp_w,
            convp_pw,
        }
    }
}

/// A running stream: the weights, the state between frames and the per-frame scratch.
pub(super) struct Network {
    weights: Weights,
    state: Box<State>,
    scratch: Box<Scratch>,
}

impl Network {
    /// Reads `weights`, a file as [`super::weights`] lays it out.
    pub fn new(weights: &[u8]) -> Network {
        let weights = Weights::read(weights);
        Network {
            state: State::new(&weights),
            scratch: Scratch::new(),
            weights,
        }
    }

    /// One frame, in place: `spec` is the unnormalised FFT of the windowed input, and becomes
    /// the enhanced spectrum.
    pub fn process(&mut self, spec: &mut Spectrum) {
        self.scratch.spec = *spec;
        for stage in STAGES {
            stage(&self.weights, &mut self.state, &mut self.scratch);
        }
        *spec = self.scratch.spec;
    }
}

type Stage = fn(&Weights, &mut State, &mut Scratch);

/// One frame, in order. Each stage reads what earlier stages left in the scratch.
const STAGES: [Stage; 18] = [
    features,
    magnitude_convs,
    |w, _, s| w.dprnn_erb[0].across_frequency(&mut s.x3, &mut s.dprnn),
    |w, st, s| w.dprnn_erb[0].across_time(&mut s.x3, &mut st.dprnn_erb[0], &mut s.dprnn),
    |w, _, s| w.dprnn_erb[1].across_frequency(&mut s.x3, &mut s.dprnn),
    |w, st, s| w.dprnn_erb[1].across_time(&mut s.x3, &mut st.dprnn_erb[1], &mut s.dprnn),
    spectrum_convs,
    |w, _, s| w.dprnn_df[0].across_frequency(&mut s.c1, &mut s.dprnn),
    |w, st, s| w.dprnn_df[0].across_time(&mut s.c1, &mut st.dprnn_df[0], &mut s.dprnn),
    |w, _, s| w.dprnn_df[1].across_frequency(&mut s.c1, &mut s.dprnn),
    |w, st, s| w.dprnn_df[1].across_time(&mut s.c1, &mut st.dprnn_df[1], &mut s.dprnn),
    embedding,
    mask_gru,
    mask_upsampling,
    mask_out,
    filter_gru,
    filter_pathway,
    apply,
];

fn features(_: &Weights, st: &mut State, s: &mut Scratch) {
    s.spec.iter_mut().for_each(|v| *v *= WNORM);
    let spec = &s.spec;
    let feat = st.mag_feat.push();
    for ((f, mu), bin) in feat
        .iter_mut()
        .zip(&mut st.erb_mu)
        .zip(spec.chunks_exact(2))
    {
        let mag = (bin[0] * bin[0] + bin[1] * bin[1]).sqrt();
        let db = (mag + 1e-10).ln() / std::f32::consts::LN_10 * 10.0;
        *mu = *mu * ALPHA + db * BETA;
        *f = (db - *mu) / 40.0;
    }
    let feat = st.spec_feat.push();
    for ((f, s), bin) in feat
        .chunks_exact_mut(2)
        .zip(&mut st.spec_s)
        .zip(spec.chunks_exact(2))
    {
        let mag = (bin[0] * bin[0] + bin[1] * bin[1]).sqrt();
        *s = *s * ALPHA + mag * BETA;
        let denom = (*s + 1e-12).sqrt();
        f[0] = bin[0] / denom;
        f[1] = bin[1] / denom;
    }
}

fn magnitude_convs(w: &Weights, st: &mut State, s: &mut Scratch) {
    for (f, row) in s.e0.chunks_exact_mut(C).enumerate() {
        row.fill(0.0);
        for t in 0..3 {
            let x = st.mag_feat.get(t);
            for k in 0..3 {
                let fi = (f + k).wrapping_sub(1);
                if fi < MAG {
                    let wk = &w.erb_conv0_w[(t * 3 + k) * C..][..C];
                    for (o, wv) in row.iter_mut().zip(wk) {
                        *o += x[fi] * wv;
                    }
                }
            }
        }
        for (o, b) in row.iter_mut().zip(&w.erb_conv0_b[..]) {
            *o = (*o + b).max(0.0);
        }
    }
    w.erb_convs[0].forward(&s.e0, &mut s.tmp, &mut s.e1);
    w.erb_convs[1].forward(&s.e1, &mut s.tmp, &mut s.e2);
    w.erb_convs[2].forward(&s.e2, &mut s.tmp, &mut s.e3);
    s.x3.copy_from_slice(&s.e3);
}

fn spectrum_convs(w: &Weights, st: &mut State, s: &mut Scratch) {
    let tmp = &mut s.tmp[..DF * C];
    for (f, row) in tmp.chunks_exact_mut(C).enumerate() {
        row.fill(0.0);
        for t in 0..3 {
            let x = st.spec_feat.get(t);
            for k in 0..3 {
                let fi = (f + k).wrapping_sub(1);
                if fi < DF {
                    let wk = &w.df_conv0_w[(t * 3 + k) * C..][..C];
                    let (re, im) = row.split_at_mut(C / 2);
                    for (o, wv) in re.iter_mut().zip(&wk[..C / 2]) {
                        *o += x[2 * fi] * wv;
                    }
                    for (o, wv) in im.iter_mut().zip(&wk[C / 2..]) {
                        *o += x[2 * fi + 1] * wv;
                    }
                }
            }
        }
    }
    let c0 = st.c0.push();
    w.df_conv0_pw.forward(tmp, c0);
    relu(c0);
    w.df_conv1
        .forward(st.c0.get(ORDER - 1), &mut s.tmp, &mut s.c1);
}

fn embedding(w: &Weights, st: &mut State, s: &mut Scratch) {
    let (erb, df) = s.emb_in.split_at_mut(EMB);
    w.erb_fc_emb.forward(&s.x3, erb);
    w.df_fc_emb.forward(&s.c1, df);
    relu(&mut s.emb_in);
    w.enc_gru
        .forward(&s.emb_in, &mut st.enc_gru, &mut s.gru, &mut s.emb);
}

fn mask_gru(w: &Weights, st: &mut State, s: &mut Scratch) {
    w.dec_gru
        .forward(&s.emb, &mut st.dec_gru, &mut s.gru, &mut s.dec_emb);
    w.dec_fc_emb.forward(&s.dec_emb, &mut s.d3);
    relu(&mut s.d3);
}

fn mask_upsampling(w: &Weights, _: &mut State, s: &mut Scratch) {
    w.pathways[0].forward(&s.e3, &mut s.d3);
    w.upsample[0].forward(&s.d3, &mut s.tmp, &mut s.d2);
    w.pathways[1].forward(&s.e2, &mut s.d2);
    w.upsample[1].forward(&s.d2, &mut s.tmp, &mut s.d1);
    w.pathways[2].forward(&s.e1, &mut s.d1);
    w.upsample[2].forward(&s.d1, &mut s.tmp, &mut s.d0);
    w.pathways[3].forward(&s.e0, &mut s.d0);
}

fn mask_out(w: &Weights, _: &mut State, s: &mut Scratch) {
    for f in 0..MAG {
        let mut acc = 0.0;
        for (k, wk) in w.mask_w.chunks_exact(C).enumerate() {
            let fi = (f + k).wrapping_sub(1);
            if fi < MAG {
                for (x, wv) in s.d0[fi * C..fi * C + C].iter().zip(wk) {
                    acc += x * wv;
                }
            }
        }
        s.mask[f] = sigmoid(acc + w.mask_b[0]);
    }
    s.mask[MAG] = s.mask[MAG - 2];
}

fn filter_gru(w: &Weights, st: &mut State, s: &mut Scratch) {
    w.df_gru
        .forward(&s.emb, &mut st.df_gru, &mut s.gru, &mut s.df_hidden);
    w.df_skip.forward(&s.emb, &mut s.df_skip);
    for (h, k) in s.df_hidden.iter_mut().zip(&s.df_skip) {
        *h += k;
    }
    w.df_out.forward(&s.df_hidden, &mut s.df_out);
    s.df_out.iter_mut().for_each(|v| *v = tanh(*v));
}

fn filter_pathway(w: &Weights, st: &mut State, s: &mut Scratch) {
    for (f, out) in s.convp_raw.chunks_exact_mut(COEFS).enumerate() {
        out.fill(0.0);
        for t in 0..ORDER {
            let x = &st.c0.get(t)[f * C..f * C + C];
            for (g, (xg, og)) in x
                .chunks_exact(C / 2)
                .zip(out.chunks_exact_mut(ORDER))
                .enumerate()
            {
                let wg = &w.convp_w[(t * 2 + g) * (C / 2) * ORDER..][..(C / 2) * ORDER];
                for (xv, wi) in xg.iter().zip(wg.chunks_exact(ORDER)) {
                    for (o, wv) in og.iter_mut().zip(wi) {
                        *o += xv * wv;
                    }
                }
            }
        }
    }
    w.convp_pw.forward(&s.convp_raw, &mut s.convp);
    relu(&mut s.convp);
    let coefs = st.coefs.push();
    for ((c, a), b) in coefs.iter_mut().zip(&s.df_out).zip(&s.convp) {
        *c = a + b;
    }
}

/// The mask on the spectrum from two frames back, then the deep filter on its low bins. The
/// result replaces the frame's spectrum in the scratch.
fn apply(_: &Weights, st: &mut State, s: &mut Scratch) {
    st.spec.push().copy_from_slice(&s.spec);
    let delayed = st.spec.get(0);
    let masked = st.masked.push();
    for ((m, x), g) in masked
        .chunks_exact_mut(2)
        .zip(delayed.chunks_exact(2))
        .zip(&s.mask)
    {
        m[0] = x[0] * g;
        m[1] = x[1] * g;
    }
    let out = &mut s.spec;
    let coefs = st.coefs.get(0);
    for f in 0..DF {
        let (mut rr, mut ii, mut ri, mut ir) = (0.0, 0.0, 0.0, 0.0);
        for n in 0..ORDER {
            let x = &st.masked.get(n)[2 * f..2 * f + 2];
            let c = &coefs[f * COEFS + 2 * n..][..2];
            rr += x[0] * c[0];
            ii += x[1] * c[1];
            ri += x[0] * c[1];
            ir += x[1] * c[0];
        }
        out[2 * f] = rr - ii;
        out[2 * f + 1] = ri + ir;
    }
    out[2 * DF..].copy_from_slice(&st.masked.get(2)[2 * DF..]);
    out.iter_mut().for_each(|v| *v *= 1.0 / WNORM);
}
