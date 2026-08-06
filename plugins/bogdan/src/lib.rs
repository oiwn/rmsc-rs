//! Truce wrapper for the Bogdan detail-preserving clipper.

use bogdan_dsp::{ClipMode, DetailClipper, DetailSettings, FoldShape};
use truce::prelude::*;

mod editor;

/// Peak-reshaping voice, mirrors [`bogdan_dsp::ClipMode`] on the framework side.
#[derive(ParamEnum)]
pub enum Mode {
    Clean,
    Detail,
    Fold,
}

impl Mode {
    fn to_dsp(self) -> ClipMode {
        match self {
            Mode::Clean => ClipMode::Clean,
            Mode::Detail => ClipMode::Detail,
            Mode::Fold => ClipMode::Fold,
        }
    }
}

/// Wavefolder transfer function, mirrors [`bogdan_dsp::FoldShape`].
#[derive(ParamEnum)]
pub enum Shape {
    Sine,
    Triangle,
}

impl Shape {
    fn to_dsp(self) -> FoldShape {
        match self {
            Shape::Sine => FoldShape::Sine,
            Shape::Triangle => FoldShape::Triangle,
        }
    }
}

#[derive(Params)]
pub struct BogdanParams {
    #[param(
        id = 0,
        name = "Drive",
        range = "linear(0, 24)",
        default = 0,
        unit = "dB",
        smooth = "exp(5)"
    )]
    pub drive: FloatParam,

    #[param(
        id = 1,
        name = "Ceiling",
        range = "linear(-24, 0)",
        default = -0.1,
        unit = "dB",
        smooth = "exp(5)"
    )]
    pub ceiling: FloatParam,

    /// Peak-reshaping mode. Defaults to `Detail` (variant index 1).
    #[param(id = 2, name = "Mode", default = 1)]
    pub mode: EnumParam<Mode>,

    /// Clipping-delta high-pass cutoff. Low values fold the peak shape inward;
    /// high values isolate fast edge detail (the classic Detail voice).
    #[param(
        id = 3,
        name = "Detail",
        range = "log(20, 2000)",
        default = 1000,
        unit = "Hz",
        smooth = "exp(20)"
    )]
    pub detail: FloatParam,

    /// Effect depth: 0% is a plain clip, 100% is full motion (inward duck for
    /// Detail, clip→fold blend for Fold).
    #[param(
        id = 4,
        name = "Amount",
        range = "linear(0, 1)",
        default = 1,
        unit = "%",
        smooth = "exp(5)"
    )]
    pub amount: FloatParam,

    /// Wavefolder transfer function (Fold mode only). Defaults to `Sine`.
    #[param(id = 5, name = "Shape", default = 0)]
    pub shape: EnumParam<Shape>,

    /// Interleaved `[driven, processed]` frames for the oscilloscope.
    #[skip]
    scope_tap: Arc<AudioTap<f32>>,
}

/// Per-instance filter state for Bogdan's supported mono/stereo layouts.
#[derive(Default)]
pub struct BogdanState {
    channels: [DetailClipper; 2],
    scope_transfer: Vec<f32>,
}

impl BogdanState {
    fn reset(&mut self, sample_rate: f64, max_block_size: usize) {
        for channel in &mut self.channels {
            channel.reset(sample_rate);
        }
        self.scope_transfer.resize(max_block_size * 2, 0.0);
    }
}

/// Plugin descriptor for Bogdan's detail-preserving clipping path.
pub struct Bogdan;

impl PluginLogic for Bogdan {
    type Params = BogdanParams;
    type DspState = BogdanState;

    const PRESERVE_DSP_STATE: bool = false;

    fn reset(state: &mut Self::DspState, params: &Self::Params, config: &AudioConfig) {
        state.reset(config.sample_rate, config.max_block_size);
        params.scope_tap.clear();
    }

    fn process(
        state: &mut Self::DspState,
        params: &Self::Params,
        buffer: &mut AudioBuffer,
        _events: &EventList,
        _context: &mut ProcessContext,
    ) -> ProcessStatus {
        let num_samples = buffer.num_samples();
        debug_assert!(state.scope_transfer.len() >= num_samples * 2);

        for sample_index in 0..buffer.num_samples() {
            // Advance every smoothed parameter exactly once per frame.
            let settings = DetailSettings {
                drive: db_to_linear(params.drive.read()),
                ceiling: db_to_linear(params.ceiling.read()),
                mode: params.mode.value().to_dsp(),
                detail_hz: params.detail.read(),
                amount: params.amount.read(),
                shape: params.shape.value().to_dsp(),
            };

            for channel in 0..buffer.channels() {
                let (input, output) = buffer.io(channel);
                let frame = state.channels[channel].process(input[sample_index], settings);
                output[sample_index] = frame.output;

                if channel == 0 {
                    let scope_index = sample_index * 2;
                    state.scope_transfer[scope_index] = frame.driven;
                    state.scope_transfer[scope_index + 1] = frame.output;
                }
            }
        }

        if buffer.channels() > 0 {
            params
                .scope_tap
                .push_frames(&state.scope_transfer[..num_samples * 2]);
        }

        ProcessStatus::Normal
    }

    fn editor(params: Arc<Self::Params>) -> Box<dyn Editor> {
        editor::create(params)
    }
}

truce::plugin! {
    logic: Bogdan,
    params: BogdanParams,
}

truce::enable_rt_paranoid!();

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_RATE: f64 = 48_000.0;

    #[test]
    fn process_taps_driven_and_processed_channel_zero_frames() {
        let params = BogdanParams::default();
        let mut state = BogdanState::default();
        Bogdan::reset(&mut state, &params, &AudioConfig::new(SAMPLE_RATE, 4));

        let left_input = [0.25, 1.25, -1.5, 0.5];
        let right_input = [-0.25, -1.25, 1.5, -0.5];
        let inputs: [&[f32]; 2] = [&left_input, &right_input];
        let mut left_output = [0.0; 4];
        let mut right_output = [0.0; 4];
        let mut outputs: [&mut [f32]; 2] = [&mut left_output, &mut right_output];
        let mut buffer = AudioBuffer::from_slices_checked(&inputs, &mut outputs, 4);
        let events = EventList::default();
        let mut output_events = EventList::default();
        let transport = TransportInfo::default();
        let mut context = ProcessContext::new(&transport, SAMPLE_RATE, 4, &mut output_events);

        assert_eq!(
            Bogdan::process(&mut state, &params, &mut buffer, &events, &mut context,),
            ProcessStatus::Normal
        );

        let mut tapped = Vec::new();
        params
            .scope_tap
            .drain_with(|samples| tapped.extend_from_slice(samples));
        assert_eq!(tapped.len(), left_input.len() * 2);
        for (index, pair) in tapped.chunks_exact(2).enumerate() {
            assert_eq!(pair[0], left_input[index]);
            assert_eq!(pair[1], left_output[index]);
        }
    }

    #[cfg(feature = "rt-paranoid")]
    #[test]
    fn process_scope_feed_is_allocation_free_even_when_tap_is_full() {
        const BLOCK_SIZE: usize = 256;
        const BLOCKS_TO_OVERFLOW_TAP: usize = 140;

        let params = BogdanParams::default();
        let mut state = BogdanState::default();
        Bogdan::reset(
            &mut state,
            &params,
            &AudioConfig::new(SAMPLE_RATE, BLOCK_SIZE),
        );

        let input = [1.5; BLOCK_SIZE];
        let inputs: [&[f32]; 1] = [&input];
        let mut output = [0.0; BLOCK_SIZE];
        let mut outputs: [&mut [f32]; 1] = [&mut output];
        let mut buffer = AudioBuffer::from_slices_checked(&inputs, &mut outputs, BLOCK_SIZE);
        let events = EventList::default();
        let mut output_events = EventList::default();
        let transport = TransportInfo::default();
        let mut context =
            ProcessContext::new(&transport, SAMPLE_RATE, BLOCK_SIZE, &mut output_events);

        let (_, allocations) = truce::rt::audit(|| {
            let _section = truce::rt::RtSection::enter();
            for _ in 0..BLOCKS_TO_OVERFLOW_TAP {
                let _ = Bogdan::process(&mut state, &params, &mut buffer, &events, &mut context);
            }
        });

        assert_eq!(allocations, 0);
    }
}
