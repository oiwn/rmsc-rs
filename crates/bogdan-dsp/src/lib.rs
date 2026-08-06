//! Framework-independent DSP building blocks for Bogdan.
//!
//! The hard-clipping reference and the first usable detail-preserving processor
//! live together here so they can be compared without loading a plugin host.

use musictools_core::{finite_or, finite_or_f64};

/// The default high-pass cutoff, used by the Detail voice and as the wrapper's
/// initial `Detail` knob value.
pub const DEFAULT_DETAIL_HIGH_PASS_HZ: f64 = 1_000.0;

const DEFAULT_SAMPLE_RATE_HZ: f64 = 44_100.0;

/// Coerce a host-supplied sample rate to a strictly positive, finite value so
/// coefficient math and Nyquist clamps can never divide by zero or invert.
#[inline]
fn safe_sample_rate(sample_rate: f64) -> f64 {
    if sample_rate.is_finite() && sample_rate > 0.0 {
        sample_rate
    } else {
        DEFAULT_SAMPLE_RATE_HZ
    }
}

/// Selects how a clipped peak is reshaped inside the ceiling.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ClipMode {
    /// Plain hard clip; the clipping delta is discarded.
    Clean,
    /// High-passed clipping delta rectified and ducked inward. The original
    /// detail-preserving voice that suits drum-and-bass material.
    #[default]
    Detail,
    /// Antialiased wavefolder: excursions past the ceiling reflect back inside
    /// it. Drive sets the fold density, Amount blends clip toward fold.
    Fold,
}

/// Fold transfer function used by [`ClipMode::Fold`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FoldShape {
    /// `C * sin(pi x / 2C)` — smooth, musical, closed-form antiderivative.
    #[default]
    Sine,
    /// `C * (2/pi) * asin(sin(pi x / 2C))` — brighter zig-zag folds.
    Triangle,
}

/// Per-sample control values for [`DetailClipper::process`].
#[derive(Clone, Copy, Debug)]
pub struct DetailSettings {
    /// Linear input gain applied before clipping.
    pub drive: f32,
    /// Symmetric linear-amplitude ceiling.
    pub ceiling: f32,
    /// Reshaping mode.
    pub mode: ClipMode,
    /// Clipping-delta high-pass cutoff in hertz (Detail mode only).
    pub detail_hz: f32,
    /// Effect depth in `0.0..=1.0`: inward duck for Detail, clip→fold blend for
    /// Fold. Ignored by Clean.
    pub amount: f32,
    /// Fold transfer function (Fold mode only).
    pub shape: FoldShape,
}

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
        self.previous_input = 0.0;
        self.previous_output = 0.0;
        self.set_cutoff(DEFAULT_DETAIL_HIGH_PASS_HZ, sample_rate);
    }

    /// Recompute the coefficient for a new cutoff, preserving filter memory so
    /// the cutoff can be swept at audio rate without clicks.
    fn set_cutoff(&mut self, cutoff_hz: f64, sample_rate: f64) {
        let sample_rate = safe_sample_rate(sample_rate);
        let nyquist_guard = (sample_rate * 0.49).max(1.0);
        let cutoff =
            finite_or_f64(cutoff_hz, DEFAULT_DETAIL_HIGH_PASS_HZ).clamp(1.0, nyquist_guard);
        self.coefficient = (-std::f64::consts::TAU * cutoff / sample_rate).exp() as f32;
    }

    fn process(&mut self, input: f32) -> f32 {
        let output =
            self.coefficient * (self.previous_output + finite_or(input, 0.0) - self.previous_input);
        self.previous_input = finite_or(input, 0.0);
        self.previous_output = finite_or(output, 0.0);
        self.previous_output
    }
}

/// Stateful, detail-preserving clipper / wavefolder for one audio channel.
#[derive(Clone, Copy, Debug)]
pub struct DetailClipper {
    delta_high_pass: OnePoleHighPass,
    sample_rate: f64,
    current_cutoff_hz: f32,
    /// Previous driven input, the one-sample memory the Fold ADAA needs.
    prev_driven: f32,
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
        let sample_rate = safe_sample_rate(sample_rate);
        Self {
            delta_high_pass: OnePoleHighPass::new(sample_rate),
            sample_rate,
            current_cutoff_hz: DEFAULT_DETAIL_HIGH_PASS_HZ as f32,
            prev_driven: 0.0,
        }
    }

    /// Clear filter memory and update coefficients for a new sample rate.
    pub fn reset(&mut self, sample_rate: f64) {
        self.sample_rate = safe_sample_rate(sample_rate);
        self.delta_high_pass.reset(self.sample_rate);
        self.current_cutoff_hz = DEFAULT_DETAIL_HIGH_PASS_HZ as f32;
        self.prev_driven = 0.0;
    }

    /// Process one sample through the selected [`ClipMode`] while exposing the
    /// intermediate analysis signals.
    ///
    /// The output can never exceed the hard-clipping reference ceiling, and
    /// unclipped samples always pass through unchanged.
    #[must_use]
    pub fn process(&mut self, input: f32, settings: DetailSettings) -> DetailFrame {
        let frame = hard_clip_frame(input, settings.drive, settings.ceiling);

        // Retune the delta high-pass only when the cutoff actually moves, so a
        // static `Detail` knob costs no `exp()` per sample.
        let nyquist_guard = (self.sample_rate * 0.49).max(1.0) as f32;
        let cutoff = finite_or(settings.detail_hz, DEFAULT_DETAIL_HIGH_PASS_HZ as f32)
            .clamp(1.0, nyquist_guard);
        if cutoff != self.current_cutoff_hz {
            self.delta_high_pass
                .set_cutoff(f64::from(cutoff), self.sample_rate);
            self.current_cutoff_hz = cutoff;
        }

        // Run the filter every sample so its memory stays warm across mode
        // switches and unclipped gaps, avoiding clicks when clipping resumes.
        let filtered_delta = self.delta_high_pass.process(frame.delta);
        let amount = finite_or(settings.amount, 1.0).clamp(0.0, 1.0);

        let output = match settings.mode {
            ClipMode::Clean => frame.clipped,
            _ if frame.delta == 0.0 => frame.clipped,
            ClipMode::Detail => {
                // Rectified inward duck: the original detail-preserving voice.
                let magnitude = (frame.clipped.abs() - amount * filtered_delta.abs()).max(0.0);
                magnitude.copysign(frame.clipped)
            }
            ClipMode::Fold => {
                // Antialiased wavefolder over the driven signal. The ceiling
                // sets the fold amplitude and Drive (already baked into
                // `frame.driven`) sets the fold density.
                let ceiling = finite_or(settings.ceiling, 1.0)
                    .abs()
                    .max(f32::MIN_POSITIVE);
                fold_adaa(
                    frame.driven,
                    self.prev_driven,
                    ceiling,
                    amount,
                    settings.shape,
                )
            }
        };

        // The wavefolder consumes the driven sample as its one-sample memory.
        self.prev_driven = frame.driven;

        DetailFrame {
            driven: frame.driven,
            clipped: frame.clipped,
            delta: frame.delta,
            filtered_delta,
            output: finite_or(output, 0.0),
        }
    }

    /// Convenience wrapper preserving the original Detail voice: default cutoff,
    /// full inward depth. Kept so existing callers and tests are unaffected.
    #[must_use]
    pub fn process_sample(&mut self, input: f32, drive: f32, ceiling: f32) -> DetailFrame {
        self.process(
            input,
            DetailSettings {
                drive,
                ceiling,
                mode: ClipMode::Detail,
                detail_hz: DEFAULT_DETAIL_HIGH_PASS_HZ as f32,
                amount: 1.0,
                shape: FoldShape::Sine,
            },
        )
    }
}

// ---------------------------------------------------------------------------
// Wavefolder (Fold mode)
// ---------------------------------------------------------------------------
//
// A memoryless nonlinearity `h(x)` that blends a hard clip toward a periodic
// fold, made alias-suppressed with first-order antiderivative antialiasing
// (ADAA): `y = (H(x) - H(x1)) / (x - x1)`, where `H` is the antiderivative of
// `h`. All functions work on the driven sample `x`; the ceiling `c` (> 0) is
// the fold amplitude.

/// Threshold on `|x - x1|` below which the ADAA quotient is ill-conditioned and
/// we fall back to evaluating the nonlinearity at the midpoint.
const ADAA_EPSILON: f32 = 1.0e-5;

/// Continuous hard clip to `[-c, c]`.
fn clip_value(x: f32, c: f32) -> f32 {
    x.clamp(-c, c)
}

/// Antiderivative of [`clip_value`]: `x^2/2` inside the ceiling, linear beyond.
fn clip_antideriv(x: f32, c: f32) -> f32 {
    if x.abs() <= c {
        0.5 * x * x
    } else {
        c * x.abs() - 0.5 * c * c
    }
}

/// Fold transfer function value, bounded to `[-c, c]`.
fn fold_value(x: f32, c: f32, shape: FoldShape) -> f32 {
    let w = std::f32::consts::FRAC_PI_2 * x / c; // pi/2 * x/c; ceiling at x = c
    match shape {
        FoldShape::Sine => c * w.sin(),
        FoldShape::Triangle => c * std::f32::consts::FRAC_2_PI * w.sin().asin(),
    }
}

/// Antiderivative of [`fold_value`].
fn fold_antideriv(x: f32, c: f32, shape: FoldShape) -> f32 {
    match shape {
        FoldShape::Sine => {
            // ∫ c sin(pi x / 2c) dx = -(2 c^2 / pi) cos(pi x / 2c)
            let w = std::f32::consts::FRAC_PI_2 * x / c;
            -(2.0 * c * c / std::f32::consts::PI) * w.cos()
        }
        FoldShape::Triangle => triangle_antideriv(x, c),
    }
}

/// Antiderivative of the triangle fold, built by phase reduction.
///
/// The triangle has period `4c`, slopes `±1`, and `f(0) = 0` rising. Over one
/// period the running integral is a chain of parabolic arcs; the triangle is
/// zero-mean so the antiderivative is itself periodic. We reduce `x` into
/// `[-2c, 2c)` around the nearest period and integrate the local segment from a
/// reference where `F(0) = 0`.
fn triangle_antideriv(x: f32, c: f32) -> f32 {
    let period = 4.0 * c;
    // Phase in [-2c, 2c): the four linear segments are
    //   [-2c,-c): f = -2c - t   (rising from 0 at -2c? no) -- see mapping below.
    // Reduce to r in [-2c, 2c).
    let mut r = x - period * (x / period).round();
    if r < -2.0 * c {
        r += period;
    } else if r >= 2.0 * c {
        r -= period;
    }
    // f(r) piecewise:
    //   |r| <= c        : f = r                     (central rising segment)
    //   r >  c          : f = 2c - r                (folds back down)
    //   r < -c          : f = -2c - r               (folds back up)
    // Integrate from 0 with F(0) = 0, matching values at the ±c breakpoints.
    if r.abs() <= c {
        0.5 * r * r
    } else if r > c {
        // F(c) = c^2/2; from c to r, ∫(2c - t) dt = 2c(r-c) - (r^2-c^2)/2
        0.5 * c * c + 2.0 * c * (r - c) - 0.5 * (r * r - c * c)
    } else {
        // r < -c, mirror of the r > c branch (F is even for this triangle)
        let r = -r;
        0.5 * c * c + 2.0 * c * (r - c) - 0.5 * (r * r - c * c)
    }
}

/// Combined clip→fold nonlinearity, blended by `amount`.
fn fold_mix_value(x: f32, c: f32, amount: f32, shape: FoldShape) -> f32 {
    (1.0 - amount) * clip_value(x, c) + amount * fold_value(x, c, shape)
}

/// Antiderivative of [`fold_mix_value`].
fn fold_mix_antideriv(x: f32, c: f32, amount: f32, shape: FoldShape) -> f32 {
    (1.0 - amount) * clip_antideriv(x, c) + amount * fold_antideriv(x, c, shape)
}

/// First-order ADAA of the clip→fold nonlinearity for one sample.
fn fold_adaa(x: f32, x1: f32, c: f32, amount: f32, shape: FoldShape) -> f32 {
    let x = finite_or(x, 0.0);
    let x1 = finite_or(x1, 0.0);
    let out = if (x - x1).abs() < ADAA_EPSILON {
        fold_mix_value(0.5 * (x + x1), c, amount, shape)
    } else {
        (fold_mix_antideriv(x, c, amount, shape) - fold_mix_antideriv(x1, c, amount, shape))
            / (x - x1)
    };
    // Numerically the quotient can nudge a hair past the ceiling; keep it bounded.
    out.clamp(-c, c)
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

    fn settings(mode: ClipMode, detail_hz: f32, amount: f32) -> DetailSettings {
        DetailSettings {
            drive: 1.0,
            ceiling: 1.0,
            mode,
            detail_hz,
            amount,
            shape: FoldShape::Sine,
        }
    }

    fn fold(drive: f32, ceiling: f32, amount: f32, shape: FoldShape) -> DetailSettings {
        DetailSettings {
            drive,
            ceiling,
            mode: ClipMode::Fold,
            detail_hz: 1_000.0,
            amount,
            shape,
        }
    }

    #[test]
    fn clean_mode_is_a_plain_hard_clip() {
        let mut processor = DetailClipper::new(48_000.0);

        for input in [-2.0, -1.5, -0.5, 0.0, 0.5, 1.5, 2.0] {
            let frame = processor.process(input, settings(ClipMode::Clean, 1_000.0, 1.0));
            assert_eq!(frame.output, frame.clipped);
            assert_eq!(frame.output, hard_clip_frame(input, 1.0, 1.0).clipped);
        }
    }

    #[test]
    fn detail_zero_amount_leaves_the_hard_clip_untouched() {
        let mut processor = DetailClipper::new(48_000.0);
        for input in [1.1, 2.0, -1.4, -3.0, 0.9] {
            let frame = processor.process(input, settings(ClipMode::Detail, 1_000.0, 0.0));
            assert_eq!(frame.output, frame.clipped);
        }
    }

    #[test]
    fn fold_output_never_exceeds_the_ceiling() {
        for shape in [FoldShape::Sine, FoldShape::Triangle] {
            let mut processor = DetailClipper::new(96_000.0);
            for ceiling in [0.01_f32, 0.1, 0.5, 1.0, 2.0] {
                for input in [-100.0, -2.3, -0.75, 0.0, 0.75, 2.3, 100.0] {
                    let out = processor
                        .process(input, fold(4.0, ceiling, 1.0, shape))
                        .output;
                    assert!(out.abs() <= ceiling + 1.0e-6, "shape {shape:?} out {out}");
                }
            }
        }
    }

    #[test]
    fn fold_reflects_back_to_zero_at_twice_the_ceiling() {
        // f(2C) = 0 for both shapes (sin(pi) = 0; triangle 2C-2C = 0).
        for shape in [FoldShape::Sine, FoldShape::Triangle] {
            let mut processor = DetailClipper::new(48_000.0);
            let mut out = 0.0;
            for _ in 0..8 {
                // Drive 2.0, ceiling 1.0 => driven = 2.0 = 2C.
                out = processor.process(1.0, fold(2.0, 1.0, 1.0, shape)).output;
            }
            assert!(out.abs() < 1.0e-4, "shape {shape:?} settled at {out}");
        }
    }

    #[test]
    fn triangle_fold_is_transparent_below_the_ceiling() {
        // The triangle fold is the identity inside [-C, C]; a slow sub-ceiling
        // ramp should pass through (ADAA averages, so allow a small tolerance).
        let mut processor = DetailClipper::new(48_000.0);
        for step in -50..=50 {
            let x = step as f32 / 100.0; // |x| <= 0.5, well inside ceiling 1.0
            let out = processor
                .process(x, fold(1.0, 1.0, 1.0, FoldShape::Triangle))
                .output;
            assert!((out - x).abs() < 5.0e-3, "x {x} -> {out}");
        }
    }

    #[test]
    fn fold_is_polarity_symmetric() {
        for shape in [FoldShape::Sine, FoldShape::Triangle] {
            let mut positive = DetailClipper::new(48_000.0);
            let mut negative = DetailClipper::new(48_000.0);
            for input in [0.3, 0.8, 1.4, 2.0, 1.1, 0.5] {
                let up = positive.process(input, fold(1.5, 1.0, 1.0, shape)).output;
                let down = negative.process(-input, fold(1.5, 1.0, 1.0, shape)).output;
                assert!((up + down).abs() < 1.0e-6, "shape {shape:?}");
            }
        }
    }

    #[test]
    fn fold_antiderivatives_match_their_functions() {
        // Central finite difference of F should track f for both shapes.
        let c = 0.8_f32;
        let h = 1.0e-3_f32;
        for shape in [FoldShape::Sine, FoldShape::Triangle] {
            let mut x = -3.0_f32;
            while x <= 3.0 {
                // Skip the triangle's slope discontinuities where the central
                // difference straddles a corner.
                let near_corner = matches!(shape, FoldShape::Triangle)
                    && ((x / c + 1.0).rem_euclid(2.0) - 1.0).abs() < 5.0e-3;
                if !near_corner {
                    let numeric = (fold_antideriv(x + h, c, shape)
                        - fold_antideriv(x - h, c, shape))
                        / (2.0 * h);
                    let exact = fold_value(x, c, shape);
                    assert!(
                        (numeric - exact).abs() < 5.0e-3,
                        "shape {shape:?} x {x}: {numeric} vs {exact}"
                    );
                }
                x += 0.05;
            }
        }
    }

    #[test]
    fn fold_adaa_converges_to_the_pointwise_fold_for_small_steps() {
        // The ADAA quotient is the mean of f over [x1, x]; for tiny steps that
        // equals f(midpoint). (Larger steps diverge on purpose — that is the
        // aliasing suppression.)
        let c = 1.0_f32;
        for shape in [FoldShape::Sine, FoldShape::Triangle] {
            let mut x1 = -1.8_f32;
            let mut x = x1;
            while x <= 1.8 {
                let y = fold_adaa(x, x1, c, 1.0, shape);
                let mid = fold_mix_value(0.5 * (x + x1), c, 1.0, shape);
                assert!(
                    (y - mid).abs() < 1.0e-3,
                    "shape {shape:?} x {x}: {y} vs {mid}"
                );
                x1 = x;
                x += 0.001;
            }
        }
    }

    #[test]
    fn clean_and_detail_still_respect_the_ceiling() {
        for mode in [ClipMode::Clean, ClipMode::Detail] {
            let mut processor = DetailClipper::new(96_000.0);
            for ceiling in [0.01_f32, 0.1, 0.5, 1.0, 2.0] {
                for input in [-100.0, -2.0, -0.25, 0.25, 2.0, 100.0] {
                    let frame = processor.process(
                        input,
                        DetailSettings {
                            drive: 4.0,
                            ceiling,
                            mode,
                            detail_hz: 30.0,
                            amount: 1.0,
                            shape: FoldShape::Sine,
                        },
                    );
                    assert!(frame.output.abs() <= ceiling + 1.0e-6);
                }
            }
        }
    }
}
