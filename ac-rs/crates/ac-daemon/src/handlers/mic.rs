//! Shared mic frequency-response correction helpers, used across both
//! the live-monitor path and the Tier 1 capture handlers (#97 / #98).
//!
//! The mic over-reads by `curve.correction_at(f)` dB at frequency `f`
//! (that's the contract `MicResponse` exposes — it stores the mic's
//! deviation from flat). Subtracting the correction recovers the
//! truthful acoustic level. These helpers do the subtraction in place
//! on dB-domain magnitudes, leaving non-finite bins (NaN / -inf
//! sentinels) untouched.

use ac_core::shared::calibration::MicResponse;
use ac_core::shared::types::AnalysisResult;

/// Subtract the curve from an `f32` dB-magnitude column in-place.
pub(crate) fn apply_mic_curve_inplace_f32(curve: &MicResponse, freqs: &[f32], mags: &mut [f32]) {
    for (m, &f) in mags.iter_mut().zip(freqs.iter()) {
        if m.is_finite() {
            *m -= curve.correction_at(f);
        }
    }
}

/// `f64` variant for the FFT-aggregator path (where
/// `spectrum_to_columns_wire` returns `Vec<f64>`) and for the Tier 1
/// `AnalysisResult.spectrum` path.
pub(crate) fn apply_mic_curve_inplace_f64(curve: &MicResponse, freqs: &[f64], mags: &mut [f64]) {
    for (m, &f) in mags.iter_mut().zip(freqs.iter()) {
        if m.is_finite() {
            *m -= curve.correction_at(f as f32) as f64;
        }
    }
}

/// Status flag stamped on every monitor / Tier-1 frame so the UI (and
/// downstream wire subscribers) can tell whether the magnitudes are
/// mic-corrected, have a curve loaded but the global toggle off, or
/// have no curve at all.
pub(crate) fn mic_correction_tag(curve_loaded: bool, enabled: bool) -> &'static str {
    match (curve_loaded, enabled) {
        (false, _)    => "none",
        (true, false) => "off",
        (true, true)  => "on",
    }
}

/// Apply the mic-curve correction to a Tier 1 `AnalysisResult` in
/// place: spectrum bins, fundamental level, harmonic levels, and
/// `thd_pct` recomputed from the corrected harmonics. The mic is
/// frequency-dependent so different bins shift by different amounts;
/// THD-as-ratio changes accordingly when the curve isn't flat across
/// the harmonic series.
///
/// Untouched (intentional, documented):
///
/// - `linear_rms` — time-domain integral of the raw electrical signal.
///   Mic-curve is an *acoustic*-domain correction; the voltage cal
///   (which uses `linear_rms`) reads electrical level, not acoustic,
///   and the mic genuinely *did* deliver that voltage to the ADC.
/// - `noise_floor_dbfs` — broadband summary; correcting it would
///   require integrating the curve over the noise band, beyond the
///   scope of #97. The displayed spectrum is corrected, so users can
///   eyeball the noise floor at frequencies they care about.
/// - `thdn_pct` — depends on `noise_floor_dbfs`; same reason.
pub(crate) fn apply_mic_curve_to_analysis(curve: &MicResponse, r: &mut AnalysisResult) {
    apply_mic_curve_inplace_f64(curve, &r.freqs, &mut r.spectrum);
    r.fundamental_dbfs -= curve.correction_at(r.fundamental_hz as f32) as f64;
    for h in r.harmonic_levels.iter_mut() {
        h.1 -= curve.correction_at(h.0 as f32) as f64;
    }
    // Recompute THD from corrected fundamental + harmonics.
    let fund_amp = 10f64.powf(r.fundamental_dbfs / 20.0);
    if fund_amp > 1e-30 && !r.harmonic_levels.is_empty() {
        let harm_pow: f64 = r
            .harmonic_levels
            .iter()
            .map(|(_, db)| 10f64.powf(db / 10.0))
            .sum();
        r.thd_pct = (harm_pow.sqrt() / fund_amp) * 100.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ac_core::shared::calibration::parse_mic_curve;

    fn flat_curve_text(n: usize, gain_db: f32) -> String {
        let mut s = String::new();
        let log_min = 20.0_f32.ln();
        let log_max = 20_000.0_f32.ln();
        for i in 0..n {
            let t = i as f32 / (n - 1) as f32;
            let f = (log_min + t * (log_max - log_min)).exp();
            s.push_str(&format!("{f}\t{gain_db}\n"));
        }
        s
    }

    #[test]
    fn flat_curve_uniform_offset_on_spectrum_f64() {
        let curve = parse_mic_curve(&flat_curve_text(32, 3.0), None).unwrap();
        let freqs: Vec<f64> = (1..=10).map(|i| 100.0 * i as f64).collect();
        let mut mags: Vec<f64> = vec![-20.0; freqs.len()];
        apply_mic_curve_inplace_f64(&curve, &freqs, &mut mags);
        // Mic over-reads by 3 dB everywhere → corrected reads -23 dB.
        for &m in &mags {
            assert!((m - -23.0).abs() < 0.01, "got {m}");
        }
    }

    #[test]
    fn analysis_result_corrected_in_place() {
        // Curve has +2 dB at 1 kHz, +5 dB at 2 kHz. A signal that
        // analyzed to fund=−10 dBFS @1 k, 2nd harmonic=−40 dBFS @2 k
        // should correct to −12 / −45 and THD% should drop accordingly.
        let curve_text = "100 0\n500 1\n1000 2\n1500 3.5\n2000 5\n4000 6\n8000 5.5\n16000 4\n\
                          200 0.4\n300 0.8\n400 1.0\n600 1.2\n700 1.4\n800 1.6\n900 1.8\n\
                          1100 2.2\n1200 2.4\n1300 2.6\n1400 3.0\n";
        // Need at least 16 points; pad.
        let mut text = String::new();
        let mut points: Vec<(f32, f32)> = vec![
            (100.0, 0.0), (200.0, 0.4), (300.0, 0.8), (400.0, 1.0),
            (500.0, 1.0), (600.0, 1.2), (700.0, 1.4), (800.0, 1.6),
            (900.0, 1.8), (1000.0, 2.0), (1100.0, 2.2), (1200.0, 2.4),
            (1300.0, 2.6), (1400.0, 3.0), (1500.0, 3.5), (1600.0, 4.0),
            (2000.0, 5.0), (4000.0, 6.0), (8000.0, 5.5), (16000.0, 4.0),
        ];
        points.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        for (f, g) in &points {
            text.push_str(&format!("{f}\t{g}\n"));
        }
        let _ = curve_text;
        let curve = parse_mic_curve(&text, None).unwrap();

        let mut r = AnalysisResult {
            fundamental_hz: 1000.0,
            fundamental_dbfs: -10.0,
            linear_rms: 0.1,
            thd_pct: 0.0,                                       // recomputed
            thdn_pct: 0.5,
            harmonic_levels: vec![(2000.0, -40.0)],
            noise_floor_dbfs: -90.0,
            spectrum: vec![-90.0; 4],
            freqs:    vec![500.0, 1000.0, 2000.0, 4000.0],
            clipping: false,
            ac_coupled: false,
        };
        let orig_thdn = r.thdn_pct;
        let orig_floor = r.noise_floor_dbfs;
        let orig_rms = r.linear_rms;
        super::apply_mic_curve_to_analysis(&curve, &mut r);
        assert!((r.fundamental_dbfs - -12.0).abs() < 0.01,
            "fund: got {}", r.fundamental_dbfs);
        assert!((r.harmonic_levels[0].1 - -45.0).abs() < 0.01,
            "h2: got {}", r.harmonic_levels[0].1);
        // Spectrum bins corrected by curve at each freq.
        let expected_curve_at = [1.0_f64, 2.0, 5.0, 6.0];
        for (i, m) in r.spectrum.iter().enumerate() {
            assert!((m - (-90.0 - expected_curve_at[i])).abs() < 0.05,
                "spec[{i}] got {m}");
        }
        // THD recomputed: corrected fund -12 dBFS = 0.2512, h2 -45 dBFS = 0.005623.
        // THD = 0.005623 / 0.2512 * 100 ≈ 2.238%.
        assert!((r.thd_pct - 2.238).abs() < 0.01, "thd_pct: got {}", r.thd_pct);
        // Untouched fields stay untouched.
        assert_eq!(r.thdn_pct, orig_thdn);
        assert_eq!(r.noise_floor_dbfs, orig_floor);
        assert_eq!(r.linear_rms, orig_rms);
    }

    #[test]
    fn correction_tag_truth_table() {
        assert_eq!(mic_correction_tag(false, true),  "none");
        assert_eq!(mic_correction_tag(false, false), "none");
        assert_eq!(mic_correction_tag(true,  true),  "on");
        assert_eq!(mic_correction_tag(true,  false), "off");
    }
}
