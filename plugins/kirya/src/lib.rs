//! Truce wrapper for the Kirya plate reverb.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use kirya_dsp::analysis::IrRender;
use kirya_dsp::{KiryaReverb, KiryaSettings};
use truce::prelude::*;

mod editor;
mod task;

pub use task::RenderIr;

/// Parameter ids are a permanent contract: a host stores automation against
/// them, so an id must never be reused or renumbered.
#[derive(Params)]
pub struct KiryaParams {
    #[param(
        id = 0,
        name = "Dry",
        range = "linear(0, 1)",
        default = 1,
        unit = "%",
        smooth = "exp(5)"
    )]
    pub dry: FloatParam,

    #[param(
        id = 1,
        name = "Wet",
        range = "linear(0, 1)",
        default = 0.5,
        unit = "%",
        smooth = "exp(5)"
    )]
    pub wet: FloatParam,

    /// Pre-delay and Size retune delay lengths, so both take the slow
    /// `exp(50)` glide: it avoids zipper noise and turns a knob sweep into a
    /// musical tape-style pitch slide.
    #[param(
        id = 2,
        name = "Pre-Delay",
        range = "skewed(0, 500, 0.5)",
        default = 0,
        unit = "ms",
        smooth = "exp(50)"
    )]
    pub pre_delay: FloatParam,

    #[param(
        id = 3,
        name = "Size",
        range = "skewed(0.05, 4, 0.4)",
        default = 1,
        smooth = "exp(50)"
    )]
    pub size: FloatParam,

    #[param(
        id = 4,
        name = "Diffusion",
        range = "linear(0, 1)",
        default = 1,
        unit = "%",
        smooth = "exp(5)"
    )]
    pub diffusion: FloatParam,

    #[param(
        id = 5,
        name = "Decay",
        range = "linear(0, 1)",
        default = 0.55,
        unit = "%",
        smooth = "exp(5)"
    )]
    pub decay: FloatParam,

    // The four cutoffs use `log` smoothing rather than `exp`: a frequency
    // reads as smooth when it ramps by a constant ratio, not a constant delta.
    // All four endpoints are strictly positive, so the log ramp is always
    // valid.
    #[param(
        id = 6,
        name = "In Low Cut",
        range = "log(20, 20000)",
        default = 20,
        unit = "Hz",
        format = "format_hz_param",
        smooth = "log(20)"
    )]
    pub input_low_cut: FloatParam,

    #[param(
        id = 7,
        name = "In High Cut",
        range = "log(20, 20000)",
        default = 20000,
        unit = "Hz",
        format = "format_hz_param",
        smooth = "log(20)"
    )]
    pub input_high_cut: FloatParam,

    #[param(
        id = 8,
        name = "Rev Low Cut",
        range = "log(20, 20000)",
        default = 20,
        unit = "Hz",
        format = "format_hz_param",
        smooth = "log(20)"
    )]
    pub reverb_low_cut: FloatParam,

    #[param(
        id = 9,
        name = "Rev High Cut",
        range = "log(20, 20000)",
        default = 10000,
        unit = "Hz",
        format = "format_hz_param",
        smooth = "log(20)"
    )]
    pub reverb_high_cut: FloatParam,

    #[param(
        id = 10,
        name = "Mod Rate",
        range = "log(0.01, 20)",
        default = 1,
        unit = "Hz",
        format = "format_hz_param",
        smooth = "log(20)"
    )]
    pub mod_rate: FloatParam,

    #[param(
        id = 11,
        name = "Mod Depth",
        range = "linear(0, 1)",
        default = 0.5,
        unit = "%",
        smooth = "exp(5)"
    )]
    pub mod_depth: FloatParam,

    /// Modulation waveform: 0% is a triangle, 100% a sine.
    #[param(
        id = 12,
        name = "Mod Shape",
        range = "linear(0, 1)",
        default = 0.5,
        unit = "%",
        smooth = "exp(5)"
    )]
    pub mod_shape: FloatParam,

    /// Hold the tank indefinitely. The input stays live, so new material can
    /// be layered into a frozen tail.
    #[param(id = 13, name = "Freeze", default = false)]
    pub freeze: BoolParam,

    /// Latest finished impulse-response render, handed from the background
    /// task to the editor. `process` never touches it, so a plain `Mutex` is
    /// safe here — no audio-thread lock is ever taken.
    #[skip]
    pub ir_slot: Arc<Mutex<Option<Box<IrRender>>>>,

    /// Bumped every time `ir_slot` is replaced, so the editor can tell a new
    /// render from the one it already uploaded.
    #[skip]
    pub ir_generation: Arc<AtomicU64>,

    /// Host sample rate as `f64` bits, published by `reset`.
    ///
    /// `PluginContext` does not carry the rate, and the IR probe has to render
    /// at whatever the host is playing or the displayed reverb time would be
    /// wrong at anything but 48 kHz. Zero until the first activation.
    #[skip]
    pub sample_rate_bits: Arc<AtomicU64>,
}

impl KiryaParams {
    /// Display formatter for every Hz parameter, named by `#[param(format)]`.
    ///
    /// Truce's built-in Hz formatter prints whole hertz below 1 kHz, which
    /// turns Mod Rate's whole `0.01..20` range into "0 Hz" and "1 Hz".
    #[allow(clippy::unused_self)]
    fn format_hz_param(&self, value: f64) -> String {
        musictools_core::format_hz(value)
    }

    /// Read every parameter once, advancing all smoothers exactly one sample.
    fn settings(&self) -> KiryaSettings {
        KiryaSettings {
            dry: self.dry.read(),
            wet: self.wet.read(),
            pre_delay_ms: self.pre_delay.read(),
            size: self.size.read(),
            diffusion: self.diffusion.read(),
            decay: self.decay.read(),
            input_low_cut_hz: self.input_low_cut.read(),
            input_high_cut_hz: self.input_high_cut.read(),
            reverb_low_cut_hz: self.reverb_low_cut.read(),
            reverb_high_cut_hz: self.reverb_high_cut.read(),
            mod_rate_hz: self.mod_rate.read(),
            mod_depth: self.mod_depth.read(),
            mod_shape: self.mod_shape.read(),
            freeze: self.freeze.value(),
        }
    }

    /// Read every parameter's raw target, advancing nothing.
    ///
    /// The smoothers belong to the audio thread; anything off it — the IR
    /// render task, the editor's change detection — must use this instead of
    /// [`Self::settings`], or it steals samples from `process`.
    pub(crate) fn target_settings(&self) -> KiryaSettings {
        KiryaSettings {
            dry: self.dry.value(),
            wet: self.wet.value(),
            pre_delay_ms: self.pre_delay.value(),
            size: self.size.value(),
            diffusion: self.diffusion.value(),
            decay: self.decay.value(),
            input_low_cut_hz: self.input_low_cut.value(),
            input_high_cut_hz: self.input_high_cut.value(),
            reverb_low_cut_hz: self.reverb_low_cut.value(),
            reverb_high_cut_hz: self.reverb_high_cut.value(),
            mod_rate_hz: self.mod_rate.value(),
            mod_depth: self.mod_depth.value(),
            mod_shape: self.mod_shape.value(),
            freeze: self.freeze.value(),
        }
    }
}

/// Per-instance reverb state.
#[derive(Default)]
pub struct KiryaState {
    reverb: KiryaReverb,
    /// Whether the last processed sample was frozen, so `tail` can report an
    /// infinite tail without reaching into the DSP.
    frozen: bool,
}

/// Plugin descriptor for the Kirya plate reverb.
pub struct Kirya;

impl PluginLogic for Kirya {
    type Params = KiryaParams;
    type DspState = KiryaState;

    // `PRESERVE_DSP_STATE` is left at the trait default of `true` so a live
    // tail survives an editor reload.

    fn reset(state: &mut Self::DspState, params: &Self::Params, config: &AudioConfig) {
        state.reverb.reset(config.sample_rate);
        state.frozen = false;
        params
            .sample_rate_bits
            .store(config.sample_rate.to_bits(), Ordering::Relaxed);
    }

    fn process(
        state: &mut Self::DspState,
        params: &Self::Params,
        buffer: &mut AudioBuffer,
        _events: &EventList,
        _context: &mut ProcessContext,
    ) -> ProcessStatus {
        let channels = buffer.channels();
        if channels == 0 {
            return ProcessStatus::Normal;
        }

        for sample_index in 0..buffer.num_samples() {
            // One read of every parameter per frame, before the channel work:
            // the tank is mono, so reading per channel would advance each
            // smoother twice on a stereo track.
            let settings = params.settings();
            state.frozen = settings.freeze;

            let left = buffer.input(0)[sample_index];
            let right = if channels > 1 {
                buffer.input(1)[sample_index]
            } else {
                left
            };

            let frame = state.reverb.process(left, right, settings);

            buffer.output(0)[sample_index] = frame.left;
            if channels > 1 {
                buffer.output(1)[sample_index] = frame.right;
            }
        }

        // Any channel past the second is a layout the plugin does not claim to
        // support; leave it silent rather than passing it through unprocessed.
        for channel in 2..channels {
            buffer.output(channel)[..].fill(0.0);
        }

        ProcessStatus::Normal
    }

    /// A frozen tank never decays, so report an infinite tail and keep the
    /// host from cutting the track off underneath it.
    fn tail(state: &Self::DspState) -> u32 {
        if state.frozen {
            u32::MAX
        } else {
            state.reverb.tail_samples()
        }
    }

    fn editor(params: Arc<Self::Params>) -> Box<dyn Editor> {
        editor::create(params)
    }
}

truce::plugin! {
    logic: Kirya,
    params: KiryaParams,
    tasks: [RenderIr],
}

truce::enable_rt_paranoid!();

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_RATE: f64 = 48_000.0;

    /// Drive `blocks` blocks of `input` through a fresh instance.
    fn run(
        state: &mut KiryaState,
        params: &KiryaParams,
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

        assert_eq!(
            Kirya::process(state, params, &mut buffer, &events, &mut context),
            ProcessStatus::Normal
        );
    }

    #[test]
    fn a_dry_impulse_passes_through_and_a_wet_tail_follows_it() {
        // The block has to outrun the first output tap on each side. At 48 kHz
        // and Size 1.0 the earliest left tap lands near sample 429 and the
        // earliest right tap near 569 — that offset between the two is the
        // paper's stereo image, so a shorter block would show a silent right
        // channel and prove nothing.
        const BLOCK: usize = 2_048;

        let params = KiryaParams::default();
        let mut state = KiryaState::default();
        Kirya::reset(&mut state, &params, &AudioConfig::new(SAMPLE_RATE, BLOCK));

        let mut input = [0.0_f32; BLOCK];
        input[0] = 1.0;
        let mut left = [0.0_f32; BLOCK];
        let mut right = [0.0_f32; BLOCK];
        run(&mut state, &params, &input, &mut left, &mut right);

        // Dry defaults to 100%, so the impulse itself survives.
        assert!((left[0] - 1.0).abs() < 0.05, "dry impulse was {}", left[0]);
        // ...and the tank answers with something in both channels.
        assert!(left[100..].iter().any(|&s| s.abs() > 1.0e-6));
        assert!(right[100..].iter().any(|&s| s.abs() > 1.0e-6));
        assert!(left.iter().chain(right.iter()).all(|s| s.is_finite()));
        // The two sides are genuinely different, not a duplicated mono tail.
        assert!(
            left.iter()
                .zip(right.iter())
                .any(|(l, r)| (l - r).abs() > 1.0e-3)
        );
    }

    #[test]
    fn every_hz_parameter_displays_with_useful_precision() {
        // Truce's built-in Hz formatter rounds to whole hertz below 1 kHz, so
        // without the `format` hook Mod Rate reads "0 Hz" across the bottom
        // half of its travel. This asserts the hook is actually wired up.
        let params = KiryaParams::default();
        let format = |id: u32, value: f64| params.format_value(id, value).unwrap();

        assert_eq!(format(10, 0.01), "0.01 Hz");
        assert_eq!(format(10, 1.0), "1.00 Hz");
        assert_eq!(format(10, 20.0), "20.0 Hz");

        for id in 6..=9 {
            assert_eq!(format(id, 20.0), "20.0 Hz");
            assert_eq!(format(id, 440.0), "440 Hz");
            assert_eq!(format(id, 10_000.0), "10.00 kHz");
        }

        // Params that were already fine must not have changed.
        assert_eq!(format(3, 1.0), "1.00");
        assert_eq!(format(5, 0.55), "55%");
        assert_eq!(format(2, 0.0), "0.0 ms");
    }

    #[test]
    fn a_mono_layout_is_processed_without_reading_a_second_channel() {
        let params = KiryaParams::default();
        let mut state = KiryaState::default();
        Kirya::reset(&mut state, &params, &AudioConfig::new(SAMPLE_RATE, 256));

        let mut input = [0.0_f32; 256];
        input[0] = 1.0;
        let inputs: [&[f32]; 1] = [&input];
        let mut output = [0.0_f32; 256];
        let mut outputs: [&mut [f32]; 1] = [&mut output];
        let mut buffer = AudioBuffer::from_slices_checked(&inputs, &mut outputs, 256);
        let events = EventList::default();
        let mut output_events = EventList::default();
        let transport = TransportInfo::default();
        let mut context = ProcessContext::new(&transport, SAMPLE_RATE, 256, &mut output_events);

        assert_eq!(
            Kirya::process(&mut state, &params, &mut buffer, &events, &mut context),
            ProcessStatus::Normal
        );
        assert!(output.iter().all(|s| s.is_finite()));
        assert!((output[0] - 1.0).abs() < 0.05);
    }

    #[test]
    fn non_finite_host_input_never_reaches_the_output() {
        let params = KiryaParams::default();
        let mut state = KiryaState::default();
        Kirya::reset(&mut state, &params, &AudioConfig::new(SAMPLE_RATE, 256));

        let input: Vec<f32> = (0..256)
            .map(|n| match n % 4 {
                0 => f32::NAN,
                1 => f32::INFINITY,
                2 => f32::NEG_INFINITY,
                _ => 0.5,
            })
            .collect();
        let mut left = [0.0_f32; 256];
        let mut right = [0.0_f32; 256];

        for _ in 0..8 {
            run(&mut state, &params, &input, &mut left, &mut right);
            assert!(left.iter().chain(right.iter()).all(|s| s.is_finite()));
        }
    }

    #[test]
    fn the_reported_tail_is_finite_while_running_and_infinite_while_frozen() {
        let params = KiryaParams::default();
        let mut state = KiryaState::default();
        Kirya::reset(&mut state, &params, &AudioConfig::new(SAMPLE_RATE, 256));

        let input = [0.0_f32; 256];
        let mut left = [0.0_f32; 256];
        let mut right = [0.0_f32; 256];

        run(&mut state, &params, &input, &mut left, &mut right);
        let running = Kirya::tail(&state);
        assert!(running > 0, "a reverb with no tail is not a reverb");
        assert!(running < u32::MAX);

        params.freeze.set_value(true);
        run(&mut state, &params, &input, &mut left, &mut right);
        assert_eq!(Kirya::tail(&state), u32::MAX);

        params.freeze.set_value(false);
        run(&mut state, &params, &input, &mut left, &mut right);
        assert!(Kirya::tail(&state) < u32::MAX);
    }

    #[test]
    fn reset_clears_the_tail_between_activations() {
        let params = KiryaParams::default();
        let mut used = KiryaState::default();
        let mut fresh = KiryaState::default();
        let config = AudioConfig::new(SAMPLE_RATE, 256);
        Kirya::reset(&mut used, &params, &config);
        Kirya::reset(&mut fresh, &params, &config);

        let mut loud = [0.0_f32; 256];
        loud[0] = 1.0;
        let mut left = [0.0_f32; 256];
        let mut right = [0.0_f32; 256];
        for _ in 0..8 {
            run(&mut used, &params, &loud, &mut left, &mut right);
        }
        Kirya::reset(&mut used, &params, &config);
        params.snap_smoothers();

        let silence = [0.0_f32; 256];
        let mut used_left = [0.0_f32; 256];
        let mut used_right = [0.0_f32; 256];
        let mut fresh_left = [0.0_f32; 256];
        let mut fresh_right = [0.0_f32; 256];
        run(
            &mut used,
            &params,
            &silence,
            &mut used_left,
            &mut used_right,
        );
        run(
            &mut fresh,
            &params,
            &silence,
            &mut fresh_left,
            &mut fresh_right,
        );
        assert_eq!(used_left, fresh_left);
        assert_eq!(used_right, fresh_right);
    }

    #[cfg(feature = "rt-paranoid")]
    #[test]
    fn process_allocates_nothing_even_while_size_and_freeze_move() {
        // Eight delay lines plus a Size control is the likeliest place for an
        // accidental reallocation on the audio thread, so sweep Size across
        // its whole range and toggle Freeze while auditing.
        const BLOCK_SIZE: usize = 256;
        const BLOCKS: usize = 400;

        let params = KiryaParams::default();
        let mut state = KiryaState::default();
        Kirya::reset(
            &mut state,
            &params,
            &AudioConfig::new(SAMPLE_RATE, BLOCK_SIZE),
        );

        let input = [0.25_f32; BLOCK_SIZE];
        let mut left = [0.0_f32; BLOCK_SIZE];
        let mut right = [0.0_f32; BLOCK_SIZE];

        let (_, allocations) = truce::rt::audit(|| {
            let _section = truce::rt::RtSection::enter();
            for block in 0..BLOCKS {
                #[allow(clippy::cast_precision_loss)]
                let sweep = block as f32 / BLOCKS as f32;
                params
                    .size
                    .set_value(f64::from(0.05 + sweep * (4.0 - 0.05)));
                params.pre_delay.set_value(f64::from(sweep * 500.0));
                params.freeze.set_value(block % 32 >= 16);

                let inputs: [&[f32]; 2] = [&input, &input];
                let mut outputs: [&mut [f32]; 2] = [&mut left, &mut right];
                let mut buffer =
                    AudioBuffer::from_slices_checked(&inputs, &mut outputs, BLOCK_SIZE);
                let events = EventList::default();
                let mut output_events = EventList::default();
                let transport = TransportInfo::default();
                let mut context =
                    ProcessContext::new(&transport, SAMPLE_RATE, BLOCK_SIZE, &mut output_events);
                let _ = Kirya::process(&mut state, &params, &mut buffer, &events, &mut context);
            }
        });

        assert_eq!(allocations, 0);
    }
}
