//! The single linear→dB conversion site (handoff: ac-scene M2, structural
//! rule 1). `meas_spectrum`/`ref_spectrum` arrive linear on both the wire
//! and the snapshot derivation (D18) — this is the one `log10` in the
//! crate; M3's renderer must never need another.

use ac_core::shared::reference_levels::MIN_DBFS;

/// `20·log10(amp)`, floored at [`MIN_DBFS`] for zero/negative input —
/// same floor convention `measurement::ccir468` already uses, not a new
/// constant invented for this crate.
pub fn linear_to_dbfs(amp: f64) -> f64 {
    if amp <= 0.0 {
        return MIN_DBFS;
    }
    (20.0 * amp.log10()).max(MIN_DBFS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unity_amplitude_is_zero_db() {
        assert!((linear_to_dbfs(1.0) - 0.0).abs() < 1e-9);
    }

    #[test]
    fn zero_and_negative_floor_at_min_dbfs() {
        assert_eq!(linear_to_dbfs(0.0), MIN_DBFS);
        assert_eq!(linear_to_dbfs(-1.0), MIN_DBFS);
    }

    #[test]
    fn half_amplitude_is_minus_6db_class() {
        // 20*log10(0.5) = -6.0206
        assert!((linear_to_dbfs(0.5) - (-6.0206)).abs() < 0.001);
    }
}
