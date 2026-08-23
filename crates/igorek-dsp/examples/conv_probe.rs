//! Offline validation of the partitioned engine against naive direct
//! convolution across IR lengths that straddle partition boundaries.
//!
//! ```sh
//! cargo run -p igorek-dsp --release --example conv_probe
//! ```

use igorek_dsp::engine::{ChannelSpectra, Convolver};
use igorek_dsp::{BINS, N, P};
use rand::Rng;
use rand::SeedableRng;
use rand::rngs::StdRng;

/// Relative deviation the probe tolerates before failing.
const TOLERANCE: f64 = 1e-3;

/// Input length in samples: eight partition blocks.
const INPUT_BLOCKS: usize = 8;

fn noise(rng: &mut StdRng, len: usize) -> Vec<f32> {
    (0..len).map(|_| rng.random_range(-1.0_f32..1.0)).collect()
}

fn naive_convolution(ir: &[f32], input: &[f32]) -> Vec<f64> {
    let mut y = vec![0.0_f64; input.len()];
    for (t, sample) in y.iter_mut().enumerate() {
        for (m, &h) in ir.iter().enumerate() {
            if t >= m {
                *sample += f64::from(h) * f64::from(input[t - m]);
            }
        }
    }
    y
}

fn main() {
    let mut rng = StdRng::seed_from_u64(0x0060_0DE5);
    let input = noise(&mut rng, INPUT_BLOCKS * P);

    let lengths = [1_usize, 64, 127, P, P + 1, 2 * P, 3 * P + 37, 1000, 5000];
    println!("partitioned overlap-save vs naive direct convolution");
    println!("P={P} N={N} bins={BINS} input={} samples", input.len());
    println!(
        "{:>10} {:>12} {:>14}",
        "IR length", "partitions", "max deviation"
    );

    let mut worst: f64 = 0.0;
    for len in lengths {
        let ir = noise(&mut rng, len);
        let expected = naive_convolution(&ir, &input);

        let mut engine = Convolver::new(ir.len().div_ceil(P).max(1));
        engine.install(Box::new(ChannelSpectra::from_time_ir(&ir)));
        let mut block_in = [0.0_f32; P];
        let mut block_out = [0.0_f32; P];
        let mut deviation = 0.0_f64;
        for (b, chunk) in input.chunks_exact(P).enumerate() {
            block_in.copy_from_slice(chunk);
            engine.process_block(&block_in, &mut block_out);
            for (t, &got) in block_out.iter().enumerate() {
                let want = expected[b * P + t];
                deviation = deviation.max((f64::from(got) - want).abs());
            }
        }
        let peak = expected.iter().fold(1e-9_f64, |a, &v| a.max(v.abs()));
        let relative = deviation / peak;
        worst = worst.max(relative);
        println!(
            "{len:>10} {:>12} {:>14.3e}{}",
            ir.len().div_ceil(P).max(1),
            relative,
            if relative > TOLERANCE { "  FAIL" } else { "" }
        );
    }

    println!("worst relative deviation: {worst:.3e} (tolerance {TOLERANCE:.0e})");
    if worst > TOLERANCE {
        std::process::exit(1);
    }
}
