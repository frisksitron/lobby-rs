//! `cargo xtask dpdfnet-weights <model.onnx>`: writes the DPDFNet weights `lobby-client` embeds
//! from Ceva's ONNX file, so the blob in the repository can be checked against its source. The
//! tensors are reordered for the engine and rounded to half floats, in the layout
//! `crates/lobby-client/src/media/dpdfnet/weights.rs` describes.
//!
//! ONNX is protobuf, and the export reads only the graph's nodes (name and inputs), its
//! initialisers and the model's metadata, so a small reader does instead of an ONNX crate.

use std::{collections::HashMap, fs, path::Path};

use sha2::{Digest as _, Sha256};

pub const BLOB: &str = "crates/lobby-client/src/media/dpdfnet/dpdfnet2.f16.bin";
/// ONNX's `TensorProto.DataType` for f32.
const FLOAT: u64 = 1;

/// Writes the blob and says what it holds.
pub fn run(workspace: &Path, model: &Path) -> Result<String, String> {
    let bytes = fs::read(model).map_err(|e| format!("{}: {e}", model.display()))?;
    let blob = export(&Onnx::parse(&bytes)?)?;
    let path = workspace.join(BLOB);
    let unchanged = fs::read(&path).is_ok_and(|old| old == blob);
    fs::write(&path, &blob).map_err(|e| format!("{BLOB}: {e}"))?;
    Ok(format!(
        "{BLOB}: {} bytes, sha256 {} ({}), from {} with sha256 {}",
        blob.len(),
        sha256(&blob),
        if unchanged { "unchanged" } else { "changed" },
        model.display(),
        sha256(&bytes),
    ))
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

enum Value<'a> {
    Int(u64),
    Bytes(&'a [u8]),
    Fixed32(u32),
}

fn take<'a>(b: &mut &'a [u8], n: usize) -> Result<&'a [u8], String> {
    let (head, rest) = b.split_at_checked(n).ok_or("protobuf ends early")?;
    *b = rest;
    Ok(head)
}

fn varint(b: &mut &[u8]) -> Result<u64, String> {
    let mut value = 0;
    for shift in (0..64).step_by(7) {
        let byte = take(b, 1)?[0];
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err("protobuf varint too long".into())
}

/// A protobuf message's fields, as (number, value), in order.
fn fields(mut b: &[u8]) -> Result<Vec<(u64, Value<'_>)>, String> {
    let mut out = Vec::new();
    while !b.is_empty() {
        let key = varint(&mut b)?;
        let value = match key & 7 {
            0 => Value::Int(varint(&mut b)?),
            1 => Value::Int(u64::from_le_bytes(take(&mut b, 8)?.try_into().unwrap())),
            2 => {
                let n = varint(&mut b)? as usize;
                Value::Bytes(take(&mut b, n)?)
            }
            5 => Value::Fixed32(u32::from_le_bytes(take(&mut b, 4)?.try_into().unwrap())),
            wire => return Err(format!("protobuf wire type {wire}")),
        };
        out.push((key >> 3, value));
    }
    Ok(out)
}

fn text(b: &[u8]) -> Result<&str, String> {
    std::str::from_utf8(b).map_err(|e| format!("ONNX name: {e}"))
}

/// A tensor: its dimensions and values.
struct T {
    dims: Vec<usize>,
    data: Vec<f32>,
}

/// What the export reads from the model, by name. Field numbers are ONNX's `onnx.proto`.
struct Onnx<'a> {
    /// Each node's inputs.
    nodes: HashMap<&'a str, Vec<&'a str>>,
    initialisers: HashMap<&'a str, T>,
    metadata: HashMap<&'a str, &'a str>,
}

impl<'a> Onnx<'a> {
    fn parse(model: &'a [u8]) -> Result<Onnx<'a>, String> {
        let mut onnx = Onnx {
            nodes: HashMap::new(),
            initialisers: HashMap::new(),
            metadata: HashMap::new(),
        };
        for (number, value) in fields(model)? {
            match (number, value) {
                (7, Value::Bytes(graph)) => onnx.graph(graph)?,
                (14, Value::Bytes(entry)) => {
                    let (mut key, mut value) = ("", "");
                    for field in fields(entry)? {
                        match field {
                            (1, Value::Bytes(b)) => key = text(b)?,
                            (2, Value::Bytes(b)) => value = text(b)?,
                            _ => {}
                        }
                    }
                    onnx.metadata.insert(key, value);
                }
                _ => {}
            }
        }
        Ok(onnx)
    }

    fn graph(&mut self, graph: &'a [u8]) -> Result<(), String> {
        for (number, value) in fields(graph)? {
            match (number, value) {
                (1, Value::Bytes(node)) => {
                    let (mut name, mut inputs) = ("", Vec::new());
                    for field in fields(node)? {
                        match field {
                            (1, Value::Bytes(b)) => inputs.push(text(b)?),
                            (3, Value::Bytes(b)) => name = text(b)?,
                            _ => {}
                        }
                    }
                    self.nodes.insert(name, inputs);
                }
                (5, Value::Bytes(tensor)) => {
                    if let Some((name, t)) = initialiser(tensor)? {
                        self.initialisers.insert(name, t);
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Input `ix` of the node `/model/{node}`, which must be an f32 initialiser.
    fn input(&self, node: &str, ix: usize) -> Result<&T, String> {
        let inputs = self.inputs(node)?;
        let name = inputs.get(ix).ok_or(format!("{node} has no input {ix}"))?;
        self.initialisers
            .get(name)
            .ok_or(format!("{node} input {ix} is not an f32 initialiser"))
    }

    /// The one input of `/model/{node}` that is an initialiser: an `Add` takes its bias on
    /// either side.
    fn constant_input(&self, node: &str) -> Result<&T, String> {
        let inputs = self.inputs(node)?;
        let ix = inputs
            .iter()
            .position(|i| self.initialisers.contains_key(i))
            .ok_or(format!("{node} has no initialiser"))?;
        self.input(node, ix)
    }

    fn inputs(&self, node: &str) -> Result<&Vec<&'a str>, String> {
        self.nodes
            .get(format!("/model/{node}").as_str())
            .ok_or(format!("no node /model/{node}"))
    }

    fn floats(&self, key: &str) -> Result<Vec<f32>, String> {
        let text = self.metadata.get(key).ok_or(format!("no metadata {key}"))?;
        text.split(',')
            .map(|x| x.trim().parse().map_err(|e| format!("{key}: {e}")))
            .collect()
    }
}

/// An initialiser's name and values, if it holds f32s.
fn initialiser(tensor: &[u8]) -> Result<Option<(&str, T)>, String> {
    let (mut name, mut data_type, mut dims) = ("", 0, Vec::new());
    let (mut raw, mut data) = (None, Vec::new());
    for field in fields(tensor)? {
        match field {
            (1, Value::Int(d)) => dims.push(d as usize),
            (1, Value::Bytes(mut packed)) => {
                while !packed.is_empty() {
                    dims.push(varint(&mut packed)? as usize);
                }
            }
            (2, Value::Int(t)) => data_type = t,
            (4, Value::Fixed32(v)) => data.push(f32::from_bits(v)),
            // Packed float_data is laid out as raw_data is.
            (4 | 9, Value::Bytes(b)) => raw = Some(b),
            (8, Value::Bytes(b)) => name = text(b)?,
            _ => {}
        }
    }
    if data_type != FLOAT {
        return Ok(None);
    }
    if let Some(raw) = raw {
        data = raw
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
            .collect();
    }
    Ok(Some((name, T { dims, data })))
}

/// `[rows][cols]` to `[cols][rows]`.
fn transpose(rows: usize, data: &[f32]) -> T {
    let cols = data.len() / rows;
    let mut out = vec![0.0; data.len()];
    for r in 0..rows {
        for c in 0..cols {
            out[c * rows + r] = data[r * cols + c];
        }
    }
    T {
        dims: vec![cols, rows],
        data: out,
    }
}

/// `[out][1][kt][kf]` to `[kt][kf][out]`.
fn conv_taps(w: &T) -> Vec<f32> {
    let (out, kt, kf) = (w.dims[0], w.dims[2], w.dims[3]);
    let mut data = vec![0.0; w.data.len()];
    for o in 0..out {
        for t in 0..kt {
            for k in 0..kf {
                data[(t * kf + k) * out + o] = w.data[(o * kt + t) * kf + k];
            }
        }
    }
    data
}

/// `v` as the nearest half float, ties to even.
fn half(v: f32) -> u16 {
    let bits = v.to_bits();
    let sign = (bits >> 16) as u16 & 0x8000;
    let exp = (bits >> 23 & 0xff) as i32 - 127 + 15;
    let man = bits & 0x7f_ffff;
    if bits & 0x7f80_0000 == 0x7f80_0000 {
        // Infinity stays infinity, and a NaN keeps a mantissa bit so that it stays a NaN.
        return sign | 0x7c00 | if man == 0 { 0 } else { 0x200 };
    }
    if exp >= 0x1f {
        return sign | 0x7c00;
    }
    if exp < -10 {
        return sign;
    }
    // The bits that stay, above the `shift` that are rounded away: a subnormal half keeps the
    // hidden bit and loses more. A carry out of the mantissa raises the exponent, as it should.
    let (base, sig, shift) = if exp > 0 {
        ((exp as u32) << 10, man, 13)
    } else {
        (0, man | 0x80_0000, (14 - exp) as u32)
    };
    let (kept, rest, halfway) = (sig >> shift, sig & ((1 << shift) - 1), 1 << (shift - 1));
    let up = rest > halfway || (rest == halfway && kept & 1 == 1);
    sign | (base + kept + u32::from(up)) as u16
}

struct Export<'a> {
    onnx: &'a Onnx<'a>,
    out: Vec<u8>,
}

impl Export<'_> {
    fn put(&mut self, name: &str, dims: &[usize], data: &[f32]) {
        assert_eq!(dims.iter().product::<usize>(), data.len(), "{name}");
        self.out.push(name.len() as u8);
        self.out.extend(name.as_bytes());
        self.out.extend((dims.len() as u32).to_le_bytes());
        for d in dims {
            self.out.extend((*d as u32).to_le_bytes());
        }
        for v in data {
            self.out.extend(half(*v).to_le_bytes());
        }
    }

    fn put_t(&mut self, name: &str, t: &T) {
        self.put(name, &t.dims, &t.data);
    }

    /// `y = x · W + b` from a Gemm (`W` is `[out][in]`) or a pointwise convolution with its
    /// folded bias (`[out][in][1][1]`).
    fn gemm(&mut self, name: &str, node: &str) -> Result<(), String> {
        let w = self.onnx.input(node, 1)?;
        self.put_t(&format!("{name}.w"), &transpose(w.dims[0], &w.data));
        self.put_t(&format!("{name}.b"), self.onnx.input(node, 2)?);
        Ok(())
    }

    /// A depthwise kernel-3 convolution `[C][1][1][3]` as `[tap][C]`.
    fn depthwise(&mut self, name: &str, node: &str) -> Result<(), String> {
        let w = self.onnx.input(node, 1)?;
        self.put_t(name, &transpose(w.dims[0], &w.data));
        Ok(())
    }

    fn grouped(&mut self, name: &str, path: &str) -> Result<(), String> {
        self.put_t(
            &format!("{name}.w"),
            self.onnx.input(&format!("{path}/MatMul"), 1)?,
        );
        let b = self.onnx.constant_input(&format!("{path}/Add"))?;
        self.put_t(&format!("{name}.b"), b);
        Ok(())
    }

    fn gru_cell(&mut self, name: &str, path: &str) -> Result<(), String> {
        self.gemm(&format!("{name}.ih"), &format!("{path}/grucell/Gemm"))?;
        self.gemm(&format!("{name}.hh"), &format!("{path}/grucell/Gemm_1"))
    }

    /// ONNX's bidirectional GRU (gates update, reset, new) as two PyTorch-order cells.
    fn bidirectional(&mut self, name: &str, node: &str) -> Result<(), String> {
        let g = self.onnx;
        let (w, r, b) = (g.input(node, 1)?, g.input(node, 2)?, g.input(node, 3)?);
        if g.input(node, 5)?.data.iter().any(|v| *v != 0.0) {
            return Err(format!("{node} starts from a non-zero state"));
        }
        let (h, n_in) = (w.dims[1] / 3, w.dims[2]);
        let reorder = |m: &[f32], width: usize| -> Vec<f32> {
            [1, 0, 2]
                .iter()
                .flat_map(|gate| m[gate * h * width..(gate + 1) * h * width].to_vec())
                .collect()
        };
        for (d, dir) in ["fwd", "bwd"].iter().enumerate() {
            let wd = &w.data[d * 3 * h * n_in..(d + 1) * 3 * h * n_in];
            let rd = &r.data[d * 3 * h * h..(d + 1) * 3 * h * h];
            let bd = &b.data[d * 6 * h..(d + 1) * 6 * h];
            let wi = transpose(3 * h, &reorder(wd, n_in));
            let wh = transpose(3 * h, &reorder(rd, h));
            self.put_t(&format!("{name}.{dir}.ih.w"), &wi);
            self.put(
                &format!("{name}.{dir}.ih.b"),
                &[3 * h],
                &reorder(&bd[..3 * h], 1),
            );
            self.put_t(&format!("{name}.{dir}.hh.w"), &wh);
            self.put(
                &format!("{name}.{dir}.hh.b"),
                &[3 * h],
                &reorder(&bd[3 * h..], 1),
            );
        }
        Ok(())
    }

    fn dprnn(&mut self, name: &str, path: &str) -> Result<(), String> {
        self.bidirectional(&format!("{name}.intra"), &format!("{path}/intra_gru/GRU"))?;
        self.grouped(&format!("{name}.fc_intra"), &format!("{path}/fc_intra"))?;
        let ln = format!("{path}/ln_intra/LayerNormalization");
        self.layer_norm(&format!("{name}.ln_intra"), &ln)?;
        self.gru_cell(&format!("{name}.inter"), &format!("{path}/inter_gru"))?;
        self.gemm(
            &format!("{name}.fc_inter"),
            &format!("{path}/fc_inter/Gemm"),
        )?;
        let ln = format!("{path}/ln_inter/LayerNormalization");
        self.layer_norm(&format!("{name}.ln_inter"), &ln)
    }

    fn layer_norm(&mut self, name: &str, node: &str) -> Result<(), String> {
        self.put_t(&format!("{name}.w"), self.onnx.input(node, 1)?);
        self.put_t(&format!("{name}.b"), self.onnx.input(node, 2)?);
        Ok(())
    }

    fn sep_conv(&mut self, name: &str, path: &str, conv: &str) -> Result<(), String> {
        self.depthwise(&format!("{name}.dw"), &format!("{path}/{conv}.0/Conv"))?;
        self.gemm(&format!("{name}.pw"), &format!("{path}/{conv}.1/Conv"))
    }

    fn squeezed(&mut self, name: &str, path: &str, cells: usize, out: bool) -> Result<(), String> {
        let linear_in = format!("{path}/linear_in/linear_in.0");
        self.grouped(&format!("{name}.linear_in"), &linear_in)?;
        for i in 0..cells {
            self.gru_cell(&format!("{name}.gru{i}"), &format!("{path}/gru.{i}"))?;
        }
        if out {
            let linear_out = format!("{path}/linear_out/linear_out.0");
            self.grouped(&format!("{name}.linear_out"), &linear_out)?;
        }
        Ok(())
    }
}

/// The blob, in the order the engine reads it.
fn export(g: &Onnx) -> Result<Vec<u8>, String> {
    if g.input("erb_norm/Div", 1)?.data.iter().any(|v| *v != 40.0) {
        return Err("the magnitude features are no longer divided by 40".into());
    }
    let mut w = Export {
        onnx: g,
        out: [b"DPDF".as_slice(), &1_u32.to_le_bytes()].concat(),
    };
    w.put("erb_norm.init", &[481], &g.floats("erb_norm_init")?);
    w.put("spec_norm.init", &[96], &g.floats("spec_norm_init")?);
    let conv0 = "enc/erb_conv0/erb_conv0.1/Conv";
    w.put(
        "enc.erb_conv0.w",
        &[3, 3, 64],
        &conv_taps(g.input(conv0, 1)?),
    );
    w.put_t("enc.erb_conv0.b", g.input(conv0, 2)?);
    for i in 1..=3 {
        let (name, conv) = (format!("enc.erb_conv{i}"), format!("erb_conv{i}"));
        w.sep_conv(&name, &format!("enc/erb_conv{i}"), &conv)?;
    }
    for b in 0..2 {
        w.dprnn(
            &format!("enc.dprnn_erb.{b}"),
            &format!("enc/dprnn_erb/blocks.{b}"),
        )?;
    }
    let re = conv_taps(g.input("enc/df_conv0/df_conv0.1/convs.0/Conv", 1)?);
    let im = conv_taps(g.input("enc/df_conv0/df_conv0.1/convs.1/Conv", 1)?);
    let taps: Vec<f32> = re
        .chunks_exact(32)
        .zip(im.chunks_exact(32))
        .flat_map(|(a, b)| [a, b].concat())
        .collect();
    w.put("enc.df_conv0.w", &[3, 3, 64], &taps);
    w.gemm("enc.df_conv0.pw", "enc/df_conv0/df_conv0.2/Conv")?;
    w.sep_conv("enc.df_conv1", "enc/df_conv1", "df_conv1")?;
    for b in 0..2 {
        w.dprnn(
            &format!("enc.dprnn_df.{b}"),
            &format!("enc/dprnn_df/blocks.{b}"),
        )?;
    }
    w.grouped("enc.erb_fc_emb", "enc/erb_fc_emb/erb_fc_emb.0")?;
    w.grouped("enc.df_fc_emb", "enc/df_fc_emb/df_fc_emb.0")?;
    w.squeezed("enc.emb_gru", "enc/emb_gru", 1, true)?;
    w.squeezed("erb_dec.emb_gru", "erb_dec/emb_gru", 2, true)?;
    w.grouped("erb_dec.erb_fc_emb", "erb_dec/erb_fc_emb/erb_fc_emb.0")?;
    for (p, factor) in [(3, 2), (2, 2), (1, 3)] {
        let node = format!("erb_dec/conv{p}p/conv{p}p.0/Conv");
        w.put(
            &format!("erb_dec.conv{p}p.scale"),
            &[64],
            &g.input(&node, 1)?.data,
        );
        w.put(
            &format!("erb_dec.conv{p}p.bias"),
            &[64],
            &g.input(&node, 2)?.data,
        );
        for s in 0..factor {
            let node = format!("erb_dec/convt{p}/convt{p}.0/convs.{s}/Conv");
            w.depthwise(&format!("erb_dec.convt{p}.dw{s}"), &node)?;
        }
        let node = format!("erb_dec/convt{p}/convt{p}.1/Conv");
        w.gemm(&format!("erb_dec.convt{p}.pw"), &node)?;
    }
    let node = "erb_dec/conv0p/conv0p.0/Conv";
    w.put("erb_dec.conv0p.scale", &[64], &g.input(node, 1)?.data);
    w.put("erb_dec.conv0p.bias", &[64], &g.input(node, 2)?.data);
    let mask = "erb_dec/conv0_out/conv0_out.0/Conv";
    w.put_t(
        "erb_dec.conv0_out.w",
        &transpose(64, &g.input(mask, 1)?.data),
    );
    w.put("erb_dec.conv0_out.b", &[1], &g.input(mask, 2)?.data);
    w.squeezed("df_dec.df_gru", "df_dec/df_gru", 2, false)?;
    w.grouped("df_dec.df_skip", "df_dec/df_skip")?;
    w.grouped("df_dec.df_out", "df_dec/df_out/df_out.0")?;
    let group = |i| g.input(&format!("df_dec/df_convp/df_convp.1/convs.{i}/Conv"), 1);
    let groups = [group(0)?, group(1)?];
    let mut convp = Vec::new();
    for t in 0..5 {
        for group in groups {
            for i in 0..32 {
                for j in 0..5 {
                    convp.push(group.data[(j * 32 + i) * 5 + t]);
                }
            }
        }
    }
    w.put("df_dec.df_convp.w", &[5, 2, 32, 5], &convp);
    w.gemm("df_dec.df_convp.pw", "df_dec/df_convp/df_convp.2/Conv")?;
    Ok(w.out)
}
