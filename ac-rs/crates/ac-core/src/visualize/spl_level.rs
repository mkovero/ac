//! Tier 2 — broadband weighted level from a linear-amplitude spectrum.
//!
//! The scalar companion to the per-band `fractional_octave` +
//! `time_integration` path: instead of per-band levels, sums weighted
//! power across an entire linear-amplitude half-spectrum into one
//! broadband dBFS reading, ready for `EmaIntegrator`/`LeqIntegrator`
//! (`n_bands = 1`). Composes [`WeightingCurve`] and the existing
//! integrators rather than re-deriving weighting or integration math —
//! one math truth (handoff: transfer-frame-v2 M0).
//!
//! **Display-only**, same caveat as the upstream `fractional_octave` /
//! `time_integration` modules: this is a Welch/FFT band-power sum, not
//! an IEC 61672 filterbank + true-RMS SPL meter. The time constants and
//! weighting formulas match the standards; the upstream levels do not.

use super::weighting_curves::WeightingCurve;

/// Floor for dB output when the input spectrum carries no energy.
/// Matches `time_integration`'s `MIN_DBFS` convention.
const MIN_DBFS: f64 = -200.0;

/// Sum per-bin power (weighting-offset applied) across a linear-amplitude
/// half-spectrum, return one broadband dBFS reading.
///
/// `spectrum_amp` and `freqs` must be the same length — `spectrum_amp[k]`
/// is the linear amplitude at `freqs[k]` Hz (peak-normalized convention,
/// see `spectrum::spectrum_only`). Bins are summed as power
/// (`amp² · 10^(weighting.db_offset(f)/10)`), the same band-power
/// statistic used everywhere else on this wire (`visualize::aggregate`) —
/// N-independent for a fixed total signal power regardless of bin count.
/// Returns `MIN_DBFS` for empty, mismatched-length, or all-silent input.
pub fn weighted_broadband_dbfs(
    spectrum_amp: &[f64],
    freqs: &[f64],
    weighting: WeightingCurve,
) -> f64 {
    if spectrum_amp.is_empty() || spectrum_amp.len() != freqs.len() {
        return MIN_DBFS;
    }
    let power_sum: f64 = spectrum_amp
        .iter()
        .zip(freqs)
        .map(|(&amp, &f)| {
            let w_pow = 10f64.powf(weighting.db_offset(f) / 10.0);
            amp * amp * w_pow
        })
        .sum();
    if power_sum > 0.0 && power_sum.is_finite() {
        (10.0 * power_sum.log10()).max(MIN_DBFS)
    } else {
        MIN_DBFS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn z_weighting_full_scale_tone_is_0dbfs() {
        let freqs = vec![1_000.0];
        let amp = vec![1.0];
        let db = weighted_broadband_dbfs(&amp, &freqs, WeightingCurve::Z);
        assert!((db - 0.0).abs() < 1e-6, "got {db}");
    }

    #[test]
    fn a_weighting_at_1khz_is_unity_gain() {
        let freqs = vec![1_000.0];
        let amp = vec![1.0];
        let db_z = weighted_broadband_dbfs(&amp, &freqs, WeightingCurve::Z);
        let db_a = weighted_broadband_dbfs(&amp, &freqs, WeightingCurve::A);
        assert!(
            (db_z - db_a).abs() < 0.02,
            "A(1 kHz) should be ~0 dB offset: z={db_z} a={db_a}"
        );
    }

    #[test]
    fn a_weighting_attenuates_low_frequency_tone() {
        let freqs = vec![50.0];
        let amp = vec![1.0];
        let db_z = weighted_broadband_dbfs(&amp, &freqs, WeightingCurve::Z);
        let db_a = weighted_broadband_dbfs(&amp, &freqs, WeightingCurve::A);
        assert!(
            db_a < db_z - 10.0,
            "A-weighting should attenuate a 50 Hz tone: z={db_z} a={db_a}"
        );
    }

    /// Anti-"dual trace" invariant (D18): the same total power split
    /// across a different number of bins reads the same broadband level.
    #[test]
    fn n_independent_broadband_power_sum() {
        let build = |n: usize| -> (Vec<f64>, Vec<f64>) {
            let per_bin_pow = 1.0 / n as f64;
            let amp = per_bin_pow.sqrt();
            (
                (0..n).map(|k| 100.0 + k as f64 * 10.0).collect(),
                vec![amp; n],
            )
        };
        let (f8, a8) = build(8);
        let (f64_, a64) = build(64);
        let db8 = weighted_broadband_dbfs(&a8, &f8, WeightingCurve::Z);
        let db64 = weighted_broadband_dbfs(&a64, &f64_, WeightingCurve::Z);
        assert!((db8 - db64).abs() < 0.01, "db8={db8} db64={db64}");
    }

    #[test]
    fn empty_input_floors() {
        assert_eq!(
            weighted_broadband_dbfs(&[], &[], WeightingCurve::Z),
            MIN_DBFS
        );
    }

    #[test]
    fn mismatched_lengths_floor() {
        assert_eq!(
            weighted_broadband_dbfs(&[1.0, 2.0], &[100.0], WeightingCurve::Z),
            MIN_DBFS
        );
    }
}
