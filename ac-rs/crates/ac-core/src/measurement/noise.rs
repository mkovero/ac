//! Tier 1 — AES17-2015 idle-channel noise measurement.
//!
//! Computes unweighted and A-weighted RMS noise levels over the
//! supplied signal, expressed in dBFS. 0 dBFS ↔ full-scale sine
//! (RMS = 1/√2, mean-square = 0.5), matching the convention used
//! throughout the Tier 1 measurement stack.
//!
//! CCIR-468 (ITU-R BS.468-4) weighted quasi-peak measurement is delegated
//! to [`crate::measurement::ccir468`].
//!
//! The duration of the input signal must be long enough for the
//! weighting filter's transient to settle; callers can use
//! [`measure_noise`] directly on a pre-recorded idle-channel buffer
//! and the implementation skips the first 100 ms before integrating
//! power.

use anyhow::{bail, Result};

use crate::measurement::ccir468;
use crate::measurement::report::StandardsCitation;
use crate::measurement::weighting::{Weighting, WeightingFilter};

/// Result of an AES17 idle-channel noise measurement.
#[derive(Debug, Clone, PartialEq)]
pub struct NoiseMetrics {
    pub sample_rate_hz: u32,
    pub duration_s: f64,
    /// Broadband RMS of the input in dBFS, computed after a 100 ms
    /// settling skip to avoid including a leading DC or pop.
    pub unweighted_dbfs: f64,
    /// A-weighted RMS in dBFS — the input is run through an
    /// IEC 61672-1 A-weighting filter, and the same settling skip is
    /// applied.
    pub a_weighted_dbfs: f64,
    /// CCIR-468 (ITU-R BS.468-4) weighted quasi-peak level in dBFS —
    /// filter per §1, two-stage QP detector per §2. Referenced to a
    /// full-scale 1 kHz sine (0 dBFS).
    pub ccir_weighted_dbfs: f64,
}

const SETTLE_SECS: f64 = 0.1;
/// Floor in dBFS below which results are clipped. Protects callers
/// from `-inf` on digital silence.
const MIN_DBFS: f64 = -200.0;

/// Measure idle-channel noise on `samples` at `sample_rate`. The
/// returned `NoiseMetrics` reports both the unweighted and A-weighted
/// RMS in dBFS, with values clipped at [`MIN_DBFS`].
pub fn measure_noise(samples: &[f32], sample_rate: u32) -> Result<NoiseMetrics> {
    if sample_rate == 0 {
        bail!("sample_rate must be positive");
    }
    if samples.is_empty() {
        bail!("samples must be non-empty");
    }
    let fs = sample_rate as f64;
    let skip_n = (fs * SETTLE_SECS) as usize;
    if samples.len() <= skip_n + 1 {
        bail!(
            "need more than {} samples for settling; got {}",
            skip_n,
            samples.len()
        );
    }

    let unweighted_dbfs = rms_dbfs(&samples[skip_n..]);

    let mut aw = WeightingFilter::new(Weighting::A, sample_rate)?;
    let a_weighted = aw.apply(samples);
    let a_weighted_dbfs = rms_dbfs(&a_weighted[skip_n..]);

    let ccir_weighted_dbfs = ccir468::weighted_quasi_peak_dbfs(&samples[skip_n..], sample_rate)
        .unwrap_or(MIN_DBFS);

    let n_integrated = samples.len() - skip_n;
    Ok(NoiseMetrics {
        sample_rate_hz: sample_rate,
        duration_s: n_integrated as f64 / fs,
        unweighted_dbfs,
        a_weighted_dbfs,
        ccir_weighted_dbfs,
    })
}

/// RMS of `samples`, expressed in dBFS where 0 dBFS ↔ a full-scale
/// sine (mean-square = 0.5). Clipped at [`MIN_DBFS`].
fn rms_dbfs(samples: &[f32]) -> f64 {
    if samples.is_empty() {
        return MIN_DBFS;
    }
    let mean_sq = samples
        .iter()
        .map(|&v| (v as f64).powi(2))
        .sum::<f64>()
        / samples.len() as f64;
    if mean_sq <= 0.0 {
        return MIN_DBFS;
    }
    let db = 10.0 * (mean_sq / 0.5).log10();
    db.max(MIN_DBFS)
}

pub fn citation() -> StandardsCitation {
    StandardsCitation {
        standard: "AES17-2015".into(),
        clause: "§6.4 Idle-channel noise".into(),
        verified: false,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    const FS: u32 = 48_000;

    fn sine(f_hz: f64, amp: f64, n: usize) -> Vec<f32> {
        let w = 2.0 * PI * f_hz / FS as f64;
        (0..n).map(|i| (amp * (w * i as f64).sin()) as f32).collect()
    }

    #[test]
    fn rejects_zero_sample_rate() {
        let buf = vec![0.0_f32; 4800];
        assert!(measure_noise(&buf, 0).is_err());
    }

    #[test]
    fn rejects_empty_buffer() {
        assert!(measure_noise(&[], FS).is_err());
    }

    #[test]
    fn rejects_below_settle_length() {
        let too_short = vec![0.0_f32; 100];
        assert!(measure_noise(&too_short, FS).is_err());
    }

    #[test]
    fn silence_is_at_floor() {
        let buf = vec![0.0_f32; FS as usize];
        let m = measure_noise(&buf, FS).unwrap();
        assert_eq!(m.unweighted_dbfs, MIN_DBFS);
        assert_eq!(m.a_weighted_dbfs, MIN_DBFS);
        assert_eq!(m.ccir_weighted_dbfs, MIN_DBFS);
    }

    #[test]
    fn full_scale_sine_reads_0_dbfs_unweighted() {
        let n = FS as usize;
        let buf = sine(1000.0, 1.0, n);
        let m = measure_noise(&buf, FS).unwrap();
        assert!(
            m.unweighted_dbfs.abs() < 0.1,
            "full-scale 1 kHz sine should read 0 dBFS, got {:.3}",
            m.unweighted_dbfs
        );
    }

    #[test]
    fn ccir_weighting_is_transparent_at_1khz() {
        // 1 kHz is the CCIR-468 calibration point — full-scale 1 kHz
        // sine reads 0 dBFS after weighting + QP.
        let n = FS as usize * 2;
        let buf = sine(1000.0, 1.0, n);
        let m = measure_noise(&buf, FS).unwrap();
        assert!(
            m.ccir_weighted_dbfs.abs() < 0.2,
            "CCIR-weighted full-scale 1 kHz should read 0 dBFS, got {:.3}",
            m.ccir_weighted_dbfs,
        );
    }

    #[test]
    fn a_weighting_is_transparent_at_1khz() {
        // 1 kHz is the A-weighting normalisation point — post-filter
        // level should match unweighted level within a fraction of a dB.
        let n = FS as usize;
        let buf = sine(1000.0, 0.5, n);
        let m = measure_noise(&buf, FS).unwrap();
        assert!(
            (m.a_weighted_dbfs - m.unweighted_dbfs).abs() < 0.2,
            "A-weighted ({:.3}) vs unweighted ({:.3}) should agree at 1 kHz",
            m.a_weighted_dbfs,
            m.unweighted_dbfs,
        );
    }

    #[test]
    fn a_weighting_attenuates_100hz() {
        // A-weighting is ~−19 dB at 100 Hz. A 100 Hz tone should come
        // out ≫ 5 dB below the unweighted level.
        let n = FS as usize * 2;
        let buf = sine(100.0, 0.5, n);
        let m = measure_noise(&buf, FS).unwrap();
        let delta = m.unweighted_dbfs - m.a_weighted_dbfs;
        assert!(
            delta > 15.0,
            "A-weighting should attenuate 100 Hz ≥ 15 dB; got {delta:.2} dB"
        );
    }

    #[test]
    fn half_scale_sine_reads_minus_6dbfs() {
        // sine of peak 0.5 → RMS 0.5/√2 → level 20·log10(0.5) = −6.02 dB
        let n = FS as usize;
        let buf = sine(1000.0, 0.5, n);
        let m = measure_noise(&buf, FS).unwrap();
        assert!(
            (m.unweighted_dbfs + 6.02).abs() < 0.1,
            "half-scale sine should read −6.02 dBFS, got {:.3}",
            m.unweighted_dbfs,
        );
    }

    #[test]
    fn reports_duration_and_rate() {
        let n = FS as usize;
        let buf = sine(1000.0, 0.5, n);
        let m = measure_noise(&buf, FS).unwrap();
        assert_eq!(m.sample_rate_hz, FS);
        // 1 s minus 100 ms settling.
        assert!((m.duration_s - 0.9).abs() < 1e-6);
    }

    #[test]
    fn citation_shape() {
        let c = citation();
        assert!(c.standard.contains("AES17"));
        assert!(!c.verified);
    }
}
