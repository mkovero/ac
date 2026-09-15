//! The single source of truth for stimulus levels (#459).
//!
//! Before this module every emitting command carried its own hardcoded
//! default, they disagreed with each other (−6 dBFS to −20 dBFS to a
//! 0 dBFS ramp top), and the only thing standing between a bare command
//! and full scale was a configurable, silently-lower-than-its-own-defaults
//! `drive_max_dbfs` clamp. A default is the level a user gets by *not*
//! deciding, so a disagreeing set of them is the silent-default class this
//! module exists to close.
//!
//! Six constants, one pure check. The daemon is the single chokepoint
//! that calls [`check_emission_level`] / [`check_emission_level_named`] on
//! every request that can emit (`ac-daemon/src/handlers/mod.rs`); nothing
//! here talks to a config, a socket, or an engine — the maximum is a build
//! constant, not a setting (operator, 2026-09-15: "less configurable /
//! settable limits and more singular global ones").

/// The single default stimulus level, in dBFS, every emitting command
/// takes when no level is typed. Provenance: assumed (operator-chosen,
/// 2026-09-15 — "something like -40dBFS").
pub const DEFAULT_LEVEL_DBFS: f64 = -40.0;

/// Default start of a level ramp (`plot level`, `generate level`).
/// Provenance: assumed (operator-chosen, 2026-09-15).
pub const DEFAULT_RAMP_START_DBFS: f64 = -40.0;

/// Default end of a level ramp. Provenance: assumed (operator-chosen,
/// 2026-09-15 — "something like -40 to -30"). It sits exactly at the
/// build-time bound for levels that play without being typed.
pub const DEFAULT_RAMP_STOP_DBFS: f64 = -30.0;

/// The one global emission ceiling: digital full scale. Not settable — see
/// the module doc.
/// A request whose level (or, for a ramp, whichever endpoint) is above
/// this is refused, never clamped: refusal is the only outcome under
/// which the typed level, the emitted level and the printed level are
/// the same number by construction.
///
/// Provenance: assumed (operator ruling, 2026-09-15). The earlier −20 dBFS
/// value tried to bound typed mistakes. The ruling moved that protection
/// off the runtime maximum and onto [`UNTYPED_LEVEL_MAX_DBFS`]: an explicit
/// request now plays exactly as typed up to full scale.
pub const MAX_EMISSION_DBFS: f64 = 0.0;

/// Build-time bound for every level that can play without a typed value.
/// It is not a runtime check or wire field: defaults are resolved before the
/// daemon can distinguish them from typed values. Provenance: assumed
/// (operator ruling, 2026-09-15).
pub const UNTYPED_LEVEL_MAX_DBFS: f64 = -30.0;

/// Level used by the daemon's self-tests (`test_hardware`'s fixed tones,
/// `test_dut`'s fixed-level checks) in place of the levels they used to
/// hardcode above the untyped bound. Provenance: assumed — it sits at the
/// operator-chosen untyped bound to retain as much test SNR as allowed.
pub const SELF_TEST_LEVEL_DBFS: f64 = -30.0;

/// Refuse a level above [`MAX_EMISSION_DBFS`], or one that is not finite.
/// `Ok` echoes the value back unchanged, so a caller can chain this
/// straight into the value it is about to use.
pub fn check_emission_level(dbfs: f64) -> Result<f64, String> {
    check_emission_level_named("", dbfs)
}

/// Same check, with a label prepended to the error text — used for a
/// ramp's two endpoints, where the error has to name which one failed
/// (`"start level ..."` / `"stop level ..."`) rather than leaving the
/// operator to guess which bound to edit. `label` is a bare word; pass
/// `""` for a scalar level (see [`check_emission_level`]), which folds
/// down to the same message with no leading word.
pub fn check_emission_level_named(label: &str, dbfs: f64) -> Result<f64, String> {
    let prefix = if label.is_empty() {
        String::new()
    } else {
        format!("{label} ")
    };
    if !dbfs.is_finite() {
        return Err(format!("{prefix}level {dbfs} dBFS is not a finite number"));
    }
    if dbfs > MAX_EMISSION_DBFS {
        return Err(format!(
            "{prefix}level +{dbfs:.1} dBFS is above full scale ({MAX_EMISSION_DBFS:.1} dBFS)"
        ));
    }
    Ok(dbfs)
}

/// Refuse a level ramp when either endpoint is above the maximum. A
/// linear ramp never exceeds its larger endpoint, so checking both bounds
/// bounds every point on it — the per-point clamp the ramp workers used
/// to carry is no longer needed once launch refuses out of range.
pub fn check_emission_range(start_dbfs: f64, stop_dbfs: f64) -> Result<(f64, f64), String> {
    let start = check_emission_level_named("start", start_dbfs)?;
    let stop = check_emission_level_named("stop", stop_dbfs)?;
    Ok((start, stop))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- coupling test ----
    //
    // Red cases (spec): restore `SELF_TEST_LEVEL_DBFS` to −20, or set the
    // ramp stop to −29; either exceeds the untyped bound.
    #[test]
    fn untyped_bound_sits_between_defaults_and_the_maximum() {
        assert!(DEFAULT_LEVEL_DBFS <= UNTYPED_LEVEL_MAX_DBFS);
        assert!(DEFAULT_RAMP_START_DBFS <= UNTYPED_LEVEL_MAX_DBFS);
        assert!(DEFAULT_RAMP_STOP_DBFS <= UNTYPED_LEVEL_MAX_DBFS);
        assert!(SELF_TEST_LEVEL_DBFS <= UNTYPED_LEVEL_MAX_DBFS);
        assert!(UNTYPED_LEVEL_MAX_DBFS <= MAX_EMISSION_DBFS);
        assert!(DEFAULT_RAMP_START_DBFS <= DEFAULT_RAMP_STOP_DBFS);
    }

    #[test]
    fn passes_through_at_or_below_the_maximum() {
        assert_eq!(
            check_emission_level(MAX_EMISSION_DBFS),
            Ok(MAX_EMISSION_DBFS)
        );
        assert_eq!(check_emission_level(-40.0), Ok(-40.0));
    }

    #[test]
    fn refuses_above_the_maximum() {
        let err = check_emission_level(0.1).unwrap_err();
        assert!(err.contains("+0.1"));
        assert!(err.contains("0.0"));
    }

    #[test]
    fn refuses_non_finite() {
        assert!(check_emission_level(f64::NAN).is_err());
        assert!(check_emission_level(f64::INFINITY).is_err());
    }

    #[test]
    fn range_names_which_endpoint_failed() {
        let err = check_emission_range(-40.0, 0.1).unwrap_err();
        assert!(err.starts_with("stop level"), "got {err:?}");

        let err = check_emission_range(0.1, -30.0).unwrap_err();
        assert!(err.starts_with("start level"), "got {err:?}");
    }

    #[test]
    fn range_passes_through_when_both_endpoints_are_in_range() {
        assert_eq!(check_emission_range(-40.0, -30.0), Ok((-40.0, -30.0)));
    }

    // ---- one understanding of level (AES17-2020 §3.12.3) ----
    //
    // At the same dBFS, every generator this daemon can emit from —
    // `generate_sine`, `generate_pink_noise`, `log_sweep` (scaled by
    // `dbfs_to_amplitude`, the same conversion every handler uses) — reads
    // the same RMS, and that RMS is `10^(dBFS/20)/√2`: the AES17-2020
    // §3.12.1 full-scale sine convention `reference_levels.rs` already
    // cites. A command whose emitted signal differs from another's for the
    // same typed dBFS value fails this test.
    //
    // `measurement::sweep` is imported here, inside `#[cfg(test)]`, so it
    // adds no runtime Tier 0 → Tier 1 dependency to this module.
    #[test]
    fn every_generator_agrees_on_what_a_dbfs_value_means() {
        use crate::measurement::sweep::{log_sweep, SweepParams};
        use crate::shared::generator::{dbfs_to_amplitude, generate_pink_noise, generate_sine_1s};

        let sr = 48_000u32;
        let dbfs = -20.0;
        let amp = dbfs_to_amplitude(dbfs);
        let expected_rms = amp / std::f64::consts::SQRT_2;

        let sine = generate_sine_1s(1000.0, amp, sr);
        let pink = generate_pink_noise(amp, sr);
        let params = SweepParams {
            f1_hz: 20.0,
            f2_hz: 20_000.0,
            duration_s: 1.0,
            sample_rate: sr,
        };
        let sweep: Vec<f32> = log_sweep(&params)
            .unwrap()
            .into_iter()
            .map(|s| s * amp as f32)
            .collect();

        let rms_of = |buf: &[f32]| -> f64 {
            (buf.iter().map(|&x| (x as f64).powi(2)).sum::<f64>() / buf.len() as f64).sqrt()
        };

        for (name, buf) in [("sine", &sine), ("pink", &pink), ("sweep", &sweep)] {
            let rms = rms_of(buf);
            let delta_db = 20.0 * (rms / expected_rms).log10();
            assert!(
                delta_db.abs() < 0.05,
                "{name} RMS {rms:.6} vs AES17 {expected_rms:.6} ({delta_db:+.3} dB)"
            );
        }
    }
}
