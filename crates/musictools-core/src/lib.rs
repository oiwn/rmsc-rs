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

#[cfg(test)]
mod tests {
    use super::*;

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
