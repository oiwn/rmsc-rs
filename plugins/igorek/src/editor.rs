//! Egui editor: two stacked IR panes with selection and envelope dragging,
//! a native-file-dialog Color loader, and the live crossfader knobs.
//!
//! Drag'n'drop is impossible in truce-egui 6.3 (the baseview-truce fork has
//! no file-drop window events), so the Load button is the interface — see
//! the gotcha in `specs/igorek.md`.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, PoisonError, TryLockError};
use std::time::Duration;

use egui::{Color32, Pos2, Rect, Response, Sense, Shape, Stroke, Ui, Vec2};
use truce::prelude::*;
use truce_egui::theme;
use truce_egui::widgets::param_knob;
use truce_egui::{EditorUi, EguiEditor};

use igorek_dsp::bake::{ENVELOPE_FLOOR_DB, IrView};
use igorek_dsp::wav::{LoadedIr, load_color_ir};

use crate::task::{BakeColor, BakeRoom};
use crate::{IgorekParams, IgorekParamsParamId as P};

const EDITOR_SIZE: (u32, u32) = (780, 820);
const DEBOUNCE: Duration = Duration::from_millis(150);
const REPAINT_INTERVAL: Duration = Duration::from_millis(33);
const PANE_HEIGHT: f32 = 190.0;
const HANDLE_RADIUS: f32 = 6.0;

const WAVE_LEFT: Color32 = Color32::from_rgb(0x7E, 0xD3, 0xC0);
const WAVE_RIGHT: Color32 = Color32::from_rgb(0x4E, 0x8F, 0x82);
const ENVELOPE_LINE: Color32 = Color32::from_rgb(0xFF, 0xB7, 0x4D);
const HANDLE_FILL: Color32 = Color32::from_rgb(0xE8, 0xE3, 0xD9);

fn selection_fill() -> Color32 {
    Color32::from_white_alpha(18)
}

fn selection_edge() -> Color32 {
    Color32::from_white_alpha(120)
}

/// Which handle a drag has grabbed.
#[derive(Clone, Copy, PartialEq)]
enum Handle {
    SelectionStart,
    SelectionEnd,
    EnvA,
    EnvB,
    EnvC,
}

/// An in-flight drag: which pane and which handle.
#[derive(Clone, Copy)]
struct ActiveDrag {
    room: bool,
    handle: Handle,
}

/// Outcome of a Color file load, produced on the decode helper thread.
enum LoadOutcome {
    Loaded(LoadedIr),
    Rejected(String),
}

/// Where a Color load stands: the open panel, or the decode running on the
/// helper thread.
///
/// The panel is rfd's *async* dialog, polled here every frame. Its sync
/// sibling spins a nested `NSApp` run loop — which, from inside baseview's
/// draw callback, re-enters rendering and crashes the host. The async path
/// attaches a sheet with a completion block instead, so creating it inside
/// `ui()` is safe and the result shows up on a later poll.
enum LoadState {
    Dialog(std::pin::Pin<Box<dyn std::future::Future<Output = Option<rfd::FileHandle>> + Send>>),
    Decoding(Arc<Mutex<Option<LoadOutcome>>>),
}

/// Change-detection key over the parameters that re-bake the Color stage.
#[derive(Clone, Copy, PartialEq)]
struct ColorKey {
    sel_start: f32,
    sel_length: f32,
    env_a: f32,
    env_b_x: f32,
    env_b: f32,
    env_c: f32,
    sample_rate: f64,
}

/// Change-detection key over the parameters that regenerate and bake the
/// room.
#[derive(Clone, Copy, PartialEq)]
struct RoomKey {
    rt60: f32,
    edt: f32,
    itdg: f32,
    er_duration: f32,
    variant: i64,
    width: f32,
    sel_start: f32,
    sel_length: f32,
    env_a: f32,
    env_b_x: f32,
    env_b: f32,
    env_c: f32,
    sample_rate: f64,
}

pub(crate) struct IgorekEditor {
    color_view: Option<Box<IrView>>,
    color_generation: u64,
    room_view: Option<Box<IrView>>,
    room_generation: u64,
    color_requested: Option<ColorKey>,
    color_changed_at: Option<std::time::Instant>,
    room_requested: Option<RoomKey>,
    room_changed_at: Option<std::time::Instant>,
    load: Option<LoadState>,
    drag: Option<ActiveDrag>,
    notice: Option<String>,
}

impl IgorekEditor {
    pub(crate) fn new() -> Self {
        Self {
            color_view: None,
            color_generation: 0,
            room_view: None,
            room_generation: 0,
            color_requested: None,
            color_changed_at: None,
            room_requested: None,
            room_changed_at: None,
            load: None,
            drag: None,
            notice: None,
        }
    }

    fn sample_rate(params: &IgorekParams) -> f64 {
        f64::from_bits(params.sample_rate_bits.load(Ordering::Relaxed))
    }
}

/// Linear plain→normalized for the ranges the drags write (all linear).
fn normalized(value: f64, min: f64, max: f64) -> f64 {
    ((value - min) / (max - min)).clamp(0.0, 1.0)
}

pub(crate) fn create(params: Arc<IgorekParams>) -> Box<dyn Editor> {
    EguiEditor::with_ui(params, EDITOR_SIZE, IgorekEditor::new())
        .with_visuals(theme::dark())
        .resizable(false)
        .into_editor()
}

impl EditorUi<IgorekParams> for IgorekEditor {
    fn ui(&mut self, ui: &mut Ui, state: &PluginContext<IgorekParams>) {
        ui.ctx().request_repaint_after(REPAINT_INTERVAL);
        self.poll_load(ui, state);
        self.maybe_request(ui, state);
        self.collect_views(state);
        if let Some(notice) = state
            .notice
            .try_lock()
            .ok()
            .and_then(|mut slot| slot.take())
        {
            self.notice = Some(notice);
        }

        let knobs = [
            (P::Dry, "Dry"),
            (P::Wet, "Wet"),
            (P::ColorMix, "Color Mix"),
            (P::RoomMix, "Room Mix"),
        ];
        ui.horizontal(|ui| {
            for (id, label) in knobs {
                param_knob(ui, state, id, label);
            }
        });

        self.color_pane(ui, state);
        self.room_pane(ui, state);

        ui.horizontal(|ui| {
            for (id, label) in [
                (P::RoomRt60, "RT60"),
                (P::RoomEdt, "EDT"),
                (P::RoomItdg, "ITDG"),
                (P::RoomErDuration, "ER Dur"),
                (P::RoomVariant, "Variant"),
                (P::RoomWidth, "Width"),
            ] {
                param_knob(ui, state, id, label);
            }
        });

        if let Some(notice) = &self.notice {
            ui.colored_label(
                Color32::from_rgb(0xFF, 0xB7, 0x4D),
                format!("note: {notice}"),
            );
        }
    }

    fn state_changed(&mut self, state: &PluginContext<IgorekParams>) {
        // A preset or session recall can change both the room parameters
        // and the stored Color IR; refresh everything.
        if let Some(spawner) = state.tasks::<BakeRoom>() {
            spawner.spawn_coalescing(BakeRoom {
                sample_rate: Self::sample_rate(state.params()),
            });
        }
        if let Some(spawner) = state.tasks::<BakeColor>() {
            spawner.spawn_coalescing(BakeColor {
                sample_rate: Self::sample_rate(state.params()),
            });
        }
    }
}

impl IgorekEditor {
    // --- Debounced bake requests (kirya's RenderKey pattern). ---

    fn maybe_request(&mut self, ui: &Ui, state: &PluginContext<IgorekParams>) {
        let rate = Self::sample_rate(state.params());
        let params = state.params();

        let color_key = ColorKey {
            sel_start: params.color_sel_start.value(),
            sel_length: params.color_sel_length.value(),
            env_a: params.color_env_a.value(),
            env_b_x: params.color_env_b_x.value(),
            env_b: params.color_env_b.value(),
            env_c: params.color_env_c.value(),
            sample_rate: rate,
        };
        if self.color_requested != Some(color_key) {
            self.color_requested = Some(color_key);
            self.color_changed_at = Some(std::time::Instant::now());
        }
        if let Some(changed_at) = self.color_changed_at {
            if changed_at.elapsed() < DEBOUNCE {
                ui.ctx().request_repaint_after(DEBOUNCE);
            } else if let Some(spawner) = state.tasks::<BakeColor>() {
                spawner.spawn_coalescing(BakeColor { sample_rate: rate });
                self.color_changed_at = None;
            }
        }

        let room_key = RoomKey {
            rt60: params.room_rt60.value(),
            edt: params.room_edt.value(),
            itdg: params.room_itdg.value(),
            er_duration: params.room_er_duration.value(),
            variant: params.room_variant.value(),
            width: params.room_width.value(),
            sel_start: params.room_sel_start.value(),
            sel_length: params.room_sel_length.value(),
            env_a: params.room_env_a.value(),
            env_b_x: params.room_env_b_x.value(),
            env_b: params.room_env_b.value(),
            env_c: params.room_env_c.value(),
            sample_rate: rate,
        };
        if self.room_requested != Some(room_key) {
            self.room_requested = Some(room_key);
            self.room_changed_at = Some(std::time::Instant::now());
        }
        if let Some(changed_at) = self.room_changed_at {
            if changed_at.elapsed() < DEBOUNCE {
                ui.ctx().request_repaint_after(DEBOUNCE);
            } else if let Some(spawner) = state.tasks::<BakeRoom>() {
                spawner.spawn_coalescing(BakeRoom { sample_rate: rate });
                self.room_changed_at = None;
            }
        }
    }

    // --- Collect finished views from the bake tasks. ---

    fn collect_views(&mut self, state: &PluginContext<IgorekParams>) {
        let params = state.params();
        let color_generation = params.color_view_generation.load(Ordering::Acquire);
        if color_generation != self.color_generation
            && let Ok(mut slot) = params.color_view.try_lock()
            && let Some(view) = slot.take()
        {
            self.color_view = Some(view);
            self.color_generation = color_generation;
        }
        let room_generation = params.room_view_generation.load(Ordering::Acquire);
        if room_generation != self.room_generation
            && let Ok(mut slot) = params.room_view.try_lock()
            && let Some(view) = slot.take()
        {
            self.room_view = Some(view);
            self.room_generation = room_generation;
        }
    }

    // --- Color WAV loading. ---

    fn load_button(&mut self, ui: &mut Ui, _state: &PluginContext<IgorekParams>) {
        let busy = self.load.is_some();
        let button = egui::Button::new(if busy { "Loading…" } else { "Load WAV…" });
        if ui.add_enabled(!busy, button).clicked() {
            // The async dialog attaches a sheet and returns immediately —
            // creating it here, inside baseview's draw callback, is safe.
            // Its sync sibling spins a nested NSApp run loop and crashes
            // hosts from this context; see LoadState.
            let request = rfd::AsyncFileDialog::new()
                .add_filter("WAV audio", &["wav", "wave"])
                .pick_file();
            self.load = Some(LoadState::Dialog(Box::pin(request)));
        }
    }

    fn spawn_decode(&mut self, path: std::path::PathBuf, rate: f64) {
        let slot: Arc<Mutex<Option<LoadOutcome>>> = Arc::new(Mutex::new(None));
        self.load = Some(LoadState::Decoding(Arc::clone(&slot)));
        std::thread::spawn(move || {
            let outcome = match std::fs::File::open(&path) {
                Ok(file) => match load_color_ir(file, rate) {
                    Ok(ir) => LoadOutcome::Loaded(ir),
                    Err(e) => LoadOutcome::Rejected(e.to_string()),
                },
                Err(e) => LoadOutcome::Rejected(format!("{}: {e}", path.display())),
            };
            let mut guard = slot.lock().unwrap_or_else(PoisonError::into_inner);
            *guard = Some(outcome);
        });
    }

    fn poll_load(&mut self, _ui: &Ui, state: &PluginContext<IgorekParams>) {
        match self.load.take() {
            Some(LoadState::Dialog(mut dialog)) => {
                // A no-op waker is fine: the editor repaints every 33 ms
                // anyway, so a completed dialog is picked up on the next
                // frame.
                let waker = std::task::Waker::noop();
                let mut cx = std::task::Context::from_waker(waker);
                match dialog.as_mut().poll(&mut cx) {
                    std::task::Poll::Ready(Some(handle)) => {
                        let rate = Self::sample_rate(state.params());
                        self.spawn_decode(handle.path().to_path_buf(), rate);
                    }
                    // Cancelled by the user.
                    std::task::Poll::Ready(None) => self.load = None,
                    std::task::Poll::Pending => self.load = Some(LoadState::Dialog(dialog)),
                }
            }
            Some(LoadState::Decoding(slot)) => {
                // The try_lock guard must be gone before `slot` can be
                // restored, so decide first and move after the match.
                let mut outcome = None;
                let still_busy = match slot.try_lock() {
                    Ok(mut guard) => {
                        outcome = guard.take();
                        outcome.is_none()
                    }
                    Err(TryLockError::Poisoned(mut poisoned)) => {
                        let guard = poisoned.get_mut();
                        outcome = guard.take();
                        outcome.is_none()
                    }
                    Err(TryLockError::WouldBlock) => true,
                };
                if still_busy {
                    self.load = Some(LoadState::Decoding(slot));
                    return;
                }
                self.load = None;
                self.deliver(outcome.expect("a finished decode"), state);
            }
            None => {}
        }
    }

    fn deliver(&mut self, outcome: LoadOutcome, state: &PluginContext<IgorekParams>) {
        match outcome {
            LoadOutcome::Loaded(ir) => {
                if ir.truncated {
                    self.notice = Some("file longer than 4 s: loaded truncated".to_string());
                } else {
                    self.notice = None;
                }
                let loaded = ir.left.len() > 1;
                let mut guard = state
                    .color_ir
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                *guard = Some(ir);
                drop(guard);
                if loaded && let Some(spawner) = state.tasks::<BakeColor>() {
                    spawner.spawn_coalescing(BakeColor {
                        sample_rate: Self::sample_rate(state.params()),
                    });
                }
            }
            LoadOutcome::Rejected(message) => {
                self.notice = Some(message);
            }
        }
    }

    // --- The two panes. ---

    fn color_pane(&mut self, ui: &mut Ui, state: &PluginContext<IgorekParams>) {
        ui.horizontal(|ui| {
            let loaded = state
                .color_ir
                .try_lock()
                .map(|slot| slot.is_some())
                .unwrap_or(false);
            ui.colored_label(
                theme::TEXT,
                if loaded {
                    "Color — WAV loaded"
                } else {
                    "Color — identity (no file loaded)"
                },
            );
            self.load_button(ui, state);
        });
        // Take the view out so the pane call borrows `self` mutably without
        // contention; restore it afterwards.
        let view = self.color_view.take();
        self.pane(
            ui,
            state,
            false,
            P::ColorSelStart,
            P::ColorSelLength,
            P::ColorEnvA,
            P::ColorEnvBX,
            P::ColorEnvB,
            P::ColorEnvC,
            view.as_deref(),
        );
        self.color_view = view;
    }

    fn room_pane(&mut self, ui: &mut Ui, state: &PluginContext<IgorekParams>) {
        ui.horizontal(|ui| {
            ui.colored_label(theme::TEXT, "Room — generated");
        });
        let view = self.room_view.take();
        self.pane(
            ui,
            state,
            true,
            P::RoomSelStart,
            P::RoomSelLength,
            P::RoomEnvA,
            P::RoomEnvBX,
            P::RoomEnvB,
            P::RoomEnvC,
            view.as_deref(),
        );
        self.room_view = view;
    }

    /// One IR pane: waveform of the shaped IR, selection shading, envelope
    /// polyline, and the drag handles for all of it.
    #[allow(clippy::too_many_arguments)]
    fn pane(
        &mut self,
        ui: &mut Ui,
        state: &PluginContext<IgorekParams>,
        room: bool,
        id_sel_start: P,
        id_sel_length: P,
        id_env_a: P,
        id_env_b_x: P,
        id_env_b: P,
        id_env_c: P,
        view: Option<&IrView>,
    ) {
        let (rect, response) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), PANE_HEIGHT), Sense::drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 3.0, theme::HEADER_BG);
        painter.rect_stroke(
            rect,
            3.0,
            Stroke::new(1.0, theme::BACKGROUND),
            egui::StrokeKind::Inside,
        );

        let sel_start = state.get_param_plain(id_sel_start).clamp(0.0, 0.95);
        let sel_length = state.get_param_plain(id_sel_length).clamp(0.05, 1.0);
        let env_a = state.get_param_plain(id_env_a);
        let env_b_x = state.get_param_plain(id_env_b_x).clamp(0.0, 1.0);
        let env_b = state.get_param_plain(id_env_b);
        let env_c = state.get_param_plain(id_env_c);

        let inner = rect.shrink(6.0);
        let width = inner.width();

        // Waveform of the shaped IR, both channels, dB to height.
        if let Some(view) = view {
            for (envelope, color) in [(&view.left_db, WAVE_LEFT), (&view.right_db, WAVE_RIGHT)] {
                let points: Vec<Pos2> = envelope
                    .iter()
                    .enumerate()
                    .map(|(i, db)| {
                        let x = inner.left() + (i as f32 / envelope.len() as f32) * width;
                        let y = db_to_y(*db, inner);
                        Pos2::new(x, y)
                    })
                    .collect();
                if points.len() > 1 {
                    painter.add(Shape::line(points, Stroke::new(1.2, color)));
                }
            }
        } else {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "waiting for first bake…",
                egui::FontId::default(),
                theme::TEXT_DIM,
            );
        }

        // Selection shading and edges.
        let sel_x0 = inner.left() + sel_start * width;
        let sel_x1 = inner.left() + (sel_start + sel_length) * width;
        painter.rect_filled(
            Rect::from_min_max(
                Pos2::new(sel_x0, inner.top()),
                Pos2::new(sel_x1, inner.bottom()),
            ),
            0.0,
            selection_fill(),
        );
        for x in [sel_x0, sel_x1] {
            painter.line_segment(
                [Pos2::new(x, inner.top()), Pos2::new(x, inner.bottom())],
                Stroke::new(1.0, selection_edge()),
            );
        }

        // Envelope polyline: A at selection start, B inside it, C at its
        // end. Y is its own dB scale (-60..+12).
        let b_x_abs = sel_x0 + env_b_x * (sel_x1 - sel_x0);
        let a_pos = Pos2::new(sel_x0, env_db_to_y(env_a, inner));
        let b_pos = Pos2::new(b_x_abs, env_db_to_y(env_b, inner));
        let c_pos = Pos2::new(sel_x1, env_db_to_y(env_c, inner));
        painter.add(Shape::line(
            vec![a_pos, b_pos, c_pos],
            Stroke::new(1.6, ENVELOPE_LINE),
        ));

        let handles = [
            (Handle::SelectionStart, Pos2::new(sel_x0, inner.center().y)),
            (Handle::SelectionEnd, Pos2::new(sel_x1, inner.center().y)),
            (Handle::EnvA, a_pos),
            (Handle::EnvB, b_pos),
            (Handle::EnvC, c_pos),
        ];
        for (index, (_, pos)) in handles.iter().enumerate() {
            let active = self
                .drag
                .is_some_and(|drag| drag.room == room && drag.handle == handles[index].0);
            painter.circle_filled(
                *pos,
                if active {
                    HANDLE_RADIUS + 2.0
                } else {
                    HANDLE_RADIUS
                },
                HANDLE_FILL,
            );
        }

        self.drag_interaction(
            ui, &response, &inner, room, state, handles, sel_start, sel_length,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn drag_interaction(
        &mut self,
        _ui: &Ui,
        response: &Response,
        inner: &Rect,
        room: bool,
        state: &PluginContext<IgorekParams>,
        handles: [(Handle, Pos2); 5],
        sel_start: f32,
        sel_length: f32,
    ) {
        let ids = if room {
            (
                P::RoomSelStart,
                P::RoomSelLength,
                P::RoomEnvA,
                P::RoomEnvBX,
                P::RoomEnvB,
                P::RoomEnvC,
            )
        } else {
            (
                P::ColorSelStart,
                P::ColorSelLength,
                P::ColorEnvA,
                P::ColorEnvBX,
                P::ColorEnvB,
                P::ColorEnvC,
            )
        };

        if response.drag_started()
            && let Some(pointer) = response.interact_pointer_pos()
        {
            let nearest = handles
                .iter()
                .map(|(handle, pos)| (pos.distance(pointer), *handle))
                .min_by(|a, b| a.0.total_cmp(&b.0));
            if let Some((distance, handle)) = nearest
                && distance <= 16.0
            {
                self.drag = Some(ActiveDrag { room, handle });
                let id = match handle {
                    Handle::SelectionStart => ids.0,
                    Handle::SelectionEnd => ids.1,
                    Handle::EnvA => ids.2,
                    Handle::EnvB => ids.4,
                    Handle::EnvC => ids.5,
                };
                if handle != Handle::EnvB {
                    state.begin_edit(id);
                } else {
                    // B drags both its x and its y; begin both.
                    state.begin_edit(ids.3);
                    state.begin_edit(ids.4);
                }
            }
        }

        if let (Some(drag), Some(pointer)) = (self.drag, response.interact_pointer_pos())
            && drag.room == room
            && response.dragged()
        {
            let width = inner.width();
            let frac = f64::from(((pointer.x - inner.left()) / width).clamp(0.0, 1.0));
            let db = f64::from(y_to_env_db(pointer.y, *inner));
            let sel_start = f64::from(sel_start);
            let sel_length = f64::from(sel_length);
            match drag.handle {
                Handle::SelectionStart => {
                    let mut start = frac.clamp(0.0, 0.95);
                    if start + sel_length > 1.0 {
                        start = 1.0 - sel_length;
                    }
                    state.set_param(ids.0, normalized(start, 0.0, 0.95));
                }
                Handle::SelectionEnd => {
                    let length = frac.clamp(sel_start + 0.05, 1.0) - sel_start;
                    state.set_param(ids.1, normalized(length, 0.05, 1.0));
                }
                Handle::EnvA => {
                    state.set_param(ids.2, normalized(db.clamp(-60.0, 12.0), -60.0, 12.0));
                }
                Handle::EnvB => {
                    let b_x = ((frac - sel_start) / sel_length).clamp(0.0, 1.0);
                    state.set_param(ids.3, normalized(b_x, 0.0, 1.0));
                    state.set_param(ids.4, normalized(db.clamp(-60.0, 12.0), -60.0, 12.0));
                }
                Handle::EnvC => {
                    state.set_param(ids.5, normalized(db.clamp(-60.0, 12.0), -60.0, 12.0));
                }
            }
        }

        if response.drag_stopped() {
            if let Some(drag) = self.drag.take()
                && drag.room == room
            {
                match drag.handle {
                    Handle::SelectionStart => state.end_edit(ids.0),
                    Handle::SelectionEnd => state.end_edit(ids.1),
                    Handle::EnvA => state.end_edit(ids.2),
                    Handle::EnvB => {
                        state.end_edit(ids.3);
                        state.end_edit(ids.4);
                    }
                    Handle::EnvC => state.end_edit(ids.5),
                }
            }
            self.drag = None;
        }
    }
}

/// Waveform dB (floor..0) to pane height.
fn db_to_y(db: f32, inner: Rect) -> f32 {
    let clamped = db.clamp(ENVELOPE_FLOOR_DB, 0.0);
    let frac = (clamped - ENVELOPE_FLOOR_DB) / -ENVELOPE_FLOOR_DB;
    inner.bottom() - frac * inner.height()
}

/// Envelope dB (-60..+12) to pane height.
fn env_db_to_y(db: f32, inner: Rect) -> f32 {
    let clamped = db.clamp(-60.0, 12.0);
    let frac = (12.0 - clamped) / 72.0;
    inner.top() + frac * inner.height()
}

/// Pane height back to envelope dB.
fn y_to_env_db(y: f32, inner: Rect) -> f32 {
    let frac = ((y - inner.top()) / inner.height()).clamp(0.0, 1.0);
    12.0 - frac * 72.0
}
