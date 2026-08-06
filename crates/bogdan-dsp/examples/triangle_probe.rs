//! Eyeball the wavefolder on a slow sine that exceeds the ceiling. A proper
//! fold reflects back inside the ceiling (output -> 0 near driven = 2C) rather
//! than flattening at the top.
//!
//! Run: `cargo run -q --example triangle_probe -p bogdan-dsp`

use bogdan_dsp::{ClipMode, DetailClipper, DetailSettings, FoldShape};

const SAMPLE_RATE: f64 = 48_000.0;
const FREQ: f32 = 110.0;
const DRIVE: f32 = 2.0; // pushes the +1.0 peak to 2C (a full fold to zero)
const CEILING: f32 = 1.0;

fn fold(shape: FoldShape) -> DetailSettings {
    DetailSettings {
        drive: DRIVE,
        ceiling: CEILING,
        mode: ClipMode::Fold,
        detail_hz: 1_000.0,
        amount: 1.0,
        shape,
    }
}

fn main() {
    let samples_per_cycle = (SAMPLE_RATE as f32 / FREQ) as usize;
    let mut sine = DetailClipper::new(SAMPLE_RATE);
    let mut tri = DetailClipper::new(SAMPLE_RATE);

    // Warm up so the one-sample ADAA memory is primed.
    for i in 0..samples_per_cycle {
        let ph = (i as f32 / SAMPLE_RATE as f32) * FREQ;
        let x = (std::f32::consts::TAU * ph).sin();
        let _ = sine.process(x, fold(FoldShape::Sine));
        let _ = tri.process(x, fold(FoldShape::Triangle));
    }

    let start = samples_per_cycle;
    println!("idx\tdriven\tsine_out\ttri_out");
    for i in start..(start + samples_per_cycle) {
        let ph = (i as f32 / SAMPLE_RATE as f32) * FREQ;
        let x = (std::f32::consts::TAU * ph).sin();
        let s = sine.process(x, fold(FoldShape::Sine));
        let t = tri.process(x, fold(FoldShape::Triangle));
        // Print near the peaks where the fold is visible.
        if s.driven.abs() > CEILING * 0.6 {
            println!(
                "{}\t{:+.3}\t{:+.3}\t{:+.3}",
                i - start,
                s.driven,
                s.output,
                t.output
            );
        }
    }
}
