//! Selection and 3-point envelope baking: IR shaping before the FFT.
//!
//! Both controls are host parameters, but they are applied to the IR during
//! the background bake rather than at runtime — shaping costs nothing on the
//! audio thread, and edits land at the next spectra swap instead of
//! zippering. That is also why they take no smoothing: a smoothed value
//! would be invisible between bakes.

use crate::engine::ChannelSpectra;

/// Envelope display floor, in dB: anything quieter draws as silence.
pub const ENVELOPE_FLOOR_DB: f32 = -90.0;

/// Most envelope points the editor view carries.
const MAX_VIEW_POINTS: usize = 2_048;

/// The audible region of an IR, as fractions of its length: samples outside
/// `[start, start + length)` are zeroed. `start + length` is kept `<= 1`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Selection {
    /// First audible sample, `0.0..=0.95` of the IR length.
    pub start: f32,
    /// Audible span, `0.05..=1.0` of the IR length.
    pub length: f32,
}

impl Default for Selection {
    fn default() -> Self {
        Self {
            start: 0.0,
            length: 1.0,
        }
    }
}

impl Selection {
    /// Clamp to the parameter ranges and keep `start + length <= 1`,
    /// replacing non-finite values with the defaults.
    #[must_use]
    pub fn sanitized(self) -> Self {
        let start = if self.start.is_finite() {
            self.start
        } else {
            0.0
        }
        .clamp(0.0, 0.95);
        let length = if self.length.is_finite() {
            self.length
        } else {
            1.0
        }
        .clamp(0.05, 1.0);
        Self {
            start,
            length: length.min(1.0 - start).clamp(0.05, 1.0),
        }
    }

    /// Half-open sample range of the selection inside an IR of `len`
    /// samples. Always at least one sample wide.
    #[must_use]
    pub fn sample_range(self, len: usize) -> (usize, usize) {
        let self_ = self.sanitized();
        let start = (self_.start * len as f32).floor() as usize;
        let end = ((self_.start + self_.length) * len as f32).ceil() as usize;
        (start.min(len.saturating_sub(1)), end.clamp(start + 1, len))
    }
}

/// The 3-point envelope riding the selection: A at its start, B at a
/// draggable interior x, C at its end. Y values in dB; 0 dB everywhere is
/// the untouched IR.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Envelope3 {
    pub a_db: f32,
    /// Position of B inside the selection, `0..=1`.
    pub b_x: f32,
    pub b_db: f32,
    pub c_db: f32,
}

impl Default for Envelope3 {
    fn default() -> Self {
        Self {
            a_db: 0.0,
            b_x: 0.5,
            b_db: 0.0,
            c_db: 0.0,
        }
    }
}

impl Envelope3 {
    /// Clamp to the parameter ranges, replacing non-finite values with 0 dB.
    #[must_use]
    pub fn sanitized(self) -> Self {
        let clean = |v: f32| if v.is_finite() { v } else { 0.0 }.clamp(-60.0, 12.0);
        Self {
            a_db: clean(self.a_db),
            b_x: if self.b_x.is_finite() { self.b_x } else { 0.5 }.clamp(0.0, 1.0),
            b_db: clean(self.b_db),
            c_db: clean(self.c_db),
        }
    }

    /// Envelope gain in dB at relative position `x` (`0..=1`) inside the
    /// selection: linear in dB from A through B to C.
    #[must_use]
    fn gain_db(self, x: f32) -> f32 {
        let x = x.clamp(0.0, 1.0);
        let b_x = self.b_x.clamp(0.0, 1.0);
        if x <= b_x {
            if b_x > 0.0 {
                self.a_db + (self.b_db - self.a_db) * x / b_x
            } else {
                self.b_db
            }
        } else if b_x < 1.0 {
            self.b_db + (self.c_db - self.b_db) * (x - b_x) / (1.0 - b_x)
        } else {
            self.b_db
        }
    }
}

/// Apply selection and envelope to one IR, producing the shaped IR that gets
/// partitioned. Everything outside the selection is zeroed; inside it the
/// gain follows the A→B→C dB polyline.
#[must_use]
pub fn shape_ir(ir: &[f32], selection: Selection, envelope: Envelope3) -> Vec<f32> {
    let selection = selection.sanitized();
    let envelope = envelope.sanitized();
    let (start, end) = selection.sample_range(ir.len());
    let mut shaped = vec![0.0_f32; ir.len()];
    let span = (end - start) as f32;
    for (i, sample) in shaped.iter_mut().enumerate().skip(start).take(end - start) {
        let x = (i - start) as f32 / span;
        let db = envelope.gain_db(x);
        *sample = ir[i] * 10.0_f32.powf(db / 20.0);
    }
    shaped
}

/// Per-channel frequency-domain partitions for one stage, ready to install
/// into the processor wholesale.
pub struct StageInstall {
    pub left: Box<ChannelSpectra>,
    pub right: Box<ChannelSpectra>,
}

/// A downsampled dB envelope of the shaped IR, for the editor panes.
#[derive(Clone, Debug)]
pub struct IrView {
    pub sample_rate: f64,
    pub length_samples: usize,
    /// Block-peak envelopes in dB, sampled every `hop` samples.
    pub left_db: Vec<f32>,
    pub right_db: Vec<f32>,
    pub hop: usize,
}

impl IrView {
    fn new(left: &[f32], right: &[f32], sample_rate: f64) -> Self {
        let length = left.len();
        let hop = length.div_ceil(MAX_VIEW_POINTS).max(1);
        let envelope = |ir: &[f32]| -> Vec<f32> {
            ir.chunks(hop)
                .map(|block| {
                    let peak = block.iter().fold(0.0_f32, |a, &v| a.max(v.abs()));
                    if peak > 0.0 {
                        (20.0 * peak.log10()).max(ENVELOPE_FLOOR_DB)
                    } else {
                        ENVELOPE_FLOOR_DB
                    }
                })
                .collect()
        };
        Self {
            sample_rate,
            length_samples: length,
            left_db: envelope(left),
            right_db: envelope(right),
            hop,
        }
    }

    /// Duration of the viewed IR in seconds.
    #[must_use]
    pub fn duration_seconds(&self) -> f64 {
        self.length_samples as f64 / self.sample_rate
    }
}

/// The finished bake of one stage: installable spectra plus the editor view.
pub struct BakedStage {
    pub install: StageInstall,
    pub view: IrView,
}

/// Shape both channels of an IR and partition them into installable
/// spectra. Off the audio thread only (FFT planning and allocation).
#[must_use]
pub fn bake_stage(
    left: &[f32],
    right: &[f32],
    selection: Selection,
    envelope: Envelope3,
    sample_rate: f64,
) -> BakedStage {
    let shaped_left = shape_ir(left, selection, envelope);
    let shaped_right = shape_ir(right, selection, envelope);
    let view = IrView::new(&shaped_left, &shaped_right, sample_rate);
    BakedStage {
        install: StageInstall {
            left: Box::new(ChannelSpectra::from_time_ir(&shaped_left)),
            right: Box::new(ChannelSpectra::from_time_ir(&shaped_right)),
        },
        view,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(len: usize) -> Vec<f32> {
        (0..len).map(|i| i as f32 + 1.0).collect()
    }

    #[test]
    fn flat_envelope_and_full_selection_leave_the_ir_untouched() {
        let ir = ramp(500);
        let shaped = shape_ir(&ir, Selection::default(), Envelope3::default());
        assert_eq!(shaped, ir);
    }

    #[test]
    fn everything_outside_the_selection_is_zeroed() {
        let ir = ramp(1_000);
        let shaped = shape_ir(
            &ir,
            Selection {
                start: 0.25,
                length: 0.5,
            },
            Envelope3::default(),
        );
        assert!(shaped[..250].iter().all(|&s| s == 0.0));
        assert!(shaped[750..].iter().all(|&s| s == 0.0));
        // Inside survives at 0 dB.
        assert_eq!(shaped[300], ir[300]);
    }

    #[test]
    fn envelope_points_apply_their_db_values_at_their_positions() {
        let ir = vec![1.0_f32; 1_000];
        let envelope = Envelope3 {
            a_db: 12.0,
            b_x: 0.5,
            b_db: 0.0,
            c_db: -60.0,
        };
        let shaped = shape_ir(&ir, Selection::default(), envelope);
        let at = |db: f32| 10.0_f32.powf(db / 20.0);
        assert!((shaped[0] - at(12.0)).abs() < 1e-4, "A at start");
        assert!((shaped[500] - at(0.0)).abs() < 1e-4, "B at b_x");
        assert!(
            (shaped[999] - at(-60.0)).abs() < 1e-4,
            "C at end: {}",
            shaped[999]
        );
        // Halfway between A and B: +6 dB.
        assert!((shaped[250] - at(6.0)).abs() < 1e-4);
    }

    #[test]
    fn selection_and_envelope_are_clamped_and_finite_safe() {
        let selection = Selection {
            start: f32::NAN,
            length: 2.0,
        }
        .sanitized();
        assert_eq!(selection.start, 0.0);
        assert_eq!(selection.length, 1.0);

        let overlapping = Selection {
            start: 0.9,
            length: 0.5,
        }
        .sanitized();
        assert!(overlapping.start + overlapping.length <= 1.0);

        let envelope = Envelope3 {
            a_db: f32::INFINITY,
            b_x: f32::NAN,
            b_db: 0.0,
            c_db: 0.0,
        }
        .sanitized();
        assert_eq!(envelope.a_db, 0.0);
        assert_eq!(envelope.b_x, 0.5);
    }

    #[test]
    fn a_tiny_selection_stays_at_least_one_sample_wide() {
        let (start, end) = Selection {
            start: 0.0,
            length: 0.05,
        }
        .sample_range(10);
        assert!(end > start);
        let (start, end) = Selection {
            start: 0.95,
            length: 0.05,
        }
        .sample_range(10);
        assert!(end > start);
        assert!(end <= 10);
    }

    #[test]
    fn baked_stage_partitions_and_builds_a_view() {
        let ir = ramp(300);
        let baked = bake_stage(
            &ir,
            &ir,
            Selection::default(),
            Envelope3::default(),
            48_000.0,
        );
        assert_eq!(baked.install.left.ir_samples(), 300);
        assert_eq!(baked.install.left.n_partitions(), 3);
        assert_eq!(baked.view.length_samples, 300);
        assert!(baked.view.left_db.len() <= MAX_VIEW_POINTS);
        assert!(baked.view.left_db.iter().all(|db| db.is_finite()));
        assert_eq!(baked.view.hop, 1);
    }

    #[test]
    fn view_downsamples_long_irs_and_floors_silence() {
        let mut ir = vec![0.0_f32; 100_000];
        ir[0] = 1.0;
        let view = IrView::new(&ir, &ir, 48_000.0);
        assert_eq!(view.hop, 100_000_usize.div_ceil(MAX_VIEW_POINTS));
        assert!(view.left_db.len() <= MAX_VIEW_POINTS);
        assert_eq!(view.left_db[0], 0.0);
        assert!(view.left_db[1..].iter().all(|&db| db == ENVELOPE_FLOOR_DB));
        assert!((view.duration_seconds() - 100_000.0 / 48_000.0).abs() < 1e-9);
    }
}
