//! Unit conversions — Vrms ↔ dBu ↔ dBFS.
//!
//! The dBu reference voltage is runtime-configurable once at startup via
//! [`set_dbu_ref`].  All other functions are pure.

use crate::shared::constants::DBU_REF_VRMS;
use std::sync::atomic::{AtomicU64, Ordering};

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

// ---------------------------------------------------------------------------
// Speed of sound — the delay readout's ms → m conversion (#243)
// ---------------------------------------------------------------------------

/// Speed of sound in dry air at 0 °C, in m/s — the intercept of
/// [`speed_of_sound_at`].
pub const SPEED_OF_SOUND_0C_M_S: f64 = 331.3;

/// Rate of change of the speed of sound with air temperature, in m/s per °C.
pub const SPEED_OF_SOUND_PER_C: f64 = 0.606;

/// The speed of sound assumed when no room temperature is configured:
/// exactly 343 m/s, the conventional 20 °C figure the delay readout used
/// unconditionally before #243.
///
/// Deliberately the round textbook value rather than
/// `speed_of_sound_at(20.0)` (343.42 m/s). An unset temperature is not a
/// claim that the room is at 20 °C, it is the absence of a claim, and the
/// conventional figure says that more honestly than a derived one carrying
/// two decimals it has not earned.
pub const SPEED_OF_SOUND_DEFAULT_M_S: f64 = 343.0;

/// Speed of sound in dry air at `temperature_c`, in m/s.
///
/// The standard linear approximation, `331.3 + 0.606·T`, good to better
/// than 0.1 % over any temperature a measurement room is in.
///
/// The error this replaces is not academic. A room at 24–26 °C runs
/// c ≈ 346 m/s against the hardcoded 343 — 1 %, which at 1 m is 25 µs, and
/// 25 µs is 2.4 samples at 96 kHz. The conversion was therefore wrong by
/// more than the measurement's own resolution, which is the bar for whether
/// a constant may be baked in.
pub fn speed_of_sound_at(temperature_c: f64) -> f64 {
    SPEED_OF_SOUND_0C_M_S + SPEED_OF_SOUND_PER_C * temperature_c
}

/// The speed of sound a configured room temperature implies, falling back
/// to [`SPEED_OF_SOUND_DEFAULT_M_S`] when none is set.
///
/// The single place `Option<temperature>` becomes a speed, so the daemon
/// and any report writer cannot disagree about what an unset temperature
/// means.
pub fn speed_of_sound_from_config(temperature_c: Option<f64>) -> f64 {
    temperature_c
        .map(speed_of_sound_at)
        .unwrap_or(SPEED_OF_SOUND_DEFAULT_M_S)
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

    // ── speed of sound (#243) ─────────────────────────────────────────

    #[test]
    fn speed_of_sound_tracks_temperature() {
        // The two figures #243 names: ~343 m/s at 20 °C, ~346 at the
        // 24–26 °C the rig room actually runs.
        assert_relative_eq!(speed_of_sound_at(20.0), 343.42, epsilon = 0.01);
        assert_relative_eq!(speed_of_sound_at(25.0), 346.45, epsilon = 0.01);
        // Linear, so a 1 °C error is a 0.606 m/s error — the property that
        // makes a temperature typo cheap and a hardcoded constant not.
        assert_relative_eq!(
            speed_of_sound_at(21.0) - speed_of_sound_at(20.0),
            0.606,
            epsilon = 1e-9
        );
    }

    #[test]
    fn unset_temperature_is_the_conventional_speed_not_a_derived_one() {
        // Distinct from speed_of_sound_at(20.0) = 343.42 on purpose: an
        // absent temperature is an absent claim, not a 20 °C claim.
        assert_relative_eq!(
            speed_of_sound_from_config(None),
            SPEED_OF_SOUND_DEFAULT_M_S,
            epsilon = 1e-12
        );
        assert_ne!(speed_of_sound_from_config(None), speed_of_sound_at(20.0));
    }

    #[test]
    fn a_configured_temperature_wins_over_the_default() {
        assert_relative_eq!(
            speed_of_sound_from_config(Some(25.0)),
            346.45,
            epsilon = 0.01
        );
    }

    #[test]
    fn the_temperature_error_exceeds_the_estimators_resolution() {
        // Why this is a defect and not a rounding preference. At 96 kHz one
        // sample is 10.4 µs; over 1 m the 343-vs-346 disagreement is larger
        // than that, so the old constant was wrong by more than the
        // measurement could resolve.
        let sample_period_s = 1.0 / 96_000.0;
        let t_at_343 = 1.0 / SPEED_OF_SOUND_DEFAULT_M_S;
        let t_at_25c = 1.0 / speed_of_sound_at(25.0);
        assert!(
            (t_at_343 - t_at_25c).abs() > sample_period_s,
            "the constant's error is below one sample; it would not be worth parameterising"
        );
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
