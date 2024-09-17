use nih_plug::prelude::*;
use nih_plug_egui::{create_egui_editor, egui, widgets, EguiState};
use std::sync::Arc;

/// The time it takes for the peak meter to decay by 12 dB after switching to complete silence.
const PEAK_METER_DECAY_MS: f64 = 150.0;

struct RingModSideChain {
    params: Arc<RingModSideChainParams>,

    /// Needed to normalize the peak meter's response based on the sample rate.
    peak_meter_decay_weight: f32,
    /// The current data for the peak meter. This is stored as an [`Arc`] so we can share it between
    /// the GUI and the audio processing parts. If you have more state to share, then it's a good
    /// idea to put all of that in a struct behind a single `Arc`.
    ///
    /// This is stored as voltage gain.
    peak_meter: Arc<AtomicF32>,
    side_chain_peak_meter: Arc<AtomicF32>,
}

#[derive(Params)]
struct RingModSideChainParams {
    /// The editor state, saved together with the parameter state so the custom scaling can be
    /// restored.
    #[persist = "editor-state"]
    editor_state: Arc<EguiState>,

    #[id = "gain"]
    pub gain: FloatParam,

    #[id = "side_chain_gain"]
    pub side_chain_gain: FloatParam,

    // TODO: Remove this parameter when we're done implementing the widgets
    #[id = "foobar"]
    pub some_int: IntParam,
}

impl Default for RingModSideChain {
    fn default() -> Self {
        Self {
            params: Arc::new(RingModSideChainParams::default()),

            peak_meter_decay_weight: 1.0,
            peak_meter: Arc::new(AtomicF32::new(util::MINUS_INFINITY_DB)),
            side_chain_peak_meter: Arc::new(AtomicF32::new(util::MINUS_INFINITY_DB)), // Initialize side chain peak meter
        }
    }
}

impl Default for RingModSideChainParams {
    fn default() -> Self {
        Self {
            editor_state: EguiState::from_size(300, 300),

            // See the main gain example for more details
            gain: FloatParam::new(
                "Gain",
                util::db_to_gain(0.0),
                FloatRange::Skewed {
                    min: util::db_to_gain(-30.0),
                    max: util::db_to_gain(30.0),
                    factor: FloatRange::gain_skew_factor(-30.0, 30.0),
                },
            )
            .with_smoother(SmoothingStyle::Logarithmic(50.0))
            .with_unit(" dB")
            .with_value_to_string(formatters::v2s_f32_gain_to_db(2))
            .with_string_to_value(formatters::s2v_f32_gain_to_db()),
            side_chain_gain: FloatParam::new(
                "Side Chain Gain",
                util::db_to_gain(0.0),
                FloatRange::Skewed {
                    min: util::db_to_gain(-30.0),
                    max: util::db_to_gain(30.0),
                    factor: FloatRange::gain_skew_factor(-30.0, 30.0),
                },
            )
            .with_smoother(SmoothingStyle::Logarithmic(50.0))
            .with_unit(" dB")
            .with_value_to_string(formatters::v2s_f32_gain_to_db(2))
            .with_string_to_value(formatters::s2v_f32_gain_to_db()),
            some_int: IntParam::new("Something", 3, IntRange::Linear { min: 0, max: 3 }),
        }
    }
}

// Move this function outside of the impl block
fn add_peak_meter_ui(ui: &mut egui::Ui, meter: &Arc<AtomicF32>) {
    let peak_meter = util::gain_to_db(meter.load(std::sync::atomic::Ordering::Relaxed));
    let peak_meter_text = if peak_meter > util::MINUS_INFINITY_DB {
        format!("{peak_meter:.1} dBFS")
    } else {
        String::from("-inf dBFS")
    };

    let peak_meter_normalized = (peak_meter + 60.0) / 60.0;
    ui.add(egui::widgets::ProgressBar::new(peak_meter_normalized).text(peak_meter_text));
}

impl Plugin for RingModSideChain {
    const NAME: &'static str = "RMSC-RS";
    const VENDOR: &'static str = "oiwn";
    const URL: &'static str = env!("CARGO_PKG_HOMEPAGE");
    const EMAIL: &'static str = "blank@gmail.com";

    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    const MIDI_INPUT: MidiConfig = MidiConfig::None;
    const MIDI_OUTPUT: MidiConfig = MidiConfig::None;

    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[
        AudioIOLayout {
            main_input_channels: NonZeroU32::new(2),
            main_output_channels: NonZeroU32::new(2),
            aux_input_ports: &[new_nonzero_u32(2)], // Add side chain input
            // aux_input_ports: &[NonZeroU32::new(2).unwrap()],
            ..AudioIOLayout::const_default()
        },
        AudioIOLayout {
            main_input_channels: NonZeroU32::new(2),
            main_output_channels: NonZeroU32::new(2),
            ..AudioIOLayout::const_default()
        },
    ];

    const SAMPLE_ACCURATE_AUTOMATION: bool = true;

    // If the plugin can send or receive SysEx messages, it can define a type to wrap around those
    // messages here. The type implements the `SysExMessage` trait, which allows conversion to and
    // from plain byte buffers.
    type SysExMessage = ();
    // More advanced plugins can use this to run expensive background tasks. See the field's
    // documentation for more information. `()` means that the plugin does not have any background
    // tasks.
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    fn editor(&mut self, _async_executor: AsyncExecutor<Self>) -> Option<Box<dyn Editor>> {
        let params = self.params.clone();
        let peak_meter = self.peak_meter.clone();
        let side_chain_peak_meter = self.side_chain_peak_meter.clone();
        create_egui_editor(
            self.params.editor_state.clone(),
            (),
            |_, _| {},
            move |egui_ctx, setter, _state| {
                egui::CentralPanel::default().show(egui_ctx, |ui| {
                    ui.heading("Ring Mod Side Chain");

                    ui.label("Main Gain");
                    ui.add(widgets::ParamSlider::for_param(&params.gain, setter));

                    ui.label("Side Chain Gain");
                    ui.add(widgets::ParamSlider::for_param(
                        &params.side_chain_gain,
                        setter,
                    ));

                    ui.group(|ui| {
                        ui.label("Main Peak Meter");
                        add_peak_meter_ui(ui, &peak_meter);
                    });

                    ui.group(|ui| {
                        ui.label("Side Chain Peak Meter");
                        add_peak_meter_ui(ui, &side_chain_peak_meter);
                    });
                });
            },
        )
    }

    fn initialize(
        &mut self,
        _audio_io_layout: &AudioIOLayout,
        buffer_config: &BufferConfig,
        _context: &mut impl InitContext<Self>,
    ) -> bool {
        // After `PEAK_METER_DECAY_MS` milliseconds of pure silence, the peak meter's value should
        // have dropped by 12 dB
        self.peak_meter_decay_weight = 0.25f64
            .powf((buffer_config.sample_rate as f64 * PEAK_METER_DECAY_MS / 1000.0).recip())
            as f32;

        true
    }

    fn reset(&mut self) {
        // Reset buffers and envelopes here. This can be called from the audio thread and may not
        // allocate. You can remove this function if you do not need it.
    }

    fn process(
        &mut self,
        buffer: &mut Buffer,
        _aux: &mut AuxiliaryBuffers,
        _context: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        for channel_samples in buffer.iter_samples() {
            let mut amplitude = 0.0;
            let num_samples = channel_samples.len();

            let gain = self.params.gain.smoothed.next();
            for sample in channel_samples {
                *sample *= gain;
                amplitude += *sample;
            }

            // To save resources, a plugin can (and probably should!) only perform expensive
            // calculations that are only displayed on the GUI while the GUI is open
            if self.params.editor_state.is_open() {
                amplitude = (amplitude / num_samples as f32).abs();
                let current_peak_meter = self.peak_meter.load(std::sync::atomic::Ordering::Relaxed);
                let new_peak_meter = if amplitude > current_peak_meter {
                    amplitude
                } else {
                    current_peak_meter * self.peak_meter_decay_weight
                        + amplitude * (1.0 - self.peak_meter_decay_weight)
                };

                self.peak_meter
                    .store(new_peak_meter, std::sync::atomic::Ordering::Relaxed)
            }
        }

        ProcessStatus::Normal
    }
}

impl RingModSideChain {
    #[allow(dead_code)]
    fn update_peak_meter(&self, amplitude: f32, num_samples: usize, meter: &Arc<AtomicF32>) {
        let amplitude = (amplitude / num_samples as f32).abs();
        let current_peak_meter = meter.load(std::sync::atomic::Ordering::Relaxed);
        let new_peak_meter = if amplitude > current_peak_meter {
            amplitude
        } else {
            current_peak_meter * self.peak_meter_decay_weight
                + amplitude * (1.0 - self.peak_meter_decay_weight)
        };

        meter.store(new_peak_meter, std::sync::atomic::Ordering::Relaxed);
    }

    #[allow(dead_code)]
    fn add_peak_meter_ui(&self, ui: &mut egui::Ui, meter: &Arc<AtomicF32>) {
        let peak_meter = util::gain_to_db(meter.load(std::sync::atomic::Ordering::Relaxed));
        let peak_meter_text = if peak_meter > util::MINUS_INFINITY_DB {
            format!("{peak_meter:.1} dBFS")
        } else {
            String::from("-inf dBFS")
        };

        let peak_meter_normalized = (peak_meter + 60.0) / 60.0;
        ui.add(egui::widgets::ProgressBar::new(peak_meter_normalized).text(peak_meter_text));
    }
}

impl ClapPlugin for RingModSideChain {
    const CLAP_ID: &'static str = "com.your-domain.rmsc-rs";
    const CLAP_DESCRIPTION: Option<&'static str> = Some("Ring Mod Side Chain apptmpt in Rust");
    const CLAP_MANUAL_URL: Option<&'static str> = Some(Self::URL);
    const CLAP_SUPPORT_URL: Option<&'static str> = None;

    // Don't forget to change these features
    const CLAP_FEATURES: &'static [ClapFeature] = &[ClapFeature::AudioEffect, ClapFeature::Stereo];
}

impl Vst3Plugin for RingModSideChain {
    const VST3_CLASS_ID: [u8; 16] = *b"Exactly16Chars!!";

    // And also don't forget to change these categories
    const VST3_SUBCATEGORIES: &'static [Vst3SubCategory] =
        &[Vst3SubCategory::Fx, Vst3SubCategory::Dynamics];
}

nih_export_clap!(RingModSideChain);
nih_export_vst3!(RingModSideChain);
