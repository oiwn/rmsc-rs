//! Framework-independent two-stage convolver for Igorek.
//!
//! Stage one, **Color**, convolves with a *material* impulse response (metal,
//! wood, shell — the user's own WAV files). Stage two, **Room**, convolves with
//! a procedurally generated stochastic room IR. Selection and a 3-point
//! envelope are baked into each IR before the FFT, so shaping costs nothing at
//! runtime. The design lives in `specs/igorek.md`.
//!
//! This is the suite's first block-based processor and first audio-path FFT:
//! uniformly partitioned overlap-save convolution with partition size
//! [`P`] and FFT size [`N`], driven on a strict [`P`]-sample grid with a
//! partial-block carry so hosts may call `process` with any block size.

pub mod bake;
pub mod engine;
pub mod processor;
pub mod room;
pub mod wav;

use musictools_core::finite_or;

/// Partition size of the overlap-save engine, in samples, at every sample
/// rate: 2.7 ms at 48 kHz, 2.9 ms at 44.1 kHz.
pub const P: usize = 128;

/// FFT size per partition: the previous [`P`] samples as history plus the new
/// [`P`] samples.
pub const N: usize = 2 * P;

/// Complex bins a real FFT of [`N`] points produces.
pub const BINS: usize = N / 2 + 1;

/// Input-to-output latency of the whole plugin, in samples: exactly one
/// partition, independent of the host's block size. Reported through
/// `PluginLogic::latency`.
pub const LATENCY_SAMPLES: u32 = P as u32;

/// Longest Color IR the buffers are sized for. Files longer than this load
/// truncated.
pub const COLOR_IR_MAX_SECONDS: f64 = 4.0;

/// Longest Room IR the buffers are sized for. Generated rooms are capped at
/// this length.
pub const ROOM_IR_MAX_SECONDS: f64 = 8.0;

/// Live control values for [`processor::IgorekProcessor::process`].
///
/// Only the four parameters that act per sample belong here. Everything that
/// is baked into an IR (selection, envelope, room shape) travels with the bake
/// requests in [`bake`] instead — a smoothed value would be invisible between
/// bakes, so those take no smoothing at all.
#[derive(Clone, Copy, Debug)]
pub struct IgorekSettings {
    /// Linear gain on the untouched input at the final Dry/Wet crossfade.
    pub dry: f32,
    /// Linear gain on the fully processed signal at the final Dry/Wet
    /// crossfade.
    pub wet: f32,
    /// Stage-one crossfade between the convolved Color signal and its input.
    pub color_mix: f32,
    /// Stage-two crossfade between the convolved Room signal and its input
    /// (the Color stage output).
    pub room_mix: f32,
}

impl Default for IgorekSettings {
    fn default() -> Self {
        Self {
            dry: 0.0,
            wet: 1.0,
            color_mix: 1.0,
            room_mix: 1.0,
        }
    }
}

impl IgorekSettings {
    /// Clamp every field to its useful range and replace non-finite values,
    /// mirroring the guard the suite applies to every public entry point.
    #[must_use]
    pub fn sanitized(mut self) -> Self {
        self.dry = finite_or(self.dry, 0.0).clamp(0.0, 1.0);
        self.wet = finite_or(self.wet, 1.0).clamp(0.0, 1.0);
        self.color_mix = finite_or(self.color_mix, 1.0).clamp(0.0, 1.0);
        self.room_mix = finite_or(self.room_mix, 1.0).clamp(0.0, 1.0);
        self
    }
}

/// Per-block signals the processor exposes for tests and the editor: the last
/// sample of the most recent internal [`P`]-sample block on each tap of the
/// chain.
///
/// `dry_*` is the sanitized input, `color_*` the stage-one output after its
/// Mix crossfade, `room_*` the stage-two output after its Mix crossfade, and
/// `out_*` the final Dry/Wet result.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct IgorekFrame {
    pub dry_left: f32,
    pub dry_right: f32,
    pub color_left: f32,
    pub color_right: f32,
    pub room_left: f32,
    pub room_right: f32,
    pub out_left: f32,
    pub out_right: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_are_the_replace_style_effect() {
        let s = IgorekSettings::default();
        assert_eq!(s.dry, 0.0);
        assert_eq!(s.wet, 1.0);
        assert_eq!(s.color_mix, 1.0);
        assert_eq!(s.room_mix, 1.0);
    }

    #[test]
    fn sanitized_clamps_and_replaces_non_finite_values() {
        let s = IgorekSettings {
            dry: f32::NAN,
            wet: 2.0,
            color_mix: -1.0,
            room_mix: f32::INFINITY,
        }
        .sanitized();
        assert_eq!(s.dry, 0.0);
        assert_eq!(s.wet, 1.0);
        assert_eq!(s.color_mix, 0.0);
        assert_eq!(s.room_mix, 1.0);
    }
}
