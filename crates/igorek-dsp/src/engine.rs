//! Uniformly partitioned overlap-save convolution.
//!
//! The IR is cut into partitions of [`P`] samples; each partition's
//! [`BINS`]-bin spectrum (zero-padded to [`N`] before the forward transform)
//! is multiplied against a circular frequency-domain delay line of past input
//! block spectra — Wefers' formulation of Gardner's uniform special case. Per
//! [`P`]-sample block the engine does one forward rFFT, one complex multiply
//! per partition, and one inverse rFFT.
//!
//! At the [`P`]-sample grid the engine itself has no inherent delay: output
//! block *j* is complete the moment input block *j* is. The one-partition
//! latency Igorek reports comes from the processor's deterministic output
//! ring ([`crate::LATENCY_SAMPLES`]), which makes the delay exact for any host
//! block size.

use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};

use crate::{BINS, N, P};

/// Frequency-domain partitions of one channel's impulse response.
///
/// Baked off the audio thread by [`ChannelSpectra::from_time_ir`] and
/// installed into a [`Convolver`] wholesale; the audio thread only ever swaps
/// the pointer.
#[derive(Clone, Debug)]
pub struct ChannelSpectra {
    n_partitions: usize,
    /// `h[k]` is the [`BINS`]-bin spectrum of IR samples `[k*P .. k*P + P)`.
    h: Vec<Vec<Complex<f32>>>,
    /// Length of the time-domain IR the spectra were baked from, in samples.
    ir_samples: usize,
}

impl ChannelSpectra {
    /// The identity impulse: a single unit sample. Convolving with it passes
    /// the block through unchanged — the default Color IR before any file is
    /// loaded, so "nothing loaded" is never silence.
    #[must_use]
    pub fn identity() -> Self {
        Self {
            n_partitions: 1,
            h: vec![vec![Complex::new(1.0, 0.0); BINS]],
            ir_samples: 1,
        }
    }

    /// Partition a time-domain IR into spectra. Off the audio thread only:
    /// this plans an FFT and allocates. Non-finite samples must be rejected
    /// by the caller — rustfft would happily spread garbage across every bin.
    #[must_use]
    pub fn from_time_ir(ir: &[f32]) -> Self {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(N);
        let n_partitions = ir.len().div_ceil(P).max(1);
        let mut input = fft.make_input_vec();
        let mut spectrum = fft.make_output_vec();
        let mut scratch = vec![Complex::default(); fft.get_scratch_len()];
        let mut h = Vec::with_capacity(n_partitions);
        for k in 0..n_partitions {
            input.fill(0.0);
            let end = ((k + 1) * P).min(ir.len());
            input[..end - k * P].copy_from_slice(&ir[k * P..end]);
            fft.process_with_scratch(&mut input, &mut spectrum, &mut scratch)
                .expect("planner-sized buffers");
            h.push(spectrum.clone());
        }
        Self {
            n_partitions,
            h,
            ir_samples: ir.len(),
        }
    }

    /// Number of partitions the spectra occupy.
    #[must_use]
    pub fn n_partitions(&self) -> usize {
        self.n_partitions
    }

    /// Length of the time-domain IR the spectra were baked from, in samples.
    #[must_use]
    pub fn ir_samples(&self) -> usize {
        self.ir_samples
    }
}

/// One channel of the overlap-save engine.
///
/// All buffers are sized for `max_partitions` at construction (or `reset`);
/// installing an IR of any length up to that bound only swaps the spectra
/// pointer and a partition count. `process_block` never allocates.
#[derive(Clone)]
pub struct Convolver {
    fft_forward: Arc<dyn RealToComplex<f32>>,
    fft_inverse: Arc<dyn ComplexToReal<f32>>,
    scratch_forward: Vec<Complex<f32>>,
    scratch_inverse: Vec<Complex<f32>>,
    /// History (previous block) plus the new input, length [`N`].
    frame: Vec<f32>,
    /// Forward transform of [`Convolver::frame`], length [`BINS`].
    spectrum: Vec<Complex<f32>>,
    /// Circular delay line of past input-block spectra, `max_partitions`
    /// slots of [`BINS`] bins each. Only the active `n_partitions` slots are
    /// ever read.
    fdline: Vec<Vec<Complex<f32>>>,
    /// Slot the newest input spectrum lands in; advances modulo the active
    /// partition count.
    cursor: usize,
    /// Frequency-domain accumulator, one per bin.
    acc: Vec<Complex<f32>>,
    /// Inverse-transform result, length [`N`]; the first [`P`] samples are
    /// the aliased half and are discarded.
    block: Vec<f32>,
    spectra: Box<ChannelSpectra>,
    max_partitions: usize,
}

impl Convolver {
    /// Build a convolver able to hold IRs up to `max_partitions` partitions,
    /// starting from the identity impulse.
    #[must_use]
    pub fn new(max_partitions: usize) -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let fft_forward = planner.plan_fft_forward(N);
        let fft_inverse = planner.plan_fft_inverse(N);
        let scratch_forward = vec![Complex::default(); fft_forward.get_scratch_len()];
        let scratch_inverse = vec![Complex::default(); fft_inverse.get_scratch_len()];
        let frame = fft_forward.make_input_vec();
        let spectrum = fft_forward.make_output_vec();
        let acc = fft_inverse.make_input_vec();
        let block = fft_inverse.make_output_vec();
        let max_partitions = max_partitions.max(1);
        let mut convolver = Self {
            fft_forward,
            fft_inverse,
            scratch_forward,
            scratch_inverse,
            frame,
            spectrum,
            fdline: vec![vec![Complex::default(); BINS]; max_partitions],
            cursor: 0,
            acc,
            block,
            spectra: Box::new(ChannelSpectra::identity()),
            max_partitions,
        };
        convolver.reset();
        convolver
    }

    /// Upper bound on the partition count this convolver accepts.
    #[must_use]
    pub fn max_partitions(&self) -> usize {
        self.max_partitions
    }

    /// Length of the installed IR, in samples.
    #[must_use]
    pub fn ir_samples(&self) -> usize {
        self.spectra.ir_samples
    }

    /// Swap in new spectra at a block boundary and clear the delay line. The
    /// previous tail is not flushed — an IR change is a user event, and a
    /// one-block discontinuity is the documented cost of the wholesale swap
    /// that keeps a half-installed state impossible.
    pub fn install(&mut self, spectra: Box<ChannelSpectra>) {
        assert!(
            spectra.n_partitions <= self.max_partitions,
            "IR of {} partitions exceeds the {} bound sized in reset",
            spectra.n_partitions,
            self.max_partitions
        );
        self.spectra = spectra;
        self.reset();
    }

    /// Clear all signal state without changing the installed spectra.
    pub fn reset(&mut self) {
        self.frame.fill(0.0);
        for slot in &mut self.fdline {
            slot.fill(Complex::default());
        }
        self.cursor = 0;
        self.acc.fill(Complex::default());
        self.block.fill(0.0);
    }

    /// Convolve one [`P`]-sample block: forward-transform the history+input
    /// frame, store it in the delay line, multiply every IR partition against
    /// its input block, inverse-transform, and keep the non-aliased half.
    pub fn process_block(&mut self, input: &[f32], output: &mut [f32]) {
        assert_eq!(input.len(), P, "engine steps on the {P}-sample grid");
        assert_eq!(output.len(), P, "engine steps on the {P}-sample grid");
        let n_partitions = self.spectra.n_partitions.max(1);

        // The previous block's input becomes this frame's history.
        self.frame.copy_within(P..N, 0);
        self.frame[P..].copy_from_slice(input);
        self.fft_forward
            .process_with_scratch(
                &mut self.frame,
                &mut self.spectrum,
                &mut self.scratch_forward,
            )
            .expect("planner-sized buffers");
        self.fdline[self.cursor].copy_from_slice(&self.spectrum);

        // Partition k meets input block j-k; the delay line wraps so slot
        // (cursor - k) mod n_partitions holds exactly that spectrum.
        self.acc.fill(Complex::default());
        let spectra = self.spectra.as_ref();
        for k in 0..n_partitions {
            let x = &self.fdline[(self.cursor + n_partitions - k) % n_partitions];
            let h = &spectra.h[k];
            for bin in 0..BINS {
                self.acc[bin] += h[bin] * x[bin];
            }
        }
        self.cursor = (self.cursor + 1) % n_partitions;

        self.fft_inverse
            .process_with_scratch(&mut self.acc, &mut self.block, &mut self.scratch_inverse)
            .expect("planner-sized buffers");
        // realfft follows rustfft's unnormalized convention — a round trip
        // gains N — so undo it here, where spectra of every origin (baked or
        // identity) get the same treatment.
        let inverse_n = 1.0 / N as f32;
        for (sample, &aliased) in output.iter_mut().zip(&self.block[P..N]) {
            *sample = aliased * inverse_n;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::Rng;
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    /// Direct convolution in f64: the reference the partitioned engine has to
    /// agree with.
    fn naive_convolution(ir: &[f32], input: &[f32]) -> Vec<f64> {
        let mut y = vec![0.0_f64; input.len()];
        for (t, sample) in y.iter_mut().enumerate() {
            for (m, &h) in ir.iter().enumerate() {
                if t >= m {
                    *sample += f64::from(h) * f64::from(input[t - m]);
                }
            }
        }
        y
    }

    fn max_abs_deviation(engine: &[f32], reference: &[f64]) -> f64 {
        engine
            .iter()
            .zip(reference)
            .map(|(&e, &r)| (f64::from(e) - r).abs())
            .fold(0.0_f64, f64::max)
    }

    /// Deterministic uniform noise in `-1.0..1.0` for IRs and test signals.
    fn noise(rng: &mut StdRng, len: usize) -> Vec<f32> {
        (0..len).map(|_| rng.random_range(-1.0_f32..1.0)).collect()
    }

    /// Feed `input` through the engine in P-sized blocks and collect the
    /// output; the input length must be a multiple of P.
    fn run_engine(ir: &[f32], input: &[f32]) -> Vec<f32> {
        let mut engine = Convolver::new(ir.len().div_ceil(P).max(1));
        engine.install(Box::new(ChannelSpectra::from_time_ir(ir)));
        let mut out = Vec::with_capacity(input.len());
        let mut block_in = [0.0_f32; P];
        let mut block_out = [0.0_f32; P];
        for chunk in input.chunks_exact(P) {
            block_in.copy_from_slice(chunk);
            engine.process_block(&block_in, &mut block_out);
            out.extend_from_slice(&block_out);
        }
        out
    }

    #[test]
    fn identity_spectra_pass_blocks_through_unchanged() {
        let mut rng = StdRng::seed_from_u64(0x1DE);
        let input = noise(&mut rng, 8 * P);
        let out = run_engine(&[1.0], &input);
        // Zero latency at the grid: block j out is block j in.
        assert!(out.iter().zip(&input).all(|(&o, &i)| (o - i).abs() < 1e-6));
    }

    #[test]
    fn empty_ir_produces_silence() {
        let mut rng = StdRng::seed_from_u64(7);
        let input = noise(&mut rng, 4 * P);
        let out = run_engine(&[], &input);
        assert!(out.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn matches_naive_convolution_across_partition_boundaries() {
        let mut rng = StdRng::seed_from_u64(0xC0FFEE);
        let input = noise(&mut rng, 8 * P);
        let lengths = [1_usize, 2, 64, 127, 128, 129, 256, 3 * 128 + 37, 1000];
        for len in lengths {
            let ir = noise(&mut rng, len);
            let expected = naive_convolution(&ir, &input);
            let out = run_engine(&ir, &input);
            let peak = expected.iter().fold(1e-9_f64, |a, &v| a.max(v.abs()));
            let deviation = max_abs_deviation(&out, &expected);
            assert!(
                deviation <= 1e-3 * peak,
                "IR length {len}: max deviation {deviation:.3e} over peak {peak:.3e}"
            );
        }
    }

    #[test]
    fn same_ir_and_input_reproduce_the_same_output() {
        let mut rng = StdRng::seed_from_u64(42);
        let input = noise(&mut rng, 4 * P);
        let ir = noise(&mut rng, 300);
        let a = run_engine(&ir, &input);
        let b = run_engine(&ir, &input);
        assert_eq!(a, b, "same IR and input must give the same output");
    }

    #[test]
    fn installing_a_new_ir_mid_stream_stays_finite() {
        let mut rng = StdRng::seed_from_u64(0xBEEF);
        let input = noise(&mut rng, 4 * P);
        let mut engine = Convolver::new(16);
        engine.install(Box::new(ChannelSpectra::from_time_ir(&noise(
            &mut rng, 200,
        ))));
        let mut block_out = [0.0_f32; P];
        for (i, chunk) in input.chunks_exact(P).enumerate() {
            if i == 2 {
                engine.install(Box::new(ChannelSpectra::from_time_ir(&noise(
                    &mut rng, 500,
                ))));
            }
            engine.process_block(chunk, &mut block_out);
            assert!(block_out.iter().all(|s| s.is_finite()));
        }
    }

    #[test]
    fn reset_restores_the_identity_passthrough() {
        let mut rng = StdRng::seed_from_u64(0x5EED);
        let input = noise(&mut rng, 4 * P);
        let mut engine = Convolver::new(8);
        engine.install(Box::new(ChannelSpectra::from_time_ir(&noise(
            &mut rng, 400,
        ))));
        let mut block_in = [0.0_f32; P];
        let mut block_out = [0.0_f32; P];
        for chunk in input.chunks_exact(P) {
            block_in.copy_from_slice(chunk);
            engine.process_block(&block_in, &mut block_out);
        }
        engine.install(Box::new(ChannelSpectra::identity()));
        for chunk in input.chunks_exact(P) {
            block_in.copy_from_slice(chunk);
            engine.process_block(&block_in, &mut block_out);
            for (&o, &i) in block_out.iter().zip(chunk) {
                assert!((o - i).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn spectra_report_their_ir_length() {
        assert_eq!(ChannelSpectra::identity().ir_samples(), 1);
        assert_eq!(ChannelSpectra::from_time_ir(&[0.0; 300]).ir_samples(), 300);
        assert_eq!(ChannelSpectra::from_time_ir(&[0.0; 300]).n_partitions(), 3);
        assert_eq!(ChannelSpectra::from_time_ir(&[]).n_partitions(), 1);
    }
}
