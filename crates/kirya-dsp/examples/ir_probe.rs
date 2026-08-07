//! Render Kirya's impulse response offline and print what the editor draws.
//!
//! ```sh
//! cargo run -p kirya-dsp --release --example ir_probe
//! ```
//!
//! Prints, for a handful of settings, the measured reverb time, a decay
//! envelope in dB, and a coarse ASCII rendering of the log-frequency
//! spectrogram — enough to sanity-check Size, Decay and the damping cutoffs
//! without loading a host.

use kirya_dsp::analysis::{
    Analyzer, IrRender, SPECTROGRAM_ROWS, ir_window_seconds, render_impulse_response,
};
use kirya_dsp::{KiryaReverb, KiryaSettings, estimated_tail_seconds};

const SAMPLE_RATE: f64 = 48_000.0;

/// Characters from quietest to loudest.
const RAMP: &[u8] = b" .:-=+*#%@";

/// Rows and columns in the ASCII spectrogram.
const PLOT_ROWS: usize = 24;
const PLOT_COLUMNS: usize = 72;

fn main() {
    let cases = [
        ("default", KiryaSettings::default()),
        (
            "small + short",
            KiryaSettings {
                size: 0.3,
                decay: 0.35,
                ..KiryaSettings::default()
            },
        ),
        (
            "large + long",
            KiryaSettings {
                size: 3.0,
                decay: 0.8,
                ..KiryaSettings::default()
            },
        ),
        (
            "dark (rev high cut 1 kHz)",
            KiryaSettings {
                reverb_high_cut_hz: 1_000.0,
                decay: 0.7,
                ..KiryaSettings::default()
            },
        ),
        (
            "undiffused",
            KiryaSettings {
                diffusion: 0.0,
                decay: 0.7,
                ..KiryaSettings::default()
            },
        ),
    ];

    let mut analyzer = Analyzer::new();
    let mut left = Vec::new();
    let mut right = Vec::new();

    for (name, settings) in cases {
        // The window follows the estimated reverb time, so a small plate is
        // not a spike at the far left and a long one is not cut off.
        let window = ir_window_seconds(settings, SAMPLE_RATE);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let samples = (SAMPLE_RATE * window) as usize;
        render_impulse_response(settings, SAMPLE_RATE, samples, &mut left, &mut right);
        let render = analyzer.analyse(&left, &right, SAMPLE_RATE);

        println!("\n=== {name} ===");
        println!(
            "size {:.2}  decay {:.2}  diffusion {:.2}  rev high cut {}",
            settings.size,
            settings.decay,
            settings.diffusion,
            musictools_core::format_hz(f64::from(settings.reverb_high_cut_hz))
        );
        println!(
            "window {window:.1} s (estimate {:.2} s), hops {}/{}",
            estimated_tail_seconds(settings, SAMPLE_RATE),
            render.envelope_hop,
            render.spectrogram_hop
        );
        match render.rt60_seconds {
            Some(rt60) => println!("RT60 {rt60:.2} s"),
            None => println!("RT60 —  (does not decay 35 dB inside {window:.1} s)"),
        }
        println!("peak tail {:.1} dBFS", peak_dbfs(&left, &right));

        print_envelope(&render, window);
        print_spectrogram(&render, window);
    }

    println!("\n=== tail estimate reported to the host ===");
    let mut reverb = KiryaReverb::new(SAMPLE_RATE);
    for decay in [0.2_f32, 0.4, 0.55, 0.7, 0.9, 1.0] {
        let _ = reverb.process(
            0.0,
            0.0,
            KiryaSettings {
                decay,
                ..KiryaSettings::default()
            },
        );
        #[allow(clippy::cast_precision_loss)]
        let seconds = reverb.tail_samples() as f64 / SAMPLE_RATE;
        println!("decay {decay:.2} -> {seconds:6.2} s");
    }
}

fn peak_dbfs(left: &[f32], right: &[f32]) -> f32 {
    let peak = left
        .iter()
        .chain(right.iter())
        .fold(0.0_f32, |peak, &s| peak.max(s.abs()));
    20.0 * peak.max(1.0e-12).log10()
}

/// One line per time slice, showing left and right block RMS in dB.
fn print_envelope(render: &IrRender, window: f64) {
    println!("\nenvelope (dB below peak, L then R)");
    let points = render.left_envelope_db.len();
    let step = (points / 16).max(1);
    for index in (0..points).step_by(step) {
        #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
        let seconds = index as f32 * window as f32 / points as f32;
        let left = render.left_envelope_db[index];
        let right = render.right_envelope_db[index];
        println!(
            "  {seconds:5.2}s  {left:7.1}  {right:7.1}  {}",
            bar(left, 40)
        );
    }
}

/// A `width`-character bar for a level between the floor and 0 dB.
fn bar(db: f32, width: usize) -> String {
    let fraction = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let filled = (fraction * width as f32) as usize;
    "#".repeat(filled)
}

/// Downsample the spectrogram onto a fixed character grid, low frequencies at
/// the bottom.
fn print_spectrogram(render: &IrRender, window: f64) {
    let spectrogram = &render.spectrogram;
    if spectrogram.columns == 0 {
        println!("\nspectrogram: (response shorter than one transform)");
        return;
    }
    println!("\nspectrogram (20 Hz bottom -> 20 kHz top, {window:.1} s wide)");

    for row in (0..PLOT_ROWS).rev() {
        let source_rows = band(row, PLOT_ROWS, SPECTROGRAM_ROWS);
        let mut line = String::with_capacity(PLOT_COLUMNS);
        for column in 0..PLOT_COLUMNS {
            let source_columns = band(column, PLOT_COLUMNS, spectrogram.columns);
            let mut peak = f32::MIN;
            for source_row in source_rows.clone() {
                for source_column in source_columns.clone() {
                    peak = peak.max(spectrogram.get(source_row, source_column));
                }
            }
            // -72 dB below the render's peak maps to blank, 0 dB to '@'.
            let fraction = ((peak + 72.0) / 72.0).clamp(0.0, 1.0);
            #[allow(
                clippy::cast_precision_loss,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss
            )]
            let index = ((fraction * (RAMP.len() - 1) as f32).round() as usize).min(RAMP.len() - 1);
            line.push(RAMP[index] as char);
        }
        let label = frequency_label(row);
        println!("  {label:>7} |{line}|");
    }
}

/// Source index range that display cell `index` of `cells` covers.
fn band(index: usize, cells: usize, source: usize) -> std::ops::Range<usize> {
    let start = index * source / cells;
    let end = ((index + 1) * source / cells).max(start + 1).min(source);
    start..end
}

/// Frequency at the bottom of display row `row`, labelled only on round rows.
fn frequency_label(row: usize) -> String {
    #[allow(clippy::cast_precision_loss)]
    let fraction = row as f64 / PLOT_ROWS as f64;
    let hz = 20.0 * 1_000.0_f64.powf(fraction);
    if row.is_multiple_of(4) {
        if hz >= 1_000.0 {
            format!("{:.0}k", hz / 1_000.0)
        } else {
            format!("{hz:.0}")
        }
    } else {
        String::new()
    }
}
