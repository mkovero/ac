//! Tier 1 — ITU-R BS.1770-5 loudness measurement.
//!
//! The pieces sit one per submodule; this file holds what more than one
//! of them needs — the LKFS conversions, the gate thresholds, and the
//! planar-push validation — and re-exports the public surface.
//!
//! - `kweighting`: the K-weighting cascade (Annex 1 §2.1 pre-filter +
//!   §2.2 RLB) and the 400 ms / 100 ms-step gating-block accumulator
//!   (§2.3).
//! - `truepeak`: true-peak metering through the 4-phase 48-tap polyphase
//!   FIR of Annex 2 Table 1. Each input sample produces four oversampled
//!   outputs; the largest absolute value over the stream is reported as
//!   dBTP.
//! - `histogram`: the bounded history the gated statistics read.
//! - `state`: the multi-channel [`LoudnessState`] aggregator — momentary
//!   (LKFS-M, 400 ms sliding), short-term (LKFS-S, 3 s sliding) and
//!   integrated (LKFS-I, two-pass gated) per §2.3–§2.4 with channel
//!   weights for mono and stereo, plus loudness range per EBU Tech 3342:
//!   the short-term values two-pass gated (absolute −70 LUFS, relative
//!   −20 LU), reporting the 95th minus the 10th percentile of the
//!   survivors in LU.
//!
//! The gated statistics (integrated, LRA, gated duration) read their
//! history from a fixed-width LKFS histogram rather than a growing list
//! of values. They are queried once per monitor emit tick per channel,
//! so a per-value history made them cost O(session length) on every
//! tick; the histogram answers each in bounded time and bounded memory,
//! at 0.1 LU resolution. The exact per-value computation survives as the
//! reference the histogram is tested against.
//!
//! The K-weighting coefficients are re-derived at runtime from the two
//! closed-form biquad designs BS.1770 uses — a custom Vh/Vb high-shelf
//! for the pre-filter (not a plain RBJ cookbook shelf) and an
//! un-normalized 1/-2/1 high-pass for the RLB stage. Their parameters are
//! chosen so that at fs = 48 kHz the coefficients reproduce Annex 1
//! Table 1 exactly; at other rates (44.1 / 88.2 / 96 / 192 kHz) the same
//! formulas give a consistent K curve without rate-specific lookup
//! tables. A unit test locks the 48 kHz derivation against Annex 1.
//! Reference implementations: libebur128, ffmpeg's `af_ebur128`.

use anyhow::{bail, Result};

use crate::measurement::report::StandardsCitation;

mod histogram;
mod kweighting;
mod state;
#[cfg(test)]
mod test_support;
mod truepeak;

pub use kweighting::{GatingBlock, KWeighting};
pub use state::{LoudnessState, WEIGHT_FRONT, WEIGHT_LFE, WEIGHT_SURROUND};
pub use truepeak::TruePeak;

/// LKFS formula offset, BS.1770-5 §2.5: `L = -0.691 + 10·log10(MS_K)`.
/// Compensates for K-weighting's ~+0.691 dB gain at the 1 kHz reference so
/// a 0 dBFS 1 kHz sine gives −3.01 LKFS.
pub const LKFS_OFFSET_DB: f64 = -0.691;

/// Gating block timing, BS.1770-5 §2.3. 400 ms window, 75 % overlap
/// (→ 100 ms step).
const BLOCK_DURATION_S: f64 = 0.400;
const BLOCK_STEP_S: f64 = 0.100;

// The gate thresholds live here rather than in `state` because
// `histogram` bins against the absolute gate and `state` applies the
// relative ones; a single definition keeps the bin floor and the gate
// that decides admission from drifting apart.
/// Absolute gating threshold on block LKFS, BS.1770-5 §2.4.
const ABSOLUTE_GATE_LKFS: f64 = -70.0;
/// Relative gate delta below ungated loudness, BS.1770-5 §2.4.
const RELATIVE_GATE_DELTA_LU: f64 = -10.0;
/// Relative gate delta for loudness range, EBU Tech 3342 §2.2.
const LRA_RELATIVE_GATE_DELTA_LU: f64 = -20.0;
/// LRA low / high percentiles, EBU Tech 3342 §2.2.
const LRA_LOW_PERCENTILE: f64 = 0.10;
const LRA_HIGH_PERCENTILE: f64 = 0.95;

/// Convert a (K-weighted) mean-square to LKFS using the BS.1770-5 §2.5
/// offset. Mono / single-channel usage passes `ms` directly; multichannel
/// callers pre-compute the channel-weighted sum of mean-squares per §2.4
/// and pass that. `ms ≤ 0` maps to `-∞` (silence).
pub fn ms_to_lkfs(ms: f64) -> f64 {
    if ms <= 0.0 {
        f64::NEG_INFINITY
    } else {
        LKFS_OFFSET_DB + 10.0 * ms.log10()
    }
}

/// Invert `ms_to_lkfs`: given an LKFS threshold, return the mean-square
/// level that corresponds to it.
fn lkfs_to_ms(lkfs: f64) -> f64 {
    10.0_f64.powf((lkfs - LKFS_OFFSET_DB) / 10.0)
}

/// Mean-square ratio for a level shift of `lu` LU. The LKFS offset
/// cancels, so `lkfs_to_ms(ms_to_lkfs(ms) + lu)` collapses to
/// `ms * lu_ratio(lu)` — one multiply instead of a log10/powf round-trip,
/// and no `ms <= 0` step through `-inf`.
fn lu_ratio(lu: f64) -> f64 {
    10.0_f64.powf(lu / 10.0)
}

/// Validate a planar push: `channels.len()` must equal `expected` and
/// every slice must have the same length. Returns that common length
/// (`0` when there are no channels).
fn check_planar(channels: &[&[f32]], expected: usize) -> Result<usize> {
    if channels.len() != expected {
        bail!("expected {expected} channels, got {}", channels.len());
    }
    let Some(first) = channels.first() else {
        return Ok(0);
    };
    let len = first.len();
    for (i, ch) in channels.iter().enumerate().skip(1) {
        if ch.len() != len {
            bail!(
                "channel {i} length {} mismatches channel 0 ({len})",
                ch.len()
            );
        }
    }
    Ok(len)
}

pub fn citation() -> StandardsCitation {
    // Verified against the EBU Tech 3341 cases 1-4 and 9 and the
    // Tech 3342 constant-tone case via synthesised stimuli to ±0.1 LU
    // (see `tests` module). Clause numbers audited against the
    // authoritative ITU-R BS.1770-5 PDF.
    StandardsCitation {
        standard: "ITU-R BS.1770-5 / EBU Tech 3342".into(),
        clause:
            "BS.1770 Annex 1 pre-filter + RLB weighting + gating; Annex 2 true-peak; Tech 3342 §2.2 LRA"
                .into(),
        verified: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ms_to_lkfs_silence_is_neg_infinity() {
        assert_eq!(ms_to_lkfs(0.0), f64::NEG_INFINITY);
        assert_eq!(ms_to_lkfs(-1e-20), f64::NEG_INFINITY);
    }

    #[test]
    fn ms_to_lkfs_roundtrip() {
        // LKFS(1.0) = -0.691 + 0 = -0.691
        assert!((ms_to_lkfs(1.0) - -0.691).abs() < 1e-12);
        // LKFS(0.5) = -0.691 - 3.0103 = -3.7013 — matches the value a
        // pre-filter (unity at HF, not 0.691 dB boost) would yield.
        assert!((ms_to_lkfs(0.5) - (-0.691 - 10.0 * 2.0_f64.log10())).abs() < 1e-12);
    }

    #[test]
    fn lkfs_to_ms_is_inverse_of_ms_to_lkfs() {
        for &lkfs in &[-70.0_f64, -23.0, -14.0, -3.01, 0.0] {
            let ms = lkfs_to_ms(lkfs);
            let back = ms_to_lkfs(ms);
            assert!(
                (back - lkfs).abs() < 1e-9,
                "roundtrip lkfs={lkfs}, got {back}"
            );
        }
    }

    #[test]
    fn lu_ratio_matches_the_lkfs_round_trip_it_replaced() {
        // `lu_ratio` replaced `lkfs_to_ms(ms_to_lkfs(ms) + lu)`. Compute
        // that replaced expression here so any divergence goes red
        // rather than silently shifting a gate threshold.
        for &ms in &[1e-9_f64, 1e-4, 0.25, 0.5, 1.0, 3.7] {
            for &lu in &[RELATIVE_GATE_DELTA_LU, LRA_RELATIVE_GATE_DELTA_LU, 0.0, 4.5] {
                let replaced = lkfs_to_ms(ms_to_lkfs(ms) + lu);
                let current = ms * lu_ratio(lu);
                assert!(
                    (replaced - current).abs() <= 1e-12 * replaced.max(current),
                    "ms={ms:e} lu={lu}: round-trip {replaced:e} vs lu_ratio {current:e}"
                );
            }
        }
    }

    #[test]
    fn check_planar_reports_shape_errors() {
        let a = vec![0.0_f32; 8];
        let b = vec![0.0_f32; 7];
        assert_eq!(check_planar(&[&a, &a], 2).unwrap(), 8);
        assert_eq!(check_planar(&[], 0).unwrap(), 0);
        assert!(check_planar(&[&a], 2).is_err());
        assert!(check_planar(&[&a, &b], 2).is_err());
    }

    #[test]
    fn citation_shape() {
        let c = citation();
        assert!(
            c.standard.contains("ITU-R BS.1770-5"),
            "got standard = {}",
            c.standard
        );
        assert!(c.clause.contains("Annex 1"));
        assert!(c.clause.contains("Annex 2"));
        assert!(c.verified);
    }
}
