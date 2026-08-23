//! Offline IR probe: renders the default room and a synthetic metallic
//! Color IR, with kirya-style ASCII envelope and spectrogram views.
//!
//! ```sh
//! cargo run -p igorek-dsp --release --example ir_probe
//! ```

use std::sync::Arc;

use igorek_dsp::BINS;
use igorek_dsp::bake::{Envelope3, Selection, shape_ir};
use igorek_dsp::room::{self, RoomParams};
use rand::Rng;
use rand::SeedableRng;
use rand::rngs::StdRng;
use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

const RATE: f64 = 48_000.0;
const SPECTROGRAM_FFT: usize = 1_024;
const SPECTROGRAM_HOP: usize = 256;
const ROWS: usize = 24;
const COLS: usize = 72;
const RAMP: &[u8] = b" .:-=+*#%@";
const FLOOR_DB: f32 = -90.0;

/// A synthetic metallic Color IR: a few exponentially decaying resonant
/// modes, like ringing a shell.
fn metallic_ir(rng: &mut StdRng) -> Vec<f32> {
    let len = (0.6 * RATE) as usize;
    let mut ir = vec![0.0_f32; len];
    for _ in 0..5 {
        let freq: f32 = rng.random_range(400.0..6_000.0);
        let decay: f32 = rng.random_range(8.0..45.0);
        let amp: f32 = rng.random_range(0.3..1.0);
        let phase: f32 = rng.random_range(0.0..std::f32::consts::TAU);
        for (i, sample) in ir.iter_mut().enumerate() {
            let t = i as f32 / RATE as f32;
            *sample += amp * (-t * decay).exp() * (std::f32::consts::TAU * freq * t + phase).sin();
        }
    }
    let peak = ir.iter().fold(0.0_f32, |a, &v| a.max(v.abs()));
    for sample in &mut ir {
        *sample /= peak;
    }
    ir
}

fn ascii_envelope(label: &str, ir: &[f32]) {
    println!("--- {label}: envelope (dB, 24 rows x {COLS} cols)");
    let hop = ir.len().div_ceil(COLS);
    let peaks: Vec<f32> = ir
        .chunks(hop)
        .map(|b| {
            let p = b.iter().fold(0.0_f32, |a, &v| a.max(v.abs()));
            if p > 0.0 {
                (20.0 * p.log10()).max(FLOOR_DB)
            } else {
                FLOOR_DB
            }
        })
        .collect();
    let top = 0.0_f32;
    for row in 0..ROWS {
        let lo = FLOOR_DB + (top - FLOOR_DB) * (ROWS - 1 - row) as f32 / ROWS as f32;
        let hi = FLOOR_DB + (top - FLOOR_DB) * (ROWS - row) as f32 / ROWS as f32;
        let line: String = peaks
            .iter()
            .map(|&p| {
                let ch = if p > lo && p < hi {
                    RAMP[(row.min(ROWS - 1)) * RAMP.len() / ROWS]
                } else {
                    b' '
                };
                ch as char
            })
            .collect();
        println!("{line}");
    }
}

fn ascii_spectrogram(label: &str, ir: &[f32]) {
    println!("--- {label}: spectrogram (log-freq bins, {ROWS} rows x {COLS} cols)");
    let fft: Arc<dyn RealToComplex<f32>> =
        RealFftPlanner::<f32>::new().plan_fft_forward(SPECTROGRAM_FFT);
    let window: Vec<f32> = (0..SPECTROGRAM_FFT)
        .map(|i| {
            (std::f32::consts::PI * i as f32 / SPECTROGRAM_FFT as f32)
                .sin()
                .powi(2)
        })
        .collect();
    let mut input = fft.make_input_vec();
    let mut spectrum = fft.make_output_vec();
    let mut scratch = vec![Complex::default(); fft.get_scratch_len()];

    let columns = ir.len().div_ceil(SPECTROGRAM_HOP).max(1);
    let col_take = COLS;
    let stride = (columns / col_take).max(1);
    let mut grid = vec![FLOOR_DB; ROWS * COLS];
    let mut col_out = 0;
    let mut hop_index: usize = 0;
    while hop_index.saturating_mul(SPECTROGRAM_HOP) < ir.len() && col_out < COLS {
        let start = hop_index * SPECTROGRAM_HOP;
        input.fill(0.0);
        let end = (start + SPECTROGRAM_FFT).min(ir.len());
        for (i, sample) in input.iter_mut().enumerate().take(end - start) {
            *sample = ir[start + i] * window[i];
        }
        fft.process_with_scratch(&mut input, &mut spectrum, &mut scratch)
            .expect("planner-sized buffers");
        for row in 0..ROWS {
            // Log-spaced bins across the spectrum.
            let frac = (row as f32 + 0.5) / ROWS as f32;
            let bin = (frac * (BINS.max(2) as f32 - 1.0)).round() as usize;
            let mag = spectrum[bin.min(BINS - 1)].norm();
            let db = if mag > 0.0 {
                (20.0 * mag.log10()).max(FLOOR_DB)
            } else {
                FLOOR_DB
            };
            grid[row * COLS + col_out] = db;
        }
        hop_index += stride;
        col_out += 1;
    }

    let top = grid.iter().cloned().fold(FLOOR_DB, f32::max);
    for row in (0..ROWS).rev() {
        let lo = FLOOR_DB + (top - FLOOR_DB) * (ROWS - 1 - row) as f32 / ROWS as f32;
        let hi = FLOOR_DB + (top - FLOOR_DB) * (ROWS - row) as f32 / ROWS as f32;
        let line: String = grid[row * COLS..(row + 1) * COLS]
            .iter()
            .map(|&db| {
                let ch = if db > lo && db < hi {
                    RAMP[(row.min(ROWS - 1)) * RAMP.len() / ROWS]
                } else {
                    b' '
                };
                ch as char
            })
            .collect();
        println!("{line}");
    }
}

fn main() {
    let mut rng = StdRng::seed_from_u64(0x00DE_FA17);

    println!("== Igorek IR probe, {RATE} Hz ==");

    let (room_l, room_r) = room::generate_stereo(RoomParams::default(), 0, 1.0, RATE);
    println!(
        "default room: {} samples ({:.3} s), RT60 target {} s",
        room_l.len(),
        room_l.len() as f64 / RATE,
        RoomParams::default().rt60_s
    );
    ascii_envelope("room left", &room_l);
    ascii_spectrogram("room left", &room_l);
    println!(
        "room decorrelation: L/R differ at {} of {} samples",
        room_l.iter().zip(&room_r).filter(|(a, b)| a != b).count(),
        room_l.len()
    );

    let metal = metallic_ir(&mut rng);
    let shaped = shape_ir(
        &metal,
        Selection {
            start: 0.1,
            length: 0.7,
        },
        Envelope3 {
            a_db: 0.0,
            b_x: 0.4,
            b_db: -6.0,
            c_db: -40.0,
        },
    );
    println!(
        "metallic color IR: {} samples, shaped by selection + 3-point envelope",
        shaped.len()
    );
    ascii_envelope("color (shaped)", &shaped);
    ascii_spectrogram("color (shaped)", &shaped);
}
