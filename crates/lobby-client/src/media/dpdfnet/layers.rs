//! The network's building blocks. Feature maps are channels-last: `[frequency][channel]`.

use super::weights::Reader;

/// Channels in every feature map.
pub(super) const C: usize = 64;
/// The width of every GRU in the embedding and the decoders.
const HID: usize = 256;

/// `e^y - 1`, branch-free so loops over it vectorise: `y = n ln2 + r` with `|r| <= ln2 / 2`,
/// then `2^n (e^r - 1) + (2^n - 1)`, which stays accurate near zero where `exp(y) - 1` would not.
#[inline(always)]
fn exp_m1(y: f32) -> f32 {
    // Adding 1.5 * 2^23 rounds to an integer, which then sits in the low mantissa bits.
    const ROUND: f32 = 12_582_912.0;
    const LN2_HI: f32 = 0.693_359_4;
    const LN2_LO: f32 = -2.121_944_4e-4;
    let y = y.clamp(-87.0, 88.0);
    let t = y * std::f32::consts::LOG2_E + ROUND;
    let n = t - ROUND;
    let r = y - n * LN2_HI - n * LN2_LO;
    let p = r
        * (1.0
            + r * (1.0 / 2.0
                + r * (1.0 / 6.0
                    + r * (1.0 / 24.0
                        + r * (1.0 / 120.0 + r * (1.0 / 720.0 + r * (1.0 / 5040.0)))))));
    let scale = f32::from_bits(t.to_bits().wrapping_sub(ROUND.to_bits()).wrapping_add(127) << 23);
    scale * p + (scale - 1.0)
}

#[inline(always)]
pub(super) fn sigmoid(x: f32) -> f32 {
    1.0 / (2.0 + exp_m1(-x))
}

#[inline(always)]
pub(super) fn tanh(x: f32) -> f32 {
    let e = exp_m1(2.0 * x);
    e / (e + 2.0)
}

pub(super) fn relu(x: &mut [f32]) {
    x.iter_mut().for_each(|v| *v = v.max(0.0));
}

/// `y = x · w` for each row of `x`, with `w` stored `[in][out]`. Each output sums its inputs
/// in order, so the result does not depend on the tiling.
fn matmul(x: &[f32], w: &[f32], n_out: usize, y: &mut [f32]) {
    let n_in = w.len() / n_out;
    let rows_in = x.len() / n_in;
    let (pairs, last) = (rows_in / 2, rows_in % 2 == 1);
    for (x, y) in x
        .chunks_exact(2 * n_in)
        .zip(y.chunks_exact_mut(2 * n_out))
        .take(pairs)
    {
        rows::<2, 16>(x, w, y);
    }
    if last {
        let (x, y) = (
            &x[(rows_in - 1) * n_in..rows_in * n_in],
            &mut y[(rows_in - 1) * n_out..rows_in * n_out],
        );
        rows::<1, 32>(x, w, y);
    }
}

/// `R` rows at once, in column tiles of `W`, then narrower tiles for what is left.
#[inline(always)]
fn rows<const R: usize, const W: usize>(x: &[f32], w: &[f32], y: &mut [f32]) {
    let n_out = y.len() / R;
    let mut o = 0;
    while o + W <= n_out {
        tile::<R, W>(x, w, y, o);
        o += W;
    }
    while o + 4 <= n_out {
        tile::<R, 4>(x, w, y, o);
        o += 4;
    }
    while o < n_out {
        tile::<R, 1>(x, w, y, o);
        o += 1;
    }
}

/// Columns `o..o + W` of `R` rows, summed in registers.
#[inline(always)]
fn tile<const R: usize, const W: usize>(x: &[f32], w: &[f32], y: &mut [f32], o: usize) {
    let n_in = x.len() / R;
    let n_out = y.len() / R;
    let mut acc = [[0.0_f32; W]; R];
    for (k, wk) in w.chunks_exact(n_out).enumerate() {
        let wk: &[f32; W] = wk[o..o + W].try_into().expect("a whole tile");
        for (r, acc) in acc.iter_mut().enumerate() {
            let xv = x[r * n_in + k];
            for (a, wv) in acc.iter_mut().zip(wk) {
                *a += xv * wv;
            }
        }
    }
    for (r, acc) in acc.iter().enumerate() {
        y[r * n_out + o..r * n_out + o + W].copy_from_slice(acc);
    }
}

fn add_bias(y: &mut [f32], b: &[f32]) {
    for (o, bv) in y.iter_mut().zip(b) {
        *o += bv;
    }
}

/// `y = x · W + b` for every row of `x`.
pub(super) struct Dense {
    w: Box<[f32]>,
    b: Box<[f32]>,
}

impl Dense {
    pub fn read(r: &mut Reader, name: &str, n_in: usize, n_out: usize) -> Dense {
        Dense {
            w: r.tensor(&format!("{name}.w"), &[n_in, n_out]),
            b: r.tensor(&format!("{name}.b"), &[n_out]),
        }
    }

    pub fn forward(&self, x: &[f32], y: &mut [f32]) {
        let n_out = self.b.len();
        let rows = x.len() / (self.w.len() / n_out);
        let y = &mut y[..rows * n_out];
        matmul(x, &self.w, n_out, y);
        for yr in y.chunks_exact_mut(n_out) {
            add_bias(yr, &self.b);
        }
    }
}

/// A linear layer split into independent groups, weights `[group][in][out]`.
pub(super) struct Grouped {
    w: Box<[f32]>,
    b: Box<[f32]>,
    groups: usize,
}

impl Grouped {
    pub fn read(r: &mut Reader, name: &str, groups: usize, n_in: usize, n_out: usize) -> Grouped {
        Grouped {
            w: r.tensor(
                &format!("{name}.w"),
                &[groups, n_in / groups, n_out / groups],
            ),
            b: r.tensor(&format!("{name}.b"), &[n_out]),
            groups,
        }
    }

    pub fn forward(&self, x: &[f32], y: &mut [f32]) {
        let (gi, go) = (x.len() / self.groups, y.len() / self.groups);
        for ((xg, yg), wg) in x
            .chunks_exact(gi)
            .zip(y.chunks_exact_mut(go))
            .zip(self.w.chunks_exact(gi * go))
        {
            matmul(xg, wg, go, yg);
        }
        add_bias(y, &self.b);
    }
}

/// A GRU cell with PyTorch's gate order (reset, update, new) and the reset gate applied after
/// the hidden projection.
struct Gru {
    input: Dense,
    hidden: Dense,
}

impl Gru {
    fn read(r: &mut Reader, name: &str, n_in: usize, n_hid: usize) -> Gru {
        Gru {
            input: Dense::read(r, &format!("{name}.ih"), n_in, 3 * n_hid),
            hidden: Dense::read(r, &format!("{name}.hh"), n_hid, 3 * n_hid),
        }
    }

    /// One step for every row of `x`; `h` holds the state rows in and the new state out.
    /// `gi` and `gh` are scratch of three state widths per row.
    fn step(&self, x: &[f32], h: &mut [f32], gi: &mut [f32], gh: &mut [f32]) {
        let n = self.hidden.b.len() / 3;
        let rows = x.len() / (self.input.w.len() / self.input.b.len());
        let (gi, gh) = (&mut gi[..rows * 3 * n], &mut gh[..rows * 3 * n]);
        self.input.forward(x, gi);
        self.hidden.forward(h, gh);
        for ((h, gi), gh) in h
            .chunks_exact_mut(n)
            .zip(gi.chunks_exact(3 * n))
            .zip(gh.chunks_exact(3 * n))
        {
            update(gi, gh, h);
        }
    }
}

fn update(gi: &[f32], gh: &[f32], h: &mut [f32]) {
    let n = h.len();
    let (ir, iz, inew) = (&gi[..n], &gi[n..2 * n], &gi[2 * n..3 * n]);
    let (hr, hz, hnew) = (&gh[..n], &gh[n..2 * n], &gh[2 * n..3 * n]);
    for j in 0..n {
        let r = sigmoid(hr[j] + ir[j]);
        let z = sigmoid(hz[j] + iz[j]);
        let candidate = tanh(inew[j] + hnew[j] * r);
        h[j] = (h[j] - candidate) * z + candidate;
    }
}

struct LayerNorm {
    w: Box<[f32]>,
    b: Box<[f32]>,
}

impl LayerNorm {
    const EPS: f32 = 1e-5;

    fn read(r: &mut Reader, name: &str, n: usize) -> LayerNorm {
        LayerNorm {
            w: r.tensor(&format!("{name}.w"), &[n]),
            b: r.tensor(&format!("{name}.b"), &[n]),
        }
    }

    fn forward(&self, x: &mut [f32]) {
        let n = self.w.len();
        for row in x.chunks_exact_mut(n) {
            let mean = row.iter().sum::<f32>() / n as f32;
            let var = row.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / n as f32;
            let inv = 1.0 / (var + Self::EPS).sqrt();
            for ((v, w), b) in row.iter_mut().zip(&self.w[..]).zip(&self.b[..]) {
                *v = (*v - mean) * inv * w + b;
            }
        }
    }
}

/// A kernel-3 convolution along frequency with one filter per channel, padded by one bin on
/// each side. `w` is `[tap][channel]`; output row `fo` reads input rows `fo * stride - 1 ..`.
fn depthwise(x: &[f32], w: &[f32], stride: usize, y: &mut [f32]) {
    let n_in = x.len() / C;
    for (fo, yr) in y.chunks_exact_mut(C).enumerate() {
        yr.fill(0.0);
        for (k, wk) in w.chunks_exact(C).enumerate() {
            let fi = (fo * stride + k).wrapping_sub(1);
            if fi < n_in {
                for ((o, xv), wv) in yr.iter_mut().zip(&x[fi * C..fi * C + C]).zip(wk) {
                    *o += xv * wv;
                }
            }
        }
    }
}

/// Depthwise kernel-3 convolution, then a pointwise convolution with bias, then ReLU.
pub(super) struct SepConv {
    dw: Box<[f32]>,
    pw: Dense,
    stride: usize,
}

impl SepConv {
    pub fn read(r: &mut Reader, name: &str, stride: usize) -> SepConv {
        SepConv {
            dw: r.tensor(&format!("{name}.dw"), &[3, C]),
            pw: Dense::read(r, &format!("{name}.pw"), C, C),
            stride,
        }
    }

    pub fn forward(&self, x: &[f32], tmp: &mut [f32], y: &mut [f32]) {
        let tmp = &mut tmp[..y.len()];
        depthwise(x, &self.dw, self.stride, tmp);
        self.pw.forward(tmp, y);
        relu(y);
    }
}

/// Sub-pixel upsampling along frequency by `S`: `S` depthwise convolutions whose outputs
/// interleave, `y[f * S + s] = conv_s(x)[f]`, then a pointwise convolution and ReLU.
pub(super) struct SubPixel {
    dw: Vec<Box<[f32]>>,
    pw: Dense,
}

impl SubPixel {
    pub fn read(r: &mut Reader, name: &str, factor: usize) -> SubPixel {
        let dw = (0..factor)
            .map(|s| r.tensor(&format!("{name}.dw{s}"), &[3, C]))
            .collect();
        SubPixel {
            dw,
            pw: Dense::read(r, &format!("{name}.pw"), C, C),
        }
    }

    pub fn forward(&self, x: &[f32], tmp: &mut [f32], y: &mut [f32]) {
        let n_in = x.len() / C;
        let factor = self.dw.len();
        let tmp = &mut tmp[..y.len()];
        for (row, out) in tmp.chunks_exact_mut(C).enumerate() {
            let (f, s) = (row / factor, row % factor);
            out.fill(0.0);
            for (k, wk) in self.dw[s].chunks_exact(C).enumerate() {
                let fi = (f + k).wrapping_sub(1);
                if fi < n_in {
                    for ((o, xv), wv) in out.iter_mut().zip(&x[fi * C..fi * C + C]).zip(wk) {
                        *o += xv * wv;
                    }
                }
            }
        }
        self.pw.forward(tmp, y);
        relu(y);
    }
}

/// The encoder-to-decoder skip: a per-channel scale and bias, ReLU, plus the decoder's input.
pub(super) struct Pathway {
    scale: Box<[f32]>,
    bias: Box<[f32]>,
}

impl Pathway {
    pub fn read(r: &mut Reader, name: &str) -> Pathway {
        Pathway {
            scale: r.tensor(&format!("{name}.scale"), &[C]),
            bias: r.tensor(&format!("{name}.bias"), &[C]),
        }
    }

    /// `y = relu(skip * scale + bias) + y`
    pub fn forward(&self, skip: &[f32], y: &mut [f32]) {
        for (sr, yr) in skip.chunks_exact(C).zip(y.chunks_exact_mut(C)) {
            for (((s, o), a), b) in sr
                .iter()
                .zip(yr.iter_mut())
                .zip(&self.scale[..])
                .zip(&self.bias[..])
            {
                *o += (s * a + b).max(0.0);
            }
        }
    }
}

/// A grouped linear layer with ReLU, one or more GRU cells, then optionally a second grouped
/// linear layer with ReLU.
pub(super) struct SqueezedGru {
    linear_in: Grouped,
    cells: Vec<Gru>,
    linear_out: Option<Grouped>,
}

impl SqueezedGru {
    pub fn read(
        r: &mut Reader,
        name: &str,
        n_in: usize,
        groups_in: usize,
        cells: usize,
        out: Option<(usize, usize)>,
    ) -> SqueezedGru {
        SqueezedGru {
            linear_in: Grouped::read(r, &format!("{name}.linear_in"), groups_in, n_in, HID),
            cells: (0..cells)
                .map(|i| Gru::read(r, &format!("{name}.gru{i}"), HID, HID))
                .collect(),
            linear_out: out.map(|(groups, n_out)| {
                Grouped::read(r, &format!("{name}.linear_out"), groups, HID, n_out)
            }),
        }
    }

    /// `h` is each cell's state, one after another. `y` is [`HID`] wide without an output layer.
    pub fn forward(&self, x: &[f32], h: &mut [f32], s: &mut GruScratch, y: &mut [f32]) {
        self.linear_in.forward(x, &mut s.a);
        relu(&mut s.a);
        for (cell, h) in self.cells.iter().zip(h.chunks_exact_mut(HID)) {
            cell.step(&s.a, h, &mut s.gi, &mut s.gh);
            s.a.copy_from_slice(h);
        }
        match &self.linear_out {
            Some(linear) => {
                linear.forward(&s.a, y);
                relu(y);
            }
            None => y.copy_from_slice(&s.a),
        }
    }
}

pub(super) struct GruScratch {
    a: [f32; HID],
    gi: [f32; 3 * HID],
    gh: [f32; 3 * HID],
}

impl GruScratch {
    pub fn new() -> GruScratch {
        GruScratch {
            a: [0.0; HID],
            gi: [0.0; 3 * HID],
            gh: [0.0; 3 * HID],
        }
    }
}

/// One dual-path block over `F` frequency rows: a bidirectional GRU across frequency, then a
/// GRU across time per frequency row, each followed by a linear layer, layer norm and a
/// residual add.
pub(super) struct DprnnBlock {
    forward: Gru,
    backward: Gru,
    fc_intra: Dense,
    ln_intra: LayerNorm,
    inter: Gru,
    fc_inter: Dense,
    ln_inter: LayerNorm,
}

pub(super) struct DprnnScratch {
    gi: Box<[f32]>,
    gh: Box<[f32]>,
    seq: Box<[f32]>,
    tmp: Box<[f32]>,
}

impl DprnnScratch {
    pub fn new(rows: usize) -> DprnnScratch {
        let zeros = |n: usize| vec![0.0; n].into_boxed_slice();
        DprnnScratch {
            gi: zeros(rows * 3 * C),
            gh: zeros(rows * 3 * C),
            seq: zeros(rows * 2 * C),
            tmp: zeros(rows * C),
        }
    }
}

impl DprnnBlock {
    pub fn read(r: &mut Reader, name: &str) -> DprnnBlock {
        DprnnBlock {
            forward: Gru::read(r, &format!("{name}.intra.fwd"), C, C),
            backward: Gru::read(r, &format!("{name}.intra.bwd"), C, C),
            fc_intra: Dense::read(r, &format!("{name}.fc_intra"), 2 * C, C),
            ln_intra: LayerNorm::read(r, &format!("{name}.ln_intra"), C),
            inter: Gru::read(r, &format!("{name}.inter"), C, C),
            fc_inter: Dense::read(r, &format!("{name}.fc_inter"), C, C),
            ln_inter: LayerNorm::read(r, &format!("{name}.ln_inter"), C),
        }
    }

    /// The bidirectional GRU across frequency, its projection and the residual add. `x` is
    /// `[F][C]`, updated in place.
    pub fn across_frequency(&self, x: &mut [f32], s: &mut DprnnScratch) {
        let rows = x.len() / C;
        let (gi, gh) = (&mut s.gi[..rows * 3 * C], &mut s.gh[..3 * C]);
        let seq = &mut s.seq[..rows * 2 * C];
        let tmp = &mut s.tmp[..rows * C];
        for (dir, gru) in [&self.forward, &self.backward].into_iter().enumerate() {
            gru.input.forward(x, gi);
            let mut state = [0.0_f32; C];
            for i in 0..rows {
                let f = if dir == 0 { i } else { rows - 1 - i };
                gru.hidden.forward(&state, gh);
                update(&gi[f * 3 * C..(f + 1) * 3 * C], gh, &mut state);
                seq[f * 2 * C + dir * C..][..C].copy_from_slice(&state);
            }
        }
        self.fc_intra.forward(seq, tmp);
        self.ln_intra.forward(tmp);
        x.iter_mut().zip(tmp.iter()).for_each(|(a, b)| *a += b);
    }

    /// The GRU across time for every frequency row, its projection and the residual add. `h`
    /// is that GRU's state, `[F][C]`.
    pub fn across_time(&self, x: &mut [f32], h: &mut [f32], s: &mut DprnnScratch) {
        let tmp = &mut s.tmp[..x.len()];
        self.inter.step(x, h, &mut s.gi, &mut s.gh);
        self.fc_inter.forward(h, tmp);
        self.ln_inter.forward(tmp);
        x.iter_mut().zip(tmp.iter()).for_each(|(a, b)| *a += b);
    }
}
