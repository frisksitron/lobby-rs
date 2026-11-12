//! The Strong noise suppressor: Ceva's DPDFNet-2 for 48 kHz (`NOTICE` beside this file) on an
//! engine of our own. The network enhances one spectrum per 10 ms; [`Dpdfnet`] wraps it in the
//! streaming STFT it was trained with, Ceva's: a 960-sample Vorbis window at 50% overlap, the
//! unnormalised FFT in and the inverse FFT, divided by 960, out by overlap-add.
//!
//! Built once with its weights, it allocates nothing per frame.

mod layers;
mod network;
mod weights;

use std::sync::Arc;

use realfft::{ComplexToReal, RealFftPlanner, RealToComplex, num_complex::Complex32};

use super::dsp::{DSP_FRAME, Frame};
use network::{BINS, Network, Spectrum};

/// Two frames: each FFT sees the last frame and this one.
const WINDOW: usize = 2 * DSP_FRAME;
static WEIGHTS: &[u8] = include_bytes!("dpdfnet2.f16.bin");

pub(crate) struct Dpdfnet {
    network: Network,
    forward: Arc<dyn RealToComplex<f32>>,
    inverse: Arc<dyn ComplexToReal<f32>>,
    window: [f32; WINDOW],
    previous: Frame,
    overlap: Frame,
    time: [f32; WINDOW],
    bins: [Complex32; BINS],
    spectrum: Spectrum,
    fft_scratch: Box<[Complex32]>,
}

impl Dpdfnet {
    pub fn new() -> Dpdfnet {
        let mut planner = RealFftPlanner::<f32>::new();
        let forward = planner.plan_fft_forward(WINDOW);
        let inverse = planner.plan_fft_inverse(WINDOW);
        let scratch_len = forward.get_scratch_len().max(inverse.get_scratch_len());
        Dpdfnet {
            network: Network::new(WEIGHTS),
            forward,
            inverse,
            window: std::array::from_fn(|i| {
                let half = DSP_FRAME as f64;
                let s = (0.5 * std::f64::consts::PI * (i as f64 + 0.5) / half).sin();
                (0.5 * std::f64::consts::PI * s * s).sin() as f32
            }),
            previous: [0.0; DSP_FRAME],
            overlap: [0.0; DSP_FRAME],
            time: [0.0; WINDOW],
            bins: [Complex32::default(); BINS],
            spectrum: [0.0; 2 * BINS],
            fft_scratch: vec![Complex32::default(); scratch_len].into_boxed_slice(),
        }
    }

    /// One 10 ms frame in place. What comes out went in 50 ms earlier: the STFT holds one frame
    /// and the network four more.
    pub fn process(&mut self, frame: &mut Frame) {
        let (older, newer) = self.window.split_at(DSP_FRAME);
        for i in 0..DSP_FRAME {
            self.time[i] = self.previous[i] * older[i];
            self.time[DSP_FRAME + i] = frame[i] * newer[i];
        }
        self.previous = *frame;
        self.forward
            .process_with_scratch(&mut self.time, &mut self.bins, &mut self.fft_scratch)
            .expect("the plan fits its buffers");
        for (pair, bin) in self.spectrum.chunks_exact_mut(2).zip(&self.bins) {
            pair[0] = bin.re;
            pair[1] = bin.im;
        }

        self.network.process(&mut self.spectrum);

        for (bin, pair) in self.bins.iter_mut().zip(self.spectrum.chunks_exact(2)) {
            *bin = Complex32::new(pair[0], pair[1]);
        }
        // A real signal's DC and Nyquist bins have no imaginary part; the inverse FFT refuses one.
        self.bins[0].im = 0.0;
        self.bins[BINS - 1].im = 0.0;
        self.inverse
            .process_with_scratch(&mut self.bins, &mut self.time, &mut self.fft_scratch)
            .expect("the plan fits its buffers");
        let norm = 1.0 / WINDOW as f32;
        let (first, second) = self.time.split_at(DSP_FRAME);
        for i in 0..DSP_FRAME {
            frame[i] = first[i] * norm * older[i] + self.overlap[i];
            self.overlap[i] = second[i] * norm * newer[i];
        }
    }
}
