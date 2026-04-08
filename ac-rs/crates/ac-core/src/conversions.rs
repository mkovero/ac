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
}
