//! Published constants from Dattorro 1997, plus the derived scaling helpers.
//!
//! Every length is in samples at the paper's reference rate [`FS_REF`]. The
//! runtime length of a line is `paper_len * sr_scale * size`, where
//! `sr_scale = sample_rate / FS_REF`. Input diffusers and the pre-delay scale
//! with sample rate only; the tank and its output taps scale with both.

/// Sample rate the paper's delay-line lengths are quoted at.
pub const FS_REF: f64 = 29_761.0;

/// Smallest Size the tank supports. Buffers are never sized for this; it only
/// bounds the runtime multiplier.
pub const SIZE_MIN: f32 = 0.05;

/// Largest Size the tank supports. Every tank buffer is allocated for this so
/// a Size sweep never reallocates on the audio thread.
pub const SIZE_MAX: f32 = 4.0;

/// Longest pre-delay the wrapper exposes, in milliseconds.
pub const PRE_DELAY_MAX_MS: f64 = 500.0;

/// Peak one-sided allpass excursion at Mod Depth 1.0, in reference samples.
/// Scales with sample rate but not with Size.
pub const MOD_EXCURSION: f64 = 16.0;

/// Input diffuser lengths. These scale with sample rate but **not** with Size:
/// they shape the initial echo density, which should not stretch when the
/// plate grows.
pub const INPUT_DIFFUSER_LENGTHS: [f64; 4] = [142.0, 107.0, 379.0, 277.0];

/// Input diffuser allpass gains, paired with [`INPUT_DIFFUSER_LENGTHS`].
pub const INPUT_DIFFUSER_GAINS: [f32; 4] = [0.75, 0.75, 0.625, 0.625];

/// The four delay-line lengths of one tank half, in paper samples.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HalfLengths {
    /// Modulated allpass, takes the decay-diffusion-1 coefficient.
    pub apf1: f64,
    /// First delay, followed by the damping filters.
    pub delay1: f64,
    /// Second allpass, takes the decay-diffusion-2 coefficient.
    pub apf2: f64,
    /// Second delay, followed by the cross into the other half.
    pub delay2: f64,
}

impl HalfLengths {
    /// Total loop length of this half, used for the reverb-time estimate.
    #[must_use]
    pub fn loop_length(&self) -> f64 {
        self.apf1 + self.delay1 + self.apf2 + self.delay2
    }
}

/// Tank half A, named after its first allpass length.
pub const HALF_A: HalfLengths = HalfLengths {
    apf1: 672.0,
    delay1: 4_453.0,
    apf2: 1_800.0,
    delay2: 3_720.0,
};

/// Tank half B, named after its first allpass length.
pub const HALF_B: HalfLengths = HalfLengths {
    apf1: 908.0,
    delay1: 4_217.0,
    apf2: 2_656.0,
    delay2: 3_163.0,
};

/// Which of a tank half's three tappable lines an output tap reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TapLine {
    /// The half's first delay line.
    Delay1,
    /// The internal delay line of the half's second allpass.
    Apf2,
    /// The half's second delay line.
    Delay2,
}

/// One entry of the paper's seven-tap output network.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tap {
    /// Tank half to read from: `false` is A, `true` is B.
    pub half_b: bool,
    /// Line within that half.
    pub line: TapLine,
    /// Tap position in paper samples.
    pub index: f64,
    /// Polarity the paper's figure annotates this tap with.
    pub sign: f32,
}

/// Gain applied to the summed seven-tap network on each output.
pub const OUTPUT_TAP_GAIN: f32 = 0.6;

/// Left output taps. Four of the seven come from half B; that crossing is what
/// produces the stereo image.
pub const LEFT_TAPS: [Tap; 7] = [
    tap(true, TapLine::Delay1, 266.0, 1.0),
    tap(true, TapLine::Delay1, 2_974.0, 1.0),
    tap(true, TapLine::Apf2, 1_913.0, -1.0),
    tap(true, TapLine::Delay2, 1_996.0, 1.0),
    tap(false, TapLine::Delay1, 1_990.0, -1.0),
    tap(false, TapLine::Apf2, 187.0, -1.0),
    tap(false, TapLine::Delay2, 1_066.0, -1.0),
];

/// Right output taps, the mirror of [`LEFT_TAPS`] with half A dominant.
pub const RIGHT_TAPS: [Tap; 7] = [
    tap(false, TapLine::Delay1, 353.0, 1.0),
    tap(false, TapLine::Delay1, 3_627.0, 1.0),
    tap(false, TapLine::Apf2, 1_228.0, -1.0),
    tap(false, TapLine::Delay2, 2_673.0, 1.0),
    tap(true, TapLine::Delay1, 2_111.0, -1.0),
    tap(true, TapLine::Apf2, 335.0, -1.0),
    tap(true, TapLine::Delay2, 121.0, -1.0),
];

const fn tap(half_b: bool, line: TapLine, index: f64, sign: f32) -> Tap {
    Tap {
        half_b,
        line,
        index,
        sign,
    }
}

/// Relative rates of the four tank modulation LFOs. Mutually non-harmonic so
/// the four allpasses never lock into a common period and beat.
pub const MOD_LFO_RATIOS: [f32; 4] = [1.0, 1.37, 1.62, 1.93];

/// Starting phases of the four tank modulation LFOs, in turns.
pub const MOD_LFO_PHASES: [f32; 4] = [0.0, 0.25, 0.5, 0.75];

/// Sample-rate scaling factor for a paper length.
#[must_use]
pub fn sr_scale(sample_rate: f64) -> f64 {
    sample_rate / FS_REF
}

/// Longest line the tank ever needs, in paper samples, used for buffer sizing.
#[must_use]
pub fn max_half_line(half: HalfLengths) -> f64 {
    half.apf1.max(half.delay1).max(half.apf2).max(half.delay2)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The line each tap reads under this half mapping, in paper samples.
    fn line_length(tap: Tap) -> f64 {
        let half = if tap.half_b { HALF_B } else { HALF_A };
        match tap.line {
            TapLine::Delay1 => half.delay1,
            TapLine::Apf2 => half.apf2,
            TapLine::Delay2 => half.delay2,
        }
    }

    #[test]
    fn every_output_tap_lands_inside_its_own_line() {
        // This self-consistency is what confirms the node -> line mapping: a
        // tap reading past the end of its line would mean the mapping is
        // wrong. Plateau reads 1913 from a line that is nominally 1800 long.
        for tap in LEFT_TAPS.iter().chain(RIGHT_TAPS.iter()) {
            assert!(
                tap.index < line_length(*tap),
                "{tap:?} reads past its {} sample line",
                line_length(*tap)
            );
        }
    }

    #[test]
    fn left_leans_on_half_b_and_right_on_half_a() {
        assert_eq!(LEFT_TAPS.iter().filter(|t| t.half_b).count(), 4);
        assert_eq!(RIGHT_TAPS.iter().filter(|t| !t.half_b).count(), 4);
    }

    #[test]
    fn reference_rate_scaling_is_unity_at_the_paper_rate() {
        assert!((sr_scale(FS_REF) - 1.0).abs() < 1.0e-12);
        assert!(sr_scale(96_000.0) > sr_scale(48_000.0));
    }
}
