//! Truce wrapper for the Igorek two-stage convolver.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use igorek_dsp::LATENCY_SAMPLES;
use igorek_dsp::bake::IrView;
use igorek_dsp::processor::{IgorekBlockSettings, IgorekProcessor, StageSwap};
use igorek_dsp::wav::LoadedIr;
use truce::prelude::*;

mod editor;
mod task;

pub use task::{BakeColor, BakeRoom};

/// The Color IR as persisted in host sessions. Empty channels mean the
/// identity impulse (nothing loaded). Stored resampled at the rate active
/// when it was saved; a recall at another rate re-resamples from it.
#[derive(State, Default)]
struct IgorekCustomState {
    color_left: Vec<f32>,
    color_right: Vec<f32>,
    source_rate: f64,
}

/// Parameter ids are a permanent contract: a host stores automation against
/// them, so an id must never be reused or renumbered. The table in
/// `specs/igorek.md` is the reference; this struct is its implementation.
#[derive(Params)]
pub struct IgorekParams {
    #[param(
        id = 0,
        name = "Dry",
        range = "linear(0, 1)",
        default = 0,
        unit = "%",
        smooth = "exp(5)"
    )]
    pub dry: FloatParam,

    #[param(
        id = 1,
        name = "Wet",
        range = "linear(0, 1)",
        default = 1,
        unit = "%",
        smooth = "exp(5)"
    )]
    pub wet: FloatParam,

    #[param(
        id = 2,
        name = "Color Mix",
        range = "linear(0, 1)",
        default = 1,
        unit = "%",
        smooth = "exp(5)"
    )]
    pub color_mix: FloatParam,

    // Selection and envelope are baked into the IR before the FFT, so they
    // take no smoothing: a smoothed value would be invisible between bakes,
    // and edits land at the next spectra swap rather than zippering.
    #[param(
        id = 3,
        name = "Color Sel Start",
        range = "linear(0, 0.95)",
        default = 0,
        unit = "%"
    )]
    pub color_sel_start: FloatParam,

    #[param(
        id = 4,
        name = "Color Sel Length",
        range = "linear(0.05, 1)",
        default = 1,
        unit = "%"
    )]
    pub color_sel_length: FloatParam,

    #[param(
        id = 5,
        name = "Color Env A",
        range = "linear(-60, 12)",
        default = 0,
        unit = "dB"
    )]
    pub color_env_a: FloatParam,

    #[param(
        id = 6,
        name = "Color Env B X",
        range = "linear(0, 1)",
        default = 0.5,
        unit = "%"
    )]
    pub color_env_b_x: FloatParam,

    #[param(
        id = 7,
        name = "Color Env B",
        range = "linear(-60, 12)",
        default = 0,
        unit = "dB"
    )]
    pub color_env_b: FloatParam,

    #[param(
        id = 8,
        name = "Color Env C",
        range = "linear(-60, 12)",
        default = 0,
        unit = "dB"
    )]
    pub color_env_c: FloatParam,

    #[param(
        id = 9,
        name = "Room Mix",
        range = "linear(0, 1)",
        default = 1,
        unit = "%",
        smooth = "exp(5)"
    )]
    pub room_mix: FloatParam,

    #[param(
        id = 10,
        name = "Room Sel Start",
        range = "linear(0, 0.95)",
        default = 0,
        unit = "%"
    )]
    pub room_sel_start: FloatParam,

    #[param(
        id = 11,
        name = "Room Sel Length",
        range = "linear(0.05, 1)",
        default = 1,
        unit = "%"
    )]
    pub room_sel_length: FloatParam,

    #[param(
        id = 12,
        name = "Room Env A",
        range = "linear(-60, 12)",
        default = 0,
        unit = "dB"
    )]
    pub room_env_a: FloatParam,

    #[param(
        id = 13,
        name = "Room Env B X",
        range = "linear(0, 1)",
        default = 0.5,
        unit = "%"
    )]
    pub room_env_b_x: FloatParam,

    #[param(
        id = 14,
        name = "Room Env B",
        range = "linear(-60, 12)",
        default = 0,
        unit = "dB"
    )]
    pub room_env_b: FloatParam,

    #[param(
        id = 15,
        name = "Room Env C",
        range = "linear(-60, 12)",
        default = 0,
        unit = "dB"
    )]
    pub room_env_c: FloatParam,

    // The baked room parameters take no smoothing either: each step of a
    // smoother would be inaudible — the room only changes at the next
    // debounced regeneration.
    #[param(
        id = 16,
        name = "Room RT60",
        range = "log(0.05, 8)",
        default = 0.8,
        unit = "s"
    )]
    pub room_rt60: FloatParam,

    #[param(
        id = 17,
        name = "Room EDT",
        range = "log(5, 1000)",
        default = 50,
        unit = "ms"
    )]
    pub room_edt: FloatParam,

    #[param(
        id = 18,
        name = "Room ITDG",
        range = "linear(0, 50)",
        default = 4,
        unit = "ms"
    )]
    pub room_itdg: FloatParam,

    #[param(
        id = 19,
        name = "Room ER Duration",
        range = "log(5, 500)",
        default = 100,
        unit = "ms"
    )]
    pub room_er_duration: FloatParam,

    /// The seed selector: one step is a fresh but deterministic room.
    #[param(
        id = 20,
        name = "Room Variant",
        range = "discrete(0, 100)",
        default = 0
    )]
    pub room_variant: IntParam,

    #[param(
        id = 21,
        name = "Room Width",
        range = "linear(0, 1)",
        default = 1,
        unit = "%"
    )]
    pub room_width: FloatParam,

    // --- Handoff fields, shared between the audio thread, the background
    // bake tasks, and the editor. ---
    /// IR install slots the audio thread drains at block boundaries.
    #[skip]
    pub color_swap: StageSwap,
    #[skip]
    pub room_swap: StageSwap,

    /// Shaped-IR views for the editor panes, published by the bake tasks.
    #[skip]
    pub color_view: Arc<Mutex<Option<Box<IrView>>>>,
    #[skip]
    pub color_view_generation: Arc<std::sync::atomic::AtomicU64>,
    #[skip]
    pub room_view: Arc<Mutex<Option<Box<IrView>>>>,
    #[skip]
    pub room_view_generation: Arc<std::sync::atomic::AtomicU64>,

    /// The loaded Color IR; `None` means the identity impulse. Shared with
    /// the DspState so `snapshot_into` can persist it without touching the
    /// audio path.
    #[skip]
    pub color_ir: Arc<Mutex<Option<LoadedIr>>>,

    /// Editor notices ("file truncated", "file rejected: ..."), taken by
    /// the editor on repaint.
    #[skip]
    pub notice: Arc<Mutex<Option<String>>>,

    /// Host sample rate as `f64` bits, published by `reset`.
    #[skip]
    pub sample_rate_bits: Arc<std::sync::atomic::AtomicU64>,
}

/// Per-instance state.
pub struct IgorekState {
    processor: IgorekProcessor,
    /// Same `Arc` as `IgorekParams::color_ir`, for persistence.
    color_ir: Arc<Mutex<Option<LoadedIr>>>,
    /// Per-sample smoother outputs for the four live crossfaders, sized in
    /// `reset` to the host's max block and never reallocated there after.
    dry: Vec<f32>,
    wet: Vec<f32>,
    color_mix: Vec<f32>,
    room_mix: Vec<f32>,
    /// Stereo copy scratch for host buffers, same sizing rules. Mono
    /// channels are duplicated in; extra output channels stay silent.
    stereo_in: [Vec<f32>; 2],
    stereo_out: [Vec<f32>; 2],
    /// Rate of the last `reset`; a change means every IR must re-bake.
    last_rate: Option<f64>,
    pending_color_rebake: bool,
    pending_room_rebake: bool,
}

impl Default for IgorekState {
    /// Standalone state with its own (unwired) handoff slots. The plugin
    /// itself always builds state through `init`, which wires the
    /// params-shared slots; `Default` only satisfies the shell's bounds.
    fn default() -> Self {
        Self::wired(&IgorekParams::default())
    }
}

impl IgorekState {
    /// State wired to the params-owned handoff slots.
    fn wired(params: &IgorekParams) -> Self {
        Self {
            processor: IgorekProcessor::new(
                44_100.0,
                params.color_swap.clone(),
                params.room_swap.clone(),
            ),
            color_ir: Arc::clone(&params.color_ir),
            dry: Vec::new(),
            wet: Vec::new(),
            color_mix: Vec::new(),
            room_mix: Vec::new(),
            stereo_in: [Vec::new(), Vec::new()],
            stereo_out: [Vec::new(), Vec::new()],
            last_rate: None,
            pending_color_rebake: false,
            pending_room_rebake: false,
        }
    }

    fn resize_scratch(&mut self, max_block: usize) {
        let n = max_block.max(1);
        self.dry.resize(n, 0.0);
        self.wet.resize(n, 1.0);
        self.color_mix.resize(n, 1.0);
        self.room_mix.resize(n, 1.0);
        for channel in &mut self.stereo_in {
            channel.resize(n, 0.0);
        }
        for channel in &mut self.stereo_out {
            channel.resize(n, 0.0);
        }
    }
}

/// Plugin descriptor for the Igorek two-stage convolver.
pub struct Igorek;

impl PluginLogic for Igorek {
    type Params = IgorekParams;
    type DspState = IgorekState;

    fn init(params: &Self::Params, _cx: &InitContext) -> Self::DspState {
        IgorekState::wired(params)
    }

    fn reset(state: &mut Self::DspState, params: &Self::Params, config: &AudioConfig) {
        let rate = if config.sample_rate.is_finite() && config.sample_rate > 0.0 {
            config.sample_rate
        } else {
            44_100.0
        };
        // A rate change re-bakes everything: the room regenerates at the new
        // rate and the Color WAV re-resamples. The audio thread runs the old
        // spectra until the new generation installs.
        let rebake = state.last_rate != Some(rate);
        state.last_rate = Some(rate);
        state.pending_color_rebake |= rebake;
        state.pending_room_rebake |= rebake;

        state.processor.reset(rate);
        state.resize_scratch(config.max_block_size);
        params
            .sample_rate_bits
            .store(rate.to_bits(), Ordering::Relaxed);
    }

    fn process(
        state: &mut Self::DspState,
        params: &Self::Params,
        buffer: &mut AudioBuffer,
        _events: &EventList,
        context: &mut ProcessContext,
    ) -> ProcessStatus {
        let channels = buffer.channels();
        if channels == 0 {
            return ProcessStatus::Normal;
        }
        let len = buffer.num_samples();

        // Pending rebakes (state recall, rate change) spawn from here:
        // `spawn_coalescing` is wait-free, and this is the one place with
        // both the state flags and a task context.
        if state.pending_color_rebake {
            if let Some(spawner) = context.tasks::<BakeColor>() {
                spawner.spawn_coalescing(BakeColor {
                    sample_rate: state.processor.sample_rate(),
                });
            }
            state.pending_color_rebake = false;
        }
        if state.pending_room_rebake {
            if let Some(spawner) = context.tasks::<BakeRoom>() {
                spawner.spawn_coalescing(BakeRoom {
                    sample_rate: state.processor.sample_rate(),
                });
            }
            state.pending_room_rebake = false;
        }

        // Advance the smoothers exactly once per sample, then hand the
        // block to the processor through the stereo scratch (the buffer's
        // input and output borrows cannot be held simultaneously). A host
        // block larger than the scratch (it promised `max_block_size` in
        // reset, but be robust anyway) is processed in chunks.
        let IgorekState {
            processor,
            dry,
            wet,
            color_mix,
            room_mix,
            stereo_in,
            stereo_out,
            ..
        } = state;
        let capacity = dry.len().max(1);
        let mut start = 0;
        while start < len {
            let end = (start + capacity).min(len);
            let n = end - start;
            params.dry.read_into(&mut dry[..n]);
            params.wet.read_into(&mut wet[..n]);
            params.color_mix.read_into(&mut color_mix[..n]);
            params.room_mix.read_into(&mut room_mix[..n]);

            for (i, &sample) in buffer.input(0)[start..end].iter().enumerate() {
                stereo_in[0][i] = sample;
                stereo_in[1][i] = if channels > 1 {
                    buffer.input(1)[start + i]
                } else {
                    sample
                };
            }
            let settings = IgorekBlockSettings {
                dry: &dry[..n],
                wet: &wet[..n],
                color_mix: &color_mix[..n],
                room_mix: &room_mix[..n],
            };
            // Array-pattern borrows give disjoint access to both channels.
            let [in_l, in_r] = &*stereo_in;
            let [out_l, out_r] = &mut *stereo_out;
            processor.process(
                &in_l[..n],
                &in_r[..n],
                &mut out_l[..n],
                &mut out_r[..n],
                settings,
            );
            for (i, &sample) in stereo_out[0][..n].iter().enumerate() {
                buffer.output(0)[start + i] = sample;
                if channels > 1 {
                    buffer.output(1)[start + i] = stereo_out[1][i];
                }
            }
            start = end;
        }

        // Any channel past the second is a layout the plugin does not claim
        // to support; leave it silent.
        for channel in 2..channels {
            buffer.output(channel)[..].fill(0.0);
        }

        ProcessStatus::Normal
    }

    /// One partition, exactly, whatever the host's block size.
    fn latency(_state: &Self::DspState) -> u32 {
        LATENCY_SAMPLES
    }

    fn tail(state: &Self::DspState) -> u32 {
        state.processor.tail_samples()
    }

    fn editor(params: Arc<Self::Params>) -> Box<dyn Editor> {
        editor::create(params)
    }

    fn snapshot_into(state: &Self::DspState, buf: &mut Vec<u8>) -> bool {
        let slot = state
            .color_ir
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let custom = match slot.as_ref() {
            Some(ir) => IgorekCustomState {
                color_left: ir.left.clone(),
                color_right: ir.right.clone(),
                source_rate: ir.sample_rate,
            },
            None => IgorekCustomState::default(),
        };
        drop(slot);
        custom.serialize_into(buf);
        true
    }

    fn load_state(state: &mut Self::DspState, data: &[u8]) -> Result<(), StateLoadError> {
        // An empty chunk means the session predates any loaded IR; keep
        // whatever is live.
        if data.is_empty() {
            return Ok(());
        }
        let Some(custom) = IgorekCustomState::deserialize(data) else {
            return Err(StateLoadError::Malformed("IgorekCustomState"));
        };
        let mut slot = state
            .color_ir
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *slot = if custom.color_left.is_empty() {
            None
        } else {
            Some(LoadedIr {
                left: custom.color_left,
                right: custom.color_right,
                sample_rate: custom.source_rate,
                truncated: false,
            })
        };
        drop(slot);
        // Recall may land at a different rate than the IR was saved at; the
        // bake task re-resamples from the stored samples.
        state.pending_color_rebake = true;
        Ok(())
    }
}

truce::plugin! {
    logic: Igorek,
    params: IgorekParams,
    tasks: [BakeColor, BakeRoom],
}

truce::enable_rt_paranoid!();

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_RATE: f64 = 48_000.0;
    const BLOCK: usize = 256;

    fn fresh() -> (IgorekParams, IgorekState) {
        let params = IgorekParams::default();
        let state = IgorekState::wired(&params);
        (params, state)
    }

    fn run(
        state: &mut IgorekState,
        params: &IgorekParams,
        input: &[f32],
        output_left: &mut [f32],
        output_right: &mut [f32],
    ) {
        let inputs: [&[f32]; 2] = [input, input];
        let mut outputs: [&mut [f32]; 2] = [output_left, output_right];
        let mut buffer = AudioBuffer::from_slices_checked(&inputs, &mut outputs, input.len());
        let events = EventList::default();
        let mut output_events = EventList::default();
        let transport = TransportInfo::default();
        let mut context =
            ProcessContext::new(&transport, SAMPLE_RATE, input.len(), &mut output_events);
        let _ = Igorek::process(state, params, &mut buffer, &events, &mut context);
    }

    #[test]
    fn parameter_defaults_match_the_id_contract() {
        let params = IgorekParams::default();
        assert_eq!(params.dry.value(), 0.0);
        assert_eq!(params.wet.value(), 1.0);
        assert_eq!(params.color_mix.value(), 1.0);
        assert_eq!(params.color_sel_start.value(), 0.0);
        assert_eq!(params.color_sel_length.value(), 1.0);
        assert_eq!(params.color_env_a.value(), 0.0);
        assert_eq!(params.color_env_b_x.value(), 0.5);
        assert_eq!(params.color_env_b.value(), 0.0);
        assert_eq!(params.color_env_c.value(), 0.0);
        assert_eq!(params.room_mix.value(), 1.0);
        assert_eq!(params.room_sel_start.value(), 0.0);
        assert_eq!(params.room_sel_length.value(), 1.0);
        assert_eq!(params.room_env_a.value(), 0.0);
        assert_eq!(params.room_env_b_x.value(), 0.5);
        assert_eq!(params.room_env_b.value(), 0.0);
        assert_eq!(params.room_env_c.value(), 0.0);
        assert_eq!(params.room_rt60.value(), 0.8);
        assert_eq!(params.room_edt.value(), 50.0);
        assert_eq!(params.room_itdg.value(), 4.0);
        assert_eq!(params.room_er_duration.value(), 100.0);
        assert_eq!(params.room_variant.value(), 0);
        assert_eq!(params.room_width.value(), 1.0);
    }

    #[test]
    fn parameter_ids_fill_the_contract_range() {
        let params = IgorekParams::default();
        let infos = params.param_infos();
        let ids: Vec<u32> = infos.iter().map(|info| info.id).collect();
        assert_eq!(ids.len(), 22, "22 parameters");
        for id in 0..22_u32 {
            assert!(ids.contains(&id), "id {id} missing");
        }
    }

    #[test]
    fn latency_is_one_partition() {
        let (_params, state) = fresh();
        assert_eq!(Igorek::latency(&state), 128);
    }

    #[test]
    fn an_impulse_comes_back_finite_with_a_room_tail() {
        let (params, mut state) = fresh();
        Igorek::reset(&mut state, &params, &AudioConfig::new(SAMPLE_RATE, BLOCK));

        let mut input = [0.0_f32; 2_048];
        input[0] = 1.0;
        let mut left = [0.0_f32; 2_048];
        let mut right = [0.0_f32; 2_048];
        run(&mut state, &params, &input, &mut left, &mut right);

        assert!(left.iter().chain(right.iter()).all(|s| s.is_finite()));
        assert!(left.iter().any(|&s| s.abs() > 1e-5));
        assert!(right.iter().any(|&s| s.abs() > 1e-5));
        // The two room channels are decorrelated by default (Width 1).
        assert!(
            left.iter()
                .zip(right.iter())
                .any(|(l, r)| (l - r).abs() > 1e-4)
        );
        assert!(Igorek::tail(&state) > 0);
    }

    #[test]
    fn non_finite_input_never_reaches_the_output() {
        let (params, mut state) = fresh();
        Igorek::reset(&mut state, &params, &AudioConfig::new(SAMPLE_RATE, BLOCK));
        let input: Vec<f32> = (0..BLOCK)
            .map(|n| match n % 4 {
                0 => f32::NAN,
                1 => f32::INFINITY,
                2 => f32::NEG_INFINITY,
                _ => 0.5,
            })
            .collect();
        let mut left = [0.0_f32; BLOCK];
        let mut right = [0.0_f32; BLOCK];
        for _ in 0..4 {
            run(&mut state, &params, &input, &mut left, &mut right);
            assert!(left.iter().chain(right.iter()).all(|s| s.is_finite()));
        }
    }

    #[test]
    fn custom_state_round_trips_the_loaded_ir() {
        let (params, mut state) = fresh();
        Igorek::reset(&mut state, &params, &AudioConfig::new(SAMPLE_RATE, BLOCK));

        let ir = LoadedIr {
            left: vec![0.5, 0.25, 0.125],
            right: vec![0.4, 0.2, 0.1],
            sample_rate: SAMPLE_RATE,
            truncated: false,
        };
        *params.color_ir.lock().unwrap() = Some(ir.clone());

        let mut bytes = Vec::new();
        assert!(Igorek::snapshot_into(&state, &mut bytes));
        assert!(!bytes.is_empty());

        // Recall into a fresh instance: the IR reappears and a rebake is
        // pending.
        let (params2, mut state2) = fresh();
        Igorek::reset(&mut state2, &params2, &AudioConfig::new(SAMPLE_RATE, BLOCK));
        state2.pending_color_rebake = false;
        Igorek::load_state(&mut state2, &bytes).unwrap();
        assert!(state2.pending_color_rebake);
        {
            let slot = params2.color_ir.lock().unwrap();
            let restored = slot.as_ref().unwrap();
            assert_eq!(restored.left, ir.left);
            assert_eq!(restored.right, ir.right);
            assert_eq!(restored.sample_rate, SAMPLE_RATE);
        }

        // An identity session persists empty channels and recalls to None.
        *params.color_ir.lock().unwrap() = None;
        let mut empty = Vec::new();
        assert!(Igorek::snapshot_into(&state, &mut empty));
        Igorek::load_state(&mut state2, &empty).unwrap();
        assert!(params2.color_ir.lock().unwrap().is_none());

        // Garbage is rejected.
        assert!(Igorek::load_state(&mut state2, &[0xDE, 0xAD]).is_err());
    }

    #[test]
    fn a_sample_rate_change_schedules_a_rebake() {
        let (params, mut state) = fresh();
        Igorek::reset(&mut state, &params, &AudioConfig::new(44_100.0, BLOCK));
        state.pending_color_rebake = false;
        state.pending_room_rebake = false;
        Igorek::reset(&mut state, &params, &AudioConfig::new(48_000.0, BLOCK));
        assert!(state.pending_color_rebake);
        assert!(state.pending_room_rebake);
    }

    #[cfg(feature = "rt-paranoid")]
    #[test]
    fn process_allocates_nothing_across_swaps_and_block_sizes() {
        // The allocation risks unique to this plugin are the IR generation
        // installs and the partial-block carries, so sweep both: publish
        // spectra between blocks and drive irregular host block sizes while
        // auditing.
        let (params, mut state) = fresh();
        Igorek::reset(&mut state, &params, &AudioConfig::new(SAMPLE_RATE, 512));

        // One process call outside the audit clears the pending rebakes.
        {
            let input = [0.0_f32; 64];
            let mut left = [0.0_f32; 64];
            let mut right = [0.0_f32; 64];
            run(&mut state, &params, &input, &mut left, &mut right);
        }

        // Stack buffers only: the audit counts every allocation, so nothing
        // inside a section may allocate.
        let input = [0.25_f32; 512];
        let mut left = [0.0_f32; 512];
        let mut right = [0.0_f32; 512];
        for round in 0..24 {
            #[allow(clippy::cast_precision_loss)]
            let sweep = round as f32 / 24.0;
            params.dry.set_value(f64::from(sweep * 0.5));
            params.wet.set_value(f64::from(1.0 - sweep * 0.5));
            params.color_mix.set_value(f64::from(sweep));
            params.room_sel_start.set_value(f64::from(sweep * 0.5));
            params.room_env_b.set_value(f64::from(-60.0 + sweep * 72.0));

            // Publish fresh spectra between rounds (outside the audit —
            // baking is task-thread work); the audited `process` below
            // installs them at a block boundary.
            if round % 8 == 0 {
                let baked = igorek_dsp::bake::bake_stage(
                    &[1.0],
                    &[1.0],
                    igorek_dsp::bake::Selection::default(),
                    igorek_dsp::bake::Envelope3::default(),
                    SAMPLE_RATE,
                );
                params.color_swap.publish(Box::new(baked.install));
            }

            let size = [1_usize, 64, 128, 129, 333, 512][round % 6];
            let (_result, allocations) = truce::rt::audit(|| {
                let _section = truce::rt::RtSection::enter();
                let inputs: [&[f32]; 2] = [&input[..size], &input[..size]];
                let mut outputs: [&mut [f32]; 2] = [&mut left[..size], &mut right[..size]];
                let mut buffer = AudioBuffer::from_slices_checked(&inputs, &mut outputs, size);
                let events = EventList::default();
                let mut output_events = EventList::default();
                let transport = TransportInfo::default();
                let mut context =
                    ProcessContext::new(&transport, SAMPLE_RATE, size, &mut output_events);
                let _ = Igorek::process(&mut state, &params, &mut buffer, &events, &mut context);
            });
            assert_eq!(allocations, 0, "round {round}");
        }
    }
}
