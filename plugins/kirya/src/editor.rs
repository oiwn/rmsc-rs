//! egui editor: three control rows under a live impulse-response probe.
//!
//! The probe is the point of the plugin's face — a plate is hard to judge from
//! knob positions alone, so the editor renders the current settings' impulse
//! response off-thread and draws its decay envelope and log-frequency
//! spectrogram above the controls.

use std::sync::Arc;
use std::sync::TryLockError;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use egui::{
    Align2, Color32, ColorImage, FontId, Pos2, Rect, RichText, Sense, Stroke, TextureHandle,
    TextureOptions, Vec2,
};
use kirya_dsp::analysis::{FLOOR_DB, IR_WINDOW_STOPS, IrRender};
use truce::prelude::*;
use truce_egui::theme::{BACKGROUND, HEADER_BG, TEXT, TEXT_DIM};
use truce_egui::widgets::{param_knob, param_toggle};
use truce_egui::{EditorUi, EguiEditor};

use crate::{KiryaParams, KiryaParamsParamId as P, RenderIr};

/// Wide enough for a six-knob row, tall enough for both analysis panels above
/// three control rows.
const EDITOR_SIZE: (u32, u32) = (720, 700);

/// Widths the `truce-egui` widgets allocate, needed to centre a row by hand.
const KNOB_WIDTH: f32 = 60.0;
const TOGGLE_WIDTH: f32 = 60.0;
const GAP: f32 = 16.0;

const ENVELOPE_HEIGHT: f32 = 140.0;
const SPECTROGRAM_HEIGHT: f32 = 180.0;
const PANEL_MARGIN: f32 = 12.0;

/// Quietest level either panel plots. Anything below reads as the floor.
const DISPLAY_FLOOR_DB: f32 = -70.0;

/// How long a knob must sit still before a re-render is posted. A drag that
/// outruns this collapses into one render, because the task lane coalesces.
const DEBOUNCE: Duration = Duration::from_millis(150);

/// The editor polls for a finished render, so it needs a steady repaint.
const REPAINT_INTERVAL: Duration = Duration::from_millis(33);

const LEFT_COLOR: Color32 = Color32::from_rgb(241, 174, 74);
const RIGHT_COLOR: Color32 = Color32::from_rgb(71, 207, 218);
const GRID_COLOR: Color32 = Color32::from_rgb(66, 67, 78);

/// The parameters an impulse-response render actually depends on.
///
/// Dry, Wet and Freeze are excluded on purpose: the probe renders wet-only
/// with Freeze forced off, so moving those would only trigger renders that
/// produce a pixel-identical picture.
#[derive(Clone, Copy, Debug, PartialEq)]
struct RenderKey {
    pre_delay_ms: f32,
    size: f32,
    diffusion: f32,
    decay: f32,
    input_low_cut_hz: f32,
    input_high_cut_hz: f32,
    reverb_low_cut_hz: f32,
    reverb_high_cut_hz: f32,
    mod_rate_hz: f32,
    mod_depth: f32,
    mod_shape: f32,
    sample_rate: f64,
}

impl RenderKey {
    fn read(params: &KiryaParams, sample_rate: f64) -> Self {
        let settings = params.target_settings();
        Self {
            pre_delay_ms: settings.pre_delay_ms,
            size: settings.size,
            diffusion: settings.diffusion,
            decay: settings.decay,
            input_low_cut_hz: settings.input_low_cut_hz,
            input_high_cut_hz: settings.input_high_cut_hz,
            reverb_low_cut_hz: settings.reverb_low_cut_hz,
            reverb_high_cut_hz: settings.reverb_high_cut_hz,
            mod_rate_hz: settings.mod_rate_hz,
            mod_depth: settings.mod_depth,
            mod_shape: settings.mod_shape,
            sample_rate,
        }
    }
}

pub(crate) struct KiryaEditor {
    /// Most recent finished render, kept locally so drawing never holds the
    /// handoff lock.
    render: Option<Box<IrRender>>,
    /// Spectrogram uploaded once per render, not rebuilt per frame.
    texture: Option<TextureHandle>,
    /// Generation of the render currently held, so a repeat is not re-uploaded.
    generation: u64,
    /// Settings the last request was posted for.
    requested: Option<RenderKey>,
    /// When the settings last changed, for the debounce.
    changed_at: Option<Instant>,
}

impl KiryaEditor {
    fn new() -> Self {
        Self {
            render: None,
            texture: None,
            generation: 0,
            requested: None,
            changed_at: None,
        }
    }

    /// Host rate if the plugin has been activated, else a sensible stand-in so
    /// the probe still draws something before the first `reset`.
    fn sample_rate(params: &KiryaParams) -> f64 {
        let bits = params.sample_rate_bits.load(Ordering::Relaxed);
        let rate = f64::from_bits(bits);
        if rate.is_finite() && rate > 0.0 {
            rate
        } else {
            48_000.0
        }
    }

    /// Post a render when the settings have been still for [`DEBOUNCE`].
    fn maybe_request(&mut self, ui: &egui::Ui, state: &PluginContext<KiryaParams>) {
        let sample_rate = Self::sample_rate(state.params());
        let key = RenderKey::read(state.params(), sample_rate);

        if self.requested != Some(key) {
            self.requested = Some(key);
            self.changed_at = Some(Instant::now());
        }

        let Some(changed_at) = self.changed_at else {
            return;
        };
        if changed_at.elapsed() < DEBOUNCE {
            // Make sure a frame lands once the debounce expires even if the
            // user has stopped moving the mouse.
            ui.ctx().request_repaint_after(DEBOUNCE);
            return;
        }

        // A `None` spawner means the host build has no task pool; the last
        // image simply stays on screen.
        if let Some(spawner) = state.tasks::<RenderIr>() {
            spawner.spawn_coalescing(RenderIr { sample_rate });
        }
        self.changed_at = None;
    }

    /// Pick up a finished render and upload its spectrogram exactly once.
    fn collect_render(&mut self, ui: &egui::Ui, state: &PluginContext<KiryaParams>) {
        let params = state.params();
        let generation = params.ir_generation.load(Ordering::Acquire);
        if generation == self.generation {
            return;
        }

        // `try_lock` keeps a busy worker from stalling the GUI thread; the
        // next frame picks the render up instead. A poisoned lock is still
        // worth reading — the slot is a plain cache, so the worst a panicking
        // writer left behind is a stale render.
        let mut slot = match params.ir_slot.try_lock() {
            Ok(slot) => slot,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return,
        };
        let Some(render) = slot.take() else {
            return;
        };
        drop(slot);

        self.texture = Some(ui.ctx().load_texture(
            "kirya-spectrogram",
            spectrogram_image(&render),
            TextureOptions::LINEAR,
        ));
        self.render = Some(render);
        self.generation = generation;
    }

    /// Seconds the panels currently span. The render picks its own window from
    /// the reverb time, so the axis follows the data rather than a constant.
    fn duration(&self) -> f32 {
        #[allow(clippy::cast_possible_truncation)]
        self.render
            .as_ref()
            .map_or(IR_WINDOW_STOPS[0] as f32, |render| {
                render.duration_seconds()
            })
    }
}

impl EditorUi<KiryaParams> for KiryaEditor {
    fn opened(&mut self, _state: &PluginContext<KiryaParams>) {
        // Forget what was requested so the first frame posts a render for
        // whatever the parameters are now.
        self.requested = None;
        self.changed_at = None;
    }

    fn ui(&mut self, ui: &mut egui::Ui, state: &PluginContext<KiryaParams>) {
        ui.ctx().request_repaint_after(REPAINT_INTERVAL);
        self.maybe_request(ui, state);
        self.collect_render(ui, state);

        ui.painter().rect_filled(ui.max_rect(), 0.0, BACKGROUND);
        ui.vertical_centered(|ui| {
            ui.add_space(12.0);
            ui.label(
                RichText::new("KIRYA")
                    .strong()
                    .size(24.0)
                    .extra_letter_spacing(2.0)
                    .color(TEXT),
            );
            ui.add_space(1.0);
            // Subtitle plus a build stamp so the loaded bundle is identifiable.
            ui.label(
                RichText::new(concat!("plate reverb  (v", env!("CARGO_PKG_VERSION"), ")"))
                    .strong()
                    .size(12.0)
                    .color(TEXT_DIM),
            );
            ui.add_space(10.0);

            self.draw_envelope(ui);
            ui.add_space(6.0);
            self.draw_spectrogram(ui);
            ui.add_space(14.0);

            controls(ui, state);
            ui.add_space(12.0);
        });
    }
}

impl KiryaEditor {
    /// Decay envelope: both channels in dB, over a shared time axis.
    fn draw_envelope(&self, ui: &mut egui::Ui) {
        let (rect, plot) = panel(ui, ENVELOPE_HEIGHT);
        let duration = self.duration();
        let painter = ui.painter_at(rect);

        // Level grid, labelled on the left.
        for db in [0.0_f32, -20.0, -40.0, -60.0] {
            let y = level_y(db, plot);
            painter.line_segment(
                [Pos2::new(plot.left(), y), Pos2::new(plot.right(), y)],
                Stroke::new(1.0, GRID_COLOR),
            );
            painter.text(
                Pos2::new(plot.left() + 2.0, y + 1.0),
                Align2::LEFT_TOP,
                format!("{db:.0}"),
                FontId::proportional(9.0),
                TEXT_DIM,
            );
        }
        time_ruler(&painter, plot, duration);

        let Some(render) = self.render.as_ref() else {
            painter.text(
                plot.center(),
                Align2::CENTER_CENTER,
                "rendering…",
                FontId::proportional(11.0),
                TEXT_DIM,
            );
            return;
        };

        for (envelope, color) in [
            (&render.left_envelope_db, LEFT_COLOR),
            (&render.right_envelope_db, RIGHT_COLOR),
        ] {
            if envelope.len() < 2 {
                continue;
            }
            let points: Vec<Pos2> = envelope
                .iter()
                .enumerate()
                .map(|(index, &db)| {
                    #[allow(clippy::cast_precision_loss)]
                    let fraction = index as f32 / (envelope.len() - 1) as f32;
                    Pos2::new(plot.left() + fraction * plot.width(), level_y(db, plot))
                })
                .collect();
            painter.add(egui::Shape::line(points, Stroke::new(1.4, color)));
        }

        let readout = match render.rt60_seconds {
            Some(rt60) => format!("RT60  {rt60:.2} s"),
            // A tail that has not fallen 35 dB by the end of the window has no
            // reliable slope to fit, so say so rather than invent a number.
            None => format!("RT60  > {duration:.0} s"),
        };
        painter.text(
            Pos2::new(plot.right() - 4.0, plot.top() + 1.0),
            Align2::RIGHT_TOP,
            readout,
            FontId::proportional(11.0),
            TEXT,
        );
        painter.text(
            Pos2::new(plot.left() + 26.0, plot.top() + 1.0),
            Align2::LEFT_TOP,
            "L",
            FontId::proportional(9.0),
            LEFT_COLOR,
        );
        painter.text(
            Pos2::new(plot.left() + 38.0, plot.top() + 1.0),
            Align2::LEFT_TOP,
            "R",
            FontId::proportional(9.0),
            RIGHT_COLOR,
        );
    }

    /// Log-frequency spectrogram, sharing the envelope's time axis.
    fn draw_spectrogram(&self, ui: &mut egui::Ui) {
        let (rect, plot) = panel(ui, SPECTROGRAM_HEIGHT);
        let duration = self.duration();
        let painter = ui.painter_at(rect);

        if let Some(texture) = self.texture.as_ref() {
            painter.image(
                texture.id(),
                plot,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        } else {
            painter.text(
                plot.center(),
                Align2::CENTER_CENTER,
                "rendering…",
                FontId::proportional(11.0),
                TEXT_DIM,
            );
        }

        // Frequency gridlines sit on top of the image.
        for (hz, label) in [(100.0, "100"), (1_000.0, "1k"), (10_000.0, "10k")] {
            let y = frequency_y(hz, plot);
            painter.line_segment(
                [Pos2::new(plot.left(), y), Pos2::new(plot.right(), y)],
                Stroke::new(1.0, GRID_COLOR.gamma_multiply(0.7)),
            );
            painter.text(
                Pos2::new(plot.left() + 2.0, y - 1.0),
                Align2::LEFT_BOTTOM,
                label,
                FontId::proportional(9.0),
                TEXT_DIM,
            );
        }
        time_ruler(&painter, plot, duration);
    }
}

/// Allocate a framed panel and return its outer rect and inner plot area.
fn panel(ui: &mut egui::Ui, height: f32) -> (Rect, Rect) {
    let width = (ui.available_width() - 2.0 * PANEL_MARGIN).max(1.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 6.0, HEADER_BG);
    painter.rect_stroke(
        rect,
        6.0,
        Stroke::new(1.0, GRID_COLOR),
        egui::StrokeKind::Inside,
    );
    (rect, rect.shrink(6.0))
}

/// Vertical position of a level, from 0 dB at the top to the display floor.
fn level_y(db: f32, plot: Rect) -> f32 {
    let fraction = ((db.max(DISPLAY_FLOOR_DB)) / DISPLAY_FLOOR_DB).clamp(0.0, 1.0);
    plot.top() + fraction * plot.height()
}

/// Vertical position of a frequency on the spectrogram's log axis.
fn frequency_y(hz: f64, plot: Rect) -> f32 {
    use kirya_dsp::analysis::{SPECTROGRAM_MAX_HZ, SPECTROGRAM_MIN_HZ};
    let span = (SPECTROGRAM_MAX_HZ / SPECTROGRAM_MIN_HZ).log10();
    #[allow(clippy::cast_possible_truncation)]
    let fraction = ((hz / SPECTROGRAM_MIN_HZ).log10() / span).clamp(0.0, 1.0) as f32;
    // Low frequencies at the bottom.
    plot.bottom() - fraction * plot.height()
}

/// Spacings the time ruler is allowed to tick at, in seconds.
const TICK_STEPS: [f32; 8] = [0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 20.0];

/// Most intervals the ruler draws before it reaches for a coarser step.
const MAX_TICKS: f32 = 8.0;

/// Pick a readable tick spacing for a `duration`-second axis.
///
/// The window ranges over a factor of 64 between the shortest and longest
/// render, so a fixed 0.5 s tick would draw four marks on the shortest and
/// sixty-four on the longest.
fn nice_step(duration: f32) -> f32 {
    let last = TICK_STEPS[TICK_STEPS.len() - 1];
    if !duration.is_finite() || duration <= 0.0 {
        return TICK_STEPS[0];
    }
    TICK_STEPS
        .into_iter()
        .find(|&step| duration / step <= MAX_TICKS)
        .unwrap_or(last)
}

/// Time markers along the bottom of a panel, spanning `duration` seconds.
fn time_ruler(painter: &egui::Painter, plot: Rect, duration: f32) {
    if !duration.is_finite() || duration <= 0.0 {
        return;
    }
    let step = nice_step(duration);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let ticks = (duration / step) as usize;

    for index in 1..=ticks {
        #[allow(clippy::cast_precision_loss)]
        let time = index as f32 * step;
        let x = plot.left() + (time / duration) * plot.width();
        painter.line_segment(
            [
                Pos2::new(x, plot.bottom() - 4.0),
                Pos2::new(x, plot.bottom()),
            ],
            Stroke::new(1.0, GRID_COLOR),
        );
        // Sub-second steps need a decimal; whole-second ones read better
        // without one.
        let label = if step < 1.0 {
            format!("{time:.1}s")
        } else {
            format!("{time:.0}s")
        };
        painter.text(
            Pos2::new(x - 2.0, plot.bottom() - 4.0),
            Align2::RIGHT_BOTTOM,
            label,
            FontId::proportional(9.0),
            TEXT_DIM,
        );
    }
}

/// Turn a render's spectrogram into an RGB image, lowest frequency at the
/// bottom (image rows run top-down, the spectrogram's run bottom-up).
fn spectrogram_image(render: &IrRender) -> ColorImage {
    let spectrogram = &render.spectrogram;
    if spectrogram.rows == 0 || spectrogram.columns == 0 {
        return ColorImage::from_rgb([1, 1], &[0, 0, 0]);
    }

    let mut pixels = Vec::with_capacity(spectrogram.rows * spectrogram.columns * 3);
    for row in (0..spectrogram.rows).rev() {
        for column in 0..spectrogram.columns {
            let [r, g, b] = heat(spectrogram.get(row, column));
            pixels.extend_from_slice(&[r, g, b]);
        }
    }
    ColorImage::from_rgb([spectrogram.columns, spectrogram.rows], &pixels)
}

/// Map a level in dB onto the display's colour ramp.
fn heat(db: f32) -> [u8; 3] {
    // Anchors from silence to peak: near-black, deep blue, cyan, amber, white.
    const STOPS: [(f32, [f32; 3]); 5] = [
        (0.00, [14.0, 15.0, 22.0]),
        (0.35, [32.0, 62.0, 140.0]),
        (0.62, [62.0, 165.0, 190.0]),
        (0.84, [226.0, 186.0, 88.0]),
        (1.00, [255.0, 246.0, 224.0]),
    ];

    let floor = FLOOR_DB.max(DISPLAY_FLOOR_DB);
    let t = ((db - floor) / -floor).clamp(0.0, 1.0);

    let mut color = STOPS[STOPS.len() - 1].1;
    for pair in STOPS.windows(2) {
        let (low_t, low) = pair[0];
        let (high_t, high) = pair[1];
        if t <= high_t {
            let span = (high_t - low_t).max(f32::EPSILON);
            let blend = (t - low_t) / span;
            color = [
                low[0] + blend * (high[0] - low[0]),
                low[1] + blend * (high[1] - low[1]),
                low[2] + blend * (high[2] - low[2]),
            ];
            break;
        }
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    {
        [
            color[0].clamp(0.0, 255.0) as u8,
            color[1].clamp(0.0, 255.0) as u8,
            color[2].clamp(0.0, 255.0) as u8,
        ]
    }
}

/// The three control rows.
fn controls(ui: &mut egui::Ui, state: &PluginContext<KiryaParams>) {
    row(ui, 6, 0, |ui| {
        knobs(
            ui,
            state,
            &[
                (P::Dry, "Dry"),
                (P::Wet, "Wet"),
                (P::PreDelay, "Pre-Dly"),
                (P::Size, "Size"),
                (P::Diffusion, "Diffuse"),
                (P::Decay, "Decay"),
            ],
        );
    });
    row(ui, 4, 0, |ui| {
        knobs(
            ui,
            state,
            &[
                (P::InputLowCut, "In Lo"),
                (P::InputHighCut, "In Hi"),
                (P::ReverbLowCut, "Rev Lo"),
                (P::ReverbHighCut, "Rev Hi"),
            ],
        );
    });
    row(ui, 3, 1, |ui| {
        knobs(
            ui,
            state,
            &[
                (P::ModRate, "Rate"),
                (P::ModDepth, "Depth"),
                (P::ModShape, "Shape"),
            ],
        );
        ui.add_space(GAP);
        param_toggle(ui, state, P::Freeze, "FREEZE");
    });
}

/// Lay out one centred row of `knob_count` knobs and `toggle_count` toggles.
///
/// Zero the auto item-spacing so the gaps are exactly `GAP` — otherwise egui
/// inflates the row past the width computed here and the last widget loses its
/// right margin — then a single leading pad centres it, because the width left
/// on the right then equals that pad.
fn row(
    ui: &mut egui::Ui,
    knob_count: usize,
    toggle_count: usize,
    contents: impl FnOnce(&mut egui::Ui),
) {
    #[allow(clippy::cast_precision_loss)]
    let width = knob_count as f32 * KNOB_WIDTH
        + toggle_count as f32 * TOGGLE_WIDTH
        + (knob_count + toggle_count).saturating_sub(1) as f32 * GAP;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let lead = ((ui.available_width() - width) / 2.0).max(0.0);
        ui.add_space(lead);
        contents(ui);
    });
}

/// Add knobs separated by `GAP`, with no trailing gap.
fn knobs(ui: &mut egui::Ui, state: &PluginContext<KiryaParams>, entries: &[(P, &str)]) {
    for (index, (id, label)) in entries.iter().enumerate() {
        if index > 0 {
            ui.add_space(GAP);
        }
        param_knob(ui, state, *id, label);
    }
}

pub(crate) fn create(params: Arc<KiryaParams>) -> Box<dyn Editor> {
    EguiEditor::with_ui(params, EDITOR_SIZE, KiryaEditor::new())
        .with_visuals(truce_egui::theme::dark())
        .resizable(false)
        .into_editor()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kirya_dsp::analysis::Analyzer;

    #[test]
    fn the_colour_ramp_runs_dark_to_bright_without_gaps() {
        let floor = heat(DISPLAY_FLOOR_DB);
        let peak = heat(0.0);
        assert!(floor.iter().all(|&c| c < 40), "floor was {floor:?}");
        assert!(peak.iter().all(|&c| c > 200), "peak was {peak:?}");

        // Brightness has to rise monotonically or the display would read as
        // louder in places that are quieter.
        let luma = |c: [u8; 3]| u32::from(c[0]) * 2 + u32::from(c[1]) * 3 + u32::from(c[2]);
        let mut previous = 0;
        for step in 0..=70 {
            #[allow(clippy::cast_precision_loss)]
            let value = luma(heat(-(step as f32)));
            assert!(value <= previous || previous == 0, "step {step}");
            previous = value;
        }

        // Anything below the display floor pins to the floor colour.
        assert_eq!(heat(-200.0), floor);
    }

    #[test]
    fn levels_and_frequencies_map_onto_the_plot_the_right_way_up() {
        let plot = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(100.0, 100.0));

        // 0 dB at the top, the floor at the bottom.
        assert!((level_y(0.0, plot) - 0.0).abs() < 1.0e-3);
        assert!((level_y(DISPLAY_FLOOR_DB, plot) - 100.0).abs() < 1.0e-3);
        assert!(level_y(-20.0, plot) < level_y(-40.0, plot));
        assert!((level_y(-500.0, plot) - 100.0).abs() < 1.0e-3);

        // 20 Hz at the bottom, 20 kHz at the top.
        assert!((frequency_y(20.0, plot) - 100.0).abs() < 1.0e-3);
        assert!((frequency_y(20_000.0, plot) - 0.0).abs() < 1.0e-3);
        assert!(frequency_y(10_000.0, plot) < frequency_y(100.0, plot));
        // A decade is a third of the three-decade axis.
        let decade = frequency_y(100.0, plot) - frequency_y(1_000.0, plot);
        assert!((decade - 100.0 / 3.0).abs() < 0.1, "{decade}");
    }

    #[test]
    fn a_render_becomes_an_image_with_one_pixel_per_cell() {
        let params = KiryaParams::default();
        RenderIr {
            sample_rate: 48_000.0,
        }
        .run(&params);
        let render = params.ir_slot.lock().unwrap().take().unwrap();

        let image = spectrogram_image(&render);
        assert_eq!(
            image.size,
            [render.spectrogram.columns, render.spectrogram.rows]
        );

        // Row 0 of the image is the *top* of the display, so it must hold the
        // spectrogram's highest-frequency row.
        let top = image.pixels[0];
        let expected = heat(render.spectrogram.get(render.spectrogram.rows - 1, 0));
        assert_eq!([top.r(), top.g(), top.b()], expected);
    }

    #[test]
    fn an_empty_render_still_produces_a_valid_image() {
        let mut analyzer = Analyzer::new();
        // Far shorter than one transform, so the spectrogram comes back empty.
        let render = Box::new(analyzer.analyse(&[0.0; 16], &[0.0; 16], 48_000.0));
        let image = spectrogram_image(&render);
        assert_eq!(image.size, [1, 1]);
    }

    #[test]
    fn the_render_key_ignores_parameters_the_probe_does_not_use() {
        let params = KiryaParams::default();
        let before = RenderKey::read(&params, 48_000.0);

        // Wet-only with Freeze forced off: none of these change the picture.
        params.dry.set_value(0.0);
        params.wet.set_value(0.25);
        params.freeze.set_value(true);
        assert_eq!(before, RenderKey::read(&params, 48_000.0));

        // Anything that shapes the tail must invalidate it.
        params.decay.set_value(0.9);
        assert_ne!(before, RenderKey::read(&params, 48_000.0));

        // ...as must the host changing rate underneath us.
        let moved = RenderKey::read(&params, 48_000.0);
        assert_ne!(moved, RenderKey::read(&params, 96_000.0));
    }

    #[test]
    fn the_time_ruler_stays_readable_across_every_window_length() {
        // The window spans a factor of 64, so a fixed tick spacing would draw
        // four marks on the shortest render and sixty-four on the longest.
        for &window in &IR_WINDOW_STOPS {
            #[allow(clippy::cast_possible_truncation)]
            let duration = window as f32;
            let step = nice_step(duration);
            let ticks = duration / step;
            assert!(
                (2.0..=MAX_TICKS).contains(&ticks),
                "{duration} s window drew {ticks} ticks at a {step} s step"
            );
            assert!(TICK_STEPS.contains(&step), "{step} is not a listed step");
        }

        // Degenerate durations must not divide by zero or hang the ruler.
        for duration in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert!(nice_step(duration) > 0.0, "{duration}");
        }
    }

    #[test]
    fn the_axis_falls_back_to_the_shortest_window_before_the_first_render() {
        let editor = KiryaEditor::new();
        #[allow(clippy::cast_possible_truncation)]
        let shortest = IR_WINDOW_STOPS[0] as f32;
        assert_eq!(editor.duration(), shortest);
    }

    #[test]
    fn the_sample_rate_falls_back_until_the_host_activates_the_plugin() {
        let params = KiryaParams::default();
        assert_eq!(KiryaEditor::sample_rate(&params), 48_000.0);

        params
            .sample_rate_bits
            .store(96_000.0_f64.to_bits(), Ordering::Relaxed);
        assert_eq!(KiryaEditor::sample_rate(&params), 96_000.0);

        // A garbage publish must not divide the display by zero.
        params
            .sample_rate_bits
            .store(f64::NAN.to_bits(), Ordering::Relaxed);
        assert_eq!(KiryaEditor::sample_rate(&params), 48_000.0);
    }
}
