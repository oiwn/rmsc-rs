//! Eyeball the three clip modes on a slow triangle: Detail should notch the
//! plateau edges (its DnB-bass voice), Fold should dip deepest at the apex.
//!
//! Run: `cargo run -q --example triangle_probe -p bogdan-dsp`

use bogdan_dsp::{ClipMode, DetailClipper, DetailSettings};

const SAMPLE_RATE: f64 = 48_000.0;
const FREQ: f32 = 110.0;
const DRIVE: f32 = 2.85; // ~9.1 dB
const CEILING: f32 = 0.99; // ~-0.1 dB
const DETAIL_HZ: f32 = 30.0; // low cutoff so Fold tracks the peak shape

fn triangle(phase: f32) -> f32 {
    // phase in cycles -> triangle in [-1, 1]
    4.0 * (phase - (phase + 0.5).floor()).abs() - 1.0
}

fn settings(mode: ClipMode) -> DetailSettings {
    DetailSettings {
        drive: DRIVE,
        ceiling: CEILING,
        mode,
        detail_hz: DETAIL_HZ,
        amount: 1.0,
    }
}

fn main() {
    let samples_per_cycle = (SAMPLE_RATE as f32 / FREQ) as usize;
    let mut detail = DetailClipper::new(SAMPLE_RATE);
    let mut fold = DetailClipper::new(SAMPLE_RATE);

    // Warm up a few cycles so both filters settle.
    for i in 0..(samples_per_cycle * 4) {
        let ph = (i as f32 / SAMPLE_RATE as f32) * FREQ;
        let _ = detail.process(triangle(ph), settings(ClipMode::Detail));
        let _ = fold.process(triangle(ph), settings(ClipMode::Fold));
    }

    let start = samples_per_cycle * 4;
    println!("idx\tdriven\tclip\tdetail_out\tfold_out");
    for i in start..(start + samples_per_cycle) {
        let ph = (i as f32 / SAMPLE_RATE as f32) * FREQ;
        let d = detail.process(triangle(ph), settings(ClipMode::Detail));
        let f = fold.process(triangle(ph), settings(ClipMode::Fold));
        // Only print the positive clipped plateau so the shapes are legible.
        if d.clipped > 0.0 && d.delta > 0.0 {
            println!(
                "{}\t{:+.3}\t{:+.3}\t{:+.3}\t{:+.3}",
                i - start,
                d.driven,
                d.clipped,
                d.output,
                f.output
            );
        }
    }
}
