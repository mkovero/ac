//! Reference levels — the 0 dBFS convention and its analog counterparts.
//!
//! AES17-2015 §5 defines the full-scale amplitude reference used
//! throughout the Tier 1 measurement stack:
//!
//!   0 dBFS ↔ a full-scale sinusoid (peak = 1.0), whose RMS is
//!   `1/√2` and whose mean-square is `0.5`.
//!
//! Every Tier 1 module that reports a level in dBFS ultimately compares
//! against this reference. Before this module the `0.5` and `-200.0`
//! floor constants were duplicated inline across `noise.rs`,
//! `ccir468.rs`, etc. with no single citation. Pull them here.
//!
//! The analog reference conversions (Vrms ↔ dBu ↔ dBV) live in
//! [`crate::shared::conversions`]; they are re-exported here so callers
//! get a single namespace for "references and levels".

use crate::measurement::report::StandardsCitation;

pub use crate::shared::conversions::{
    dbfs_to_vrms, dbu_to_dbv, dbu_to_vrms, dbv_to_dbu, dbv_to_vrms,
    vrms_to_dbu, vrms_to_dbv,
};

/// Mean-square of a full-scale sinusoid (peak = 1.0). This is the
/// normalisation constant for mean-square → dBFS conversions in the
/// AES17-2015 §5 convention.
pub const DBFS_FULL_SCALE_SINE_MEAN_SQ: f64 = 0.5;

/// Floor for dBFS results. Values below this are clipped so callers
/// never see `-inf` or `NaN` on digital silence.
pub const MIN_DBFS: f64 = -200.0;

/// Amplitude floor used before `log10` in voltage-style dBFS
/// conversions (`20·log10(amp)`). `20·log10(1e-12) = -240 dBFS`, well
/// below [`MIN_DBFS`], so the subsequent clamp dominates.
pub const MIN_AMPLITUDE: f64 = 1e-12;

/// Convert a mean-square power reading to dBFS per AES17-2015 §5:
/// `10·log10(mean_sq / 0.5)`, clipped at [`MIN_DBFS`]. Silence
/// (`mean_sq <= 0`) returns [`MIN_DBFS`].
pub fn mean_sq_to_dbfs(mean_sq: f64) -> f64 {
    if mean_sq.is_nan() || mean_sq <= 0.0 {
        return MIN_DBFS;
    }
    (10.0 * (mean_sq / DBFS_FULL_SCALE_SINE_MEAN_SQ).log10()).max(MIN_DBFS)
}

/// Inverse of [`mean_sq_to_dbfs`].
pub fn dbfs_to_mean_sq(dbfs: f64) -> f64 {
    DBFS_FULL_SCALE_SINE_MEAN_SQ * 10.0_f64.powf(dbfs / 10.0)
}

/// Convert a voltage-style amplitude to dBFS: `20·log10(|amp|)`, with
/// the amplitude floored at [`MIN_AMPLITUDE`] to avoid `-inf`, and the
/// result clipped at [`MIN_DBFS`]. Use this when the underlying
/// quantity is an RMS voltage or an FFT-bin magnitude that already
/// equals 1.0 for a full-scale sinusoid.
pub fn amplitude_to_dbfs(amp: f64) -> f64 {
    (20.0 * amp.abs().max(MIN_AMPLITUDE).log10()).max(MIN_DBFS)
}

/// AES17-2015 — the governing reference for the 0 dBFS convention. The
/// full-scale amplitude is defined in §3 (Terms and definitions); the
/// exact sub-clause number is not accessible from the iteh.ai preview
/// under `stddocs/`, so this citation points to §3 at the clause level
/// and leaves `verified` false pending full-text access.
pub fn citation() -> StandardsCitation {
    StandardsCitation {
        standard: "AES17-2015".into(),
        clause: "§3 Terms and definitions (full-scale amplitude)".into(),
        verified: false,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;

    #[test]
    fn full_scale_sine_reads_0_dbfs() {
        assert_relative_eq!(
            mean_sq_to_dbfs(DBFS_FULL_SCALE_SINE_MEAN_SQ),
            0.0,
            epsilon = 1e-12
        );
    }

    #[test]
    fn half_amplitude_sine_reads_minus_6dbfs() {
        // sine peak 0.5 → RMS 0.5/√2 → mean_sq = 0.125.
        // 10·log10(0.125 / 0.5) = −6.02 dB.
        let ms = 0.5_f64.powi(2) / 2.0;
        assert_relative_eq!(mean_sq_to_dbfs(ms), -6.020599913279624, epsilon = 1e-9);
    }

    #[test]
    fn silence_is_floor() {
        assert_eq!(mean_sq_to_dbfs(0.0), MIN_DBFS);
        assert_eq!(mean_sq_to_dbfs(-1.0), MIN_DBFS);
        assert_eq!(mean_sq_to_dbfs(f64::NAN), MIN_DBFS);
    }

    #[test]
    fn mean_sq_round_trip() {
        for db in [-120.0, -60.0, -6.0, 0.0, 6.0] {
            let ms = dbfs_to_mean_sq(db);
            assert_relative_eq!(mean_sq_to_dbfs(ms), db, epsilon = 1e-10);
        }
    }

    #[test]
    fn dbfs_to_mean_sq_full_scale() {
        assert_relative_eq!(
            dbfs_to_mean_sq(0.0),
            DBFS_FULL_SCALE_SINE_MEAN_SQ,
            epsilon = 1e-12,
        );
    }

    #[test]
    fn amplitude_full_scale_sine_peak() {
        // A unit-amplitude sinusoid has FFT-bin amplitude 1.0 in the
        // analyzer's convention — reads 0 dBFS.
        assert_relative_eq!(amplitude_to_dbfs(1.0), 0.0, epsilon = 1e-12);
    }

    #[test]
    fn amplitude_silence_is_floor() {
        assert_eq!(amplitude_to_dbfs(0.0), MIN_DBFS);
    }

    #[test]
    fn dbu_reference_is_0_7746v() {
        // 0 dBu = V_ref (default ≈ 0.7746 Vrms, i.e. √0.6 on the 600 Ω
        // reference). Round-trip checks live in conversions.rs; here we
        // just smoke-test that the re-export is wired up.
        assert_relative_eq!(dbu_to_vrms(0.0), 0.7746, epsilon = 1e-3);
    }

    #[test]
    fn dbv_reference_is_1v() {
        assert_relative_eq!(dbv_to_vrms(0.0), 1.0, epsilon = 1e-12);
    }

    #[test]
    fn citation_is_aes17() {
        let c = citation();
        assert!(c.standard.contains("AES17"));
        assert!(c.clause.contains("§3"));
        assert!(!c.verified);
    }
}
