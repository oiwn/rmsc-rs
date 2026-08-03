//! Framework-independent DSP building blocks for Bogdan.
//!
//! The hard-clipping reference and the first usable detail-preserving processor
//! live together here so they can be compared without loading a plugin host.

use musictools_core::finite_or;

/// The fixed high-pass cutoff used by the first usable processor.
pub const DEFAULT_DETAIL_HIGH_PASS_HZ: f64 = 1_000.0;

const DEFAULT_SAMPLE_RATE_HZ: f64 = 44_100.0;

/// The per-sample components produced by the hard-clipping reference path.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClippingFrame {
    /// Input after applying drive.
    pub driven: f32,
    /// Driven input constrained to the symmetric ceiling.
    pub clipped: f32,
    /// Information removed by clipping: `driven - clipped`.
    pub delta: f32,
}

/// The per-sample components produced by the detail-preserving path.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DetailFrame {
    /// Input after applying drive.
    pub driven: f32,
    /// Driven input constrained to the symmetric ceiling.
    pub clipped: f32,
    /// Information removed by clipping: `driven - clipped`.
    pub delta: f32,
    /// High-passed clipping delta used as the inward modulation signal.
    pub filtered_delta: f32,
    /// Final bounded output sample.
    pub output: f32,
}

#[derive(Clone, Copy, Debug)]
struct OnePoleHighPass {
    coefficient: f32,
    previous_input: f32,
    previous_output: f32,
}

impl OnePoleHighPass {
    fn new(sample_rate: f64) -> Self {
        let mut filter = Self {
            coefficient: 0.0,
            previous_input: 0.0,
            previous_output: 0.0,
        };
        filter.reset(sample_rate);
        filter
    }

    fn reset(&mut self, sample_rate: f64) {
        let safe_sample_rate = if sample_rate.is_finite() && sample_rate > 0.0 {
            sample_rate
        } else {
            DEFAULT_SAMPLE_RATE_HZ
        };
        let cutoff = DEFAULT_DETAIL_HIGH_PASS_HZ.min(safe_sample_rate * 0.49);
        self.coefficient = (-std::f64::consts::TAU * cutoff / safe_sample_rate).exp() as f32;
        self.previous_input = 0.0;
        self.previous_output = 0.0;
    }

    fn process(&mut self, input: f32) -> f32 {
        let output =
            self.coefficient * (self.previous_output + finite_or(input, 0.0) - self.previous_input);
        self.previous_input = finite_or(input, 0.0);
        self.previous_output = finite_or(output, 0.0);
        self.previous_output
    }
}

/// Stateful, zero-latency detail-preserving clipper for one audio channel.
#[derive(Clone, Copy, Debug)]
pub struct DetailClipper {
    delta_high_pass: OnePoleHighPass,
}

impl Default for DetailClipper {
    fn default() -> Self {
        Self::new(DEFAULT_SAMPLE_RATE_HZ)
    }
}

impl DetailClipper {
    /// Construct a processor configured for `sample_rate` hertz.
    #[must_use]
    pub fn new(sample_rate: f64) -> Self {
        Self {
            delta_high_pass: OnePoleHighPass::new(sample_rate),
        }
    }

    /// Clear filter memory and update coefficients for a new sample rate.
    pub fn reset(&mut self, sample_rate: f64) {
        self.delta_high_pass.reset(sample_rate);
    }

    /// Process one sample while exposing the intermediate analysis signals.
    ///
    /// The high-passed magnitude is subtracted from a clipped plateau toward
    /// zero. Unclipped samples are unchanged and the output can never exceed
    /// the hard-clipping reference ceiling.
    #[must_use]
    pub fn process_sample(&mut self, input: f32, drive: f32, ceiling: f32) -> DetailFrame {
        let frame = hard_clip_frame(input, drive, ceiling);
        let filtered_delta = self.delta_high_pass.process(frame.delta);
        let output = if frame.delta == 0.0 {
            frame.clipped
        } else {
            let magnitude = (frame.clipped.abs() - filtered_delta.abs()).max(0.0);
            magnitude.copysign(frame.clipped)
        };

        DetailFrame {
            driven: frame.driven,
            clipped: frame.clipped,
            delta: frame.delta,
            filtered_delta,
            output: finite_or(output, 0.0),
        }
    }
}

/// Analyze one sample against a symmetric linear-amplitude ceiling.
///
/// `drive` and `ceiling` are sanitized so malformed host input cannot panic or
/// emit infinities. A non-finite input sample is converted to silence.
#[must_use]
pub fn hard_clip_frame(input: f32, drive: f32, ceiling: f32) -> ClippingFrame {
    let safe_input = finite_or(input, 0.0);
    let safe_drive = finite_or(drive, 1.0).max(0.0);
    let safe_ceiling = finite_or(ceiling, 1.0).abs().max(f32::MIN_POSITIVE);

    let driven = (safe_input * safe_drive).clamp(-f32::MAX, f32::MAX);
    let clipped = driven.clamp(-safe_ceiling, safe_ceiling);

    ClippingFrame {
        driven,
        clipped,
        delta: driven - clipped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn below_ceiling_is_unchanged() {
        let frame = hard_clip_frame(0.25, 2.0, 1.0);
        assert_eq!(frame.driven, 0.5);
        assert_eq!(frame.clipped, 0.5);
        assert_eq!(frame.delta, 0.0);
    }

    #[test]
    fn reports_positive_and_negative_clipping_delta() {
        let positive = hard_clip_frame(0.75, 2.0, 1.0);
        let negative = hard_clip_frame(-0.75, 2.0, 1.0);

        assert_eq!(
            positive,
            ClippingFrame {
                driven: 1.5,
                clipped: 1.0,
                delta: 0.5
            }
        );
        assert_eq!(
            negative,
            ClippingFrame {
                driven: -1.5,
                clipped: -1.0,
                delta: -0.5
            }
        );
    }

    #[test]
    fn malformed_values_stay_finite_and_bounded() {
        for input in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let frame = hard_clip_frame(input, f32::NAN, f32::INFINITY);
            assert!(frame.driven.is_finite());
            assert!(frame.clipped.is_finite());
            assert!(frame.delta.is_finite());
            assert!(frame.clipped.abs() <= 1.0);
        }
    }

    #[test]
    fn detail_processor_leaves_unclipped_samples_unchanged() {
        let mut processor = DetailClipper::new(48_000.0);

        for input in [-0.75, -0.25, 0.0, 0.25, 0.75] {
            let frame = processor.process_sample(input, 1.0, 1.0);
            assert_eq!(frame.output, input);
            assert_eq!(frame.delta, 0.0);
        }
    }

    #[test]
    fn detail_processor_adds_inward_motion_to_a_clipped_plateau() {
        let mut processor = DetailClipper::new(48_000.0);
        let mut changed_samples = 0;

        for index in 0..256 {
            let input = if index % 2 == 0 { 1.25 } else { 2.0 };
            let frame = processor.process_sample(input, 1.0, 1.0);
            assert!(frame.output >= 0.0);
            assert!(frame.output <= frame.clipped);
            if frame.output < frame.clipped {
                changed_samples += 1;
            }
        }

        assert!(changed_samples > 200);
    }

    #[test]
    fn detail_processor_is_polarity_symmetric() {
        let mut positive = DetailClipper::new(48_000.0);
        let mut negative = DetailClipper::new(48_000.0);

        for input in [1.1, 1.4, 2.0, 1.2, 0.8] {
            let positive_output = positive.process_sample(input, 1.0, 1.0).output;
            let negative_output = negative.process_sample(-input, 1.0, 1.0).output;
            assert!((positive_output + negative_output).abs() < 1.0e-6);
        }
    }

    #[test]
    fn constant_clipping_settles_back_to_the_hard_ceiling() {
        let mut processor = DetailClipper::new(48_000.0);
        let first = processor.process_sample(2.0, 1.0, 1.0);
        let mut settled = first;

        for _ in 0..1_024 {
            settled = processor.process_sample(2.0, 1.0, 1.0);
        }

        assert!(first.output < first.clipped);
        assert!((settled.output - settled.clipped).abs() < 1.0e-5);
    }

    #[test]
    fn detail_processor_stays_finite_and_bounded() {
        let mut processor = DetailClipper::new(f64::NAN);

        for input in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 100.0, -100.0] {
            let frame = processor.process_sample(input, f32::NAN, f32::INFINITY);
            assert!(frame.filtered_delta.is_finite());
            assert!(frame.output.is_finite());
            assert!(frame.output.abs() <= 1.0);
        }

        processor.reset(0.0);
        assert!(processor.process_sample(2.0, 1.0, 1.0).output.is_finite());
    }

    #[test]
    fn detail_processor_respects_each_requested_ceiling() {
        let mut processor = DetailClipper::new(96_000.0);

        for ceiling in [0.01, 0.1, 0.5, 1.0, 2.0] {
            for input in [-100.0, -2.0, -0.25, 0.25, 2.0, 100.0] {
                let output = processor.process_sample(input, 4.0, ceiling).output;
                assert!(output.abs() <= ceiling);
            }
        }
    }

    #[test]
    fn reset_clears_filter_history() {
        let mut reset_processor = DetailClipper::new(48_000.0);
        let mut fresh_processor = DetailClipper::new(48_000.0);

        for input in [2.0, 1.1, -2.0, 0.0] {
            let _ = reset_processor.process_sample(input, 1.0, 1.0);
        }
        reset_processor.reset(48_000.0);

        let after_reset = reset_processor.process_sample(1.5, 1.0, 1.0);
        let fresh = fresh_processor.process_sample(1.5, 1.0, 1.0);
        assert_eq!(after_reset, fresh);
    }
}
