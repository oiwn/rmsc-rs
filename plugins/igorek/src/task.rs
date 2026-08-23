//! Background IR baking: shape, partition, FFT — everything off the audio
//! thread. The editor posts a task whenever a baked parameter changes
//! (debounced); `process` may also post one after a state recall or a
//! sample-rate change. Results reach the audio thread through the
//! [`StageSwap`] slots and the editor through the view slots.

use std::sync::atomic::Ordering;
use std::sync::{Mutex, PoisonError};

use igorek_dsp::bake::{Envelope3, Selection, bake_stage};
use igorek_dsp::room::{self, RoomParams};
use truce::prelude::*;

use crate::IgorekParams;

/// A request to regenerate and bake the room at `sample_rate`.
#[derive(Clone, Copy, Debug)]
pub struct BakeRoom {
    pub sample_rate: f64,
}

impl BackgroundTask for BakeRoom {
    type Params = IgorekParams;

    /// One bake at a time per instance keeps the newest bake the last one
    /// published; two overlapping renders could otherwise install out of
    /// order.
    const SERIALIZED: bool = true;

    fn run(self, params: &Self::Params) {
        // `value()` everywhere: `read()` would advance the smoothers, which
        // belong to the audio thread.
        let room = RoomParams {
            rt60_s: params.room_rt60.value(),
            edt_ms: params.room_edt.value(),
            itdg_ms: params.room_itdg.value(),
            er_duration_ms: params.room_er_duration.value(),
        };
        let variant: u32 = params.room_variant.value().clamp(0, u32::MAX as i64) as u32;
        let width = params.room_width.value();
        let (left, right) = room::generate_stereo(room, variant, width, self.sample_rate);

        let selection = Selection {
            start: params.room_sel_start.value(),
            length: params.room_sel_length.value(),
        };
        let envelope = Envelope3 {
            a_db: params.room_env_a.value(),
            b_x: params.room_env_b_x.value(),
            b_db: params.room_env_b.value(),
            c_db: params.room_env_c.value(),
        };
        let baked = bake_stage(&left, &right, selection, envelope, self.sample_rate);

        publish_view(&params.room_view, &params.room_view_generation, baked.view);
        params.room_swap.publish(Box::new(baked.install));
    }
}

/// A request to re-bake the Color IR at `sample_rate` from the currently
/// loaded WAV (or the identity impulse when nothing is loaded).
#[derive(Clone, Copy, Debug)]
pub struct BakeColor {
    pub sample_rate: f64,
}

impl BackgroundTask for BakeColor {
    type Params = IgorekParams;

    const SERIALIZED: bool = true;

    fn run(self, params: &Self::Params) {
        // The stored IR may have been saved or loaded at a different rate;
        // re-resample from it when the host rate moved.
        let loaded = params
            .color_ir
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|ir| ir.resampled(self.sample_rate));
        let (left, right) = match &loaded {
            Some(ir) => (ir.left.as_slice(), ir.right.as_slice()),
            None => ([1.0_f32].as_slice(), [1.0_f32].as_slice()),
        };

        let selection = Selection {
            start: params.color_sel_start.value(),
            length: params.color_sel_length.value(),
        };
        let envelope = Envelope3 {
            a_db: params.color_env_a.value(),
            b_x: params.color_env_b_x.value(),
            b_db: params.color_env_b.value(),
            c_db: params.color_env_c.value(),
        };
        let baked = bake_stage(left, right, selection, envelope, self.sample_rate);

        publish_view(
            &params.color_view,
            &params.color_view_generation,
            baked.view,
        );
        params.color_swap.publish(Box::new(baked.install));
    }
}

/// Hand a finished view to the editor and bump its generation.
fn publish_view(
    slot: &Mutex<Option<Box<igorek_dsp::bake::IrView>>>,
    generation: &std::sync::atomic::AtomicU64,
    view: igorek_dsp::bake::IrView,
) {
    let mut slot = slot.lock().unwrap_or_else(PoisonError::into_inner);
    *slot = Some(Box::new(view));
    drop(slot);
    generation.fetch_add(1, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IgorekParams;
    use igorek_dsp::wav::LoadedIr;

    #[test]
    fn a_room_bake_publishes_view_and_install() {
        let params = IgorekParams::default();
        assert_eq!(params.room_view_generation.load(Ordering::Acquire), 0);

        BakeRoom {
            sample_rate: 48_000.0,
        }
        .run(&params);

        assert!(params.room_view.lock().unwrap().is_some());
        assert!(params.room_swap.generation() >= 1);
    }

    #[test]
    fn a_color_bake_without_a_file_uses_the_identity_ir() {
        let params = IgorekParams::default();
        BakeColor {
            sample_rate: 48_000.0,
        }
        .run(&params);

        let view = params.color_view.lock().unwrap().take().unwrap();
        assert_eq!(view.length_samples, 1);
    }

    #[test]
    fn a_color_bake_uses_the_loaded_ir_and_reshapes_it() {
        let params = IgorekParams::default();
        let ir = LoadedIr {
            left: vec![0.5; 4_800],
            right: vec![0.5; 4_800],
            sample_rate: 48_000.0,
            truncated: false,
        };
        *params.color_ir.lock().unwrap() = Some(ir);
        // A short selection trims the shaped IR.
        params.color_sel_length.set_value(0.5);

        BakeColor {
            sample_rate: 48_000.0,
        }
        .run(&params);

        let view = params.color_view.lock().unwrap().take().unwrap();
        assert!(view.length_samples <= 4_800);
        assert!(view.length_samples > 0);
        // Outside the selection everything is zeroed: the back half of the
        // envelope floor sits at silence.
        let floor = igorek_dsp::bake::ENVELOPE_FLOOR_DB;
        assert!(view.left_db.iter().any(|db| *db > floor));
    }

    #[test]
    fn baking_does_not_advance_the_audio_threads_smoothers() {
        let params = IgorekParams::default();
        params.room_rt60.set_value(2.0);
        let before = params.room_rt60.current();

        BakeRoom {
            sample_rate: 48_000.0,
        }
        .run(&params);

        assert_eq!(params.room_rt60.current(), before);
    }
}
