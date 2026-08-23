//! Stochastic room IR generation, ported from stoRIR-rs.
//!
//! Ported from the author's own GPL-3 repository
//! <https://github.com/oiwn/stoRIR-rs> (`src/simple.rs`), embedded here as
//! MIT with the sole author's explicit permission — see the licensing note in
//! `specs/igorek.md`. Do not accept third-party patches into this module
//! without re-clearing that provenance.
//!
//! Deliberate deviations from the original, all recorded in the spec:
//!
//! - One seeded `StdRng` per channel replaces `thread_rng`, making rooms
//!   deterministic and letting L/R decorrelate by seed instead of by chance.
//! - `rt60 <= edt` no longer panics; EDT is clamped to `0.9 × RT60`.
//! - The IR runs `1.3 × RT60` long (capped at 8 s) so the decay is fully
//!   visible; the original stopped at exactly RT60.
//! - The output is peak-normalized, where the original relied on the direct
//!   sound landing at 0 dBFS by construction.
//!
//! Everything else — the uniform noise floor, the EDT/RT60 slope shaping, the
//! DRR thinning loop, the ITDG gap — follows the original algorithm.

use rand::Rng;
use rand::SeedableRng;
use rand::rngs::StdRng;
use rand::seq::IndexedRandom;

use crate::ROOM_IR_MAX_SECONDS;

/// Direct-to-reverberant ratio the generator aims for, in the original's
/// sum-of-amplitudes metric. Igorek exposes no DRR parameter; this is the
/// stoRIR-rs test value, held fixed.
const DRR_TARGET: f32 = -1.0;

/// Headroom multiplier on RT60: the IR runs this much longer than the target
/// reverb time so the tail can reach its floor.
const LENGTH_HEADROOM: f32 = 1.3;

/// How far a channel's room parameters may drift from the left channel's, at
/// full width, as a fraction.
const PARAM_JITTER: f32 = 0.02;

/// The baked room-shape parameters. Variant and Width travel separately —
/// they select the seed and the decorrelation, not the room itself.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RoomParams {
    /// Target reverberation time in seconds.
    pub rt60_s: f32,
    /// Early decay time in milliseconds.
    pub edt_ms: f32,
    /// Initial time delay gap in milliseconds: the dead time between the
    /// direct sound and the first reflection.
    pub itdg_ms: f32,
    /// Span of the early-reflection cluster in milliseconds.
    pub er_duration_ms: f32,
}

impl Default for RoomParams {
    /// The plugin's default room: a short RT60 0.8 s / EDT 50 ms / ITDG 4 ms
    /// / ER 100 ms — the spec's parameter defaults.
    fn default() -> Self {
        Self {
            rt60_s: 0.8,
            edt_ms: 50.0,
            itdg_ms: 4.0,
            er_duration_ms: 100.0,
        }
    }
}

impl RoomParams {
    /// Clamp every field to the ranges the plugin parameters allow and
    /// enforce `edt < rt60`, replacing non-finite values with the defaults.
    #[must_use]
    pub fn sanitized(self) -> Self {
        let rt60_s = if self.rt60_s.is_finite() {
            self.rt60_s
        } else {
            0.8
        }
        .clamp(0.05, 8.0);
        let edt_ms = if self.edt_ms.is_finite() {
            self.edt_ms
        } else {
            50.0
        }
        .clamp(5.0, 1000.0);
        let itdg_ms = if self.itdg_ms.is_finite() {
            self.itdg_ms
        } else {
            4.0
        }
        .clamp(0.0, 50.0);
        let er_duration_ms = if self.er_duration_ms.is_finite() {
            self.er_duration_ms
        } else {
            100.0
        }
        .clamp(5.0, 500.0);
        // The original panicked when EDT reached RT60; the plugin's ranges
        // allow it, so clamp instead.
        Self {
            rt60_s,
            edt_ms: edt_ms.min(0.9 * rt60_s * 1000.0),
            itdg_ms,
            er_duration_ms,
        }
    }
}

/// Seed for a room variant: spread the small integer the host parameter
/// carries across the full u64 space so adjacent variants differ thoroughly.
#[must_use]
pub fn variant_seed(variant: u32) -> u64 {
    u64::from(variant).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0x5EED_C0DE_0000_0001
}

/// Generate one channel's room IR.
///
/// Deterministic: the same `(params, seed, sample_rate)` always produces the
/// same IR. Off the audio thread only (allocates heavily).
#[must_use]
pub fn generate(params: RoomParams, seed: u64, sample_rate: f64) -> Vec<f32> {
    let params = params.sanitized();
    let rate = if sample_rate.is_finite() && sample_rate > 0.0 {
        sample_rate
    } else {
        44_100.0
    };
    let mut rng = StdRng::seed_from_u64(seed);

    let edt_num = ms_to_samples(params.edt_ms, rate);
    let rt60_num = ms_to_samples(params.rt60_s * 1000.0, rate).max(edt_num + 1);
    let er_num = ms_to_samples(params.er_duration_ms, rate);
    let itdg_num = ms_to_samples(params.itdg_ms, rate);

    #[allow(clippy::cast_precision_loss)]
    let total = ((LENGTH_HEADROOM as f64) * (rt60_num as f64)).min(ROOM_IR_MAX_SECONDS * rate);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let total = (total.round() as usize).max(rt60_num).max(edt_num + 1);

    // Uniform white noise, as in the original.
    let mut data: Vec<f32> = (0..total)
        .map(|_| rng.random_range(-5.0_f32..5.0))
        .collect();

    // Shape the EDT slope, then scale so the ramp spans 10 dB.
    for (i, sample) in data.iter_mut().enumerate().take(edt_num.saturating_sub(1)) {
        *sample -= i as f32;
    }
    let edt_hold = (edt_num.saturating_sub(1)) as f32;
    for sample in data.iter_mut().skip(edt_num.saturating_sub(1)) {
        *sample -= edt_hold;
    }
    let scale = 10.0 / edt_num as f32;
    for sample in &mut data {
        *sample *= scale;
    }

    // Shape the RT60 slope after EDT. The original stopped this ramp at
    // exactly RT60 (its noise buffer ended there); the extended buffer here
    // keeps ramping so the 1.3x headroom decays instead of sitting on a
    // noise floor.
    for (i, sample) in data.iter_mut().enumerate().skip(edt_num) {
        *sample -= (i as f32 - edt_num as f32 - 1.0) * 50.0 / rt60_num as f32;
    }

    // Scale to dBFS (max = 0 dB), map to gain, square — the square makes the
    // decay steeper than the linear-in-dB ramp alone.
    let max_val = data.iter().fold(f32::NEG_INFINITY, |a, &v| a.max(v));
    for sample in &mut data {
        let db = *sample - max_val;
        let gain = 10.0_f32.powf(db / 20.0);
        *sample = gain * gain;
    }

    // The direct sound is wherever the shaped envelope peaked.
    let direct_sound_idx = data
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(i, _)| i)
        .unwrap_or(0);

    let er_start_idx = (direct_sound_idx + 1).min(data.len() - 1);
    let er_end_idx = (er_start_idx + er_num).min(data.len() - 1);

    create_initial_time_delay_gap(&mut data, direct_sound_idx, itdg_num);
    randomize_reflections(
        &mut rng,
        &mut data,
        direct_sound_idx,
        er_start_idx,
        er_end_idx,
    );

    // Everything before the direct sound is not part of the room.
    let mut ir = data[direct_sound_idx..].to_vec();
    peak_normalize(&mut ir);
    ir
}

/// Generate a decorrelated stereo pair.
///
/// The left IR comes from `(seed, params)`; the right from
/// `(seed + round(width · 997), params jittered by width · small%)`. At Width
/// 0 the offset and jitter vanish and both channels are identical; at Width 1
/// they are fully decorrelated. Nothing is delayed or Haas-tricked between
/// channels — the variation is in the generated material itself.
#[must_use]
pub fn generate_stereo(
    params: RoomParams,
    variant: u32,
    width: f32,
    sample_rate: f64,
) -> (Vec<f32>, Vec<f32>) {
    let width = if width.is_finite() {
        width.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let seed = variant_seed(variant);

    let left = generate(params, seed, sample_rate);
    if width == 0.0 {
        return (left.clone(), left);
    }

    let offset = (width * 997.0).round() as u64;
    let right_seed = seed.wrapping_add(offset);
    let mut jitter_rng = StdRng::seed_from_u64(right_seed ^ 0x51DE_D0E5_0000_0001);
    let jitter = |rng: &mut StdRng| 1.0 + width * rng.random_range(-PARAM_JITTER..PARAM_JITTER);
    let right_params = RoomParams {
        rt60_s: params.rt60_s * jitter(&mut jitter_rng),
        edt_ms: params.edt_ms * jitter(&mut jitter_rng),
        itdg_ms: params.itdg_ms * jitter(&mut jitter_rng),
        er_duration_ms: params.er_duration_ms * jitter(&mut jitter_rng),
    };
    let right = generate(right_params, right_seed, sample_rate);
    (left, right)
}

fn ms_to_samples(ms: f32, rate: f64) -> usize {
    ((f64::from(ms) / 1000.0) * rate).round() as usize
}

fn create_initial_time_delay_gap(data: &mut [f32], direct_sound_idx: usize, itdg_num: usize) {
    let end = (direct_sound_idx + 1 + itdg_num).min(data.len());
    for sample in &mut data[direct_sound_idx + 1..end] {
        *sample = 0.0;
    }
}

/// Thin out reflections until the direct-to-reverberant ratio reaches the
/// target, as in the original: the early cluster at 1/8 rate, the tail at
/// 1/10.
fn randomize_reflections(
    rng: &mut StdRng,
    data: &mut [f32],
    direct_sound_idx: usize,
    early_ref_start: usize,
    early_ref_end: usize,
) {
    let drr_low = DRR_TARGET - 0.5;
    let drr_high = DRR_TARGET + 0.5;

    let mut current_drr = drr_ratio(data, direct_sound_idx);
    if current_drr > drr_high {
        return;
    }
    while drr_low > current_drr && current_drr.is_finite() {
        thin_out_reflections(rng, data, early_ref_start, early_ref_end, 1.0 / 8.0);
        thin_out_reflections(rng, data, early_ref_end, data.len() - 1, 1.0 / 10.0);
        let previous_drr = current_drr;
        current_drr = drr_ratio(data, direct_sound_idx);
        if (previous_drr - current_drr).abs() < f32::EPSILON {
            break;
        }
    }
}

/// The original's DRR metric: 10·log10 of the ratio of summed amplitudes
/// before and after the direct sound.
fn drr_ratio(data: &[f32], direct_sound_idx: usize) -> f32 {
    let direct: f32 = data[..=direct_sound_idx].iter().sum();
    let reverberant: f32 = data[direct_sound_idx + 1..].iter().sum();
    10.0 * (direct / reverberant).log10()
}

fn thin_out_reflections(rng: &mut StdRng, data: &mut [f32], start: usize, end: usize, rate: f32) {
    let end = end.min(data.len() - 1);
    if start > end {
        return;
    }
    let candidates: Vec<usize> = (start..=end).filter(|&idx| data[idx] != 0.0).collect();
    let num_rays = ((candidates.len() as f32) * rate).round() as usize;
    if num_rays >= 1 {
        for index in candidates.choose_multiple(rng, num_rays) {
            data[*index] = 0.0;
        }
    }
}

fn peak_normalize(data: &mut [f32]) {
    let peak = data.iter().fold(0.0_f32, |a, &v| a.max(v.abs()));
    if peak > 0.0 && peak.is_finite() {
        for sample in data.iter_mut() {
            *sample /= peak;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f64 = 48_000.0;

    fn default_params() -> RoomParams {
        RoomParams {
            rt60_s: 0.8,
            edt_ms: 50.0,
            itdg_ms: 4.0,
            er_duration_ms: 100.0,
        }
    }

    /// Schroeder backward-integral RT60, for checking the generator against
    /// its target.
    fn measured_rt60(ir: &[f32], rate: f64) -> Option<f64> {
        let energy: Vec<f64> = ir.iter().map(|&s| f64::from(s) * f64::from(s)).collect();
        let total: f64 = energy.iter().sum();
        if total <= 0.0 {
            return None;
        }
        let mut decay = vec![0.0_f64; energy.len()];
        let mut acc = 0.0;
        for i in (0..energy.len()).rev() {
            acc += energy[i];
            decay[i] = 10.0 * (acc / total).log10();
        }
        let direct = decay
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(i, _)| i)
            .unwrap_or(0);
        let at = |db: f64| -> Option<usize> {
            decay[direct..]
                .iter()
                .position(|&d| d <= db)
                .map(|p| direct + p)
        };
        let t5 = at(-5.0)?;
        let t35 = at(-35.0)?;
        if t35 <= t5 {
            return None;
        }
        Some(2.0 * (t35 - t5) as f64 / rate)
    }

    #[test]
    fn same_seed_and_params_reproduce_the_same_room() {
        let params = default_params();
        let a = generate(params, variant_seed(7), RATE);
        let b = generate(params, variant_seed(7), RATE);
        assert_eq!(a, b);
        assert!(!a.is_empty());
    }

    #[test]
    fn different_variants_generate_different_rooms() {
        let params = default_params();
        let a = generate(params, variant_seed(0), RATE);
        let b = generate(params, variant_seed(1), RATE);
        assert_ne!(a, b);
    }

    #[test]
    fn zero_width_produces_identical_channels() {
        let (left, right) = generate_stereo(default_params(), 3, 0.0, RATE);
        assert_eq!(left, right);
    }

    #[test]
    fn full_width_decorrelates_the_channels() {
        let (left, right) = generate_stereo(default_params(), 3, 1.0, RATE);
        assert_ne!(left, right);
        // Normalized cross-correlation of the reverberant material (the
        // direct-sound spike at sample 0 is excluded — both channels share
        // it by construction, and it would dominate the metric).
        let n = left.len().min(right.len());
        let dot: f64 = left[1..n]
            .iter()
            .zip(&right[1..n])
            .map(|(&a, &b)| f64::from(a) * f64::from(b))
            .sum();
        let norm = |x: &[f32]| {
            x.iter()
                .map(|&v| f64::from(v) * f64::from(v))
                .sum::<f64>()
                .sqrt()
        };
        let ncc = dot / (norm(&left[1..n]) * norm(&right[1..n]));
        assert!(ncc.abs() < 0.3, "normalized cross-correlation {ncc:.3}");
    }

    #[test]
    fn direct_sound_starts_the_ir_and_the_itdg_gap_follows_it() {
        let itdg_ms = 20.0;
        let params = RoomParams {
            rt60_s: 0.8,
            edt_ms: 50.0,
            itdg_ms,
            er_duration_ms: 100.0,
        };
        let ir = generate(params, variant_seed(5), RATE);
        let gap = ms_to_samples(itdg_ms, RATE);
        // Sample 0 is the direct sound (peak ~1 after normalization).
        assert!((ir[0] - 1.0).abs() < 1e-3, "direct sound {}", ir[0]);
        // Then the gap: near-silence until the first reflection.
        let window = &ir[1..gap.saturating_sub(2)];
        assert!(
            window.iter().all(|&s| s.abs() < 1e-6),
            "ITDG gap not silent"
        );
        // And material does arrive after the gap.
        assert!(ir[gap + 1..].iter().any(|&s| s.abs() > 1e-4));
    }

    #[test]
    fn length_follows_rt60_with_headroom_and_respects_the_cap() {
        let short = generate(
            RoomParams {
                rt60_s: 0.2,
                edt_ms: 50.0,
                itdg_ms: 4.0,
                er_duration_ms: 100.0,
            },
            variant_seed(0),
            RATE,
        );
        // The IR starts at the direct sound, so it sits slightly under the
        // shaped buffer length.
        let expected = (0.2 * 1.3 * RATE) as usize;
        assert!(
            (short.len() as i64 - expected as i64).abs() < 500,
            "len {} vs expected {expected}",
            short.len()
        );

        let long = generate(
            RoomParams {
                rt60_s: 8.0,
                edt_ms: 200.0,
                itdg_ms: 4.0,
                er_duration_ms: 100.0,
            },
            variant_seed(0),
            RATE,
        );
        let cap = (ROOM_IR_MAX_SECONDS * RATE) as usize;
        assert!(
            long.len() <= cap && long.len() > cap - 1_000,
            "len {} vs cap {cap}",
            long.len()
        );
    }

    #[test]
    fn measured_rt60_scales_with_the_target() {
        // The stoRIR shaping measures at roughly 0.6x its nominal target
        // (the squared gain map steepens the decay); that ratio is the
        // port's known characteristic, recorded in the spec. These windows
        // sit around the observed values and guard regressions, not
        // physical accuracy.
        for (target, expected) in [(0.4_f32, 0.234_f64), (0.8, 0.503), (1.6, 0.944)] {
            let params = RoomParams {
                rt60_s: target,
                edt_ms: 50.0,
                itdg_ms: 4.0,
                er_duration_ms: 100.0,
            };
            let ir = generate(params, variant_seed(11), RATE);
            let measured = measured_rt60(&ir, RATE).expect("a measurable decay");
            assert!(
                (measured - expected).abs() / expected < 0.15,
                "target {target} s measured {measured:.3} s (expected ~{expected})"
            );
        }
    }

    #[test]
    fn edt_larger_than_rt60_is_clamped_not_fatal() {
        let params = RoomParams {
            rt60_s: 0.05,
            edt_ms: 1000.0,
            itdg_ms: 4.0,
            er_duration_ms: 100.0,
        };
        let ir = generate(params, variant_seed(2), RATE);
        assert!(!ir.is_empty());
        assert!(ir.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn sanitized_clamps_and_guards_non_finite_values() {
        let params = RoomParams {
            rt60_s: f32::NAN,
            edt_ms: 5000.0,
            itdg_ms: f32::INFINITY,
            er_duration_ms: -1.0,
        }
        .sanitized();
        assert!((params.rt60_s - 0.8).abs() < 1e-6);
        assert_eq!(params.edt_ms, 5000.0_f32.min(0.9 * 0.8 * 1000.0));
        assert_eq!(params.itdg_ms, 4.0);
        assert_eq!(params.er_duration_ms, 5.0);
    }
}
