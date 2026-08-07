//! Framework-independent Dattorro plate reverb for Kirya.
//!
//! A clean-room implementation of the reverberator in Jon Dattorro's "Effect
//! Design Part 1: Reverberator and Other Filters" (JAES 45(9), 1997), wearing
//! the user-facing control set popularised by ValleyAudio's Plateau: size,
//! freeze, four modulated tank allpasses, and split input / reverb damping.
//!
//! <https://ccrma.stanford.edu/~dattorro/EffectDesignPart1.pdf>
//!
//! The paper's topology is followed exactly; the feature set is Plateau's.
//! Kirya therefore does not sound identical to Plateau, which deviates from
//! the paper in several ways this crate deliberately does not reproduce.
//!
//! ```text
//! in L,R -> x0.5 sum -> DC block -> in low cut -> in high cut
//!        -> pre-delay -> 4 input diffusers -> split -> tank -> taps -> DC block
//! ```

pub mod allpass;
#[cfg(feature = "analysis")]
pub mod analysis;
pub mod constants;
pub mod delay;
pub mod filters;
pub mod lfo;
pub mod tank;

use musictools_core::{finite_or, finite_or_f64};

use crate::allpass::Allpass;
use crate::constants::{
    HALF_A, INPUT_DIFFUSER_GAINS, INPUT_DIFFUSER_LENGTHS, MOD_EXCURSION, PRE_DELAY_MAX_MS,
    SIZE_MAX, SIZE_MIN, sr_scale,
};
use crate::delay::InterpDelay;
use crate::filters::{DcBlocker, OnePoleHighPass, OnePoleLowPass};
use crate::lfo::ModBank;
use crate::tank::{Tank, TankSettings};

const DEFAULT_SAMPLE_RATE_HZ: f64 = 44_100.0;

/// Interpolation headroom added to every delay line, in samples.
pub(crate) const READ_HEADROOM: usize = 4;

/// How long Freeze takes to fade the damping out and the decay up.
const FREEZE_FADE_MS: f64 = 50.0;

/// Longest tail the reverb will ever report, in seconds. At decay 0.9999 the
/// raw estimate runs to hours, which no host wants to keep a track alive for.
const MAX_TAIL_SECONDS: f64 = 30.0;

/// Fixed output trim, so wet at 100% sits at roughly the same level as dry.
///
/// Measured at 48 kHz with white noise at the default settings (Size 1.0,
/// Decay 0.55, Diffusion 1.0, Rev High Cut 10 kHz), skipping the first second
/// so the tank has filled: the untrimmed seven-tap sum came out at 0.893x the
/// dry RMS, and 1/0.893 is the reciprocal applied here. Guarded by
/// `output_trim_matches_dry_level`.
const OUTPUT_TRIM: f32 = 1.12;

/// Coerce a host-supplied sample rate to a strictly positive, finite value so
/// coefficient math and delay-length scaling can never divide by zero, invert,
/// or size a buffer from a NaN.
#[inline]
fn safe_sample_rate(sample_rate: f64) -> f64 {
    if sample_rate.is_finite() && sample_rate > 0.0 {
        sample_rate
    } else {
        DEFAULT_SAMPLE_RATE_HZ
    }
}

/// Per-sample control values for [`KiryaReverb::process`].
///
/// One `Copy` struct in, one [`KiryaFrame`] out, mirroring the shape the suite
/// already uses for Bogdan.
#[derive(Clone, Copy, Debug)]
pub struct KiryaSettings {
    /// Linear gain on the untouched input.
    pub dry: f32,
    /// Linear gain on the reverb output.
    pub wet: f32,
    /// Pre-delay in milliseconds. Does not scale with Size.
    pub pre_delay_ms: f32,
    /// Plate size multiplier applied to every tank length and output tap.
    pub size: f32,
    /// Diffusion amount in `0.0..=1.0`, scaling both tank allpass coefficients.
    pub diffusion: f32,
    /// Decay knob in `0.0..=1.0`, before the perceptual curve.
    pub decay: f32,
    /// Input high-pass corner in hertz.
    pub input_low_cut_hz: f32,
    /// Input low-pass corner in hertz.
    pub input_high_cut_hz: f32,
    /// Tank damping high-pass corner in hertz.
    pub reverb_low_cut_hz: f32,
    /// Tank damping low-pass corner in hertz.
    pub reverb_high_cut_hz: f32,
    /// Modulation rate of the slowest of the four tank LFOs, in hertz.
    pub mod_rate_hz: f32,
    /// Modulation depth in `0.0..=1.0`.
    pub mod_depth: f32,
    /// Modulation waveform: `0.0` triangle, `1.0` sine.
    pub mod_shape: f32,
    /// Hold the tank indefinitely, with the input still live.
    pub freeze: bool,
}

impl Default for KiryaSettings {
    fn default() -> Self {
        Self {
            dry: 1.0,
            wet: 0.5,
            pre_delay_ms: 0.0,
            size: 1.0,
            diffusion: 1.0,
            decay: 0.55,
            input_low_cut_hz: 20.0,
            input_high_cut_hz: 20_000.0,
            reverb_low_cut_hz: 20.0,
            reverb_high_cut_hz: 10_000.0,
            mod_rate_hz: 1.0,
            mod_depth: 0.5,
            mod_shape: 0.5,
            freeze: false,
        }
    }
}

/// The per-sample components produced by [`KiryaReverb::process`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KiryaFrame {
    /// Final left output, dry and wet mixed.
    pub left: f32,
    /// Final right output, dry and wet mixed.
    pub right: f32,
    /// Trimmed reverb-only left output, before the wet gain.
    pub wet_left: f32,
    /// Trimmed reverb-only right output, before the wet gain.
    pub wet_right: f32,
}

/// A complete plate reverb for one stereo pair.
///
/// Inputs are summed to mono before the tank — the paper's topology is a mono
/// plate whose stereo image comes from the crossed output tap network, not
/// from two independent tanks.
#[derive(Clone, Debug)]
pub struct KiryaReverb {
    sample_rate: f64,

    input_block: DcBlocker,
    input_low_cut: OnePoleHighPass,
    input_high_cut: OnePoleLowPass,
    pre_delay: InterpDelay,
    diffusers: [Allpass; 4],

    tank: Tank,
    modulation: ModBank,

    left_block: DcBlocker,
    right_block: DcBlocker,

    /// Freeze crossfade position: `1.0` running, `0.0` fully frozen.
    freeze_fade: f32,
    freeze_step: f32,

    /// Cached reverb-time estimate and the inputs it was computed from.
    tail_samples: u32,
    cached_decay: f32,
    cached_size: f32,
    cached_pre_delay_ms: f32,
}

impl Default for KiryaReverb {
    fn default() -> Self {
        Self::new(DEFAULT_SAMPLE_RATE_HZ)
    }
}

impl KiryaReverb {
    /// Construct a reverb sized for `sample_rate` hertz.
    #[must_use]
    pub fn new(sample_rate: f64) -> Self {
        let mut reverb = Self {
            sample_rate: DEFAULT_SAMPLE_RATE_HZ,
            input_block: DcBlocker::default(),
            input_low_cut: OnePoleHighPass::default(),
            input_high_cut: OnePoleLowPass::default(),
            pre_delay: InterpDelay::default(),
            diffusers: Default::default(),
            tank: Tank::default(),
            modulation: ModBank::default(),
            left_block: DcBlocker::default(),
            right_block: DcBlocker::default(),
            freeze_fade: 1.0,
            freeze_step: 0.0,
            tail_samples: 0,
            cached_decay: f32::NAN,
            cached_size: f32::NAN,
            cached_pre_delay_ms: f32::NAN,
        };
        reverb.reset(sample_rate);
        reverb
    }

    /// Resize every line for a new sample rate and clear all state.
    ///
    /// Buffers are sized for the largest Size plus the widest modulation
    /// excursion, so changing Size at runtime never reallocates.
    pub fn reset(&mut self, sample_rate: f64) {
        let sample_rate = safe_sample_rate(sample_rate);
        self.sample_rate = sample_rate;
        let scale = sr_scale(sample_rate);

        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let samples = |value: f64| value.ceil() as usize + READ_HEADROOM;

        self.pre_delay
            .resize(samples(PRE_DELAY_MAX_MS * sample_rate / 1_000.0));
        for (diffuser, length) in self.diffusers.iter_mut().zip(INPUT_DIFFUSER_LENGTHS) {
            // Input diffusers scale with sample rate but not with Size: they
            // set the initial echo density, which should not stretch when the
            // plate grows.
            diffuser.resize(samples(length * scale));
        }
        self.tank
            .resize(scale * f64::from(SIZE_MAX), MOD_EXCURSION * scale);

        self.input_block.reset(sample_rate);
        self.left_block.reset(sample_rate);
        self.right_block.reset(sample_rate);
        self.input_low_cut.clear();
        self.input_high_cut.clear();
        self.modulation.reset();

        #[allow(clippy::cast_possible_truncation)]
        {
            self.freeze_step = (1_000.0 / (FREEZE_FADE_MS * sample_rate)) as f32;
        }
        self.freeze_fade = 1.0;

        self.tail_samples = 0;
        self.cached_decay = f32::NAN;
        self.cached_size = f32::NAN;
        self.cached_pre_delay_ms = f32::NAN;
    }

    /// Clear all filter and delay memory without reallocating.
    pub fn clear(&mut self) {
        self.pre_delay.clear();
        for diffuser in &mut self.diffusers {
            diffuser.clear();
        }
        self.tank.clear();
        self.input_block.reset(self.sample_rate);
        self.left_block.reset(self.sample_rate);
        self.right_block.reset(self.sample_rate);
        self.input_low_cut.clear();
        self.input_high_cut.clear();
        self.modulation.reset();
        self.freeze_fade = 1.0;
    }

    /// Sample rate the reverb is currently sized for.
    #[must_use]
    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    /// Estimated reverb tail in samples, for the host's tail-time report.
    ///
    /// Updated by [`process`](Self::process); zero until the first sample.
    #[must_use]
    pub fn tail_samples(&self) -> u32 {
        self.tail_samples
    }

    /// Process one stereo sample.
    #[must_use]
    pub fn process(&mut self, left: f32, right: f32, settings: KiryaSettings) -> KiryaFrame {
        let left = finite_or(left, 0.0);
        let right = finite_or(right, 0.0);

        let size = finite_or(settings.size, 1.0).clamp(SIZE_MIN, SIZE_MAX);
        let decay = map_decay(settings.decay);
        let diffusion = finite_or(settings.diffusion, 1.0).clamp(0.0, 1.0);
        let pre_delay_ms =
            finite_or(settings.pre_delay_ms, 0.0).clamp(0.0, PRE_DELAY_MAX_MS as f32);

        self.update_tail_estimate(decay, size, pre_delay_ms);

        // Freeze crossfades rather than switches, so engaging it on a dark
        // tail does not snap the tail bright.
        let target = if settings.freeze { 0.0 } else { 1.0 };
        self.freeze_fade = if self.freeze_fade < target {
            (self.freeze_fade + self.freeze_step).min(target)
        } else {
            (self.freeze_fade - self.freeze_step).max(target)
        };
        let fade = self.freeze_fade;
        // At full freeze the loop gain reaches exactly 1.0 and the tank holds.
        let effective_decay = decay + (1.0 - decay) * (1.0 - fade);

        // ---- input chain -------------------------------------------------
        let mono = 0.5 * (left + right);
        let mono = self.input_block.process(mono);
        let mono = self
            .input_low_cut
            .process(mono, settings.input_low_cut_hz, self.sample_rate);
        let mut mono =
            self.input_high_cut
                .process(mono, settings.input_high_cut_hz, self.sample_rate);

        #[allow(clippy::cast_possible_truncation)]
        let pre_delay_samples = (f64::from(pre_delay_ms) * self.sample_rate / 1_000.0) as f32;
        let delayed = self.pre_delay.read(pre_delay_samples.max(1.0));
        self.pre_delay.write(mono);
        mono = delayed;

        #[allow(clippy::cast_possible_truncation)]
        let input_scale = sr_scale(self.sample_rate) as f32;
        for (diffuser, (length, gain)) in self
            .diffusers
            .iter_mut()
            .zip(INPUT_DIFFUSER_LENGTHS.into_iter().zip(INPUT_DIFFUSER_GAINS))
        {
            #[allow(clippy::cast_possible_truncation)]
            let delay = length as f32 * input_scale;
            mono = diffuser.process(mono, delay, gain);
        }

        // ---- tank ---------------------------------------------------------
        let modulation =
            self.modulation
                .next(settings.mod_rate_hz, settings.mod_shape, self.sample_rate);
        #[allow(clippy::cast_possible_truncation)]
        let length_scale = (sr_scale(self.sample_rate) * f64::from(size)) as f32;
        #[allow(clippy::cast_possible_truncation)]
        let excursion = (MOD_EXCURSION * sr_scale(self.sample_rate)) as f32
            * finite_or(settings.mod_depth, 0.0).clamp(0.0, 1.0);

        let (raw_left, raw_right) = self.tank.process(
            mono,
            TankSettings {
                decay: effective_decay,
                decay_diffusion_1: diffusion * 0.70,
                // The paper's stability clamp is on decay diffusion *2*, not
                // 1. Scaling by Diffusion on top still lets one knob remove
                // all tank diffusion.
                decay_diffusion_2: diffusion * (decay + 0.15).clamp(0.25, 0.50),
                high_cut_hz: settings.reverb_high_cut_hz,
                low_cut_hz: settings.reverb_low_cut_hz,
                damp_fade: fade,
                length_scale,
                excursion,
                sample_rate: self.sample_rate,
            },
            modulation,
        );

        let wet_left = OUTPUT_TRIM * self.left_block.process(raw_left);
        let wet_right = OUTPUT_TRIM * self.right_block.process(raw_right);

        let dry = finite_or(settings.dry, 1.0);
        let wet = finite_or(settings.wet, 0.0);
        KiryaFrame {
            left: finite_or(dry * left + wet * wet_left, 0.0),
            right: finite_or(dry * right + wet * wet_right, 0.0),
            wet_left: finite_or(wet_left, 0.0),
            wet_right: finite_or(wet_right, 0.0),
        }
    }

    /// Recompute the tail estimate only when its inputs actually move, so a
    /// static knob costs no `log10()` per sample.
    fn update_tail_estimate(&mut self, decay: f32, size: f32, pre_delay_ms: f32) {
        if decay == self.cached_decay
            && size == self.cached_size
            && pre_delay_ms == self.cached_pre_delay_ms
        {
            return;
        }
        self.cached_decay = decay;
        self.cached_size = size;
        self.cached_pre_delay_ms = pre_delay_ms;

        let seconds =
            tail_seconds(decay, size, pre_delay_ms, self.sample_rate).clamp(0.0, MAX_TAIL_SECONDS);

        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        {
            self.tail_samples = (seconds * self.sample_rate) as u32;
        }
    }
}

/// Estimated time for the tail to fall 60 dB, in seconds.
///
/// Unclamped, so a decay at the top of its range returns infinity — callers
/// apply whatever ceiling suits them. The host-facing
/// [`KiryaReverb::tail_samples`] caps at 30 seconds; the editor's IR probe uses
/// this to size its render window.
///
/// Freeze is ignored: it is a hold, not a decay, and every caller either forces
/// it off or handles it separately.
#[must_use]
pub fn estimated_tail_seconds(settings: KiryaSettings, sample_rate: f64) -> f64 {
    let size = finite_or(settings.size, 1.0).clamp(SIZE_MIN, SIZE_MAX);
    let pre_delay_ms = finite_or(settings.pre_delay_ms, 0.0).clamp(0.0, PRE_DELAY_MAX_MS as f32);
    tail_seconds(
        map_decay(settings.decay),
        size,
        pre_delay_ms,
        safe_sample_rate(sample_rate),
    )
}

/// Shared reverb-time maths. `decay` is already mapped, `sample_rate` sanitised.
fn tail_seconds(decay: f32, size: f32, pre_delay_ms: f32, sample_rate: f64) -> f64 {
    // One traversal of a half applies `decay` twice, so the loop gain is
    // `decay^2` and the 60 dB point is `loop * 3 / -2*log10(decay)`.
    let loop_samples = HALF_A.loop_length() * sr_scale(sample_rate) * f64::from(size);
    let attenuation = -2.0 * finite_or_f64(f64::from(decay), 0.5).log10();
    let decay_samples = if attenuation > 0.0 {
        loop_samples * 3.0 / attenuation
    } else {
        f64::INFINITY
    };
    let pre_delay_samples = f64::from(pre_delay_ms) * sample_rate / 1_000.0;
    (decay_samples + pre_delay_samples) / sample_rate
}

/// Map the Decay knob onto loop gain.
///
/// `1 - (1-x)^2` puts most of the knob's travel in the long-tail region where
/// small gain changes are audible, and the clamp keeps the tank strictly
/// inside the unit circle so it always eventually decays.
fn map_decay(knob: f32) -> f32 {
    let knob = finite_or(knob, 0.5).clamp(0.0, 1.0);
    let inverted = 1.0 - knob;
    (1.0 - inverted * inverted).clamp(0.1, 0.9999)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_RATE: f64 = 48_000.0;

    fn settings(decay: f32) -> KiryaSettings {
        KiryaSettings {
            decay,
            wet: 1.0,
            dry: 0.0,
            ..KiryaSettings::default()
        }
    }

    /// Render a wet-only impulse response and return its left channel.
    fn impulse_response(
        reverb: &mut KiryaReverb,
        config: KiryaSettings,
        samples: usize,
    ) -> Vec<f32> {
        (0..samples)
            .map(|n| {
                let input = if n == 0 { 1.0 } else { 0.0 };
                reverb.process(input, input, config).wet_left
            })
            .collect()
    }

    fn rms(samples: &[f32]) -> f64 {
        if samples.is_empty() {
            return 0.0;
        }
        let sum: f64 = samples.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
        #[allow(clippy::cast_precision_loss)]
        {
            (sum / samples.len() as f64).sqrt()
        }
    }

    fn to_db(value: f64) -> f64 {
        20.0 * value.max(1.0e-12).log10()
    }

    /// Deterministic white-ish noise, so the trim calibration is repeatable.
    fn noise(count: usize) -> Vec<f32> {
        let mut state = 0x2545_F491_4F6C_DD1D_u64;
        (0..count)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                #[allow(clippy::cast_precision_loss)]
                {
                    ((state >> 40) as f32 / 8_388_608.0) - 1.0
                }
            })
            .collect()
    }

    /// Energy decay curve in dB, from the tail backwards (Schroeder).
    fn rt60_seconds(response: &[f32], sample_rate: f64) -> Option<f64> {
        let mut running = 0.0_f64;
        let mut curve = vec![0.0_f64; response.len()];
        for (index, &sample) in response.iter().enumerate().rev() {
            running += f64::from(sample) * f64::from(sample);
            curve[index] = running;
        }
        let total = curve[0];
        if total <= 0.0 {
            return None;
        }
        let level = |index: usize| 10.0 * (curve[index] / total).max(1.0e-30).log10();
        let find = |target: f64| (0..curve.len()).find(|&index| level(index) <= target);
        // Fit over the -5 dB to -35 dB span and extrapolate, the usual T30.
        let start = find(-5.0)?;
        let end = find(-35.0)?;
        #[allow(clippy::cast_precision_loss)]
        Some((end - start) as f64 / sample_rate * 2.0)
    }

    #[test]
    fn non_finite_input_and_settings_yield_finite_output() {
        let mut reverb = KiryaReverb::new(f64::NAN);
        let broken = KiryaSettings {
            dry: f32::NAN,
            wet: f32::INFINITY,
            pre_delay_ms: f32::NAN,
            size: f32::INFINITY,
            diffusion: f32::NAN,
            decay: f32::INFINITY,
            input_low_cut_hz: f32::NAN,
            input_high_cut_hz: f32::NAN,
            reverb_low_cut_hz: f32::NEG_INFINITY,
            reverb_high_cut_hz: f32::NAN,
            mod_rate_hz: f32::NAN,
            mod_depth: f32::INFINITY,
            mod_shape: f32::NAN,
            freeze: false,
        };
        for n in 0..10_000 {
            let sample = match n % 4 {
                0 => f32::NAN,
                1 => f32::INFINITY,
                2 => f32::NEG_INFINITY,
                _ => 0.5,
            };
            let frame = reverb.process(sample, sample, broken);
            assert!(frame.left.is_finite(), "sample {n}: {frame:?}");
            assert!(frame.right.is_finite(), "sample {n}: {frame:?}");
            assert!(frame.wet_left.is_finite(), "sample {n}: {frame:?}");
            assert!(frame.wet_right.is_finite(), "sample {n}: {frame:?}");
        }
    }

    #[test]
    fn a_short_decay_dies_away_where_a_long_one_does_not() {
        // Three seconds rendered, the last half-second measured.
        const WINDOW: usize = 144_000;
        const TAIL: usize = 120_000;

        let mut short = KiryaReverb::new(SAMPLE_RATE);
        let response = impulse_response(&mut short, settings(0.5), WINDOW);
        let tail = to_db(rms(&response[TAIL..]));
        assert!(tail < -60.0, "decay 0.5 tail sat at {tail} dB");

        let mut long = KiryaReverb::new(SAMPLE_RATE);
        let response = impulse_response(&mut long, settings(0.99), WINDOW);
        let tail = to_db(rms(&response[TAIL..]));
        assert!(tail > -60.0, "decay 0.99 tail sat at {tail} dB");
    }

    #[test]
    fn a_larger_plate_rings_for_longer() {
        // Decay 0.3 keeps RT60 comfortably inside the ten-second window even
        // at Size 4.0; a longer decay would truncate the Schroeder curve and
        // make the fit, not the reverb, decide the answer.
        let mut previous = 0.0;
        for size in [0.25_f32, 0.5, 1.0, 2.0, 4.0] {
            let mut reverb = KiryaReverb::new(SAMPLE_RATE);
            let config = KiryaSettings {
                size,
                ..settings(0.3)
            };
            let response = impulse_response(&mut reverb, config, 480_000);
            let rt60 = rt60_seconds(&response, SAMPLE_RATE).expect("decays within the window");
            assert!(
                rt60 > previous,
                "size {size}: RT60 {rt60} did not exceed {previous}"
            );
            previous = rt60;
        }
    }

    #[test]
    fn a_lower_reverb_high_cut_darkens_the_tail() {
        let centroid = |cutoff: f32| {
            let mut reverb = KiryaReverb::new(SAMPLE_RATE);
            let config = KiryaSettings {
                reverb_high_cut_hz: cutoff,
                ..settings(0.8)
            };
            let response = impulse_response(&mut reverb, config, 96_000);
            spectral_centroid(&response[24_000..48_000], SAMPLE_RATE)
        };

        let dark = centroid(1_000.0);
        let bright = centroid(16_000.0);
        assert!(
            dark < bright,
            "dark {dark} Hz was not below bright {bright} Hz"
        );
    }

    #[test]
    fn reset_makes_a_used_instance_match_a_fresh_one() {
        let mut used = KiryaReverb::new(SAMPLE_RATE);
        let mut fresh = KiryaReverb::new(SAMPLE_RATE);
        let config = settings(0.8);

        for n in 0..20_000 {
            let _ = used.process(if n == 0 { 1.0 } else { 0.0 }, 0.5, config);
        }
        used.reset(SAMPLE_RATE);

        for n in 0..8_000 {
            let input = if n == 0 { 1.0 } else { 0.0 };
            assert_eq!(
                used.process(input, input, config),
                fresh.process(input, input, config),
                "sample {n}"
            );
        }
    }

    #[test]
    fn a_frozen_tank_holds_its_level_for_five_seconds() {
        // Measured against the same tank left running, which is the claim
        // that matters: Freeze turns a decaying tail into a sustained one.
        //
        // The frozen loop gain is exactly 1.0 and the damping is faded out, so
        // the tank is lossless apart from the fractional-delay interpolator,
        // which is a mild low-pass. That costs about 2.5 dB of RMS over five
        // seconds at Size 1.0 (an RT60 near two minutes) as the tail slowly
        // darkens — inherent to the linear interpolation the paper's taps need
        // for smooth Size sweeps, and it shrinks as Size grows because the
        // loop is traversed fewer times per second.
        let drift = |freeze: bool| {
            let mut reverb = KiryaReverb::new(SAMPLE_RATE);
            let running = settings(0.6);
            let held = KiryaSettings { freeze, ..running };

            for n in 0..24_000 {
                let _ = reverb.process(if n == 0 { 1.0 } else { 0.0 }, 0.0, running);
            }
            // Let the 50 ms crossfade settle before the first measurement.
            for _ in 0..24_000 {
                let _ = reverb.process(0.0, 0.0, held);
            }

            let second = SAMPLE_RATE as usize;
            let first: Vec<f32> = (0..second)
                .map(|_| reverb.process(0.0, 0.0, held).wet_left)
                .collect();
            for _ in 0..4 * second {
                let _ = reverb.process(0.0, 0.0, held);
            }
            let last: Vec<f32> = (0..second)
                .map(|_| reverb.process(0.0, 0.0, held).wet_left)
                .collect();
            to_db(rms(&last)) - to_db(rms(&first))
        };

        let frozen = drift(true);
        let running = drift(false);
        assert!(frozen > -3.0, "frozen level fell {frozen} dB over 5 s");
        assert!(
            frozen > running + 30.0,
            "freeze ({frozen} dB) barely beat the running tank ({running} dB)"
        );
    }

    #[test]
    fn freeze_still_accepts_new_material() {
        let mut reverb = KiryaReverb::new(SAMPLE_RATE);
        let frozen = KiryaSettings {
            freeze: true,
            ..settings(0.6)
        };
        for _ in 0..24_000 {
            let _ = reverb.process(0.0, 0.0, frozen);
        }
        let quiet = rms(&(0..24_000)
            .map(|_| reverb.process(0.0, 0.0, frozen).wet_left)
            .collect::<Vec<_>>());

        let input = noise(24_000);
        for &sample in &input {
            let _ = reverb.process(sample, sample, frozen);
        }
        let loud = rms(&(0..24_000)
            .map(|_| reverb.process(0.0, 0.0, frozen).wet_left)
            .collect::<Vec<_>>());

        assert!(loud > quiet + 0.01, "frozen tank ignored new input");
    }

    #[test]
    fn the_tail_estimate_grows_with_decay_and_size_and_stays_bounded() {
        let mut reverb = KiryaReverb::new(SAMPLE_RATE);
        let measure = |reverb: &mut KiryaReverb, config: KiryaSettings| {
            let _ = reverb.process(0.0, 0.0, config);
            reverb.tail_samples()
        };

        // Decays chosen to stay under the 30 s cap, so the comparison tests
        // the estimate rather than the clamp.
        let short = measure(&mut reverb, settings(0.2));
        let long = measure(&mut reverb, settings(0.5));
        assert!(long > short, "{long} was not longer than {short}");

        let big = measure(
            &mut reverb,
            KiryaSettings {
                size: 4.0,
                ..settings(0.5)
            },
        );
        assert!(big > long, "{big} was not longer than {long}");

        let extreme = measure(&mut reverb, settings(1.0));
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let ceiling = (MAX_TAIL_SECONDS * SAMPLE_RATE) as u32;
        assert!(extreme <= ceiling, "{extreme} exceeded the {ceiling} cap");
    }

    #[test]
    fn sweeping_size_across_its_whole_range_stays_finite() {
        // Buffers are sized in `reset` for SIZE_MAX, so a Size sweep only
        // changes read distances. The zero-allocation guarantee itself is
        // asserted by the wrapper's `rt-paranoid` test.
        let mut reverb = KiryaReverb::new(SAMPLE_RATE);
        for step in 0..1_000 {
            #[allow(clippy::cast_precision_loss)]
            let size = SIZE_MIN + (SIZE_MAX - SIZE_MIN) * (step as f32 / 1_000.0);
            let frame = reverb.process(
                0.1,
                -0.1,
                KiryaSettings {
                    size,
                    ..settings(0.8)
                },
            );
            assert!(frame.wet_left.is_finite());
        }
    }

    #[test]
    fn output_trim_matches_dry_level() {
        // Calibration guard: wet at 100% should land within a few dB of the
        // dry signal so the Dry/Wet knobs are usable without a gain stage.
        let mut reverb = KiryaReverb::new(SAMPLE_RATE);
        let config = KiryaSettings {
            dry: 0.0,
            wet: 1.0,
            ..KiryaSettings::default()
        };
        let input = noise(SAMPLE_RATE as usize * 3);
        let mut wet = Vec::with_capacity(input.len());
        for &sample in &input {
            wet.push(reverb.process(sample, sample, config).wet_left);
        }

        // Skip the first second so the tank has filled.
        let dry_rms = rms(&input[SAMPLE_RATE as usize..]);
        let wet_rms = rms(&wet[SAMPLE_RATE as usize..]);
        let difference = to_db(wet_rms) - to_db(dry_rms);
        assert!(
            difference.abs() < 3.0,
            "wet sits {difference} dB from dry (raw ratio {})",
            wet_rms / dry_rms / f64::from(OUTPUT_TRIM)
        );
    }

    #[test]
    fn the_public_tail_estimate_agrees_with_the_reported_one() {
        // The editor sizes its render window from `estimated_tail_seconds`
        // while the host reads `tail_samples`; they must not disagree below
        // the 30 s cap or the probe would frame a tail the host cuts off.
        let mut reverb = KiryaReverb::new(SAMPLE_RATE);
        for decay in [0.1_f32, 0.3, 0.5, 0.6] {
            for size in [0.25_f32, 1.0, 2.0] {
                let config = KiryaSettings {
                    size,
                    ..settings(decay)
                };
                let _ = reverb.process(0.0, 0.0, config);
                let estimate = estimated_tail_seconds(config, SAMPLE_RATE);
                assert!(estimate < MAX_TAIL_SECONDS, "{estimate} hit the cap");

                #[allow(clippy::cast_precision_loss)]
                let reported = f64::from(reverb.tail_samples()) / SAMPLE_RATE;
                assert!(
                    (estimate - reported).abs() < 0.01,
                    "decay {decay} size {size}: {estimate} vs {reported}"
                );
            }
        }
    }

    #[test]
    fn the_tail_estimate_rises_with_decay_and_size_and_survives_the_extremes() {
        let seconds = |decay: f32, size: f32| {
            estimated_tail_seconds(
                KiryaSettings {
                    size,
                    ..settings(decay)
                },
                SAMPLE_RATE,
            )
        };

        let mut previous = 0.0;
        for decay in [0.1_f32, 0.3, 0.5, 0.7, 0.9] {
            let value = seconds(decay, 1.0);
            assert!(value > previous, "decay {decay}: {value} <= {previous}");
            previous = value;
        }

        previous = 0.0;
        for size in [0.05_f32, 0.5, 1.0, 2.0, 4.0] {
            let value = seconds(0.5, size);
            assert!(value > previous, "size {size}: {value} <= {previous}");
            previous = value;
        }

        // The decay map stops just short of 1.0, so the top of the knob is a
        // finite but enormous estimate (hours) rather than an infinity. Either
        // way it is the caller's job to cap it.
        let longest = seconds(1.0, 1.0);
        assert!(
            longest > MAX_TAIL_SECONDS * 100.0,
            "top of the decay range only reached {longest} s"
        );
        // Garbage settings must not produce a NaN window length.
        assert!(
            estimated_tail_seconds(
                KiryaSettings {
                    size: f32::NAN,
                    pre_delay_ms: f32::NAN,
                    ..settings(f32::NAN)
                },
                f64::NAN,
            )
            .is_finite()
        );
        // Pre-delay is part of the tail the host has to keep alive.
        let config = settings(0.4);
        assert!(
            estimated_tail_seconds(
                KiryaSettings {
                    pre_delay_ms: 500.0,
                    ..config
                },
                SAMPLE_RATE
            ) > estimated_tail_seconds(config, SAMPLE_RATE) + 0.4
        );
    }

    #[test]
    fn decay_mapping_is_monotonic_and_bounded() {
        let mut previous = 0.0;
        for step in 0..=100 {
            #[allow(clippy::cast_precision_loss)]
            let value = map_decay(step as f32 / 100.0);
            assert!((0.1..=0.9999).contains(&value), "{value}");
            assert!(value >= previous, "{value} < {previous}");
            previous = value;
        }
    }

    /// Magnitude-weighted mean frequency, via a naive DFT over a coarse grid.
    fn spectral_centroid(signal: &[f32], sample_rate: f64) -> f64 {
        use std::f64::consts::TAU;
        let n = signal.len() as f64;
        let (mut weighted, mut total) = (0.0, 0.0);
        for bin in (1..signal.len() / 2).step_by(37) {
            let (mut re, mut im) = (0.0, 0.0);
            for (index, &sample) in signal.iter().enumerate() {
                let angle = -TAU * bin as f64 * index as f64 / n;
                re += f64::from(sample) * angle.cos();
                im += f64::from(sample) * angle.sin();
            }
            let magnitude = re.hypot(im);
            let frequency = bin as f64 * sample_rate / n;
            weighted += magnitude * frequency;
            total += magnitude;
        }
        if total > 0.0 { weighted / total } else { 0.0 }
    }
}
