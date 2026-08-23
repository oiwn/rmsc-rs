//! Color-IR WAV loading: decode, sanitize, resample, normalize, cap.
//!
//! Runs entirely off the audio thread (the dialog helper thread or a
//! background task). The result, [`LoadedIr`], is both the input to the next
//! Color bake and the payload persisted in host sessions.

use std::io::{Read, Seek};

use hound::{SampleFormat, WavReader};

use crate::COLOR_IR_MAX_SECONDS;

/// A decoded, sanitized, resampled Color IR at the host sample rate.
#[derive(Clone, Debug, PartialEq)]
pub struct LoadedIr {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
    /// The host rate the samples were resampled to.
    pub sample_rate: f64,
    /// Whether the source file was longer than the cap and got truncated.
    pub truncated: bool,
}

impl LoadedIr {
    /// The identity impulse: the Color IR before any file is loaded.
    #[must_use]
    pub fn identity(sample_rate: f64) -> Self {
        Self {
            left: vec![1.0],
            right: vec![1.0],
            sample_rate,
            truncated: false,
        }
    }

    /// A copy resampled to `target_rate` (linear interpolation) when the
    /// stored rate differs. Used when a session saved at one rate is
    /// recalled at another, and when the host rate changes under a loaded
    /// file.
    #[must_use]
    pub fn resampled(&self, target_rate: f64) -> Self {
        if self.sample_rate == target_rate || self.left.len() < 2 || self.right.len() < 2 {
            let mut copy = self.clone();
            copy.sample_rate = target_rate;
            return copy;
        }
        let (left, right) =
            resample_channels(&self.left, &self.right, self.sample_rate, target_rate);
        Self {
            left,
            right,
            sample_rate: target_rate,
            truncated: self.truncated,
        }
    }
}

/// Why a Color IR file was rejected.
#[derive(Debug, PartialEq)]
pub enum ColorIrError {
    /// hound could not parse the container or the samples.
    Decode(String),
    /// More than two channels.
    TooManyChannels { channels: usize },
    /// A NaN or infinity in the decoded samples — rustfft would happily
    /// spread garbage across every bin, so it never reaches a bake.
    NonFiniteSample,
    /// No samples at all.
    Empty,
}

impl std::fmt::Display for ColorIrError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Decode(message) => write!(f, "unparsable WAV: {message}"),
            Self::TooManyChannels { channels } => {
                write!(f, "{channels} channels: mono or stereo files only")
            }
            Self::NonFiniteSample => write!(f, "file contains non-finite samples"),
            Self::Empty => write!(f, "file contains no samples"),
        }
    }
}

impl std::error::Error for ColorIrError {}

/// Decode a WAV file into a [`LoadedIr`] at `host_rate`.
///
/// Mono files duplicate to both channels; stereo files stay diagonal
/// (in L → IR L, in R → IR R). Integer formats are normalized to ±1. The
/// file is linearly resampled to `host_rate` — the same mild HF rolloff
/// kirya's fractional delays accept — and scaled to unit total energy across
/// both channels, so arbitrary files sit at a predictable level relative to
/// the Mix knob: white in, white out at the same RMS in expectation.
pub fn load_color_ir<R: Read + Seek>(reader: R, host_rate: f64) -> Result<LoadedIr, ColorIrError> {
    let host_rate = if host_rate.is_finite() && host_rate > 0.0 {
        host_rate
    } else {
        44_100.0
    };

    let mut wav = WavReader::new(reader).map_err(|e| ColorIrError::Decode(e.to_string()))?;
    let spec = wav.spec();
    let channels = usize::from(spec.channels);
    if !(1..=2).contains(&channels) {
        return Err(ColorIrError::TooManyChannels { channels });
    }

    // hound reads each integer width as its own type, so pick the reader
    // and normalization to ±1 up front.
    type SampleIter<'a> = Box<dyn Iterator<Item = Result<f32, hound::Error>> + 'a>;
    let samples: SampleIter = match (spec.sample_format, spec.bits_per_sample) {
        (SampleFormat::Float, 32) => Box::new(wav.samples::<f32>()),
        (SampleFormat::Int, 8) => {
            Box::new(wav.samples::<i8>().map(|s| s.map(|v| f32::from(v) / 128.0)))
        }
        (SampleFormat::Int, 16) => Box::new(
            wav.samples::<i16>()
                .map(|s| s.map(|v| f32::from(v) / 32_768.0)),
        ),
        (SampleFormat::Int, 24) => Box::new(
            wav.samples::<i32>()
                .map(|s| s.map(|v| v as f32 / 8_388_608.0)),
        ),
        (SampleFormat::Int, 32) => Box::new(
            wav.samples::<i32>()
                .map(|s| s.map(|v| v as f32 / 2_147_483_648.0)),
        ),
        _ => {
            return Err(ColorIrError::Decode(format!(
                "unsupported sample format: {:?}/{} bit",
                spec.sample_format, spec.bits_per_sample
            )));
        }
    };

    let mut frames: Vec<[f32; 2]> = Vec::new();
    let mut frame = [0.0_f32; 2];
    let mut channel = 0usize;
    for sample in samples {
        let sample = sample.map_err(|e| ColorIrError::Decode(e.to_string()))?;
        if !sample.is_finite() {
            return Err(ColorIrError::NonFiniteSample);
        }
        frame[channel] = sample;
        if channel + 1 == channels {
            if channels == 1 {
                frame[1] = frame[0];
            }
            frames.push(frame);
            frame = [0.0; 2];
            channel = 0;
        } else {
            channel += 1;
        }
    }
    if frames.is_empty() {
        return Err(ColorIrError::Empty);
    }

    let (mut left, mut right) = {
        let frame_left: Vec<f32> = frames.iter().map(|f| f[0]).collect();
        let frame_right: Vec<f32> = frames.iter().map(|f| f[1]).collect();
        let (l, r) = resample_channels(
            &frame_left,
            &frame_right,
            f64::from(spec.sample_rate),
            host_rate,
        );
        (l, r)
    };

    let cap = (COLOR_IR_MAX_SECONDS * host_rate) as usize;
    let truncated = left.len() > cap;
    if truncated {
        left.truncate(cap);
        right.truncate(cap);
    }

    normalize_energy(&mut left, &mut right);

    Ok(LoadedIr {
        left,
        right,
        sample_rate: host_rate,
        truncated,
    })
}

/// Linear-interpolation resample of a channel pair to `target_rate`.
fn resample_channels(
    left: &[f32],
    right: &[f32],
    source_rate: f64,
    target_rate: f64,
) -> (Vec<f32>, Vec<f32>) {
    if source_rate == target_rate || left.len() < 2 {
        return (left.to_vec(), right.to_vec());
    }
    let step = source_rate / target_rate;
    let out_len = ((left.len() as f64 - 1.0) / step).floor() as usize + 1;
    let last = left.len() - 1;
    let mut out_left = Vec::with_capacity(out_len);
    let mut out_right = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let pos = i as f64 * step;
        let i0 = pos.floor() as usize;
        let frac = (pos - pos.floor()) as f32;
        let a = (left[i0], right[i0]);
        let b = (left[(i0 + 1).min(last)], right[(i0 + 1).min(last)]);
        out_left.push(a.0 + (b.0 - a.0) * frac);
        out_right.push(a.1 + (b.1 - a.1) * frac);
    }
    (out_left, out_right)
}

/// Scale both channels jointly so the total energy (sum of squares across L
/// and R) is 1, preserving the file's own L/R balance.
fn normalize_energy(left: &mut [f32], right: &mut [f32]) {
    let energy: f64 = left
        .iter()
        .chain(right.iter())
        .map(|&s| f64::from(s) * f64::from(s))
        .sum();
    if energy > 0.0 && energy.is_finite() {
        let gain = (1.0 / energy).sqrt() as f32;
        for sample in left.iter_mut() {
            *sample *= gain;
        }
        for sample in right.iter_mut() {
            *sample *= gain;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Write an interleaved f32 WAV into memory.
    fn float_wav(samples: &[f32], channels: u16, rate: u32) -> Cursor<Vec<u8>> {
        let mut bytes = Vec::new();
        {
            let cursor = Cursor::new(&mut bytes);
            let mut writer = hound::WavWriter::new(
                cursor,
                hound::WavSpec {
                    channels,
                    sample_rate: rate,
                    bits_per_sample: 32,
                    sample_format: hound::SampleFormat::Float,
                },
            )
            .unwrap();
            for &sample in samples {
                writer.write_sample(sample).unwrap();
            }
            writer.finalize().unwrap();
        }
        Cursor::new(bytes)
    }

    #[test]
    fn a_stereo_file_round_trips_at_the_same_rate() {
        let interleaved: Vec<f32> = vec![0.5, -0.25, 0.1, -0.05, 0.0, 0.3, -0.4, 0.2];
        let wav = float_wav(&interleaved, 2, 48_000);
        let ir = load_color_ir(wav, 48_000.0).unwrap();
        assert_eq!(ir.sample_rate, 48_000.0);
        assert!(!ir.truncated);
        // Energy-normalized, so scale differs; check shape and balance.
        assert_eq!(ir.left.len(), 4);
        assert_eq!(ir.right.len(), 4);
        let energy: f64 = ir
            .left
            .iter()
            .chain(&ir.right)
            .map(|&s| f64::from(s) * f64::from(s))
            .sum();
        assert!((energy - 1.0).abs() < 1e-5);
        // L/R balance of the file is preserved: L had more energy going in.
        let l_energy: f64 = ir.left.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
        let r_energy: f64 = ir.right.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
        assert!(l_energy > r_energy);
    }

    #[test]
    fn a_mono_file_duplices_to_both_channels() {
        let wav = float_wav(&[0.5, -0.5, 0.5, -0.5], 1, 48_000);
        let ir = load_color_ir(wav, 48_000.0).unwrap();
        assert_eq!(ir.left, ir.right);
    }

    #[test]
    fn files_longer_than_four_seconds_truncate_with_a_flag() {
        let rate = 8_000_u32;
        let samples: Vec<f32> = vec![0.25_f32; 8 * rate as usize]; // 8 s of samples
        let wav = float_wav(&samples, 1, rate);
        let ir = load_color_ir(wav, f64::from(rate)).unwrap();
        assert!(ir.truncated);
        assert_eq!(
            ir.left.len(),
            (COLOR_IR_MAX_SECONDS * f64::from(rate)) as usize
        );
    }

    #[test]
    fn non_finite_samples_reject_the_whole_file() {
        let wav = float_wav(&[0.5, f32::NAN, 0.5], 1, 48_000);
        assert_eq!(
            load_color_ir(wav, 48_000.0).unwrap_err(),
            ColorIrError::NonFiniteSample
        );
    }

    #[test]
    fn three_channel_files_are_rejected() {
        let wav = float_wav(&[0.1, 0.2, 0.3, 0.1, 0.2, 0.3], 3, 48_000);
        assert_eq!(
            load_color_ir(wav, 48_000.0).unwrap_err(),
            ColorIrError::TooManyChannels { channels: 3 }
        );
    }

    #[test]
    fn garbage_is_rejected_as_a_decode_error() {
        let err = load_color_ir(Cursor::new(vec![0_u8; 32]), 48_000.0).unwrap_err();
        assert!(matches!(err, ColorIrError::Decode(_)));
    }

    #[test]
    fn resampling_preserves_frequency() {
        // 1 kHz sine at 44.1 kHz, resampled to 48 kHz: count zero crossings
        // to confirm the frequency survives.
        let rate = 44_100_u32;
        let freq = 1_000.0_f32;
        let samples: Vec<f32> = (0..rate as usize) // one second
            .map(|i| (2.0 * std::f32::consts::PI * freq * i as f32 / rate as f32).sin() * 0.5)
            .collect();
        let wav = float_wav(&samples, 1, rate);
        let ir = load_color_ir(wav, 48_000.0).unwrap();
        assert_eq!(ir.sample_rate, 48_000.0);
        // One second at 48 kHz after resampling.
        assert!((ir.left.len() as i64 - 48_000).abs() < 100);
        let crossings = ir
            .left
            .windows(2)
            .filter(|w| w[0].signum() != w[1].signum())
            .count();
        // 1 kHz sine crosses zero 2000 times per second.
        assert!(
            (crossings as i64 - 2_000).abs() < 40,
            "{crossings} zero crossings"
        );
    }

    #[test]
    fn resampled_moves_a_stored_ir_to_a_new_rate() {
        let ir = LoadedIr {
            left: vec![0.5, 0.25, 0.125, 0.0625],
            right: vec![0.5, 0.25, 0.125, 0.0625],
            sample_rate: 44_100.0,
            truncated: true,
        };
        let moved = ir.resampled(88_200.0);
        assert_eq!(moved.sample_rate, 88_200.0);
        assert!(moved.truncated);
        // Doubling the rate roughly doubles the length via interpolation.
        assert_eq!(moved.left.len(), 7);
        // At the same rate it is a plain copy.
        let same = ir.resampled(44_100.0);
        assert_eq!(same.left, ir.left);
    }

    #[test]
    fn a_sixteen_bit_integer_file_normalizes_into_pm_one() {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut bytes = Vec::new();
        {
            let cursor = Cursor::new(&mut bytes);
            let mut writer = hound::WavWriter::new(cursor, spec).unwrap();
            for sample in [10_000_i16, -20_000, 5_000, -5_000] {
                writer.write_sample(sample).unwrap();
            }
            writer.finalize().unwrap();
        }
        let cursor = Cursor::new(bytes);
        let ir = load_color_ir(cursor, 48_000.0).unwrap();
        assert!(ir.left.iter().all(|s| s.abs() <= 1.0));
        // Raw i16 values scaled by 1/32768 before energy normalization:
        // the -20000 sample is the largest in magnitude.
        let peak_index = ir
            .left
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.abs().partial_cmp(&b.abs()).unwrap())
            .map(|(i, _)| i)
            .unwrap();
        assert_eq!(peak_index, 1);
    }
}
