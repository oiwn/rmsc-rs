//! Offline impulse-response analysis for the editor's IR probe.
//!
//! Never touched by the audio thread — this runs on the background task pool,
//! so it allocates freely. Roughly 20 ms for the reverb pass plus 17 ms for the
//! spectrogram at three seconds / 48 kHz.

use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

use crate::{KiryaReverb, KiryaSettings, estimated_tail_seconds};

/// Render lengths the IR probe is allowed to choose from, in seconds.
///
/// A fixed window cannot serve this reverb: at Size 0.05 / Decay 0 the tail is
/// gone in about 30 ms, and at the top of the Decay range it runs for hours.
/// Snapping to a stop rather than tracking the estimate continuously keeps the
/// time axis stable while a knob moves, so successive renders stay comparable.
///
/// The stops step by roughly 1.6x rather than doubling: with a 2x progression a
/// 13 s tail lands in a 32 s window and spends half the panel on silence.
pub const IR_WINDOW_STOPS: [f64; 9] = [0.5, 1.0, 2.0, 3.0, 5.0, 8.0, 12.0, 20.0, 32.0];

/// Extra window past the estimated reverb time, so the tail visibly reaches the
/// floor instead of ending exactly at the panel's right edge.
const IR_WINDOW_HEADROOM: f64 = 1.3;

/// Pick the render window for these settings: the shortest stop that holds the
/// estimated tail plus headroom, or the longest stop if nothing does.
#[must_use]
pub fn ir_window_seconds(settings: KiryaSettings, sample_rate: f64) -> f64 {
    let longest = IR_WINDOW_STOPS[IR_WINDOW_STOPS.len() - 1];
    let wanted = estimated_tail_seconds(settings, sample_rate) * IR_WINDOW_HEADROOM;
    if !wanted.is_finite() {
        // Only reachable from a decay with no 60 dB point at all, which wants
        // the longest window we are willing to render.
        return longest;
    }
    IR_WINDOW_STOPS
        .into_iter()
        .find(|&stop| stop >= wanted)
        .unwrap_or(longest)
}

/// Finest spacing between envelope points, in samples.
pub const ENVELOPE_HOP: usize = 256;

/// Spectrogram transform length.
pub const SPECTROGRAM_FFT: usize = 1_024;

/// Finest spacing between spectrogram columns, in samples.
pub const SPECTROGRAM_HOP: usize = 256;

/// Most columns a spectrogram will ever have.
///
/// The hop widens on a long render rather than the column count growing without
/// bound. Two reasons: the display is a few hundred pixels wide, so past this
/// the extra resolution is invisible; and the columns become a texture, which
/// at a fixed 256-sample hop would reach ~24 000 for a 32 s render at 192 kHz —
/// past the 8192 limit common on GPUs, and 24 000 transforms per render.
pub const MAX_SPECTROGRAM_COLUMNS: usize = 2_048;

/// Most points an envelope will ever have, for the same reason.
pub const MAX_ENVELOPE_POINTS: usize = 2_048;

/// Widen `base` until `length` divides into at most `max` steps.
fn hop_for(length: usize, base: usize, max: usize) -> usize {
    base.max(length.div_ceil(max.max(1))).max(1)
}

/// Log-spaced frequency rows in the spectrogram.
pub const SPECTROGRAM_ROWS: usize = 128;

/// Lowest frequency the spectrogram shows.
pub const SPECTROGRAM_MIN_HZ: f64 = 20.0;

/// Highest frequency the spectrogram shows.
pub const SPECTROGRAM_MAX_HZ: f64 = 20_000.0;

/// Level, in dB below the render's peak, that the displays bottom out at.
pub const FLOOR_DB: f32 = -90.0;

/// A rendered spectrogram, in dB relative to the render's peak.
///
/// Stored row-major with row 0 at [`SPECTROGRAM_MIN_HZ`], so the editor can
/// walk it top-down after flipping.
#[derive(Clone, Debug, Default)]
pub struct Spectrogram {
    /// Number of log-spaced frequency rows.
    pub rows: usize,
    /// Number of time columns.
    pub columns: usize,
    /// `rows * columns` decibel values, row-major, floored at [`FLOOR_DB`].
    pub values: Vec<f32>,
}

impl Spectrogram {
    /// Read one cell. Returns [`FLOOR_DB`] when out of range.
    #[must_use]
    pub fn get(&self, row: usize, column: usize) -> f32 {
        if row >= self.rows || column >= self.columns {
            return FLOOR_DB;
        }
        self.values[row * self.columns + column]
    }
}

/// Everything the editor draws for one impulse response.
#[derive(Clone, Debug, Default)]
pub struct IrRender {
    /// Rate the response was rendered at.
    pub sample_rate: f64,
    /// Length of the rendered response in samples.
    pub length: usize,
    /// Left-channel block RMS in dB relative to the render's peak.
    pub left_envelope_db: Vec<f32>,
    /// Right-channel block RMS in dB relative to the render's peak.
    pub right_envelope_db: Vec<f32>,
    /// Log-frequency spectrogram of the summed channels.
    pub spectrogram: Spectrogram,
    /// Measured reverb time, when the response decays far enough to fit one.
    pub rt60_seconds: Option<f32>,
    /// Samples between envelope points. At or above [`ENVELOPE_HOP`], widened
    /// on a long render to hold the point count under [`MAX_ENVELOPE_POINTS`].
    pub envelope_hop: usize,
    /// Samples between spectrogram columns, widened the same way against
    /// [`MAX_SPECTROGRAM_COLUMNS`].
    pub spectrogram_hop: usize,
}

impl IrRender {
    /// Seconds covered by the render, for the editor's time ruler.
    #[must_use]
    pub fn duration_seconds(&self) -> f32 {
        if self.sample_rate <= 0.0 {
            return 0.0;
        }
        #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
        {
            (self.length as f64 / self.sample_rate) as f32
        }
    }
}

/// Reusable transform and scratch buffers.
///
/// Build one per worker and keep it: planning an FFT is far more expensive
/// than running one.
pub struct Analyzer {
    fft: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    input: Vec<f32>,
    spectrum: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
}

impl Default for Analyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer {
    /// Plan the spectrogram transform and allocate its buffers.
    #[must_use]
    pub fn new() -> Self {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(SPECTROGRAM_FFT);
        let input = fft.make_input_vec();
        let spectrum = fft.make_output_vec();
        let scratch = vec![Complex::default(); fft.get_scratch_len()];
        Self {
            window: hann(SPECTROGRAM_FFT),
            fft,
            input,
            spectrum,
            scratch,
        }
    }

    /// Forward transform of exactly [`SPECTROGRAM_FFT`] samples, unwindowed.
    ///
    /// Exposed so tests can check the transform against a naive DFT.
    pub fn forward(&mut self, signal: &[f32]) -> &[Complex<f32>] {
        self.input.fill(0.0);
        let count = signal.len().min(SPECTROGRAM_FFT);
        self.input[..count].copy_from_slice(&signal[..count]);
        // `process_with_scratch` only fails on a length mismatch, and the
        // buffers came from the planner itself.
        self.fft
            .process_with_scratch(&mut self.input, &mut self.spectrum, &mut self.scratch)
            .expect("planner-sized buffers");
        &self.spectrum
    }

    /// Analyse a rendered stereo impulse response.
    pub fn analyse(&mut self, left: &[f32], right: &[f32], sample_rate: f64) -> IrRender {
        let length = left.len().min(right.len());
        let peak = left
            .iter()
            .chain(right.iter())
            .fold(0.0_f32, |peak, &s| peak.max(s.abs()))
            .max(f32::MIN_POSITIVE);

        let envelope_hop = hop_for(length, ENVELOPE_HOP, MAX_ENVELOPE_POINTS);
        let spectrogram_hop = hop_for(length, SPECTROGRAM_HOP, MAX_SPECTROGRAM_COLUMNS);

        IrRender {
            sample_rate,
            length,
            left_envelope_db: envelope_db(&left[..length], peak, envelope_hop),
            right_envelope_db: envelope_db(&right[..length], peak, envelope_hop),
            spectrogram: self.spectrogram(
                &left[..length],
                &right[..length],
                sample_rate,
                spectrogram_hop,
            ),
            rt60_seconds: rt60_seconds(&left[..length], &right[..length], sample_rate),
            envelope_hop,
            spectrogram_hop,
        }
    }

    fn spectrogram(
        &mut self,
        left: &[f32],
        right: &[f32],
        sample_rate: f64,
        hop: usize,
    ) -> Spectrogram {
        let length = left.len();
        if length < SPECTROGRAM_FFT || sample_rate <= 0.0 {
            return Spectrogram::default();
        }
        let columns = (length - SPECTROGRAM_FFT) / hop + 1;
        let bands = row_bands(sample_rate);

        let mut magnitudes = vec![0.0_f32; self.spectrum.len()];
        let mut values = vec![FLOOR_DB; SPECTROGRAM_ROWS * columns];
        let mut peak = f32::MIN_POSITIVE;

        // First pass fills raw magnitudes so the dB scale can be referenced to
        // the loudest cell in the whole render.
        let mut raw = vec![0.0_f32; SPECTROGRAM_ROWS * columns];
        for column in 0..columns {
            let start = column * hop;
            self.input.fill(0.0);
            for (slot, (&sample, &window)) in self.input.iter_mut().zip(
                left[start..start + SPECTROGRAM_FFT]
                    .iter()
                    .zip(self.window.iter()),
            ) {
                *slot = sample * window;
            }
            // The two channels share one display, so analyse their sum.
            for (slot, (&sample, &window)) in self.input.iter_mut().zip(
                right[start..start + SPECTROGRAM_FFT]
                    .iter()
                    .zip(self.window.iter()),
            ) {
                *slot += sample * window;
            }
            self.fft
                .process_with_scratch(&mut self.input, &mut self.spectrum, &mut self.scratch)
                .expect("planner-sized buffers");
            for (slot, value) in magnitudes.iter_mut().zip(self.spectrum.iter()) {
                *slot = value.norm();
            }

            for (row, band) in bands.iter().enumerate() {
                let magnitude = band_magnitude(&magnitudes, *band);
                raw[row * columns + column] = magnitude;
                peak = peak.max(magnitude);
            }
        }

        for (slot, &magnitude) in values.iter_mut().zip(raw.iter()) {
            *slot = (20.0 * (magnitude / peak).max(1.0e-12).log10()).max(FLOOR_DB);
        }

        Spectrogram {
            rows: SPECTROGRAM_ROWS,
            columns,
            values,
        }
    }
}

/// Render a wet-only impulse response into `left` and `right`.
///
/// Freeze is forced off and the dry path muted, so the display shows the tail
/// rather than the input spike. Pre-delay is included, since it is part of what
/// the user is looking at.
pub fn render_impulse_response(
    settings: KiryaSettings,
    sample_rate: f64,
    samples: usize,
    left: &mut Vec<f32>,
    right: &mut Vec<f32>,
) {
    let mut reverb = KiryaReverb::new(sample_rate);
    let settings = KiryaSettings {
        dry: 0.0,
        wet: 1.0,
        freeze: false,
        ..settings
    };

    left.clear();
    right.clear();
    left.reserve(samples);
    right.reserve(samples);
    for n in 0..samples {
        let input = if n == 0 { 1.0 } else { 0.0 };
        let frame = reverb.process(input, input, settings);
        left.push(frame.wet_left);
        right.push(frame.wet_right);
    }
}

/// Block RMS in dB relative to `peak`, one value per `hop` samples.
#[must_use]
pub fn envelope_db(signal: &[f32], peak: f32, hop: usize) -> Vec<f32> {
    let peak = peak.max(f32::MIN_POSITIVE);
    signal
        .chunks(hop.max(1))
        .map(|block| {
            let sum: f64 = block.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
            #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
            let rms = (sum / block.len() as f64).sqrt() as f32;
            (20.0 * (rms / peak).max(1.0e-12).log10()).max(FLOOR_DB)
        })
        .collect()
}

/// Reverb time from a backward-integrated energy decay curve.
///
/// Fits the -5 dB to -35 dB span (T30) and doubles it, the usual estimator when
/// the noise floor makes a full 60 dB fit unreliable. `None` when the response
/// never falls 35 dB inside the rendered window.
#[must_use]
pub fn rt60_seconds(left: &[f32], right: &[f32], sample_rate: f64) -> Option<f32> {
    if left.is_empty() || sample_rate <= 0.0 {
        return None;
    }

    let mut curve = vec![0.0_f64; left.len()];
    let mut running = 0.0_f64;
    for index in (0..left.len()).rev() {
        let l = f64::from(left[index]);
        let r = f64::from(*right.get(index).unwrap_or(&0.0));
        running += l * l + r * r;
        curve[index] = running;
    }

    let total = curve[0];
    if total <= 0.0 {
        return None;
    }
    let level = |index: usize| 10.0 * (curve[index] / total).max(1.0e-30).log10();
    let start = (0..curve.len()).find(|&index| level(index) <= -5.0)?;
    let end = (start..curve.len()).find(|&index| level(index) <= -35.0)?;

    // A backward integral always runs to zero at the end of the window, so a
    // response that never really decays still crosses -35 dB in its final
    // fraction of a percent. Reject a fit that lands that late: the slope
    // would be the truncation, not the reverb. Freeze is exactly this case.
    if end > curve.len() * 9 / 10 {
        return None;
    }

    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    Some(((end - start) as f64 / sample_rate * 2.0) as f32)
}

/// Periodic Hann window.
fn hann(length: usize) -> Vec<f32> {
    (0..length)
        .map(|n| {
            #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
            {
                (0.5 - 0.5 * (std::f64::consts::TAU * n as f64 / length as f64).cos()) as f32
            }
        })
        .collect()
}

/// Inclusive bin range each display row draws from.
fn row_bands(sample_rate: f64) -> Vec<(usize, usize)> {
    let bins = SPECTROGRAM_FFT / 2 + 1;
    let bin_of = |frequency: f64| {
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )]
        {
            ((frequency * SPECTROGRAM_FFT as f64 / sample_rate).round() as usize).min(bins - 1)
        }
    };
    let ratio = SPECTROGRAM_MAX_HZ / SPECTROGRAM_MIN_HZ;

    (0..SPECTROGRAM_ROWS)
        .map(|row| {
            #[allow(clippy::cast_precision_loss)]
            let fraction = |offset: f64| (row as f64 + offset) / SPECTROGRAM_ROWS as f64;
            let low = bin_of(SPECTROGRAM_MIN_HZ * ratio.powf(fraction(0.0)));
            let high = bin_of(SPECTROGRAM_MIN_HZ * ratio.powf(fraction(1.0)));
            // Below roughly 1 kHz the rows are finer than the bin spacing, so
            // several rows share one bin rather than showing gaps.
            (low, high.max(low))
        })
        .collect()
}

/// Loudest bin in a row's band.
fn band_magnitude(magnitudes: &[f32], band: (usize, usize)) -> f32 {
    let (low, high) = band;
    magnitudes[low..=high.min(magnitudes.len() - 1)]
        .iter()
        .fold(0.0_f32, |peak, &value| peak.max(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    const SAMPLE_RATE: f64 = 48_000.0;

    /// Naive DFT bin, the independent reference for the planned transform.
    fn dft(signal: &[f32], bin: usize) -> (f64, f64) {
        #[allow(clippy::cast_precision_loss)]
        let n = signal.len() as f64;
        let (mut re, mut im) = (0.0, 0.0);
        for (index, &sample) in signal.iter().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let angle = -TAU * bin as f64 * index as f64 / n;
            re += f64::from(sample) * angle.cos();
            im += f64::from(sample) * angle.sin();
        }
        (re, im)
    }

    #[test]
    fn the_planned_transform_matches_a_naive_dft() {
        // A tone at bin 64 plus a second at bin 200, so the check covers both
        // an exact bin and the leakage around it.
        let signal: Vec<f32> = (0..SPECTROGRAM_FFT)
            .map(|n| {
                #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
                {
                    ((TAU * 64.0 * n as f64 / SPECTROGRAM_FFT as f64).sin()
                        + 0.5 * (TAU * 200.5 * n as f64 / SPECTROGRAM_FFT as f64).cos())
                        as f32
                }
            })
            .collect();

        let mut analyzer = Analyzer::new();
        let spectrum = analyzer.forward(&signal).to_vec();
        assert_eq!(spectrum.len(), SPECTROGRAM_FFT / 2 + 1);

        for (bin, value) in spectrum.iter().enumerate() {
            let (re, im) = dft(&signal, bin);
            assert!(
                (f64::from(value.re) - re).abs() < 1.0e-2,
                "bin {bin} real: {} vs {re}",
                value.re
            );
            assert!(
                (f64::from(value.im) - im).abs() < 1.0e-2,
                "bin {bin} imag: {} vs {im}",
                value.im
            );
        }
    }

    #[test]
    fn a_synthetic_exponential_decay_gives_its_known_rt60() {
        // A noise burst with a 1.5 s RT60: amplitude falls 60 dB in 1.5 s.
        const EXPECTED: f64 = 1.5;
        let length = (SAMPLE_RATE * 4.0) as usize;
        let mut state = 0x2545_F491_4F6C_DD1D_u64;
        let signal: Vec<f32> = (0..length)
            .map(|n| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
                {
                    let noise = ((state >> 40) as f32 / 8_388_608.0) - 1.0;
                    let seconds = n as f64 / SAMPLE_RATE;
                    let gain = 10.0_f64.powf(-3.0 * seconds / EXPECTED);
                    noise * gain as f32
                }
            })
            .collect();

        let measured = rt60_seconds(&signal, &signal, SAMPLE_RATE).expect("decays far enough");
        assert!(
            (f64::from(measured) - EXPECTED).abs() < 0.1,
            "measured {measured} s against {EXPECTED} s"
        );
    }

    #[test]
    fn a_response_that_never_decays_reports_no_rt60() {
        // What a frozen tank looks like: the editor should print no reverb
        // time rather than a number invented by the end of the window.
        let steady = vec![0.5_f32; 48_000];
        assert!(rt60_seconds(&steady, &steady, SAMPLE_RATE).is_none());
        assert!(rt60_seconds(&[], &[], SAMPLE_RATE).is_none());
        assert!(rt60_seconds(&steady, &steady, 0.0).is_none());
    }

    #[test]
    fn envelope_tracks_a_known_decay_in_db() {
        // Constant 0.5 against a peak of 1.0 is -6 dB.
        let signal = vec![0.5_f32; ENVELOPE_HOP * 8];
        let envelope = envelope_db(&signal, 1.0, ENVELOPE_HOP);
        assert_eq!(envelope.len(), 8);
        for value in envelope {
            assert!((value + 6.0206).abs() < 1.0e-3, "{value}");
        }

        // Silence bottoms out at the floor rather than negative infinity.
        let silence = vec![0.0_f32; ENVELOPE_HOP * 2];
        assert!(
            envelope_db(&silence, 1.0, ENVELOPE_HOP)
                .iter()
                .all(|&v| v == FLOOR_DB)
        );

        // A zero hop would divide by zero inside `chunks`.
        assert_eq!(envelope_db(&signal, 1.0, 0).len(), signal.len());
    }

    #[test]
    fn spectrogram_rows_cover_the_audible_range_in_order() {
        let bands = row_bands(SAMPLE_RATE);
        assert_eq!(bands.len(), SPECTROGRAM_ROWS);
        for pair in bands.windows(2) {
            assert!(pair[1].0 >= pair[0].0, "rows are not monotonic");
        }
        // Row 0 sits at 20 Hz, which is below the first bin at 48 kHz.
        assert_eq!(bands[0].0, 0);
        // The top row reaches 20 kHz — short of the 24 kHz Nyquist bin, which
        // is deliberate: the display ends where hearing does.
        #[allow(clippy::cast_precision_loss)]
        let top_hz = bands[SPECTROGRAM_ROWS - 1].1 as f64 * SAMPLE_RATE / SPECTROGRAM_FFT as f64;
        assert!(
            (top_hz - SPECTROGRAM_MAX_HZ).abs() < 100.0,
            "top row sits at {top_hz} Hz"
        );
        assert!(bands[SPECTROGRAM_ROWS - 1].1 < SPECTROGRAM_FFT / 2);
    }

    #[test]
    fn a_rendered_response_analyses_into_a_populated_display() {
        let mut left = Vec::new();
        let mut right = Vec::new();
        render_impulse_response(
            KiryaSettings::default(),
            SAMPLE_RATE,
            (SAMPLE_RATE * 3.0) as usize,
            &mut left,
            &mut right,
        );

        let mut analyzer = Analyzer::new();
        let render = analyzer.analyse(&left, &right, SAMPLE_RATE);

        assert_eq!(render.length, left.len());
        assert!((render.duration_seconds() - 3.0).abs() < 1.0e-3);
        // A three-second render at 48 kHz is short enough to keep the finest
        // hop, so this is the same output the fixed-hop version produced.
        assert_eq!(render.envelope_hop, ENVELOPE_HOP);
        assert_eq!(render.spectrogram_hop, SPECTROGRAM_HOP);
        assert_eq!(
            render.left_envelope_db.len(),
            left.len().div_ceil(ENVELOPE_HOP)
        );
        assert_eq!(render.spectrogram.rows, SPECTROGRAM_ROWS);
        assert!(
            render.spectrogram.columns > 500,
            "{}",
            render.spectrogram.columns
        );
        assert!(
            render.rt60_seconds.is_some(),
            "default settings should decay"
        );

        // The response starts loud and ends quiet.
        let first = render.left_envelope_db[4];
        let last = *render.left_envelope_db.last().unwrap();
        assert!(first > last + 20.0, "{first} dB to {last} dB");

        // At least one spectrogram cell reaches the 0 dB reference.
        assert!(render.spectrogram.values.iter().any(|&v| v > -1.0));
        assert!(render.spectrogram.values.iter().all(|&v| v <= 0.0 + 1.0e-3));
    }

    #[test]
    fn the_window_snaps_to_a_stop_that_holds_the_tail() {
        let window = |decay: f32, size: f32| {
            ir_window_seconds(
                KiryaSettings {
                    decay,
                    size,
                    ..KiryaSettings::default()
                },
                48_000.0,
            )
        };

        // Every window is one of the stops, never an arbitrary length.
        let mut previous = 0.0_f64;
        for step in 0..=40 {
            #[allow(clippy::cast_precision_loss)]
            let decay = step as f32 / 40.0;
            let value = window(decay, 1.0);
            assert!(
                IR_WINDOW_STOPS.contains(&value),
                "decay {decay} gave {value}, not a stop"
            );
            // Longer decays never shorten the window.
            assert!(value >= previous, "decay {decay}: {value} < {previous}");
            previous = value;
        }

        // A tiny plate gets the shortest stop instead of a spike at the far
        // left; the top of the range is capped at the longest.
        assert_eq!(window(0.0, 0.05), IR_WINDOW_STOPS[0]);
        assert_eq!(window(1.0, 4.0), IR_WINDOW_STOPS[IR_WINDOW_STOPS.len() - 1]);

        // A bigger plate rings longer, so it must not get a shorter window.
        assert!(window(0.5, 4.0) >= window(0.5, 0.25));

        // Garbage settings must still produce a renderable length.
        let broken = ir_window_seconds(
            KiryaSettings {
                decay: f32::NAN,
                size: f32::INFINITY,
                pre_delay_ms: f32::NAN,
                ..KiryaSettings::default()
            },
            f64::NAN,
        );
        assert!(IR_WINDOW_STOPS.contains(&broken), "{broken}");
    }

    #[test]
    fn a_long_render_widens_the_hops_instead_of_growing_without_bound() {
        // The worst case the editor can ask for: the longest window at the
        // highest sample rate. At a fixed 256-sample hop this would be ~24 000
        // spectrogram columns — past the 8192 texture limit common on GPUs,
        // and 24 000 transforms per render.
        const RATE: f64 = 192_000.0;
        let length = (RATE * 32.0) as usize;
        let silence = vec![0.0_f32; length];

        let mut analyzer = Analyzer::new();
        let render = analyzer.analyse(&silence, &silence, RATE);

        assert!(render.spectrogram.columns <= MAX_SPECTROGRAM_COLUMNS);
        assert!(render.left_envelope_db.len() <= MAX_ENVELOPE_POINTS);
        assert!(render.envelope_hop > ENVELOPE_HOP);
        assert!(render.spectrogram_hop > SPECTROGRAM_HOP);
        assert_eq!(
            render.spectrogram.values.len(),
            render.spectrogram.rows * render.spectrogram.columns
        );
    }

    #[test]
    fn the_hop_helper_only_widens_once_the_cap_is_reached() {
        // Under the cap the finest hop is kept, so short renders are unchanged.
        assert_eq!(hop_for(1_000, 256, 2_048), 256);
        assert_eq!(hop_for(256 * 2_048, 256, 2_048), 256);
        // Past it the hop grows just enough to stay inside the cap.
        assert_eq!(hop_for(256 * 2_048 + 1, 256, 2_048), 257);
        assert_eq!(hop_for(1_000_000, 256, 2_048), 489);
        // Degenerate inputs must not divide by zero or return a zero hop.
        assert_eq!(hop_for(0, 256, 2_048), 256);
        assert!(hop_for(1_000, 0, 0) >= 1);
    }

    #[test]
    fn a_darker_tail_puts_less_energy_in_the_top_rows() {
        let top_row_energy = |cutoff: f32| {
            let mut left = Vec::new();
            let mut right = Vec::new();
            render_impulse_response(
                KiryaSettings {
                    reverb_high_cut_hz: cutoff,
                    decay: 0.7,
                    ..KiryaSettings::default()
                },
                SAMPLE_RATE,
                (SAMPLE_RATE * 2.0) as usize,
                &mut left,
                &mut right,
            );
            let mut analyzer = Analyzer::new();
            let render = analyzer.analyse(&left, &right, SAMPLE_RATE);
            let spectrogram = &render.spectrogram;
            let rows = SPECTROGRAM_ROWS - 16..SPECTROGRAM_ROWS;
            let sum: f32 = rows
                .flat_map(|row| (0..spectrogram.columns).map(move |column| (row, column)))
                .map(|(row, column)| spectrogram.get(row, column))
                .sum();
            #[allow(clippy::cast_precision_loss)]
            {
                sum / (16 * spectrogram.columns) as f32
            }
        };

        let dark = top_row_energy(1_000.0);
        let bright = top_row_energy(16_000.0);
        assert!(dark < bright - 3.0, "dark {dark} dB vs bright {bright} dB");
    }
}
