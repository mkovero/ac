//! Tier 2 — Analytic A/C/Z weighting curves for per-band dBFS display.
//!
//! Evaluates the IEC 61672-1:2013 Annex E response curves at a given
//! frequency, in dB, without instantiating the biquad cascade from
//! [`crate::measurement::weighting`]. Used by the daemon's
//! fractional-octave emitter to add a per-band dB offset before
//! publishing; the IIR cascade is still the right tool for weighting a
//! time-domain signal (see `measurement/weighting.rs`).
//!
//! **Display-only**, matching the caveat on the upstream
//! `visualize::fractional_octave` module: band energies come from a
//! Morlet CWT aggregation, not IEC 61260 filters, so the output of
//! "weight the bands" must not be quoted as an IEC 61672 SPL reading.
//! The curve formulas here are standard; the upstream data is not.
//!
//! # Formulas (IEC 61672-1:2013 Annex E)
//!
//! A-weighting:
//! ```text
//! R_A(f) = (12194²·f⁴)
//!        / ((f² + 20.6²)(f² + 12194²)·√((f² + 107.7²)(f² + 737.9²)))
//! A(f)   = 20·log10(R_A(f)) + 2.0   // normalises A(1000) = 0
//! ```
//!
//! C-weighting:
//! ```text
//! R_C(f) = (12194²·f²) / ((f² + 20.6²)(f² + 12194²))
//! C(f)   = 20·log10(R_C(f)) + 0.06  // normalises C(1000) = 0
//! ```
//!
//! Z-weighting: identically 0 dB at all frequencies.

/// Frequency-weighting curve used to shape per-band dBFS levels before
/// display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WeightingCurve {
    Z,
    A,
    C,
}

impl WeightingCurve {
    /// Parse a lowercase tag (`"z"`, `"a"`, `"c"`) into a curve. Returns
    /// `None` for any other value so the caller can reject cleanly.
    pub fn from_tag(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "z" => Some(Self::Z),
            "a" => Some(Self::A),
            "c" => Some(Self::C),
            _ => None,
        }
    }

    pub fn tag(self) -> &'static str {
        match self {
            Self::Z => "Z",
            Self::A => "A",
            Self::C => "C",
        }
    }

    /// Gain in dB at `f_hz`. `Z` is identically 0; `A` and `C` evaluate
    /// the Annex E closed form. Returns 0 for non-positive frequencies
    /// to protect callers from domain errors on a DC or negative bin.
    pub fn db_offset(self, f_hz: f64) -> f64 {
        if f_hz <= 0.0 {
            return 0.0;
        }
        match self {
            Self::Z => 0.0,
            Self::A => a_weighting_db(f_hz),
            Self::C => c_weighting_db(f_hz),
        }
    }
}

const F1: f64 = 20.598_997;
const F2: f64 = 107.65265;
const F3: f64 = 737.86223;
const F4: f64 = 12194.217;
const A_NORM_DB: f64 = 2.0;
const C_NORM_DB: f64 = 0.062;

fn a_weighting_db(f: f64) -> f64 {
    let f2 = f * f;
    let num = F4 * F4 * f2 * f2;
    let den = (f2 + F1 * F1)
        * ((f2 + F2 * F2) * (f2 + F3 * F3)).sqrt()
        * (f2 + F4 * F4);
    20.0 * (num / den).log10() + A_NORM_DB
}

fn c_weighting_db(f: f64) -> f64 {
    let f2 = f * f;
    let num = F4 * F4 * f2;
    let den = (f2 + F1 * F1) * (f2 + F4 * F4);
    20.0 * (num / den).log10() + C_NORM_DB
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64, eps: f64) -> bool {
        (a - b).abs() < eps
    }

    #[test]
    fn z_is_flat() {
        for f in [10.0, 100.0, 1_000.0, 10_000.0, 20_000.0] {
            assert_eq!(WeightingCurve::Z.db_offset(f), 0.0);
        }
    }

    #[test]
    fn a_weighting_is_zero_at_1khz() {
        assert!(
            approx(WeightingCurve::A.db_offset(1_000.0), 0.0, 0.01),
            "A(1 kHz) should be ~0 dB (got {:.4})",
            WeightingCurve::A.db_offset(1_000.0),
        );
    }

    #[test]
    fn c_weighting_is_zero_at_1khz() {
        assert!(
            approx(WeightingCurve::C.db_offset(1_000.0), 0.0, 0.01),
            "C(1 kHz) should be ~0 dB (got {:.4})",
            WeightingCurve::C.db_offset(1_000.0),
        );
    }

    #[test]
    fn a_weighting_standard_table_values() {
        // IEC 61672-1:2013 Table 2 / Annex E reference values, tolerance
        // matches the Class 1 ± range at each octave centre.
        // (f_hz, expected_db, tol)
        let cases = [
            (10.0, -70.4, 0.3),
            (100.0, -19.1, 0.1),
            (1_000.0, 0.0, 0.02),
            (10_000.0, -2.5, 0.1),
            (20_000.0, -9.34, 0.2),
        ];
        for (f, expected, tol) in cases {
            let got = WeightingCurve::A.db_offset(f);
            assert!(
                approx(got, expected, tol),
                "A({f:.0} Hz) = {got:.3} dB, expected {expected:.3} ± {tol}",
            );
        }
    }

    #[test]
    fn c_weighting_standard_table_values() {
        let cases = [
            (10.0, -14.3, 0.2),
            (100.0, -0.3, 0.1),
            (1_000.0, 0.0, 0.02),
            (10_000.0, -4.4, 0.1),
            (20_000.0, -11.2, 0.2),
        ];
        for (f, expected, tol) in cases {
            let got = WeightingCurve::C.db_offset(f);
            assert!(
                approx(got, expected, tol),
                "C({f:.0} Hz) = {got:.3} dB, expected {expected:.3} ± {tol}",
            );
        }
    }

    #[test]
    fn non_positive_frequency_returns_zero() {
        assert_eq!(WeightingCurve::A.db_offset(0.0), 0.0);
        assert_eq!(WeightingCurve::A.db_offset(-10.0), 0.0);
        assert_eq!(WeightingCurve::C.db_offset(0.0), 0.0);
    }

    #[test]
    fn tag_round_trip() {
        for (c, s) in [
            (WeightingCurve::Z, "Z"),
            (WeightingCurve::A, "A"),
            (WeightingCurve::C, "C"),
        ] {
            assert_eq!(c.tag(), s);
            assert_eq!(WeightingCurve::from_tag(&s.to_lowercase()), Some(c));
            assert_eq!(WeightingCurve::from_tag(s), Some(c)); // case-insensitive
        }
        assert_eq!(WeightingCurve::from_tag("b"), None);
        assert_eq!(WeightingCurve::from_tag("off"), None);
    }
}
