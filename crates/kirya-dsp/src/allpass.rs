//! Two-multiplier lattice allpass.
//!
//! Both the fixed input diffusers and the modulated tank allpasses are this
//! one structure; modulation is nothing more than a per-sample read distance.
//!
//! ```text
//! w = x - g * d_out
//! y = g * w + d_out
//! d_in = w
//! ```
//!
//! which is `H(z) = (g + z^-m) / (1 + g z^-m)` — unit magnitude at every
//! frequency, phase only.

use musictools_core::finite_or;

use crate::delay::InterpDelay;

/// Largest coefficient magnitude the allpass accepts. At `|g| = 1` the
/// structure sits exactly on the unit circle and never decays, so the tank
/// would ring forever on a rounding error.
const MAX_GAIN: f32 = 0.98;

/// One allpass section over its own delay line.
#[derive(Clone, Debug, Default)]
pub struct Allpass {
    line: InterpDelay,
}

impl Allpass {
    /// Allocate the internal line for at least `min_capacity` samples. Not
    /// real-time safe — call from `reset`.
    pub fn resize(&mut self, min_capacity: usize) {
        self.line.resize(min_capacity);
    }

    /// Zero the internal line without touching its allocation.
    pub fn clear(&mut self) {
        self.line.clear();
    }

    /// Largest delay this section can be asked for.
    #[must_use]
    pub fn max_delay(&self) -> f32 {
        self.line.max_delay()
    }

    /// Run one sample through the section at `delay` samples and gain `gain`.
    pub fn process(&mut self, input: f32, delay: f32, gain: f32) -> f32 {
        let gain = finite_or(gain, 0.0).clamp(-MAX_GAIN, MAX_GAIN);
        let delayed = self.line.read(delay);
        let stored = finite_or(input, 0.0) - gain * delayed;
        self.line.write(stored);
        gain * stored + delayed
    }

    /// Read the section's internal line, for the paper's output tap network.
    #[must_use]
    pub fn tap(&self, delay: f32) -> f32 {
        self.line.read(delay)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    /// Impulse response of a single section, long enough for the lattice to
    /// have decayed into the noise floor.
    fn impulse_response(delay: f32, gain: f32, length: usize) -> Vec<f32> {
        let mut section = Allpass::default();
        section.resize(length);
        (0..length)
            .map(|n| section.process(if n == 0 { 1.0 } else { 0.0 }, delay, gain))
            .collect()
    }

    /// Naive DFT magnitude at bin `k`. Deliberately independent of the crate's
    /// own FFT so this test checks the allpass, not our transform.
    fn dft_magnitude(signal: &[f32], bin: usize) -> f64 {
        let n = signal.len() as f64;
        let (mut re, mut im) = (0.0, 0.0);
        for (index, &sample) in signal.iter().enumerate() {
            let angle = -TAU * bin as f64 * index as f64 / n;
            re += f64::from(sample) * angle.cos();
            im += f64::from(sample) * angle.sin();
        }
        re.hypot(im)
    }

    #[test]
    fn magnitude_response_is_flat_for_every_gain() {
        for gain in [-0.75, -0.625, -0.5, 0.5, 0.625, 0.7, 0.75] {
            for delay in [7.0_f32, 23.0, 64.0] {
                // The lattice decays by `gain` per loop, so the window has to
                // hold enough loops that truncation lands below the tolerance:
                // 0.75^80 is about 1e-10.
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let length = delay as usize * 80;
                let response = impulse_response(delay, gain, length);
                for bin in (0..length / 2).step_by(length / 48) {
                    let magnitude = dft_magnitude(&response, bin);
                    assert!(
                        (magnitude - 1.0).abs() < 1.0e-4,
                        "gain {gain} delay {delay} bin {bin}: |H| = {magnitude}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_zero_gain_section_is_a_plain_delay() {
        let response = impulse_response(5.0, 0.0, 32);
        for (index, &sample) in response.iter().enumerate() {
            let expected = if index == 5 { 1.0 } else { 0.0 };
            assert!((sample - expected).abs() < 1.0e-6, "index {index}");
        }
    }

    #[test]
    fn extreme_gains_clamp_short_of_the_unit_circle() {
        // A section driven at |g| = 1 would never decay. Feeding a constant
        // through a clamped section must still settle to a finite level.
        let mut section = Allpass::default();
        section.resize(64);
        let mut output = 0.0;
        for _ in 0..20_000 {
            output = section.process(1.0, 16.0, 1.0);
            assert!(output.is_finite());
        }
        assert!(output.abs() < 100.0, "settled at {output}");
    }

    #[test]
    fn non_finite_input_and_coefficients_stay_finite() {
        let mut section = Allpass::default();
        section.resize(64);
        for input in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.5] {
            let output = section.process(input, f32::NAN, f32::INFINITY);
            assert!(output.is_finite(), "input {input} produced {output}");
        }
    }

    #[test]
    fn tapping_the_internal_line_sees_the_stored_signal() {
        let mut section = Allpass::default();
        section.resize(64);
        section.process(1.0, 8.0, 0.5);
        // The first stored sample is `x - g * 0 = 1.0`, now one write back.
        assert!((section.tap(1.0) - 1.0).abs() < 1.0e-6);
    }
}
