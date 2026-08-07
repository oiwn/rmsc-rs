//! The figure-of-eight reverberation tank.
//!
//! Two halves, each `APF1 -> Delay 1 -> damping -> x decay -> APF2 -> Delay 2
//! -> x decay`, with each half's final output crossing into the *other* half's
//! first allpass. The diffused input is summed into both first allpasses.
//!
//! Halves are named by their first allpass length: A is 672, B is 908.
//! `APF1` takes the coefficient with the opposite sign to `APF2` — the paper's
//! "note sign" annotation on Fig. 1. Relative polarity is what matters, not
//! which of the two is negative.

use musictools_core::finite_or;

use crate::allpass::Allpass;
use crate::constants::{
    HALF_A, HALF_B, HalfLengths, LEFT_TAPS, OUTPUT_TAP_GAIN, RIGHT_TAPS, Tap, TapLine,
};
use crate::delay::InterpDelay;
use crate::filters::{OnePoleHighPass, OnePoleLowPass};

use crate::READ_HEADROOM;

/// Per-sample control values for [`Tank::process`].
#[derive(Clone, Copy, Debug)]
pub struct TankSettings {
    /// Loop gain applied twice per traversal.
    pub decay: f32,
    /// Coefficient magnitude for `APF1`, applied negated.
    pub decay_diffusion_1: f32,
    /// Coefficient magnitude for `APF2`, applied as-is.
    pub decay_diffusion_2: f32,
    /// Damping high-cut corner in hertz.
    pub high_cut_hz: f32,
    /// Damping low-cut corner in hertz.
    pub low_cut_hz: f32,
    /// Damping blend: `1.0` is fully damped, `0.0` bypasses damping entirely
    /// (what Freeze crossfades to).
    pub damp_fade: f32,
    /// Combined `sample_rate / FS_REF * size` multiplier for every tank length.
    pub length_scale: f32,
    /// Peak one-sided allpass excursion in samples at the host rate.
    pub excursion: f32,
    /// Host sample rate, for the damping filter coefficients.
    pub sample_rate: f64,
}

/// One half of the tank.
#[derive(Clone, Debug, Default)]
struct TankHalf {
    apf1: Allpass,
    delay1: InterpDelay,
    apf2: Allpass,
    delay2: InterpDelay,
    high_cut: OnePoleLowPass,
    low_cut: OnePoleHighPass,
}

impl TankHalf {
    /// Allocate every line for the largest Size plus modulation headroom.
    fn resize(&mut self, lengths: HalfLengths, max_scale: f64, max_excursion: f64) {
        let capacity = |paper: f64, extra: f64| -> usize {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::cast_precision_loss
            )]
            {
                (paper * max_scale + extra).ceil() as usize + READ_HEADROOM
            }
        };
        // Only the allpasses are modulated, so only they carry excursion
        // headroom.
        self.apf1.resize(capacity(lengths.apf1, max_excursion));
        self.delay1.resize(capacity(lengths.delay1, 0.0));
        self.apf2.resize(capacity(lengths.apf2, max_excursion));
        self.delay2.resize(capacity(lengths.delay2, 0.0));
        // Resizing the lines is not enough: the damping filters carry their
        // own memory, and a retained `previous_output` would inject a decaying
        // tail into an otherwise empty tank.
        self.high_cut.clear();
        self.low_cut.clear();
    }

    fn clear(&mut self) {
        self.apf1.clear();
        self.delay1.clear();
        self.apf2.clear();
        self.delay2.clear();
        self.high_cut.clear();
        self.low_cut.clear();
    }

    /// The value crossing into the *other* half this sample. Read before any
    /// of this sample's writes, so the cross costs no extra delay.
    fn feedback(&self, lengths: HalfLengths, settings: TankSettings) -> f32 {
        #[allow(clippy::cast_possible_truncation)]
        let length = lengths.delay2 as f32 * settings.length_scale;
        self.delay2.read(length) * settings.decay
    }

    /// Run one sample from this half's first allpass through to its second
    /// delay line.
    fn process(
        &mut self,
        input: f32,
        lengths: HalfLengths,
        settings: TankSettings,
        modulation: (f32, f32),
    ) {
        #[allow(clippy::cast_possible_truncation)]
        let scale = |paper: f64| paper as f32 * settings.length_scale;

        let apf1_out = self.apf1.process(
            input,
            scale(lengths.apf1) + settings.excursion * modulation.0,
            -settings.decay_diffusion_1,
        );

        let delay1_out = self.delay1.read(scale(lengths.delay1));
        self.delay1.write(apf1_out);

        // Damping sits after Delay 1 and before the decay multiply. Freeze
        // crossfades it out rather than switching it, so engaging Freeze on a
        // dark tail does not snap the tail bright.
        let damped = self.low_cut.process(
            self.high_cut
                .process(delay1_out, settings.high_cut_hz, settings.sample_rate),
            settings.low_cut_hz,
            settings.sample_rate,
        );
        let blended = delay1_out + settings.damp_fade * (damped - delay1_out);

        let apf2_out = self.apf2.process(
            blended * settings.decay,
            scale(lengths.apf2) + settings.excursion * modulation.1,
            settings.decay_diffusion_2,
        );

        self.delay2.write(apf2_out);
    }

    /// Read one output tap from this half.
    fn tap(&self, tap: Tap, length_scale: f32) -> f32 {
        #[allow(clippy::cast_possible_truncation)]
        let position = tap.index as f32 * length_scale;
        let value = match tap.line {
            TapLine::Delay1 => self.delay1.read(position),
            TapLine::Apf2 => self.apf2.tap(position),
            TapLine::Delay2 => self.delay2.read(position),
        };
        tap.sign * value
    }
}

/// The complete tank: both halves plus the cross-coupling.
#[derive(Clone, Debug, Default)]
pub struct Tank {
    a: TankHalf,
    b: TankHalf,
}

impl Tank {
    /// Allocate both halves for `max_scale` (largest `sr_scale * size`) plus
    /// `max_excursion` samples of modulation headroom. Not real-time safe.
    pub fn resize(&mut self, max_scale: f64, max_excursion: f64) {
        self.a.resize(HALF_A, max_scale, max_excursion);
        self.b.resize(HALF_B, max_scale, max_excursion);
    }

    /// Zero both halves without touching their allocations.
    pub fn clear(&mut self) {
        self.a.clear();
        self.b.clear();
    }

    /// Run one sample of diffused input through the tank and read the paper's
    /// seven-tap output network.
    ///
    /// `modulation` is the four LFO outputs in `[-1, 1]`, ordered
    /// `[A.apf1, A.apf2, B.apf1, B.apf2]`.
    pub fn process(
        &mut self,
        split: f32,
        settings: TankSettings,
        modulation: [f32; 4],
    ) -> (f32, f32) {
        let split = finite_or(split, 0.0);

        // Both crossings are read from the past before either half writes, so
        // the figure-of-eight adds no delay beyond the paper's own lines.
        let from_a = self.a.feedback(HALF_A, settings);
        let from_b = self.b.feedback(HALF_B, settings);

        self.a.process(
            split + from_b,
            HALF_A,
            settings,
            (modulation[0], modulation[1]),
        );
        self.b.process(
            split + from_a,
            HALF_B,
            settings,
            (modulation[2], modulation[3]),
        );

        (
            OUTPUT_TAP_GAIN * self.sum_taps(&LEFT_TAPS, settings.length_scale),
            OUTPUT_TAP_GAIN * self.sum_taps(&RIGHT_TAPS, settings.length_scale),
        )
    }

    fn sum_taps(&self, taps: &[Tap; 7], length_scale: f32) -> f32 {
        taps.iter()
            .map(|tap| {
                let half = if tap.half_b { &self.b } else { &self.a };
                half.tap(*tap, length_scale)
            })
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::{MOD_EXCURSION, SIZE_MAX, SIZE_MIN, sr_scale};

    /// The capacity a line gets under the reverb's own sizing rules.
    fn sized_tank(sample_rate: f64) -> Tank {
        let mut tank = Tank::default();
        tank.resize(
            sr_scale(sample_rate) * f64::from(SIZE_MAX),
            MOD_EXCURSION * sr_scale(sample_rate),
        );
        tank
    }

    fn settings(length_scale: f32, sample_rate: f64) -> TankSettings {
        TankSettings {
            decay: 0.5,
            decay_diffusion_1: 0.7,
            decay_diffusion_2: 0.5,
            high_cut_hz: 10_000.0,
            low_cut_hz: 20.0,
            damp_fade: 1.0,
            length_scale,
            excursion: 0.0,
            sample_rate,
        }
    }

    #[test]
    fn every_tap_stays_inside_its_line_at_both_size_extremes() {
        // The clamp in `InterpDelay::read` would silently turn an oversized
        // tap into a comb filter, so assert the taps never reach it. This is
        // exactly what Plateau gets wrong.
        for sample_rate in [44_100.0, 48_000.0, 96_000.0, 192_000.0] {
            let tank = sized_tank(sample_rate);
            for size in [SIZE_MIN, 1.0, SIZE_MAX] {
                #[allow(clippy::cast_possible_truncation)]
                let length_scale = (sr_scale(sample_rate) * f64::from(size)) as f32;
                for tap in LEFT_TAPS.iter().chain(RIGHT_TAPS.iter()) {
                    let half = if tap.half_b { &tank.b } else { &tank.a };
                    #[allow(clippy::cast_possible_truncation)]
                    let position = tap.index as f32 * length_scale;
                    let limit = match tap.line {
                        TapLine::Delay1 => half.delay1.max_delay(),
                        TapLine::Apf2 => half.apf2.max_delay(),
                        TapLine::Delay2 => half.delay2.max_delay(),
                    };
                    assert!(
                        position <= limit,
                        "{sample_rate} Hz size {size}: {tap:?} reads {position} of {limit}"
                    );
                }
            }
        }
    }

    #[test]
    fn every_modulated_allpass_stays_inside_its_line_at_full_excursion() {
        for sample_rate in [44_100.0, 48_000.0, 96_000.0, 192_000.0] {
            let tank = sized_tank(sample_rate);
            #[allow(clippy::cast_possible_truncation)]
            let length_scale = (sr_scale(sample_rate) * f64::from(SIZE_MAX)) as f32;
            #[allow(clippy::cast_possible_truncation)]
            let excursion = (MOD_EXCURSION * sr_scale(sample_rate)) as f32;
            for (lengths, section) in [(HALF_A, &tank.a), (HALF_B, &tank.b)] {
                #[allow(clippy::cast_possible_truncation)]
                let apf1 = lengths.apf1 as f32 * length_scale + excursion;
                #[allow(clippy::cast_possible_truncation)]
                let apf2 = lengths.apf2 as f32 * length_scale + excursion;
                assert!(apf1 <= section.apf1.max_delay(), "{sample_rate} Hz APF1");
                assert!(apf2 <= section.apf2.max_delay(), "{sample_rate} Hz APF2");
            }
        }
    }

    #[test]
    fn an_impulse_produces_a_decaying_stereo_pair() {
        let mut tank = sized_tank(48_000.0);
        #[allow(clippy::cast_possible_truncation)]
        let scale = sr_scale(48_000.0) as f32;
        let config = settings(scale, 48_000.0);

        let mut early = 0.0_f32;
        let mut late = 0.0_f32;
        for n in 0..48_000 {
            let (left, right) = tank.process(if n == 0 { 1.0 } else { 0.0 }, config, [0.0; 4]);
            assert!(left.is_finite() && right.is_finite(), "sample {n}");
            let level = left.abs().max(right.abs());
            if n < 12_000 {
                early = early.max(level);
            } else {
                late = late.max(level);
            }
        }
        assert!(early > 0.0, "tank produced silence");
        assert!(late < early, "tank did not decay: {early} -> {late}");
    }

    #[test]
    fn the_two_outputs_differ_so_the_tank_is_actually_stereo() {
        let mut tank = sized_tank(48_000.0);
        #[allow(clippy::cast_possible_truncation)]
        let config = settings(sr_scale(48_000.0) as f32, 48_000.0);

        let mut difference = 0.0_f32;
        for n in 0..24_000 {
            let (left, right) = tank.process(if n == 0 { 1.0 } else { 0.0 }, config, [0.0; 4]);
            difference = difference.max((left - right).abs());
        }
        assert!(
            difference > 1.0e-3,
            "outputs are near-identical: {difference}"
        );
    }

    #[test]
    fn clear_makes_a_used_tank_match_a_fresh_one() {
        let mut used = sized_tank(48_000.0);
        let mut fresh = sized_tank(48_000.0);
        #[allow(clippy::cast_possible_truncation)]
        let config = settings(sr_scale(48_000.0) as f32, 48_000.0);

        for n in 0..5_000 {
            let _ = used.process(if n == 0 { 1.0 } else { 0.0 }, config, [0.2; 4]);
        }
        used.clear();

        for n in 0..2_000 {
            let input = if n == 0 { 1.0 } else { 0.0 };
            assert_eq!(
                used.process(input, config, [0.0; 4]),
                fresh.process(input, config, [0.0; 4]),
                "sample {n}"
            );
        }
    }
}
