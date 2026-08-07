//! One-pole damping filters and a DC blocker.
//!
//! Each filter caches the cutoff it was last tuned to and only recomputes its
//! coefficient when that value actually moves, so a knob left alone costs no
//! `exp()` per sample. Filter memory survives retuning, so a cutoff can be
//! swept at audio rate without clicking.

use musictools_core::{finite_or, finite_or_f64};

use crate::safe_sample_rate;

/// Corner frequency of the fixed DC blockers on the input and both outputs.
const DC_BLOCK_HZ: f64 = 10.0;

/// Fallback cutoff used when a host hands us a non-finite frequency.
const FALLBACK_CUTOFF_HZ: f32 = 1_000.0;

/// Clamp a requested cutoff into a range the coefficient math stays sane over.
fn guard_cutoff(cutoff_hz: f32, sample_rate: f64) -> f32 {
    #[allow(clippy::cast_possible_truncation)]
    let nyquist_guard = (sample_rate * 0.49).max(1.0) as f32;
    finite_or(cutoff_hz, FALLBACK_CUTOFF_HZ).clamp(1.0, nyquist_guard)
}

/// The shared pole of every filter here: `exp(-2*pi*fc/fs)`.
fn pole(cutoff_hz: f32, sample_rate: f64) -> f32 {
    let cutoff = finite_or_f64(f64::from(cutoff_hz), f64::from(FALLBACK_CUTOFF_HZ));
    #[allow(clippy::cast_possible_truncation)]
    {
        (-std::f64::consts::TAU * cutoff / safe_sample_rate(sample_rate)).exp() as f32
    }
}

/// One-pole low-pass, the tank's high-cut damping.
///
/// `y += (1 - a) * (x - y)`, where `a` is the pole. Down 3 dB at the cutoff.
#[derive(Clone, Copy, Debug, Default)]
pub struct OnePoleLowPass {
    pole: f32,
    cached_cutoff: f32,
    state: f32,
}

impl OnePoleLowPass {
    /// Clear filter memory and drop the cached cutoff so the next `process`
    /// retunes from scratch.
    pub fn clear(&mut self) {
        self.state = 0.0;
        self.cached_cutoff = 0.0;
    }

    /// Run one sample at `cutoff_hz`.
    pub fn process(&mut self, input: f32, cutoff_hz: f32, sample_rate: f64) -> f32 {
        let cutoff = guard_cutoff(cutoff_hz, sample_rate);
        if cutoff != self.cached_cutoff {
            self.pole = pole(cutoff, sample_rate);
            self.cached_cutoff = cutoff;
        }
        self.state += (1.0 - self.pole) * (finite_or(input, 0.0) - self.state);
        self.state = finite_or(self.state, 0.0);
        self.state
    }
}

/// One-pole high-pass in direct form I, the tank's low-cut damping.
///
/// `b1 = exp(-2*pi*fc/fs)`, `a0 = (1 + b1) / 2`, `a1 = -a0`. Down 3 dB at the
/// cutoff, and exactly zero gain at DC.
#[derive(Clone, Copy, Debug, Default)]
pub struct OnePoleHighPass {
    b1: f32,
    a0: f32,
    cached_cutoff: f32,
    previous_input: f32,
    previous_output: f32,
}

impl OnePoleHighPass {
    /// Clear filter memory and drop the cached cutoff.
    pub fn clear(&mut self) {
        self.previous_input = 0.0;
        self.previous_output = 0.0;
        self.cached_cutoff = 0.0;
    }

    /// Run one sample at `cutoff_hz`.
    pub fn process(&mut self, input: f32, cutoff_hz: f32, sample_rate: f64) -> f32 {
        let cutoff = guard_cutoff(cutoff_hz, sample_rate);
        if cutoff != self.cached_cutoff {
            self.b1 = pole(cutoff, sample_rate);
            self.a0 = 0.5 * (1.0 + self.b1);
            self.cached_cutoff = cutoff;
        }
        let input = finite_or(input, 0.0);
        let output =
            self.a0 * input - self.a0 * self.previous_input + self.b1 * self.previous_output;
        self.previous_input = input;
        self.previous_output = finite_or(output, 0.0);
        self.previous_output
    }
}

/// Fixed 10 Hz DC blocker: `y = x - x1 + r * y1`.
///
/// Sits on the summed input and on both outputs. A plate tank feeds back on
/// itself indefinitely, so any DC offset that gets in would accumulate.
#[derive(Clone, Copy, Debug, Default)]
pub struct DcBlocker {
    coefficient: f32,
    previous_input: f32,
    previous_output: f32,
}

impl DcBlocker {
    /// Retune for a new sample rate and clear filter memory.
    pub fn reset(&mut self, sample_rate: f64) {
        #[allow(clippy::cast_possible_truncation)]
        {
            self.coefficient = pole(DC_BLOCK_HZ as f32, sample_rate);
        }
        self.previous_input = 0.0;
        self.previous_output = 0.0;
    }

    /// Run one sample.
    pub fn process(&mut self, input: f32) -> f32 {
        let input = finite_or(input, 0.0);
        let output = input - self.previous_input + self.coefficient * self.previous_output;
        self.previous_input = input;
        self.previous_output = finite_or(output, 0.0);
        self.previous_output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    const SAMPLE_RATE: f64 = 48_000.0;

    /// Steady-state gain at `frequency`, measured by driving the filter with a
    /// sine until the transient has passed and taking the peak.
    fn steady_state_gain(mut run: impl FnMut(f32) -> f32, frequency: f64) -> f64 {
        let step = TAU * frequency / SAMPLE_RATE;
        let settle = (SAMPLE_RATE / frequency).max(4_000.0) as usize * 4;
        for n in 0..settle {
            let _ = run((step * n as f64).sin() as f32);
        }
        let mut peak = 0.0_f64;
        for n in settle..settle + (SAMPLE_RATE / frequency) as usize * 4 {
            peak = peak.max(f64::from(run((step * n as f64).sin() as f32)).abs());
        }
        peak
    }

    fn to_db(gain: f64) -> f64 {
        20.0 * gain.max(1.0e-12).log10()
    }

    #[test]
    fn low_pass_passes_dc_and_is_down_three_db_at_its_cutoff() {
        let mut filter = OnePoleLowPass::default();
        for _ in 0..20_000 {
            filter.process(1.0, 1_000.0, SAMPLE_RATE);
        }
        assert!((filter.process(1.0, 1_000.0, SAMPLE_RATE) - 1.0).abs() < 1.0e-4);

        let mut filter = OnePoleLowPass::default();
        let gain = steady_state_gain(|x| filter.process(x, 1_000.0, SAMPLE_RATE), 1_000.0);
        assert!((to_db(gain) + 3.0).abs() < 0.2, "{} dB", to_db(gain));
    }

    #[test]
    fn low_pass_rejects_well_above_its_cutoff() {
        let mut filter = OnePoleLowPass::default();
        let gain = steady_state_gain(|x| filter.process(x, 500.0, SAMPLE_RATE), 8_000.0);
        assert!(to_db(gain) < -20.0, "{} dB", to_db(gain));
    }

    #[test]
    fn high_pass_blocks_dc_and_is_down_three_db_at_its_cutoff() {
        let mut filter = OnePoleHighPass::default();
        for _ in 0..40_000 {
            filter.process(1.0, 1_000.0, SAMPLE_RATE);
        }
        assert!(filter.process(1.0, 1_000.0, SAMPLE_RATE).abs() < 1.0e-4);

        let mut filter = OnePoleHighPass::default();
        let gain = steady_state_gain(|x| filter.process(x, 1_000.0, SAMPLE_RATE), 1_000.0);
        assert!((to_db(gain) + 3.0).abs() < 0.2, "{} dB", to_db(gain));
    }

    #[test]
    fn dc_blocker_removes_a_constant_offset_but_keeps_audio() {
        let mut blocker = DcBlocker::default();
        blocker.reset(SAMPLE_RATE);
        let mut settled = 0.0;
        for _ in 0..40_000 {
            settled = blocker.process(0.5);
        }
        assert!(settled.abs() < 1.0e-3, "settled at {settled}");

        let mut blocker = DcBlocker::default();
        blocker.reset(SAMPLE_RATE);
        let gain = steady_state_gain(|x| blocker.process(x), 1_000.0);
        assert!(to_db(gain).abs() < 0.1, "{} dB", to_db(gain));
    }

    #[test]
    fn cutoffs_are_clamped_below_nyquist() {
        // A cutoff above Nyquist would make the pole meaningless; the guard
        // keeps the filter a filter instead of a passthrough or an oscillator.
        let mut filter = OnePoleLowPass::default();
        let gain = steady_state_gain(|x| filter.process(x, 1.0e9, SAMPLE_RATE), 1_000.0);
        assert!(gain.is_finite() && gain <= 1.0 + 1.0e-3);
    }

    #[test]
    fn non_finite_input_and_sample_rate_stay_finite() {
        let mut low = OnePoleLowPass::default();
        let mut high = OnePoleHighPass::default();
        let mut blocker = DcBlocker::default();
        blocker.reset(f64::NAN);

        for input in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.25] {
            assert!(low.process(input, f32::NAN, f64::NAN).is_finite());
            assert!(high.process(input, f32::INFINITY, 0.0).is_finite());
            assert!(blocker.process(input).is_finite());
        }
    }

    #[test]
    fn clear_makes_a_used_filter_match_a_fresh_one() {
        let mut used = OnePoleLowPass::default();
        for _ in 0..100 {
            used.process(1.0, 500.0, SAMPLE_RATE);
        }
        used.clear();
        let mut fresh = OnePoleLowPass::default();
        for _ in 0..10 {
            assert_eq!(
                used.process(0.3, 2_000.0, SAMPLE_RATE),
                fresh.process(0.3, 2_000.0, SAMPLE_RATE)
            );
        }
    }
}
