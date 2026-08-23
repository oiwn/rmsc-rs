//! The two-stage block processor: Color, Room, stage mixes, Dry/Wet, and
//! the IR install protocol.
//!
//! This is the suite's first block-based processor: the per-sample
//! settings-in/frame-out pattern carries over at block granularity — a
//! `Copy` settings read per block, a frame struct exposing the intermediate
//! signals — while `process` additionally owns the partial-block carry so
//! hosts may call it with any block size. The engine steps on a strict
//! [`P`]-sample grid; the processor's output ring holds every block for one
//! partition, making the input-to-output delay exactly
//! [`LATENCY_SAMPLES`](crate::LATENCY_SAMPLES) regardless of how the host
//! chunks its callbacks.
//!
//! Signal flow per internal block:
//!
//! ```text
//! sanitized in -> [Color conv -> Color Mix] -> [Room conv -> Room Mix]
//!               -> Dry/Wet -> out ring
//! ```

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, TryLockError};

use musictools_core::finite_or;

use crate::bake::{Envelope3, Selection, StageInstall, bake_stage};
use crate::engine::Convolver;
use crate::room::{self, RoomParams};
use crate::wav::LoadedIr;
use crate::{COLOR_IR_MAX_SECONDS, IgorekFrame, P, ROOM_IR_MAX_SECONDS};

/// The handoff for one stage's IR: the background task publishes a finished
/// bake, the processor installs it at the next block boundary.
///
/// Cloned freely — the `Arc`s are shared, so the plugin's parameter struct,
/// the tasks, and the processor all see the same slot. The audio thread
/// takes with `try_lock` only; if the lock is busy the engine keeps the
/// previous spectra for one more block. No allocation, no blocking, no
/// re-FFT on the audio thread, ever.
#[derive(Clone, Default)]
pub struct StageSwap {
    install: Arc<Mutex<Option<Box<StageInstall>>>>,
    generation: Arc<AtomicU64>,
}

impl StageSwap {
    /// Place a finished bake. Off the audio thread only.
    pub fn publish(&self, install: Box<StageInstall>) {
        let mut slot = self.install.lock().unwrap_or_else(PoisonError::into_inner);
        *slot = Some(install);
        drop(slot);
        self.generation.fetch_add(1, Ordering::Release);
    }

    /// Which generation the latest publish belongs to.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Force the platform mutex's lazy initialization. On macOS the first
    /// ever lock of a `std::sync::Mutex` allocates its pthread storage, so
    /// the reset path pays that cost off the audio thread before `process`
    /// can ever `try_lock`.
    pub fn warm_up(&self) {
        drop(self.install.lock().unwrap_or_else(PoisonError::into_inner));
    }

    /// Take a waiting install if the lock is free right now.
    fn drain(&self) -> Option<Box<StageInstall>> {
        let mut slot = match self.install.try_lock() {
            Ok(slot) => slot,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return None,
        };
        slot.take()
    }
}

/// Per-sample values for [`IgorekProcessor::process`]: the four live
/// crossfaders as slices, so a host block rides the parameter smoothers
/// sample by sample. All slices must match the audio length.
pub struct IgorekBlockSettings<'a> {
    pub dry: &'a [f32],
    pub wet: &'a [f32],
    pub color_mix: &'a [f32],
    pub room_mix: &'a [f32],
}

/// Equal-power crossfade gains for a mix position in `0.0..=1.0`.
fn crossfade(mix: f32) -> (f32, f32) {
    let m = mix.clamp(0.0, 1.0);
    (m.sqrt(), (1.0 - m).sqrt())
}

/// The two-stage convolver with partial-block carry and IR swapping.
pub struct IgorekProcessor {
    sample_rate: f64,
    color: [Convolver; 2],
    room: [Convolver; 2],
    color_swap: StageSwap,
    room_swap: StageSwap,
    /// Input staging: samples accumulate here until a full block exists.
    staged: [[f32; P]; 2],
    staged_dry: [f32; P],
    staged_wet: [f32; P],
    staged_color_mix: [f32; P],
    staged_room_mix: [f32; P],
    staged_count: usize,
    /// Engine outputs per stage.
    color_out: [[f32; P]; 2],
    room_out: [[f32; P]; 2],
    /// Post-mix stage signals.
    stage1: [[f32; P]; 2],
    stage2: [[f32; P]; 2],
    /// Output ring: holds one block; the delay it introduces is the whole
    /// plugin's latency.
    out_ring: [[f32; P]; 2],
    out_remaining: usize,
    frame: IgorekFrame,
}

impl IgorekProcessor {
    /// Build a processor around the given swap slots. Allocation happens
    /// here and in [`Self::reset`] only; `process` never allocates.
    #[must_use]
    pub fn new(sample_rate: f64, color_swap: StageSwap, room_swap: StageSwap) -> Self {
        let mut processor = Self {
            sample_rate: 44_100.0,
            color: [Convolver::new(1), Convolver::new(1)],
            room: [Convolver::new(1), Convolver::new(1)],
            color_swap,
            room_swap,
            staged: [[0.0; P]; 2],
            staged_dry: [0.0; P],
            staged_wet: [0.0; P],
            staged_color_mix: [0.0; P],
            staged_room_mix: [0.0; P],
            staged_count: 0,
            color_out: [[0.0; P]; 2],
            room_out: [[0.0; P]; 2],
            stage1: [[0.0; P]; 2],
            stage2: [[0.0; P]; 2],
            out_ring: [[0.0; P]; 2],
            out_remaining: 0,
            frame: IgorekFrame::default(),
        };
        processor.reset(sample_rate);
        processor
    }

    /// Host sample rate the processor is running at.
    #[must_use]
    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    /// The Color stage's swap slot (clone of the one passed to `new`).
    #[must_use]
    pub fn color_swap(&self) -> StageSwap {
        self.color_swap.clone()
    }

    /// The Room stage's swap slot.
    #[must_use]
    pub fn room_swap(&self) -> StageSwap {
        self.room_swap.clone()
    }

    /// Last internal block's signals.
    #[must_use]
    pub fn frame(&self) -> IgorekFrame {
        self.frame
    }

    /// Estimated tail: the installed room IR plus the latency ring.
    #[must_use]
    pub fn tail_samples(&self) -> u32 {
        self.room[0]
            .ir_samples()
            .saturating_add(P)
            .try_into()
            .unwrap_or(u32::MAX)
    }

    /// Size everything for the worst case at `sample_rate` and install the
    /// defaults: the identity Color IR and a short default room (RT60 0.8 s,
    /// variant 0, full width — matching the parameter defaults, so the
    /// plugin passes audio uncolored on insert with a room on it).
    ///
    /// A sample-rate change re-bakes everything; until a task publishes new
    /// spectra the engine runs whatever `reset` installed here.
    pub fn reset(&mut self, sample_rate: f64) {
        let rate = if sample_rate.is_finite() && sample_rate > 0.0 {
            sample_rate
        } else {
            44_100.0
        };
        self.sample_rate = rate;
        self.color_swap.warm_up();
        self.room_swap.warm_up();
        let color_partitions = seconds_to_partitions(COLOR_IR_MAX_SECONDS, rate);
        let room_partitions = seconds_to_partitions(ROOM_IR_MAX_SECONDS, rate);
        self.color = [
            Convolver::new(color_partitions),
            Convolver::new(color_partitions),
        ];
        self.room = [
            Convolver::new(room_partitions),
            Convolver::new(room_partitions),
        ];
        self.staged = [[0.0; P]; 2];
        self.staged_dry = [0.0; P];
        self.staged_wet = [0.0; P];
        self.staged_color_mix = [0.0; P];
        self.staged_room_mix = [0.0; P];
        self.staged_count = 0;
        self.out_ring = [[0.0; P]; 2];
        self.out_remaining = 0;
        self.frame = IgorekFrame::default();

        let identity = LoadedIr::identity(rate);
        let baked = bake_stage(
            &identity.left,
            &identity.right,
            Selection::default(),
            Envelope3::default(),
            rate,
        );
        self.color[0].install(baked.install.left);
        self.color[1].install(baked.install.right);

        let (room_left, room_right) = room::generate_stereo(RoomParams::default(), 0, 1.0, rate);
        let baked = bake_stage(
            &room_left,
            &room_right,
            Selection::default(),
            Envelope3::default(),
            rate,
        );
        self.room[0].install(baked.install.left);
        self.room[1].install(baked.install.right);
    }

    /// Process arbitrarily many samples in and out. Inputs are sanitized;
    /// the outputs are the Dry/Wet mix of the sanitized input and the two
    /// convolution stages, delayed by exactly [`crate::LATENCY_SAMPLES`]
    /// relative to the input.
    pub fn process(
        &mut self,
        left_in: &[f32],
        right_in: &[f32],
        left_out: &mut [f32],
        right_out: &mut [f32],
        settings: IgorekBlockSettings<'_>,
    ) -> IgorekFrame {
        let len = left_out.len();
        assert_eq!(left_in.len(), len, "io slices must match");
        assert_eq!(right_in.len(), len, "io slices must match");
        assert_eq!(right_out.len(), len, "io slices must match");
        assert_eq!(settings.dry.len(), len, "settings slices must match");
        assert_eq!(settings.wet.len(), len, "settings slices must match");
        assert_eq!(settings.color_mix.len(), len, "settings slices must match");
        assert_eq!(settings.room_mix.len(), len, "settings slices must match");

        for i in 0..len {
            // Serve output first: position i delivers the engine's sample
            // i - P, which was produced by an earlier position and has been
            // waiting in the ring. Popping after the push would leak block
            // boundaries one sample early and make the latency P - 1.
            if self.out_remaining > 0 {
                let read = P - self.out_remaining;
                left_out[i] = self.out_ring[0][read];
                right_out[i] = self.out_ring[1][read];
                self.out_remaining -= 1;
            } else {
                left_out[i] = 0.0;
                right_out[i] = 0.0;
            }

            let n = self.staged_count;
            self.staged[0][n] = finite_or(left_in[i], 0.0);
            self.staged[1][n] = finite_or(right_in[i], 0.0);
            self.staged_dry[n] = finite_or(settings.dry[i], 0.0).clamp(0.0, 1.0);
            self.staged_wet[n] = finite_or(settings.wet[i], 0.0).clamp(0.0, 1.0);
            self.staged_color_mix[n] = finite_or(settings.color_mix[i], 1.0).clamp(0.0, 1.0);
            self.staged_room_mix[n] = finite_or(settings.room_mix[i], 1.0).clamp(0.0, 1.0);
            self.staged_count = n + 1;
            if self.staged_count == P {
                self.run_block();
                self.staged_count = 0;
            }
        }
        self.frame
    }

    /// One internal [`P`]-sample block: install any waiting IRs, convolve
    /// both stages, apply the crossfades, and refill the output ring.
    fn run_block(&mut self) {
        if let Some(install) = self.color_swap.drain() {
            self.color[0].install(install.left);
            self.color[1].install(install.right);
        }
        if let Some(install) = self.room_swap.drain() {
            self.room[0].install(install.left);
            self.room[1].install(install.right);
        }

        let staged_l = self.staged[0];
        let staged_r = self.staged[1];
        self.color[0].process_block(&staged_l, &mut self.color_out[0]);
        self.color[1].process_block(&staged_r, &mut self.color_out[1]);

        for s in 0..P {
            let (wet, dry) = crossfade(self.staged_color_mix[s]);
            self.stage1[0][s] = self.color_out[0][s] * wet + staged_l[s] * dry;
            self.stage1[1][s] = self.color_out[1][s] * wet + staged_r[s] * dry;
        }

        let stage1_l = self.stage1[0];
        let stage1_r = self.stage1[1];
        self.room[0].process_block(&stage1_l, &mut self.room_out[0]);
        self.room[1].process_block(&stage1_r, &mut self.room_out[1]);

        for s in 0..P {
            let (wet, dry) = crossfade(self.staged_room_mix[s]);
            self.stage2[0][s] = self.room_out[0][s] * wet + stage1_l[s] * dry;
            self.stage2[1][s] = self.room_out[1][s] * wet + stage1_r[s] * dry;

            let (wet_gain, dry_gain) = crossfade_gain_pair(self.staged_wet[s], self.staged_dry[s]);
            self.out_ring[0][s] =
                finite_or(self.stage2[0][s] * wet_gain + staged_l[s] * dry_gain, 0.0);
            self.out_ring[1][s] =
                finite_or(self.stage2[1][s] * wet_gain + staged_r[s] * dry_gain, 0.0);
        }
        self.out_remaining = P;

        let last = P - 1;
        self.frame = IgorekFrame {
            dry_left: staged_l[last],
            dry_right: staged_r[last],
            color_left: self.stage1[0][last],
            color_right: self.stage1[1][last],
            room_left: self.stage2[0][last],
            room_right: self.stage2[1][last],
            out_left: self.out_ring[0][last],
            out_right: self.out_ring[1][last],
        };
    }
}

/// Dry/Wet are two knobs rather than one crossfade position, so the
/// equal-power pair comes from both values directly.
fn crossfade_gain_pair(wet: f32, dry: f32) -> (f32, f32) {
    (wet.clamp(0.0, 1.0).sqrt(), dry.clamp(0.0, 1.0).sqrt())
}

fn seconds_to_partitions(seconds: f64, rate: f64) -> usize {
    let samples = (seconds * rate).ceil() as usize;
    samples.div_ceil(P).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LATENCY_SAMPLES;

    const RATE: f64 = 48_000.0;

    fn settings(len: usize, dry: f32, wet: f32) -> IgorekBlockSettings<'static> {
        IgorekBlockSettings {
            dry: leak_slice(dry, len),
            wet: leak_slice(wet, len),
            color_mix: leak_slice(1.0, len),
            room_mix: leak_slice(1.0, len),
        }
    }

    /// A 'static slice of one value, so the test settings outlive the call.
    fn leak_slice(value: f32, len: usize) -> &'static [f32] {
        Box::leak(vec![value; len].into_boxed_slice())
    }

    fn identity_room(processor: &mut IgorekProcessor) {
        let baked = bake_stage(
            &[1.0],
            &[1.0],
            Selection::default(),
            Envelope3::default(),
            RATE,
        );
        processor.room[0].install(baked.install.left);
        processor.room[1].install(baked.install.right);
    }

    #[test]
    fn identity_irs_pass_audio_through_delayed_by_one_partition() {
        let mut processor = IgorekProcessor::new(RATE, StageSwap::default(), StageSwap::default());
        identity_room(&mut processor);

        let mut input = vec![0.0_f32; 1_024];
        let mut rng_like = 0usize;
        for sample in &mut input {
            // Cheap deterministic signal: a small LCG-ish walk.
            rng_like = rng_left(rng_like);
            *sample = (rng_like % 1_000) as f32 / 1_000.0 - 0.5;
        }
        let mut left = vec![0.0_f32; input.len()];
        let mut right = vec![0.0_f32; input.len()];
        processor.process(
            &input,
            &input,
            &mut left,
            &mut right,
            settings(input.len(), 0.0, 1.0),
        );

        // The first P samples are the ring's initial silence.
        assert!(left[..P].iter().all(|&s| s == 0.0));
        for i in 0..input.len() - P {
            assert!(
                (left[i + P] - input[i]).abs() < 1e-5,
                "sample {i}: {} vs {}",
                left[i + P],
                input[i]
            );
            assert_eq!(right[i + P], left[i + P], "channels must match");
        }
    }

    fn rng_left(state: usize) -> usize {
        state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407)
    }

    #[test]
    fn arbitrary_host_block_sizes_match_the_128_grid_stream() {
        let mut reference = IgorekProcessor::new(RATE, StageSwap::default(), StageSwap::default());
        identity_room(&mut reference);
        let mut chunked = IgorekProcessor::new(RATE, StageSwap::default(), StageSwap::default());
        identity_room(&mut chunked);

        let mut input = vec![0.0_f32; 2_000];
        let mut state = 42usize;
        for sample in &mut input {
            state = rng_left(state);
            *sample = (state % 511) as f32 / 511.0 - 0.5;
        }

        let mut ref_out = vec![0.0_f32; input.len()];
        let mut ref_dummy = vec![0.0_f32; input.len()];
        reference.process(
            &input,
            &input,
            &mut ref_out,
            &mut ref_dummy,
            settings(input.len(), 0.0, 1.0),
        );

        let mut chunk_out = Vec::with_capacity(input.len());
        let mut pos = 0;
        for size in [1_usize, 7, 128, 129, 500, 3, 128, 128, 64, 312] {
            let end = (pos + size).min(input.len());
            if end == pos {
                break;
            }
            let mut l = vec![0.0_f32; end - pos];
            let mut r = vec![0.0_f32; end - pos];
            chunked.process(
                &input[pos..end],
                &input[pos..end],
                &mut l,
                &mut r,
                settings(end - pos, 0.0, 1.0),
            );
            chunk_out.extend_from_slice(&l);
            pos = end;
        }
        // Cover whatever the sizes did not reach, in one final block.
        if pos < input.len() {
            let tail = input.len() - pos;
            let mut l = vec![0.0_f32; tail];
            let mut r = vec![0.0_f32; tail];
            chunked.process(
                &input[pos..],
                &input[pos..],
                &mut l,
                &mut r,
                settings(tail, 0.0, 1.0),
            );
            chunk_out.extend_from_slice(&l);
        }
        assert_eq!(chunk_out, ref_out);
    }

    #[test]
    fn channels_are_independent() {
        let mut processor = IgorekProcessor::new(RATE, StageSwap::default(), StageSwap::default());
        identity_room(&mut processor);
        let left_in = vec![0.5_f32; 512];
        let right_in = vec![0.0_f32; 512];
        let mut left = vec![0.0_f32; 512];
        let mut right = vec![0.0_f32; 512];
        processor.process(
            &left_in,
            &right_in,
            &mut left,
            &mut right,
            settings(512, 0.0, 1.0),
        );
        assert!(right[P + 8..].iter().all(|&s| s.abs() < 1e-6));
        assert!(left[P + 8..].iter().any(|&s| s.abs() > 1e-3));
    }

    #[test]
    fn a_published_install_takes_effect_and_silence_follows_a_zero_ir() {
        let color_swap = StageSwap::default();
        let mut processor = IgorekProcessor::new(RATE, color_swap.clone(), StageSwap::default());
        identity_room(&mut processor);

        let input = vec![0.5_f32; 8 * P];
        let mut left = vec![0.0_f32; input.len()];
        let mut right = vec![0.0_f32; input.len()];
        processor.process(
            &input,
            &input,
            &mut left,
            &mut right,
            settings(input.len(), 0.0, 1.0),
        );
        assert!(left.iter().any(|&s| s.abs() > 1e-3));

        // A silent Color IR: convolving with it mutes the wet path.
        let baked = bake_stage(
            &[0.0; 64],
            &[0.0; 64],
            Selection::default(),
            Envelope3::default(),
            RATE,
        );
        color_swap.publish(Box::new(baked.install));
        assert_eq!(color_swap.generation(), 1);

        let mut left2 = vec![0.0_f32; input.len()];
        let mut right2 = vec![0.0_f32; input.len()];
        processor.process(
            &input,
            &input,
            &mut left2,
            &mut right2,
            settings(input.len(), 0.0, 1.0),
        );
        // The ring drains the pre-install block first; after two partitions
        // the wet path is silent.
        assert!(left2[2 * P..].iter().all(|&s| s.abs() < 1e-6));
        assert!(left2.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn non_finite_input_never_reaches_the_output() {
        let mut processor = IgorekProcessor::new(RATE, StageSwap::default(), StageSwap::default());
        let input: Vec<f32> = (0..1_024)
            .map(|n| match n % 4 {
                0 => f32::NAN,
                1 => f32::INFINITY,
                2 => f32::NEG_INFINITY,
                _ => 0.25,
            })
            .collect();
        let mut left = vec![0.0_f32; input.len()];
        let mut right = vec![0.0_f32; input.len()];
        for _ in 0..2 {
            processor.process(
                &input,
                &input,
                &mut left,
                &mut right,
                settings(input.len(), 0.0, 1.0),
            );
            assert!(left.iter().chain(right.iter()).all(|s| s.is_finite()));
        }
    }

    #[test]
    fn dry_at_full_passes_the_sanitized_input_at_the_plugin_latency() {
        // Dry rides the same output ring as wet: the plugin reports one
        // partition of latency for everything, so a host compensating PDC
        // keeps dry and wet aligned both inside and outside the plugin.
        let mut processor = IgorekProcessor::new(RATE, StageSwap::default(), StageSwap::default());
        identity_room(&mut processor);
        let input: Vec<f32> = (0..512).map(|i| (i % 17) as f32 / 17.0 - 0.5).collect();
        let mut left = vec![0.0_f32; input.len()];
        let mut right = vec![0.0_f32; input.len()];
        processor.process(
            &input,
            &input,
            &mut left,
            &mut right,
            settings(input.len(), 1.0, 0.0),
        );
        assert!(left[..P].iter().all(|&s| s.abs() < 1e-6));
        for i in 0..input.len() - P {
            assert!((left[i + P] - input[i]).abs() < 1e-6);
            assert!((right[i + P] - input[i]).abs() < 1e-6);
        }
    }

    #[test]
    fn reset_restores_a_fresh_default_state() {
        let mut processor = IgorekProcessor::new(RATE, StageSwap::default(), StageSwap::default());
        let input = vec![0.5_f32; 4 * P];
        let mut left = vec![0.0_f32; input.len()];
        let mut right = vec![0.0_f32; input.len()];
        processor.process(
            &input,
            &input,
            &mut left,
            &mut right,
            settings(input.len(), 0.0, 1.0),
        );

        processor.reset(RATE);
        let mut fresh = IgorekProcessor::new(RATE, StageSwap::default(), StageSwap::default());
        let mut left2 = vec![0.0_f32; input.len()];
        let mut right2 = vec![0.0_f32; input.len()];
        let mut left3 = vec![0.0_f32; input.len()];
        let mut right3 = vec![0.0_f32; input.len()];
        processor.process(
            &input,
            &input,
            &mut left2,
            &mut right2,
            settings(input.len(), 0.0, 1.0),
        );
        fresh.process(
            &input,
            &input,
            &mut left3,
            &mut right3,
            settings(input.len(), 0.0, 1.0),
        );
        assert_eq!(left2, left3);
        assert_eq!(right2, right3);
        assert_eq!(processor.tail_samples(), fresh.tail_samples());
    }

    #[test]
    fn latency_constant_matches_one_partition() {
        assert_eq!(LATENCY_SAMPLES, P as u32);
    }

    #[test]
    fn the_frame_exposes_every_stage_of_the_chain() {
        let mut processor = IgorekProcessor::new(RATE, StageSwap::default(), StageSwap::default());
        identity_room(&mut processor);
        let input = vec![0.5_f32; 2 * P];
        let mut left = vec![0.0_f32; input.len()];
        let mut right = vec![0.0_f32; input.len()];
        let frame = processor.process(
            &input,
            &input,
            &mut left,
            &mut right,
            settings(input.len(), 0.0, 1.0),
        );
        assert!((frame.dry_left - 0.5).abs() < 1e-6);
        // With identity IRs and mixes at 1, every stage carries the input.
        assert!((frame.color_left - 0.5).abs() < 1e-5);
        assert!((frame.room_left - 0.5).abs() < 1e-5);
        assert!((frame.out_left - 0.5).abs() < 1e-5);
    }
}
