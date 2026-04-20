//! Unit conversions — Vrms ↔ dBu ↔ dBFS.
//!
//! The dBu reference voltage is runtime-configurable once at startup via
//! [`set_dbu_ref`].  All other functions are pure.

use std::sync::atomic::{AtomicU64, Ordering};
use crate::constants::DBU_REF_VRMS;

// Store the dBu reference as raw f64 bits so we can use an atomic.
// Initialised to DBU_REF_VRMS; f64::to_bits is const since Rust 1.58.
static DBU_REF_BITS: AtomicU64 = AtomicU64::new(DBU_REF_VRMS.to_bits());

/// Override the 0 dBu reference voltage.  Call once at startup after
/// loading config; safe to call from any thread.
pub fn set_dbu_ref(vrms: f64) {
    DBU_REF_BITS.store(vrms.to_bits(), Ordering::Relaxed);
}

/// Return the currently configured 0 dBu reference voltage (Vrms).
pub fn get_dbu_ref() -> f64 {
    f64::from_bits(DBU_REF_BITS.load(Ordering::Relaxed))
}

/// Vrms → dBu relative to the configured reference.
pub fn vrms_to_dbu(vrms: f64) -> f64 {
    20.0 * (vrms.max(1e-12) / get_dbu_ref()).log10()
}

/// dBu → Vrms relative to the configured reference.
pub fn dbu_to_vrms(dbu: f64) -> f64 {
    get_dbu_ref() * 10.0_f64.powf(dbu / 20.0)
}

/// dBFS + full-scale calibration voltage → Vrms.
pub fn dbfs_to_vrms(dbfs: f64, vrms_at_0dbfs: f64) -> f64 {
    vrms_at_0dbfs * 10.0_f64.powf(dbfs / 20.0)
}

/// Vrms → dBV. 0 dBV = 1.0 Vrms by definition (unlike dBu, this reference
/// is fixed by the standard and not runtime-configurable).
pub fn vrms_to_dbv(vrms: f64) -> f64 {
    20.0 * vrms.max(1e-12).log10()
}

/// dBV → Vrms.
pub fn dbv_to_vrms(dbv: f64) -> f64 {
    10.0_f64.powf(dbv / 20.0)
}

/// Convert a dBu reading to dBV. Derivation:
///   dBV = 20·log10(Vrms / 1.0)
///   dBu = 20·log10(Vrms / V_ref_dbu)
///   dBV = dBu + 20·log10(V_ref_dbu)
/// With the default `DBU_REF_EXACT = sqrt(0.6)`, the offset is
/// `-10·log10(5/3) ≈ -2.21848749616357` dB.
pub fn dbu_to_dbv(dbu: f64) -> f64 {
    dbu + 20.0 * get_dbu_ref().log10()
}

/// Convert a dBV reading to dBu (inverse of [`dbu_to_dbv`]).
pub fn dbv_to_dbu(dbv: f64) -> f64 {
    dbv - 20.0 * get_dbu_ref().log10()
}

/// Vrms → peak-to-peak voltage (sinusoidal signal).
pub fn vrms_to_vpp(vrms: f64) -> f64 {
    vrms * 2.0 * std::f64::consts::SQRT_2
}

/// Auto-scale Vrms to human-readable string (mVrms or Vrms).
pub fn fmt_vrms(vrms: f64) -> String {
    if vrms < 1.0 {
        format!("{:.3} mVrms", vrms * 1000.0)
    } else {
        format!("{:.4} Vrms", vrms)
    }
}

/// Auto-scale Vpp to human-readable string (mVpp or Vpp).
pub fn fmt_vpp(vrms: f64) -> String {
    let vpp = vrms_to_vpp(vrms);
    if vpp < 1.0 {
        format!("{:.2} mVpp", vpp * 1000.0)
    } else {
        format!("{:.4} Vpp", vpp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;

    #[test]
    fn round_trip_dbu_vrms() {
        let dbu = 4.0_f64;
        let vrms = dbu_to_vrms(dbu);
        assert_relative_eq!(vrms_to_dbu(vrms), dbu, epsilon = 1e-10);
    }

    #[test]
    fn dbfs_to_vrms_unity() {
        // 0 dBFS at 1.0 V full scale → 1.0 Vrms
        assert_relative_eq!(dbfs_to_vrms(0.0, 1.0), 1.0, epsilon = 1e-12);
        // -6 dBFS at 1.0 V full scale → ~0.5012 Vrms
        let expected = 10.0_f64.powf(-6.0 / 20.0);
        assert_relative_eq!(dbfs_to_vrms(-6.0, 1.0), expected, epsilon = 1e-10);
    }

    #[test]
    fn fmt_vrms_scales() {
        assert!(fmt_vrms(0.001).contains("mVrms"));
        assert!(fmt_vrms(1.5).contains("Vrms"));
    }

    // ── dBV ───────────────────────────────────────────────────────────

    #[test]
    fn vrms_to_dbv_unity_is_zero() {
        // 0 dBV is defined as 1.0 Vrms.
        assert_relative_eq!(vrms_to_dbv(1.0), 0.0, epsilon = 1e-12);
    }

    #[test]
    fn vrms_to_dbv_known_values() {
        // 0.5 Vrms → -6.02 dBV
        assert_relative_eq!(vrms_to_dbv(0.5), -6.020599913279624, epsilon = 1e-9);
        // 2.0 Vrms → +6.02 dBV
        assert_relative_eq!(vrms_to_dbv(2.0), 6.020599913279624, epsilon = 1e-9);
    }

    #[test]
    fn round_trip_dbv_vrms() {
        for dbv in [-40.0, -3.0, 0.0, 4.0, 12.5] {
            let v = dbv_to_vrms(dbv);
            assert_relative_eq!(vrms_to_dbv(v), dbv, epsilon = 1e-10);
        }
    }

    #[test]
    fn dbu_to_dbv_offset_is_exact() {
        // The offset is exactly 20·log10(V_ref_dbu). The default reference
        // is DBU_REF_VRMS = 0.7746, so the offset ≈ −2.21836 dB (for the
        // mathematically exact sqrt(0.6) reference it would be −2.21849 dB,
        // a difference of 0.0001 dB). Tie the expected value to the
        // runtime reference so the test tracks any config override.
        let expected = 20.0 * get_dbu_ref().log10();
        assert_relative_eq!(dbu_to_dbv(0.0), expected, epsilon = 1e-12);
        assert_relative_eq!(dbv_to_dbu(0.0), -expected, epsilon = 1e-12);
        // Sanity bound: whatever the configured reference, the offset must
        // be in the expected neighbourhood of −2.218 dB ± a few mdB.
        assert!(
            (-2.22..-2.21).contains(&expected),
            "offset {expected} dB not near −2.218"
        );
    }

    #[test]
    fn dbu_to_dbv_round_trip() {
        for dbu in [-20.0, -4.0, 0.0, 4.0, 24.0] {
            assert_relative_eq!(dbv_to_dbu(dbu_to_dbv(dbu)), dbu, epsilon = 1e-10);
        }
    }

    #[test]
    fn dbfs_dbu_dbv_consistent_at_cal_point() {
        // If the input is calibrated so that 0 dBFS = 1.0 Vrms, then a
        // -6 dBFS signal converts to 10^(-6/20) = 0.5011872... Vrms and
        // back to exactly -6.0 dBV (since dBV's reference is 1.0 Vrms).
        let v = dbfs_to_vrms(-6.0, 1.0);
        assert_relative_eq!(v, 10f64.powf(-6.0 / 20.0), epsilon = 1e-12);
        assert_relative_eq!(vrms_to_dbv(v), -6.0, epsilon = 1e-9);
        // And for ANY voltage, dBu and dBV must differ by exactly the
        // configured offset. Equivalently: `dbu_to_dbv(vrms_to_dbu(v))`
        // must equal `vrms_to_dbv(v)` to float precision.
        let dbu = vrms_to_dbu(v);
        assert_relative_eq!(dbu_to_dbv(dbu), vrms_to_dbv(v), epsilon = 1e-9);
    }
}
