//! The weights file, `dpdfnet2.f16.bin`: `DPDF`, a little-endian u32 precision (1, half floats),
//! then the tensors in the order the network reads them, each as a one-byte name length, the
//! name, a u32 rank, its u32 dimensions and its little-endian f16 values. `cargo xtask
//! dpdfnet-weights` writes it.
//!
//! The file is embedded, so one that does not match the engine is a build defect: reading it
//! panics with the tensor it stopped at, and any test that builds the engine fails.

const MAGIC: &[u8] = b"DPDF";
const HALF: u32 = 1;

pub(super) struct Reader<'a> {
    bytes: &'a [u8],
}

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8]) -> Reader<'a> {
        let mut reader = Reader { bytes };
        assert_eq!(
            reader.take(MAGIC.len()),
            MAGIC,
            "not a DPDFNet weights file"
        );
        assert_eq!(reader.u32(), HALF, "the weights are not half floats");
        reader
    }

    fn take(&mut self, n: usize) -> &'a [u8] {
        let (head, rest) = self
            .bytes
            .split_at_checked(n)
            .expect("the weights file ends early");
        self.bytes = rest;
        head
    }

    fn u32(&mut self) -> u32 {
        u32::from_le_bytes(self.take(4).try_into().expect("four bytes"))
    }

    pub fn tensor(&mut self, name: &str, dims: &[usize]) -> Box<[f32]> {
        let name_len = usize::from(self.take(1)[0]);
        let found_name = self.take(name_len);
        let rank = self.u32() as usize;
        let found_dims: Vec<usize> = (0..rank).map(|_| self.u32() as usize).collect();
        assert!(
            found_name == name.as_bytes() && found_dims == dims,
            "the weights hold {}{found_dims:?} where the engine reads {name}{dims:?}",
            String::from_utf8_lossy(found_name),
        );
        let n: usize = dims.iter().product();
        self.take(2 * n)
            .chunks_exact(2)
            .map(|b| f16_to_f32(u16::from_le_bytes([b[0], b[1]])))
            .collect()
    }

    pub fn finish(self) {
        assert!(
            self.bytes.is_empty(),
            "{} bytes after the last tensor",
            self.bytes.len()
        );
    }
}

fn f16_to_f32(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = u32::from(h >> 10) & 0x1f;
    let man = u32::from(h & 0x3ff);
    match exp {
        // Subnormal: the mantissa times 2^-24.
        0 => sign * man as f32 * (1.0 / 16_777_216.0),
        31 if man == 0 => sign * f32::INFINITY,
        31 => f32::NAN,
        // Rebias the exponent from 15 to 127 and widen the mantissa from 10 bits to 23.
        _ => f32::from_bits(u32::from(h >> 15) << 31 | (exp + 112) << 23 | man << 13),
    }
}
