use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use egui::{Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use truce::prelude::*;
use truce_egui::theme::{BACKGROUND, HEADER_BG, METER_CLIP, TEXT, TEXT_DIM};
use truce_egui::widgets::{param_dropdown, param_knob};
use truce_egui::{EditorUi, EguiEditor};

use crate::{BogdanParams, BogdanParamsParamId as P};

const EDITOR_SIZE: (u32, u32) = (520, 320);
const VIEW_SAMPLES: usize = 1_024;
const HISTORY_SAMPLES: usize = 8_192;
const SCOPE_HEIGHT: f32 = 184.0;
const REPAINT_INTERVAL: Duration = Duration::from_millis(16);

const DRIVEN_COLOR: Color32 = Color32::from_rgb(241, 174, 74);
const PROCESSED_COLOR: Color32 = Color32::from_rgb(71, 207, 218);
const GRID_COLOR: Color32 = Color32::from_rgb(66, 67, 78);

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct ScopeFrame {
    driven: f32,
    processed: f32,
}

struct ScopeHistory {
    frames: VecDeque<ScopeFrame>,
}

impl ScopeHistory {
    fn new() -> Self {
        Self {
            frames: VecDeque::with_capacity(HISTORY_SAMPLES),
        }
    }

    fn clear(&mut self) {
        self.frames.clear();
    }

    fn ingest_interleaved(&mut self, interleaved: &[f32]) {
        for pair in interleaved.chunks_exact(2) {
            if self.frames.len() == HISTORY_SAMPLES {
                let _ = self.frames.pop_front();
            }
            self.frames.push_back(ScopeFrame {
                driven: finite_sample(pair[0]),
                processed: finite_sample(pair[1]),
            });
        }
    }

    fn display_range(&self) -> std::ops::Range<usize> {
        let len = self.frames.len();
        if len <= VIEW_SAMPLES {
            return 0..len;
        }

        let latest_start = len - VIEW_SAMPLES;
        let start = (1..=latest_start)
            .rev()
            .find(|&index| self.frames[index - 1].driven <= 0.0 && self.frames[index].driven > 0.0)
            .unwrap_or(latest_start);
        start..start + VIEW_SAMPLES
    }
}

pub(crate) struct BogdanEditor {
    history: ScopeHistory,
}

impl BogdanEditor {
    fn new() -> Self {
        Self {
            history: ScopeHistory::new(),
        }
    }

    fn drain_scope(&mut self, state: &PluginContext<BogdanParams>) {
        state
            .scope_tap
            .drain_with(|samples| self.history.ingest_interleaved(samples));
    }

    fn draw_scope(&self, ui: &mut egui::Ui, ceiling: f32) {
        let width = (ui.available_width() - 24.0).max(1.0);
        let (rect, _) = ui.allocate_exact_size(Vec2::new(width, SCOPE_HEIGHT), Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 6.0, HEADER_BG);
        painter.rect_stroke(
            rect,
            6.0,
            Stroke::new(1.0, GRID_COLOR),
            egui::StrokeKind::Inside,
        );

        let plot = rect.shrink2(Vec2::new(10.0, 12.0));
        painter.line_segment(
            [
                Pos2::new(plot.left(), plot.center().y),
                Pos2::new(plot.right(), plot.center().y),
            ],
            Stroke::new(1.0, GRID_COLOR),
        );

        let safe_ceiling = finite_sample(ceiling).abs().max(f32::MIN_POSITIVE);
        let vertical_extent = safe_ceiling * 2.0;
        for polarity in [-1.0_f32, 1.0] {
            let y = sample_y(polarity * safe_ceiling, vertical_extent, plot);
            painter.line_segment(
                [Pos2::new(plot.left(), y), Pos2::new(plot.right(), y)],
                Stroke::new(1.0, METER_CLIP),
            );
        }

        let range = self.history.display_range();
        if !range.is_empty() {
            let mut driven = Vec::with_capacity(range.len());
            let mut processed = Vec::with_capacity(range.len());
            for (display_index, history_index) in range.enumerate() {
                let x = sample_x(display_index, plot);
                let frame = self.history.frames[history_index];
                driven.push(Pos2::new(x, sample_y(frame.driven, vertical_extent, plot)));
                processed.push(Pos2::new(
                    x,
                    sample_y(frame.processed, vertical_extent, plot),
                ));
            }
            if driven.len() > 1 {
                painter.add(egui::Shape::line(driven, Stroke::new(1.2, DRIVEN_COLOR)));
                painter.add(egui::Shape::line(
                    processed,
                    Stroke::new(1.6, PROCESSED_COLOR),
                ));
            }
        }

        draw_legend(&painter, plot);
    }
}

impl EditorUi<BogdanParams> for BogdanEditor {
    fn opened(&mut self, state: &PluginContext<BogdanParams>) {
        self.history.clear();
        state.scope_tap.clear();
    }

    fn ui(&mut self, ui: &mut egui::Ui, state: &PluginContext<BogdanParams>) {
        ui.ctx().request_repaint_after(REPAINT_INTERVAL);
        self.drain_scope(state);

        ui.painter().rect_filled(ui.max_rect(), 0.0, BACKGROUND);
        ui.vertical_centered(|ui| {
            ui.add_space(5.0);
            ui.label(RichText::new("BOGDAN").strong().size(16.0).color(TEXT));
            ui.label(
                RichText::new("detail-preserving clipper")
                    .size(10.0)
                    .color(TEXT_DIM),
            );
            ui.add_space(5.0);

            let ceiling = db_to_linear(state.get_param_plain(P::Ceiling));
            self.draw_scope(ui, ceiling);

            ui.add_space(5.0);
            ui.horizontal_centered(|ui| {
                // The dropdown's label baseline aligns with the knob labels, so
                // Mode sits in the same control row rather than above it.
                param_dropdown(ui, state, P::Mode, "Mode", 1);
                ui.add_space(12.0);
                param_knob(ui, state, P::Drive, "Drive");
                ui.add_space(12.0);
                param_knob(ui, state, P::Ceiling, "Ceiling");
                ui.add_space(12.0);
                param_knob(ui, state, P::Detail, "Detail");
                ui.add_space(12.0);
                param_knob(ui, state, P::Amount, "Amount");
            });
        });
    }
}

pub(crate) fn create(params: Arc<BogdanParams>) -> Box<dyn Editor> {
    EguiEditor::with_ui(params, EDITOR_SIZE, BogdanEditor::new())
        .with_visuals(truce_egui::theme::dark())
        .resizable(false)
        .into_editor()
}

fn finite_sample(sample: f32) -> f32 {
    if sample.is_finite() { sample } else { 0.0 }
}

#[allow(clippy::cast_precision_loss)]
fn sample_x(index: usize, plot: Rect) -> f32 {
    let fraction = index as f32 / (VIEW_SAMPLES - 1) as f32;
    plot.left() + fraction * plot.width()
}

fn sample_y(sample: f32, vertical_extent: f32, plot: Rect) -> f32 {
    let normalized = (finite_sample(sample) / vertical_extent).clamp(-1.0, 1.0);
    plot.center().y - normalized * plot.height() * 0.5
}

fn draw_legend(painter: &egui::Painter, plot: Rect) {
    let top = plot.top() + 2.0;
    let driven_left = plot.left() + 4.0;
    painter.line_segment(
        [
            Pos2::new(driven_left, top + 5.0),
            Pos2::new(driven_left + 14.0, top + 5.0),
        ],
        Stroke::new(1.5, DRIVEN_COLOR),
    );
    painter.text(
        Pos2::new(driven_left + 19.0, top),
        Align2::LEFT_TOP,
        "DRIVEN",
        FontId::proportional(9.0),
        DRIVEN_COLOR,
    );

    let processed_left = driven_left + 76.0;
    painter.line_segment(
        [
            Pos2::new(processed_left, top + 5.0),
            Pos2::new(processed_left + 14.0, top + 5.0),
        ],
        Stroke::new(1.5, PROCESSED_COLOR),
    );
    painter.text(
        Pos2::new(processed_left + 19.0, top),
        Align2::LEFT_TOP,
        "PROCESSED",
        FontId::proportional(9.0),
        PROCESSED_COLOR,
    );

    painter.text(
        Pos2::new(plot.right() - 4.0, top),
        Align2::RIGHT_TOP,
        "CEILING",
        FontId::proportional(9.0),
        METER_CLIP,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ingests_interleaved_frames_and_sanitizes_non_finite_values() {
        let mut history = ScopeHistory::new();
        history.ingest_interleaved(&[1.0, 0.5, f32::NAN, f32::INFINITY, 99.0]);

        assert_eq!(
            history.frames,
            VecDeque::from([
                ScopeFrame {
                    driven: 1.0,
                    processed: 0.5,
                },
                ScopeFrame {
                    driven: 0.0,
                    processed: 0.0,
                },
            ])
        );
    }

    #[test]
    fn circular_history_retains_only_the_newest_frames() {
        let mut history = ScopeHistory::new();
        let samples: Vec<f32> = (0..HISTORY_SAMPLES + 3)
            .flat_map(|index| [index as f32, -(index as f32)])
            .collect();
        history.ingest_interleaved(&samples);

        assert_eq!(history.frames.len(), HISTORY_SAMPLES);
        assert_eq!(history.frames.front().unwrap().driven, 3.0);
        assert_eq!(
            history.frames.back().unwrap().driven,
            (HISTORY_SAMPLES + 2) as f32
        );
    }

    #[test]
    fn display_uses_newest_trigger_with_a_complete_view() {
        let mut history = ScopeHistory::new();
        for index in 0..VIEW_SAMPLES + 20 {
            let driven = if index == 8 || index == 20 { 1.0 } else { -1.0 };
            history.frames.push_back(ScopeFrame {
                driven,
                processed: 0.0,
            });
        }

        assert_eq!(history.display_range(), 20..20 + VIEW_SAMPLES);
    }

    #[test]
    fn display_falls_back_to_newest_complete_view_without_a_trigger() {
        let mut history = ScopeHistory::new();
        history.frames.resize(
            VIEW_SAMPLES + 12,
            ScopeFrame {
                driven: 1.0,
                processed: 0.0,
            },
        );

        assert_eq!(history.display_range(), 12..12 + VIEW_SAMPLES);
    }

    #[test]
    fn display_uses_all_available_frames_while_history_is_short() {
        let mut history = ScopeHistory::new();
        history.frames.resize(32, ScopeFrame::default());

        assert_eq!(history.display_range(), 0..32);
    }

    #[test]
    fn ceiling_guides_land_halfway_between_center_and_edge() {
        let plot = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(100.0, 100.0));

        assert_eq!(sample_y(0.5, 1.0, plot), 25.0);
        assert_eq!(sample_y(-0.5, 1.0, plot), 75.0);
    }
}
