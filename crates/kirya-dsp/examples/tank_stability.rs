//! Long-run stability sweep for the reverberation tank.
//!
//! ```sh
//! cargo run -p kirya-dsp --release --example tank_stability
//! ```
//!
//! A feedback tank with eight delay lines, a Size control and a Freeze switch
//! has plenty of ways to run away. This drives the extremes — every corner of
//! the Size / Decay / Diffusion space, plus continuous Size sweeps and Freeze
//! toggling — and reports the peak each case reaches. Anything that grows
//! without bound, goes non-finite, or rings on after the input stops shows up
//! here rather than in a host.

use kirya_dsp::{KiryaReverb, KiryaSettings};

const RATES: [f64; 4] = [44_100.0, 48_000.0, 96_000.0, 192_000.0];

/// Level above which a case is called unstable, in dBFS. The tank is fed a
/// unit impulse or unit-amplitude noise, so anything past this is growth, not
/// gain staging.
const ALARM_DBFS: f32 = 24.0;

fn main() {
    let mut failures = 0;

    println!("=== impulse, every Size / Decay / Diffusion corner ===");
    println!(
        "{:>9} {:>6} {:>6} {:>6} {:>10} {:>8}",
        "rate", "size", "decay", "diff", "peak dBFS", "verdict"
    );
    for &rate in &RATES {
        for size in [0.05_f32, 1.0, 4.0] {
            for decay in [0.0_f32, 0.55, 1.0] {
                for diffusion in [0.0_f32, 1.0] {
                    let settings = KiryaSettings {
                        dry: 0.0,
                        wet: 1.0,
                        size,
                        decay,
                        diffusion,
                        mod_depth: 1.0,
                        ..KiryaSettings::default()
                    };
                    let peak = run(rate, 10.0, settings, |n, _| if n == 0 { 1.0 } else { 0.0 });
                    failures += report(rate, size, decay, diffusion, peak);
                }
            }
        }
    }

    println!("\n=== sustained noise at maximum decay ===");
    for &rate in &RATES {
        for size in [0.05_f32, 1.0, 4.0] {
            let settings = KiryaSettings {
                dry: 0.0,
                wet: 1.0,
                size,
                decay: 1.0,
                mod_depth: 1.0,
                ..KiryaSettings::default()
            };
            let mut noise = Noise::default();
            let peak = run(rate, 10.0, settings, |_, _| noise.next());
            failures += report(rate, size, 1.0, 1.0, peak);
        }
    }

    println!("\n=== continuous Size sweep while ringing ===");
    for &rate in &RATES {
        let peak = sweep(rate);
        failures += report(rate, f32::NAN, 0.8, 1.0, peak);
    }

    println!("\n=== Freeze toggled every 250 ms with live input ===");
    for &rate in &RATES {
        let peak = freeze_toggle(rate);
        failures += report(rate, 1.0, 0.6, 1.0, peak);
    }

    println!();
    if failures == 0 {
        println!("all cases stable");
    } else {
        println!("{failures} case(s) exceeded {ALARM_DBFS:.0} dBFS or went non-finite");
        std::process::exit(1);
    }
}

/// Drive the reverb for `seconds` and return the peak output in dBFS, or
/// `None` if it ever went non-finite.
fn run(
    rate: f64,
    seconds: f64,
    settings: KiryaSettings,
    mut input: impl FnMut(usize, f64) -> f32,
) -> Option<f32> {
    let mut reverb = KiryaReverb::new(rate);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let samples = (rate * seconds) as usize;
    let mut peak = 0.0_f32;
    for n in 0..samples {
        let sample = input(n, rate);
        let frame = reverb.process(sample, sample, settings);
        if !frame.wet_left.is_finite() || !frame.wet_right.is_finite() {
            return None;
        }
        peak = peak.max(frame.wet_left.abs()).max(frame.wet_right.abs());
    }
    Some(to_dbfs(peak))
}

/// Ring the tank, then sweep Size across its whole range and back.
fn sweep(rate: f64) -> Option<f32> {
    let mut reverb = KiryaReverb::new(rate);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let samples = (rate * 10.0) as usize;
    let mut peak = 0.0_f32;
    for n in 0..samples {
        #[allow(clippy::cast_precision_loss)]
        let phase = (n as f32 / samples as f32 * 4.0) % 2.0;
        let ramp = if phase < 1.0 { phase } else { 2.0 - phase };
        let settings = KiryaSettings {
            dry: 0.0,
            wet: 1.0,
            size: 0.05 + ramp * (4.0 - 0.05),
            decay: 0.8,
            mod_depth: 1.0,
            ..KiryaSettings::default()
        };
        let sample = if n == 0 { 1.0 } else { 0.0 };
        let frame = reverb.process(sample, sample, settings);
        if !frame.wet_left.is_finite() || !frame.wet_right.is_finite() {
            return None;
        }
        peak = peak.max(frame.wet_left.abs()).max(frame.wet_right.abs());
    }
    Some(to_dbfs(peak))
}

/// Toggle Freeze four times a second while feeding noise in.
fn freeze_toggle(rate: f64) -> Option<f32> {
    let mut reverb = KiryaReverb::new(rate);
    let mut noise = Noise::default();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let samples = (rate * 20.0) as usize;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let quarter = (rate * 0.25) as usize;
    let mut peak = 0.0_f32;
    for n in 0..samples {
        let settings = KiryaSettings {
            dry: 0.0,
            wet: 1.0,
            decay: 0.6,
            freeze: (n / quarter) % 2 == 1,
            mod_depth: 1.0,
            ..KiryaSettings::default()
        };
        let sample = noise.next() * 0.5;
        let frame = reverb.process(sample, sample, settings);
        if !frame.wet_left.is_finite() || !frame.wet_right.is_finite() {
            return None;
        }
        peak = peak.max(frame.wet_left.abs()).max(frame.wet_right.abs());
    }
    Some(to_dbfs(peak))
}

fn report(rate: f64, size: f32, decay: f32, diffusion: f32, peak: Option<f32>) -> usize {
    let size = if size.is_nan() {
        "sweep".to_string()
    } else {
        format!("{size:.2}")
    };
    match peak {
        Some(db) if db <= ALARM_DBFS => {
            println!(
                "{rate:>9.0} {size:>6} {decay:>6.2} {diffusion:>6.2} {db:>10.1} {:>8}",
                "ok"
            );
            0
        }
        Some(db) => {
            println!(
                "{rate:>9.0} {size:>6} {decay:>6.2} {diffusion:>6.2} {db:>10.1} {:>8}",
                "LOUD"
            );
            1
        }
        None => {
            println!(
                "{rate:>9.0} {size:>6} {decay:>6.2} {diffusion:>6.2} {:>10} {:>8}",
                "-", "NAN"
            );
            1
        }
    }
}

fn to_dbfs(peak: f32) -> f32 {
    20.0 * peak.max(1.0e-12).log10()
}

/// Deterministic xorshift noise, so a run is reproducible.
struct Noise(u64);

impl Default for Noise {
    fn default() -> Self {
        Self(0x2545_F491_4F6C_DD1D)
    }
}

impl Noise {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        #[allow(clippy::cast_precision_loss)]
        {
            ((self.0 >> 40) as f32 / 8_388_608.0) - 1.0
        }
    }
}
