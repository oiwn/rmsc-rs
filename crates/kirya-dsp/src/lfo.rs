//! Modulation LFOs for the tank allpasses.
//!
//! Shape morphs continuously between a triangle at `0.0` and a sine at `1.0`.
//! Both waveforms are bipolar in `[-1, 1]` and rise through zero at phase
//! zero, so the morph never steps.
//!
//! Four of these drive the two allpasses of each tank half. Their rates are
//! mutually non-harmonic multiples of one knob so the four never lock into a
//! common period, and their starting phases are spread across the cycle so
//! they do not all swing the same way at once.

use musictools_core::{finite_or, finite_or_f64};

use crate::constants::{MOD_LFO_PHASES, MOD_LFO_RATIOS};
use crate::safe_sample_rate;

/// A single phase accumulator with a triangle-to-sine output stage.
///
/// Phase is carried in `f64`. The rate knob reaches down to 0.01 Hz, where one
/// turn is nearly five million samples at 48 kHz; an `f32` accumulator's step
/// would be a fraction of an ulp there and the LFO would quantise into a
/// staircase instead of gliding.
#[derive(Clone, Copy, Debug, Default)]
pub struct Lfo {
    phase: f64,
    increment: f64,
    cached_rate: f32,
    cached_sample_rate: f64,
}

impl Lfo {
    /// Start at `phase` turns with no cached rate, so the next call retunes.
    pub fn reset(&mut self, phase: f32) {
        self.phase = f64::from(finite_or(phase, 0.0)).rem_euclid(1.0);
        self.increment = 0.0;
        self.cached_rate = -1.0;
        self.cached_sample_rate = 0.0;
    }

    /// Current output at `shape`, without advancing.
    #[must_use]
    pub fn value(&self, shape: f32) -> f32 {
        let shape = finite_or(shape, 0.0).clamp(0.0, 1.0);
        #[allow(clippy::cast_possible_truncation)]
        let phase = self.phase as f32;
        let triangle = triangle(phase);
        let sine = (std::f32::consts::TAU * phase).sin();
        triangle + shape * (sine - triangle)
    }

    /// Read the output at `shape`, then advance one sample at `rate_hz`.
    ///
    /// The step is computed from the real sample rate, so the modulation runs
    /// at the requested frequency in hertz regardless of the host's rate.
    pub fn next(&mut self, rate_hz: f32, shape: f32, sample_rate: f64) -> f32 {
        let rate = finite_or(rate_hz, 0.0).clamp(0.0, 100.0);
        let sample_rate = safe_sample_rate(sample_rate);
        if rate != self.cached_rate || sample_rate != self.cached_sample_rate {
            self.increment = finite_or_f64(f64::from(rate), 0.0) / sample_rate;
            self.cached_rate = rate;
            self.cached_sample_rate = sample_rate;
        }

        let output = self.value(shape);
        self.phase = (self.phase + self.increment).fract();
        output
    }
}

/// Symmetric bipolar triangle over one turn: `0 -> 1 -> 0 -> -1 -> 0`.
fn triangle(phase: f32) -> f32 {
    let phase = phase.rem_euclid(1.0);
    if phase < 0.25 {
        4.0 * phase
    } else if phase < 0.75 {
        2.0 - 4.0 * phase
    } else {
        4.0 * phase - 4.0
    }
}

/// The four tank LFOs, sharing one rate knob.
#[derive(Clone, Copy, Debug, Default)]
pub struct ModBank {
    lfos: [Lfo; 4],
}

impl ModBank {
    /// Reset all four to their spread starting phases.
    pub fn reset(&mut self) {
        for (lfo, phase) in self.lfos.iter_mut().zip(MOD_LFO_PHASES) {
            lfo.reset(phase);
        }
    }

    /// Advance all four one sample and return their bipolar outputs.
    pub fn next(&mut self, rate_hz: f32, shape: f32, sample_rate: f64) -> [f32; 4] {
        let mut out = [0.0; 4];
        for (index, lfo) in self.lfos.iter_mut().enumerate() {
            out[index] = lfo.next(rate_hz * MOD_LFO_RATIOS[index], shape, sample_rate);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_RATE: f64 = 48_000.0;

    fn cycle(shape: f32, rate: f32) -> Vec<f32> {
        let mut lfo = Lfo::default();
        lfo.reset(0.0);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let period = (SAMPLE_RATE / f64::from(rate)) as usize;
        (0..period)
            .map(|_| lfo.next(rate, shape, SAMPLE_RATE))
            .collect()
    }

    #[test]
    fn output_is_bipolar_and_reaches_both_rails_at_either_shape() {
        for shape in [0.0, 0.5, 1.0] {
            let samples = cycle(shape, 10.0);
            let min = samples.iter().copied().fold(f32::MAX, f32::min);
            let max = samples.iter().copied().fold(f32::MIN, f32::max);
            assert!(min >= -1.0 && max <= 1.0, "shape {shape}: {min}..{max}");
            assert!((min + 1.0).abs() < 1.0e-3, "shape {shape} min {min}");
            assert!((max - 1.0).abs() < 1.0e-3, "shape {shape} max {max}");
        }
    }

    #[test]
    fn shape_zero_is_a_symmetric_triangle() {
        let samples = cycle(0.0, 10.0);
        let half = samples.len() / 2;
        // A symmetric triangle is odd about the half-cycle.
        for index in 0..half {
            let mirrored = samples[index] + samples[index + half];
            assert!(mirrored.abs() < 1.0e-3, "index {index}: {mirrored}");
        }
        // ...and its slope is constant in magnitude everywhere.
        let step = (samples[1] - samples[0]).abs();
        for pair in samples.windows(2) {
            let slope = (pair[1] - pair[0]).abs();
            assert!((slope - step).abs() < 1.0e-4, "slope {slope} vs {step}");
        }
    }

    #[test]
    fn shape_one_is_a_sine() {
        let samples = cycle(1.0, 10.0);
        #[allow(clippy::cast_precision_loss)]
        let period = samples.len() as f32;
        for (index, &sample) in samples.iter().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let expected = (std::f32::consts::TAU * index as f32 / period).sin();
            assert!((sample - expected).abs() < 1.0e-5, "index {index}");
        }
    }

    #[test]
    fn shape_one_half_sits_between_the_two_waveforms() {
        let triangle = cycle(0.0, 10.0);
        let sine = cycle(1.0, 10.0);
        let morph = cycle(0.5, 10.0);
        for index in 0..morph.len() {
            let expected = 0.5 * (triangle[index] + sine[index]);
            assert!((morph[index] - expected).abs() < 1.0e-5, "index {index}");
        }
    }

    #[test]
    fn one_cycle_of_samples_returns_to_the_starting_phase() {
        let mut lfo = Lfo::default();
        lfo.reset(0.0);
        for _ in 0..48_000 {
            lfo.next(1.0, 0.5, SAMPLE_RATE);
        }
        // 1 Hz at 48 kHz is exactly 48000 samples per turn.
        assert!(lfo.value(0.5).abs() < 1.0e-3, "{}", lfo.value(0.5));

        // Doubling the rate halves the period.
        let mut lfo = Lfo::default();
        lfo.reset(0.0);
        for _ in 0..24_000 {
            lfo.next(2.0, 0.5, SAMPLE_RATE);
        }
        assert!(lfo.value(0.5).abs() < 1.0e-3, "{}", lfo.value(0.5));
    }

    #[test]
    fn the_four_bank_phases_actually_differ_at_the_start() {
        // Plateau carries four phase offsets that are never read; ours are
        // applied, so the four allpasses do not all swing together.
        let mut bank = ModBank::default();
        bank.reset();
        let first = bank.next(1.0, 0.0, SAMPLE_RATE);
        assert_eq!(first, [0.0, 1.0, 0.0, -1.0]);

        // The mid-cycle pair is distinguished by direction, not by value.
        let second = bank.next(1.0, 0.0, SAMPLE_RATE);
        assert!(second[0] > first[0]);
        assert!(second[2] < first[2]);
    }

    #[test]
    fn the_four_bank_rates_are_mutually_non_harmonic() {
        for (index, ratio) in MOD_LFO_RATIOS.iter().enumerate() {
            for other in MOD_LFO_RATIOS.iter().skip(index + 1) {
                let quotient = other / ratio;
                assert!(
                    (quotient - quotient.round()).abs() > 0.05,
                    "{ratio} and {other} are near-harmonic"
                );
            }
        }
    }

    #[test]
    fn non_finite_rate_shape_and_sample_rate_stay_finite() {
        let mut bank = ModBank::default();
        bank.reset();
        for _ in 0..100 {
            for value in bank.next(f32::NAN, f32::INFINITY, f64::NAN) {
                assert!(value.is_finite());
                assert!(value.abs() <= 1.0);
            }
        }
    }
}
