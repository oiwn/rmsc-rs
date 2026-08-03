use bogdan_dsp::{DetailClipper, hard_clip_frame};

const SAMPLE_RATE: f64 = 48_000.0;
const SAMPLE_COUNT: usize = 4_800;
const DRIVE: f32 = 3.0;
const CEILING: f32 = 0.8;

fn main() {
    let mut processor = DetailClipper::new(SAMPLE_RATE);
    let mut hard_peak = 0.0_f32;
    let mut detail_peak = 0.0_f32;
    let mut clipped_samples = 0_usize;
    let mut detailed_samples = 0_usize;

    for index in 0..SAMPLE_COUNT {
        let time = index as f32 / SAMPLE_RATE as f32;
        let carrier = (std::f32::consts::TAU * 220.0 * time).sin();
        let detail = 0.2 * (std::f32::consts::TAU * 2_300.0 * time).sin();
        let input = 0.55 * carrier + detail;
        let hard = hard_clip_frame(input, DRIVE, CEILING);
        let processed = processor.process_sample(input, DRIVE, CEILING);

        hard_peak = hard_peak.max(hard.clipped.abs());
        detail_peak = detail_peak.max(processed.output.abs());
        if hard.delta != 0.0 {
            clipped_samples += 1;
            if processed.output.abs() < hard.clipped.abs() {
                detailed_samples += 1;
            }
        }
    }

    println!("hard peak: {hard_peak:.6}");
    println!("detail peak: {detail_peak:.6}");
    println!("clipped samples: {clipped_samples}");
    println!("clipped samples with inward detail: {detailed_samples}");

    assert!(detail_peak <= hard_peak);
    assert!(detailed_samples > 0);
}
