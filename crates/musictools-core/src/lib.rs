//! Shared, framework-independent support for the MusicTools plugin suite.

/// Human-readable suite name.
pub const SUITE_NAME: &str = "MusicTools";

/// Vendor name exposed by every plugin in the suite.
pub const VENDOR_NAME: &str = "oiwn";

/// Reverse-DNS prefix used for stable plugin identities.
pub const VENDOR_ID: &str = "com.oiwn";

/// Return `value` when it is finite, otherwise return `fallback`.
///
/// This small primitive is shared by real-time DSP code so malformed host
/// samples or parameters cannot leak NaNs and infinities through a plugin.
#[inline]
#[must_use]
pub fn finite_or(value: f32, fallback: f32) -> f32 {
    if value.is_finite() { value } else { fallback }
}

/// Double-precision counterpart to [`finite_or`], used for sample rates and
/// filter cutoffs where the coefficient math runs in `f64`.
#[inline]
#[must_use]
pub fn finite_or_f64(value: f64, fallback: f64) -> f64 {
    if value.is_finite() { value } else { fallback }
}

/// Shown in place of a frequency that is not a finite number.
const HZ_PLACEHOLDER: &str = "-- Hz";

/// Format a frequency with the precision its magnitude deserves.
///
/// Truce's built-in `ParamUnit::Hz` formatter prints whole hertz below 1 kHz,
/// which collapses a modulation-rate knob spanning `0.01..20` Hz into "0 Hz"
/// and "1 Hz". This keeps two decimals where they carry information and drops
/// them where they would only add noise:
///
/// | Range | Example |
/// |---|---|
/// | below 10 Hz | `0.01 Hz` |
/// | below 100 Hz | `20.0 Hz` |
/// | below 1 kHz | `440 Hz` |
/// | 1 kHz and up | `10.00 kHz` |
///
/// Negative frequencies are formatted by magnitude with the sign preserved, so
/// a malformed value reads as a number rather than flipping into another band.
#[must_use]
pub fn format_hz(value: f64) -> String {
    if !value.is_finite() {
        return HZ_PLACEHOLDER.to_string();
    }

    let magnitude = value.abs();
    let sign = if value.is_sign_negative() { "-" } else { "" };

    if magnitude < 10.0 {
        format!("{sign}{magnitude:.2} Hz")
    } else if magnitude < 100.0 {
        format!("{sign}{magnitude:.1} Hz")
    } else if magnitude < 1_000.0 {
        format!("{sign}{magnitude:.0} Hz")
    } else {
        format!("{sign}{:.2} kHz", magnitude / 1_000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frequencies_keep_the_decimals_their_magnitude_deserves() {
        // The case that prompted this: a modulation rate knob whose whole
        // useful bottom end used to render as "0 Hz".
        assert_eq!(format_hz(0.01), "0.01 Hz");
        assert_eq!(format_hz(1.0), "1.00 Hz");
        assert_eq!(format_hz(12.5), "12.5 Hz");
        assert_eq!(format_hz(20.0), "20.0 Hz");
        assert_eq!(format_hz(440.0), "440 Hz");
        assert_eq!(format_hz(1_000.0), "1.00 kHz");
        assert_eq!(format_hz(10_000.0), "10.00 kHz");
        assert_eq!(format_hz(20_000.0), "20.00 kHz");
    }

    #[test]
    fn each_precision_boundary_switches_on_the_right_side() {
        assert_eq!(format_hz(9.99), "9.99 Hz");
        assert_eq!(format_hz(10.0), "10.0 Hz");
        assert_eq!(format_hz(99.9), "99.9 Hz");
        assert_eq!(format_hz(100.0), "100 Hz");
        assert_eq!(format_hz(999.0), "999 Hz");
        assert_eq!(format_hz(999.9), "1000 Hz");
    }

    #[test]
    fn malformed_frequencies_render_as_a_placeholder_not_nan() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(format_hz(value), HZ_PLACEHOLDER);
        }
        assert_eq!(format_hz(0.0), "0.00 Hz");
        assert_eq!(format_hz(-50.0), "-50.0 Hz");
    }

    #[test]
    fn finite_values_pass_through() {
        assert_eq!(finite_or(0.25, 1.0), 0.25);
    }

    #[test]
    fn non_finite_values_use_fallback() {
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(finite_or(value, 0.0), 0.0);
        }
    }

    #[test]
    fn finite_or_f64_matches_the_f32_contract() {
        assert_eq!(finite_or_f64(0.25, 1.0), 0.25);
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(finite_or_f64(value, 0.0), 0.0);
        }
    }
}
