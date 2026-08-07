//! Fractional-delay ring buffer.
//!
//! Every line in the reverb is one of these. Capacity is always a power of two
//! so the wrap is a mask rather than a branch or a modulo, and allocation only
//! ever happens in [`InterpDelay::resize`], which the reverb calls from
//! `reset`. Size sweeps and modulation only change the *read* distance, never
//! the buffer, so the audio thread stays allocation-free.

use musictools_core::finite_or;

/// Smallest ring a line can have. Keeps the `capacity - 2` read clamp valid
/// even before the reverb has been given a sample rate.
const MIN_CAPACITY: usize = 4;

/// Power-of-two ring buffer with linear-interpolated reads.
///
/// Read distances are measured back from the most recently written sample: a
/// delay of `1.0` returns the sample handed to the last [`write`](Self::write).
#[derive(Clone, Debug)]
pub struct InterpDelay {
    buffer: Vec<f32>,
    mask: usize,
    write: usize,
}

impl Default for InterpDelay {
    fn default() -> Self {
        Self {
            buffer: vec![0.0; MIN_CAPACITY],
            mask: MIN_CAPACITY - 1,
            write: 0,
        }
    }
}

impl InterpDelay {
    /// Allocate for at least `min_capacity` samples, rounded up to a power of
    /// two, and clear the line. Not real-time safe — call from `reset`.
    pub fn resize(&mut self, min_capacity: usize) {
        let capacity = min_capacity.max(MIN_CAPACITY).next_power_of_two();
        self.buffer.clear();
        self.buffer.resize(capacity, 0.0);
        self.mask = capacity - 1;
        self.write = 0;
    }

    /// Zero the line without touching its allocation.
    pub fn clear(&mut self) {
        self.buffer.fill(0.0);
        self.write = 0;
    }

    /// Number of samples this line can hold.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.buffer.len()
    }

    /// Largest read distance this line accepts. Reads clamp to it, so a caller
    /// that asks for more gets the oldest available sample rather than a
    /// wrapped-around one.
    #[must_use]
    pub fn max_delay(&self) -> f32 {
        // `capacity - 2` leaves room for the interpolation partner one slot
        // further back.
        #[allow(clippy::cast_precision_loss)]
        {
            (self.capacity() - 2) as f32
        }
    }

    /// Push one sample in. A non-finite sample is stored as silence so a bad
    /// host block cannot poison a line that feeds back on itself forever.
    pub fn write(&mut self, sample: f32) {
        self.buffer[self.write] = finite_or(sample, 0.0);
        self.write = (self.write + 1) & self.mask;
    }

    /// Read `delay` samples back, linearly interpolated.
    #[must_use]
    pub fn read(&self, delay: f32) -> f32 {
        let delay = finite_or(delay, 1.0).clamp(1.0, self.max_delay());
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let whole = delay as usize;
        #[allow(clippy::cast_precision_loss)]
        let fraction = delay - whole as f32;

        let newer = self.buffer[(self.write + self.capacity() - whole) & self.mask];
        let older = self.buffer[(self.write + self.capacity() - whole - 1) & self.mask];
        newer + fraction * (older - newer)
    }

    /// Read a whole number of samples back, skipping the interpolation.
    ///
    /// Agrees exactly with [`read`](Self::read) at integer distances.
    #[must_use]
    pub fn tap(&self, delay: usize) -> f32 {
        let delay = delay.clamp(1, self.capacity() - 2);
        self.buffer[(self.write + self.capacity() - delay) & self.mask]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filled(capacity: usize, samples: &[f32]) -> InterpDelay {
        let mut line = InterpDelay::default();
        line.resize(capacity);
        for &sample in samples {
            line.write(sample);
        }
        line
    }

    #[test]
    fn capacity_rounds_up_to_a_power_of_two() {
        let mut line = InterpDelay::default();
        line.resize(1_000);
        assert_eq!(line.capacity(), 1_024);
        line.resize(1_024);
        assert_eq!(line.capacity(), 1_024);
    }

    #[test]
    fn delay_of_one_returns_the_most_recent_sample() {
        let line = filled(16, &[1.0, 2.0, 3.0]);
        assert_eq!(line.read(1.0), 3.0);
        assert_eq!(line.read(2.0), 2.0);
        assert_eq!(line.read(3.0), 1.0);
    }

    #[test]
    fn fractional_reads_interpolate_between_neighbours() {
        let line = filled(16, &[0.0, 10.0, 20.0]);
        // Distance 1.0 is 20.0 and distance 2.0 is 10.0, so 1.25 sits a
        // quarter of the way toward the older sample.
        assert!((line.read(1.25) - 17.5).abs() < 1.0e-6);
        assert!((line.read(1.5) - 15.0).abs() < 1.0e-6);
        assert!((line.read(1.75) - 12.5).abs() < 1.0e-6);
    }

    #[test]
    fn tap_and_read_agree_at_integer_distances() {
        let samples: Vec<f32> = (0..200).map(|i| (i as f32) * 0.5 - 25.0).collect();
        let line = filled(64, &samples);
        for delay in 1..=line.capacity() - 2 {
            #[allow(clippy::cast_precision_loss)]
            let interpolated = line.read(delay as f32);
            assert_eq!(line.tap(delay), interpolated, "delay {delay}");
        }
    }

    #[test]
    fn writes_wrap_around_and_keep_the_newest_samples() {
        // Three full laps of a 16-slot ring: the oldest readable sample is
        // still exactly `capacity - 2` writes back.
        let samples: Vec<f32> = (0..48).map(|i| i as f32).collect();
        let line = filled(16, &samples);
        assert_eq!(line.read(1.0), 47.0);
        assert_eq!(line.read(14.0), 34.0);
    }

    #[test]
    fn reads_past_the_ring_clamp_instead_of_wrapping_onto_themselves() {
        let samples: Vec<f32> = (0..48).map(|i| i as f32).collect();
        let line = filled(16, &samples);
        // Without the clamp, a distance of 16 would alias back onto the
        // newest sample and turn the line into a comb filter.
        assert_eq!(line.read(1_000.0), line.read(line.max_delay()));
        assert!(line.read(1_000.0) < line.read(1.0));
    }

    #[test]
    fn non_finite_writes_and_reads_stay_finite() {
        let mut line = InterpDelay::default();
        line.resize(16);
        for sample in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 1.0] {
            line.write(sample);
        }
        assert!(line.read(f32::NAN).is_finite());
        assert!(line.read(2.0).is_finite());
        assert_eq!(line.read(4.0), 0.0);
    }

    #[test]
    fn clear_zeroes_the_line_without_reallocating() {
        let mut line = filled(32, &[1.0, 2.0, 3.0]);
        let capacity = line.capacity();
        line.clear();
        assert_eq!(line.capacity(), capacity);
        for delay in 1..=line.capacity() - 2 {
            assert_eq!(line.tap(delay), 0.0);
        }
    }
}
