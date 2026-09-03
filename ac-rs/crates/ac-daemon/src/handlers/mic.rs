//! Shared mic frequency-response correction helpers, used across both
//! the live-monitor path and the Tier 1 capture handlers (#97 / #98).
//!
//! The mic over-reads by `curve.correction_at(f)` dB at frequency `f`
//! (that's the contract `MicResponse` exposes — it stores the mic's
//! deviation from flat). Subtracting the correction recovers the
//! truthful acoustic level. These helpers do the subtraction in place
//! on dB-domain magnitudes, leaving non-finite bins (NaN / -inf
//! sentinels) untouched.

use ac_core::measurement::sweep::GatedResponsePoint;
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

/// Linear-amplitude counterpart of [`apply_mic_curve_inplace_f64`].
///
/// Same correction and same sign, different domain: subtracting
/// `corr_db` from a dB magnitude and scaling a linear amplitude by
/// `10^(-corr_db/20)` are one operation, and they have to stay one.
/// `transfer_stream` corrects three views of a single measurement — the
/// dB `magnitude_db`, the complex `re`/`im` pair, and the calibrated
/// `meas_spectrum` — and a curve applied to one but not the others makes
/// those views disagree with no symptom at the point of the mistake.
/// Kept beside the dB form so the two are read and edited together.
pub(crate) fn mic_curve_scale(curve: &MicResponse, f: f64) -> f64 {
    10.0_f64.powf(-(curve.correction_at(f as f32) as f64) / 20.0)
}

/// Status flag stamped on every monitor / Tier-1 frame so the UI (and
/// downstream wire subscribers) can tell whether the magnitudes are
/// mic-corrected, have a curve loaded but the global toggle off, or
/// have no curve at all.
pub(crate) fn mic_correction_tag(curve_loaded: bool, enabled: bool) -> &'static str {
    match (curve_loaded, enabled) {
        (false, _) => "none",
        (true, false) => "off",
        (true, true) => "on",
    }
}

/// Apply the mic-curve correction to a Tier 1 `AnalysisResult` in
/// place: spectrum bins, fundamental level, harmonic levels, and
/// `thd_pct` recomputed from the corrected harmonics. The mic is
/// frequency-dependent so different bins shift by different amounts;
/// THD-as-ratio changes accordingly when the curve isn't flat across
/// the harmonic series.
///
/// `thd_pct`'s denominator, `total_output_rms`, is **not** corrected: it
/// is the uncorrected total output `thd::analyze` published, and only the
/// numerator (the harmonic sum) is rescaled here. This mirrors the
/// existing, documented choice not to mic-correct `thdn_pct` below — the
/// residual is not corrected, so the total it contributes to is not
/// either.
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
/// - `thdn_pct` and `total_output_rms` — `thdn_pct` depends on
///   `noise_floor_dbfs`; same reason. `total_output_rms` is their shared
///   denominator and the residual within it is likewise uncorrected, so
///   leaving it alone keeps `thd_pct` and `thdn_pct` on the same basis.
pub(crate) fn apply_mic_curve_to_analysis(curve: &MicResponse, r: &mut AnalysisResult) {
    apply_mic_curve_inplace_f64(curve, &r.freqs, &mut r.spectrum);
    r.fundamental_dbfs -= curve.correction_at(r.fundamental_hz as f32) as f64;
    for h in r.harmonic_levels.iter_mut() {
        h.1 -= curve.correction_at(h.0 as f32) as f64;
    }
    // Recompute THD from corrected harmonics over the uncorrected total
    // output denominator -- the same basis `thdn_pct` already uses.
    if r.total_output_rms > 1e-30 && !r.harmonic_levels.is_empty() {
        let harm_pow: f64 = r
            .harmonic_levels
            .iter()
            .map(|(_, db)| 10f64.powf(db / 10.0))
            .sum();
        r.thd_pct = (harm_pow.sqrt() / r.total_output_rms) * 100.0;
    }
}

/// Apply mic-curve correction to a gated (quasi-anechoic) frequency
/// response in place — the frequency-domain route #285 requires for
/// `plot_ir`. Reuses [`apply_mic_curve_inplace_f64`]'s subtraction on
/// the derived `magnitude_db` column, keyed by each point's `freq_hz`.
///
/// Deliberately does not touch any impulse response: by the time a
/// [`GatedResponsePoint`] slice exists, arrival estimation and gating
/// are already done, so there is no time axis left to disturb. Contrast
/// with [`ac_core::shared::mic_curve_filter::MicCurveFir`] — a
/// linear-phase FIR meant for *time-domain* correction — convolving
/// that into the IR ahead of gating would shift the IR peak by the
/// filter's group delay and corrupt the arrival sample the gate was
/// anchored to (#285's `mic_curve_correction_does_not_move_ir_peak`
/// test demonstrates exactly this failure mode).
pub(crate) fn apply_mic_curve_to_gated_response(
    curve: &MicResponse,
    points: &mut [GatedResponsePoint],
) {
    let freqs: Vec<f64> = points.iter().map(|p| p.freq_hz).collect();
    let mut mags: Vec<f64> = points.iter().map(|p| p.magnitude_db).collect();
    apply_mic_curve_inplace_f64(curve, &freqs, &mut mags);
    for (p, m) in points.iter_mut().zip(mags) {
        p.magnitude_db = m;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ac_core::measurement::sweep::gated_frequency_response;
    use ac_core::shared::calibration::parse_mic_curve;
    use ac_core::shared::mic_curve_filter::MicCurveFir;

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
            (100.0, 0.0),
            (200.0, 0.4),
            (300.0, 0.8),
            (400.0, 1.0),
            (500.0, 1.0),
            (600.0, 1.2),
            (700.0, 1.4),
            (800.0, 1.6),
            (900.0, 1.8),
            (1000.0, 2.0),
            (1100.0, 2.2),
            (1200.0, 2.4),
            (1300.0, 2.6),
            (1400.0, 3.0),
            (1500.0, 3.5),
            (1600.0, 4.0),
            (2000.0, 5.0),
            (4000.0, 6.0),
            (8000.0, 5.5),
            (16000.0, 4.0),
        ];
        points.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        for (f, g) in &points {
            text.push_str(&format!("{f}\t{g}\n"));
        }
        let _ = curve_text;
        let curve = parse_mic_curve(&text, None).unwrap();

        // Uncorrected total output: sqrt(fund_amp^2 + h2_amp^2) at the
        // pre-correction levels (-10 dBFS fundamental, -40 dBFS H2) -- the
        // denominator thd::analyze would have published, and which mic
        // correction must leave alone.
        let total_output_rms =
            (10f64.powf(-10.0 / 20.0).powi(2) + 10f64.powf(-40.0 / 20.0).powi(2)).sqrt();

        let mut r = AnalysisResult {
            fundamental_hz: 1000.0,
            fundamental_dbfs: -10.0,
            linear_rms: 0.1,
            thd_pct: 0.0, // recomputed
            thdn_pct: 0.5,
            total_output_rms,
            harmonic_levels: vec![(2000.0, -40.0)],
            noise_floor_dbfs: -90.0,
            spectrum: vec![-90.0; 4],
            freqs: vec![500.0, 1000.0, 2000.0, 4000.0],
            clipping: false,
            ac_coupled: false,
        };
        let orig_thdn = r.thdn_pct;
        let orig_floor = r.noise_floor_dbfs;
        let orig_rms = r.linear_rms;
        let orig_total_output_rms = r.total_output_rms;
        super::apply_mic_curve_to_analysis(&curve, &mut r);
        assert!(
            (r.fundamental_dbfs - -12.0).abs() < 0.01,
            "fund: got {}",
            r.fundamental_dbfs
        );
        assert!(
            (r.harmonic_levels[0].1 - -45.0).abs() < 0.01,
            "h2: got {}",
            r.harmonic_levels[0].1
        );
        // Spectrum bins corrected by curve at each freq.
        let expected_curve_at = [1.0_f64, 2.0, 5.0, 6.0];
        for (i, m) in r.spectrum.iter().enumerate() {
            assert!(
                (m - (-90.0 - expected_curve_at[i])).abs() < 0.05,
                "spec[{i}] got {m}"
            );
        }
        // THD recomputed over the *uncorrected* total-output denominator:
        // corrected h2 -45 dBFS = 0.005623, total_output_rms unchanged =
        // 0.316386. THD = 0.005623 / 0.316386 * 100 ≈ 1.777%.
        // The rejected re-fundamental value (corrected fund -12 dBFS =
        // 0.2512) would give 0.005623 / 0.2512 * 100 ≈ 2.238% -- assert
        // against that too so a revert to the fundamental denominator
        // cannot pass silently.
        let rejected_re_fundamental = 2.238;
        assert!(
            (r.thd_pct - 1.777).abs() < 0.01,
            "thd_pct: got {}",
            r.thd_pct
        );
        assert!(
            (r.thd_pct - rejected_re_fundamental).abs() > 0.1,
            "thd_pct must not match the rejected re-fundamental value: got {}",
            r.thd_pct
        );
        // Untouched fields stay untouched.
        assert_eq!(r.thdn_pct, orig_thdn);
        assert_eq!(r.noise_floor_dbfs, orig_floor);
        assert_eq!(r.linear_rms, orig_rms);
        assert_eq!(r.total_output_rms, orig_total_output_rms);
    }

    #[test]
    fn correction_tag_truth_table() {
        assert_eq!(mic_correction_tag(false, true), "none");
        assert_eq!(mic_correction_tag(false, false), "none");
        assert_eq!(mic_correction_tag(true, true), "on");
        assert_eq!(mic_correction_tag(true, false), "off");
    }

    /// Non-flat, log-spaced curve — 6 dB at 20 Hz sloping to 0 dB at
    /// 20 kHz — so a `MicCurveFir` built from it has genuine taps
    /// rather than degenerating toward a near-delta.
    fn tilted_curve() -> MicResponse {
        let mut s = String::new();
        let log_min = 20.0_f32.ln();
        let log_max = 20_000.0_f32.ln();
        for i in 0..24 {
            let t = i as f32 / 23.0;
            let f = (log_min + t * (log_max - log_min)).exp();
            let gain = 6.0 * (1.0 - t);
            s.push_str(&format!("{f}\t{gain}\n"));
        }
        parse_mic_curve(&s, None).unwrap()
    }

    #[test]
    fn gated_response_corrected_by_curve_at_each_bin() {
        let curve = tilted_curve();
        let mut points = vec![
            GatedResponsePoint {
                freq_hz: 200.0,
                magnitude_db: -10.0,
                phase_deg: 0.0,
            },
            GatedResponsePoint {
                freq_hz: 1000.0,
                magnitude_db: -5.0,
                phase_deg: 45.0,
            },
            GatedResponsePoint {
                freq_hz: 8000.0,
                magnitude_db: -2.0,
                phase_deg: -90.0,
            },
        ];
        let expected: Vec<f64> = points
            .iter()
            .map(|p| p.magnitude_db - curve.correction_at(p.freq_hz as f32) as f64)
            .collect();
        let phases: Vec<f64> = points.iter().map(|p| p.phase_deg).collect();
        apply_mic_curve_to_gated_response(&curve, &mut points);
        for ((p, exp), ph) in points.iter().zip(expected).zip(phases) {
            assert!(
                (p.magnitude_db - exp).abs() < 1e-9,
                "got {} want {}",
                p.magnitude_db,
                exp
            );
            assert_eq!(
                p.phase_deg, ph,
                "phase must be untouched by mic-curve correction"
            );
        }
    }

    /// (#306 QA test coverage gap) `apply_mic_curve_to_gated_response`
    /// inherits `apply_mic_curve_inplace_f64`'s non-finite-bin skip
    /// contract (module doc above) but had no direct test on this call
    /// site — a zero/degenerate FFT bin producing `-inf` dB is plausible
    /// on `GatedResponsePoint`.
    #[test]
    fn gated_response_correction_skips_non_finite_bins() {
        let curve = tilted_curve();
        let mut points = vec![
            GatedResponsePoint {
                freq_hz: 100.0,
                magnitude_db: f64::NEG_INFINITY,
                phase_deg: 0.0,
            },
            GatedResponsePoint {
                freq_hz: 200.0,
                magnitude_db: f64::NAN,
                phase_deg: 0.0,
            },
            GatedResponsePoint {
                freq_hz: 1000.0,
                magnitude_db: -5.0,
                phase_deg: 0.0,
            },
        ];
        apply_mic_curve_to_gated_response(&curve, &mut points);
        assert_eq!(points[0].magnitude_db, f64::NEG_INFINITY);
        assert!(points[1].magnitude_db.is_nan());
        assert!(
            (points[2].magnitude_db - (-5.0 - curve.correction_at(1000.0) as f64)).abs() < 1e-9
        );
    }

    /// The load-bearing test (#285): mic-curve correction on the derived
    /// gated spectrum must leave the IR peak — and therefore the gate
    /// anchored to it — exactly where it was. Demonstrates the failure
    /// mode this guards against by computing, inside the test, what the
    /// rejected implementation (convolving the equivalent `MicCurveFir`
    /// into the time-domain IR *before* gating) would have done: shift
    /// the arrival sample by the filter's group delay.
    #[test]
    fn mic_curve_correction_does_not_move_ir_peak() {
        let sr = 48_000u32;
        let n = 4096usize;
        let peak_idx = 2100usize; // arrival sample, offset from centre (2048)
        let mut ir = vec![0.0_f64; n];
        ir[peak_idx] = 1.0;

        let curve = tilted_curve();

        // --- the real implementation: gate/FFT the raw IR, correct the
        // derived spectrum afterward. `ir` is only ever read. ---
        let gate_length_s = 0.02;
        let raw_points = gated_frequency_response(&ir, sr, 0.0, gate_length_s, 0.25);
        let mut corrected_points = raw_points.clone();
        apply_mic_curve_to_gated_response(&curve, &mut corrected_points);

        let ir_peak_after = ir
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap()
            .0;
        assert_eq!(
            ir_peak_after, peak_idx,
            "mic-curve correction on the derived spectrum must not move the IR peak"
        );

        for (raw, corrected) in raw_points.iter().zip(&corrected_points) {
            let expected = raw.magnitude_db - curve.correction_at(raw.freq_hz as f32) as f64;
            assert!(
                (corrected.magnitude_db - expected).abs() < 1e-6,
                "freq {}: got {}, want {}",
                raw.freq_hz,
                corrected.magnitude_db,
                expected
            );
        }

        // --- what the rejected implementation would have done: convolve
        // the equivalent MicCurveFir into the time-domain IR ahead of
        // gating. Its linear-phase group delay shifts the impulse by
        // `group_delay_samples`, corrupting exactly the arrival sample
        // the gate is anchored to.
        let n_taps = 512;
        let mut fir = MicCurveFir::new(&curve, sr, n_taps);
        let mut wrong_ir: Vec<f32> = ir.iter().map(|&v| v as f32).collect();
        fir.process_inplace(&mut wrong_ir);
        let wrong_peak = wrong_ir
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap()
            .0;
        // Not an exact `peak_idx + group_delay_samples` match — the
        // curve isn't flat, so the FIR's impulse response is a shaped
        // pulse, not a pure delta, and its peak can land a sample or two
        // off the nominal group delay. Still unmistakably in that
        // neighbourhood, nowhere near the untouched `peak_idx`.
        assert!(
            wrong_peak.abs_diff(peak_idx + fir.group_delay_samples) <= 4,
            "sanity: the rejected time-domain-FIR route shifts the impulse by ~its group delay \
             ({}), got peak at {wrong_peak}",
            fir.group_delay_samples
        );
        assert_ne!(
            wrong_peak, ir_peak_after,
            "the rejected implementation moves the IR peak; the real implementation above must \
             not — this is exactly the bug #285 exists to prevent"
        );
    }
}
