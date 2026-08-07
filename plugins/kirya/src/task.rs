//! Background impulse-response rendering for the editor's IR probe.
//!
//! The editor posts one of these whenever a knob changes, the pool runs it off
//! both the audio and GUI threads, and the finished render lands in the
//! `ir_slot` handoff field.

use std::sync::PoisonError;
use std::sync::atomic::Ordering;

use kirya_dsp::analysis::{Analyzer, ir_window_seconds, render_impulse_response};
use truce::prelude::*;

use crate::KiryaParams;

/// A request to re-render the impulse response at `sample_rate`.
///
/// Deliberately tiny and `Copy`: `spawn_coalescing` drops the displaced
/// request on the caller's thread, so the request must be cheap to drop.
#[derive(Clone, Copy, Debug)]
pub struct RenderIr {
    /// Rate to render at, so the display matches what the host is playing.
    pub sample_rate: f64,
}

impl BackgroundTask for RenderIr {
    type Params = KiryaParams;

    /// Two concurrent renders would both be valid but could finish out of
    /// order, leaving the editor showing an older tail than the knobs. One at
    /// a time per instance keeps the newest render the last one published.
    const SERIALIZED: bool = true;

    fn run(self, params: &Self::Params) {
        // `target_settings` reads the raw targets rather than the smoothed
        // values: `read()` advances the smoothers, which belong to the audio
        // thread. Calling it here would steal samples from `process`.
        let settings = params.target_settings();

        // Size the window to the tail rather than rendering a fixed length: a
        // short plate would otherwise be a spike at the far left, and a long
        // one would run off the right edge without ever reaching the floor.
        let window = ir_window_seconds(settings, self.sample_rate);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let samples = (self.sample_rate * window) as usize;
        let mut left = Vec::new();
        let mut right = Vec::new();
        render_impulse_response(settings, self.sample_rate, samples, &mut left, &mut right);

        let render = Analyzer::new().analyse(&left, &right, self.sample_rate);

        // Recover from a poisoned lock rather than leaving the display frozen
        // forever: the slot is a plain cache, so a panicking reader has not
        // corrupted anything a later render cannot simply overwrite.
        let mut slot = params
            .ir_slot
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *slot = Some(Box::new(render));
        drop(slot);

        params.ir_generation.fetch_add(1, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_render_lands_in_the_slot_and_bumps_the_generation() {
        let params = KiryaParams::default();
        assert!(params.ir_slot.lock().unwrap().is_none());
        assert_eq!(params.ir_generation.load(Ordering::Acquire), 0);

        RenderIr {
            sample_rate: 48_000.0,
        }
        .run(&params);

        assert_eq!(params.ir_generation.load(Ordering::Acquire), 1);
        let slot = params.ir_slot.lock().unwrap();
        let render = slot.as_ref().expect("a render was published");
        assert_eq!(render.sample_rate, 48_000.0);
        let window = ir_window_seconds(params.target_settings(), 48_000.0);
        assert_eq!(render.length, (48_000.0 * window) as usize);
        assert!(render.rt60_seconds.is_some());
        assert!(!render.left_envelope_db.is_empty());
        assert!(render.spectrogram.columns > 0);
    }

    #[test]
    fn rendering_does_not_advance_the_audio_threads_smoothers() {
        // The task runs on a pool thread while `process` is running. Reading
        // the smoothed value here instead of the target would consume samples
        // the audio thread is about to need, audibly slowing every glide.
        let params = KiryaParams::default();
        params.size.set_value(3.0);
        let before = params.size.current();

        RenderIr {
            sample_rate: 48_000.0,
        }
        .run(&params);

        assert_eq!(params.size.current(), before);
    }

    #[test]
    fn a_second_render_replaces_the_first() {
        let params = KiryaParams::default();
        let task = RenderIr {
            sample_rate: 48_000.0,
        };

        // Both decays have to fall 35 dB inside the three-second render, or
        // the measurement reports `None` and there is nothing to compare.
        params.decay.set_value(0.2);
        task.run(&params);
        let short = params
            .ir_slot
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .rt60_seconds;

        params.decay.set_value(0.45);
        task.run(&params);
        let long = params
            .ir_slot
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .rt60_seconds;

        assert_eq!(params.ir_generation.load(Ordering::Acquire), 2);
        assert!(short.is_some() && long.is_some(), "{short:?} / {long:?}");
        assert!(long > short, "{long:?} was not longer than {short:?}");
    }

    #[test]
    fn the_window_actually_holds_the_measured_reverb_time() {
        // The point of sizing the window: RT60 stays measurable instead of
        // degrading to "> 3 s" the moment the decay gets long.
        let params = KiryaParams::default();
        for decay in [0.2_f64, 0.4, 0.55, 0.7, 0.8] {
            params.decay.set_value(decay);
            RenderIr {
                sample_rate: 48_000.0,
            }
            .run(&params);

            let slot = params.ir_slot.lock().unwrap();
            let render = slot.as_ref().unwrap();
            assert!(
                render.rt60_seconds.is_some(),
                "decay {decay} produced no measurable RT60 in a {} s window",
                render.duration_seconds()
            );
        }
    }

    #[test]
    fn freeze_is_ignored_so_the_probe_always_shows_a_tail() {
        // A frozen render would never decay and the display would be a flat
        // block, so the probe forces Freeze off.
        let params = KiryaParams::default();
        params.freeze.set_value(true);
        params.decay.set_value(0.3);

        RenderIr {
            sample_rate: 48_000.0,
        }
        .run(&params);

        let slot = params.ir_slot.lock().unwrap();
        assert!(slot.as_ref().unwrap().rt60_seconds.is_some());
    }
}
