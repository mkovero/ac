//! Shared mic frequency-response correction helpers, used across both
//! the live-monitor path and the Tier 1 capture handlers (#97 / #98).
//!
//! The mic over-reads by `curve.correction_at(f)` dB at frequency `f`
//! (that's the contract `MicResponse` exposes — it stores the mic's
//! deviation from flat). Removing the correction recovers the truthful
//! acoustic level. The same correction takes two forms, and the helper
//! name states which one a call site is in (#167):
//!
//! - **dB domain** (`apply_mic_curve_db_f32` / `apply_mic_curve_db_f64`):
//!   subtract `corr_db` from a dB magnitude.
//! - **linear domain** (`apply_mic_curve_linear_f64`, built on
//!   `mic_curve_scale`): multiply a linear amplitude by `10^(-corr_db/20)`.
//!
//! Both leave non-finite values (NaN / -inf sentinels) untouched. Picking
//! the wrong one is not a small error: subtracting a dB offset from a
//! linear amplitude of ~0.1 drives it negative.

use ac_core::measurement::sweep::GatedResponsePoint;
use ac_core::shared::calibration::MicResponse;
use ac_core::shared::types::AnalysisResult;

/// Subtract the curve from an `f32` dB-magnitude column in-place.
pub(crate) fn apply_mic_curve_db_f32(curve: &MicResponse, freqs: &[f32], mags: &mut [f32]) {
    for (m, &f) in mags.iter_mut().zip(freqs.iter()) {
        if m.is_finite() {
            *m -= curve.correction_at(f);
        }
    }
}

/// Subtract the curve from an `f64` dB-magnitude column in-place — the
/// transfer `magnitude_db` and gated-response paths. Not for linear
/// amplitudes (monitor columns, `AnalysisResult.spectrum`,
/// `harmonic_levels`): those take [`apply_mic_curve_linear_f64`].
///
/// Delegates to [`MicResponse::corrected_db`], the sign rule `ac report
/// verify` also applies post-hoc (#398).
pub(crate) fn apply_mic_curve_db_f64(curve: &MicResponse, freqs: &[f64], mags: &mut [f64]) {
    for (m, &f) in mags.iter_mut().zip(freqs.iter()) {
        *m = curve.corrected_db(f, *m);
    }
}

/// Linear-amplitude counterpart of [`apply_mic_curve_db_f64`].
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

/// Scale an `f64` linear-amplitude column in-place by
/// [`mic_curve_scale`] at each frequency — the monitor `spectrum` columns
/// and `AnalysisResult.spectrum`. Same non-finite skip contract as
/// [`apply_mic_curve_db_f64`], so the two read as a pair.
pub(crate) fn apply_mic_curve_linear_f64(curve: &MicResponse, freqs: &[f64], amps: &mut [f64]) {
    for (a, &f) in amps.iter_mut().zip(freqs.iter()) {
        if a.is_finite() {
            *a *= mic_curve_scale(curve, f);
        }
    }
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
/// `thd_pct` recomputed from the corrected harmonics. `spectrum` and
/// `harmonic_levels` are linear amplitudes (as `thd::analyze` fills
/// them) and are scaled; `fundamental_dbfs` is dB and is subtracted. The mic is
/// frequency-dependent so different bins shift by different amounts;
/// THD-as-ratio changes accordingly when the curve isn't flat across
/// the harmonic series.
///
/// `thd_pct` is recomputed as `√(Σ (hᵢ·sᵢ)²) / (total_output_rms · s₁)`,
/// with `sᵢ = mic_curve_scale(curve, fᵢ)` and `s₁` the scale at
/// `fundamental_hz` (#167):
///
/// - THD is a ratio of two voltages taken at the same terminals
///   (IEC 60268-3:2018 §15.12.3.2 e), `d_tot = U2'/U2`), so the curve's
///   gain at the fundamental cancels. A flat curve leaves `thd_pct` equal
///   to the uncorrected value; a non-flat one moves it only by each
///   harmonic's response relative to the fundamental, `sᵢ/s₁`. The
///   published `total_output_rms` itself is not rewritten.
/// - `thdn_pct` stays uncorrected (both its terms are raw), so it is
///   gain-invariant as well.
/// - The residual part of `U2` is scaled by `s₁` rather than bin by bin.
///   The relative error on `thd_pct` is at most
///   `½·(thdn_pct/100)²·maxₖ|1 − (sₖ/s₁)²|` over the residual band, zero
///   for a flat curve. `thd_pct` reads low when the curve over-reads the
///   residual band more than the fundamental, high in the opposite case.
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
///   `noise_floor_dbfs`; same reason. `total_output_rms` is published as
///   measured.
pub(crate) fn apply_mic_curve_to_analysis(curve: &MicResponse, r: &mut AnalysisResult) {
    apply_mic_curve_linear_f64(curve, &r.freqs, &mut r.spectrum);
    r.fundamental_dbfs -= curve.correction_at(r.fundamental_hz as f32) as f64;
    for h in r.harmonic_levels.iter_mut() {
        h.1 *= mic_curve_scale(curve, h.0);
    }
    // Recompute THD from corrected harmonics over the total output at the
    // fundamental's correction: a same-terminal ratio, so the curve's gain
    // at the fundamental cancels (IEC 60268-3 §15.12.3.2 e), #167).
    let denom = r.total_output_rms * mic_curve_scale(curve, r.fundamental_hz);
    if denom > 1e-30 && !r.harmonic_levels.is_empty() {
        let harm_pow: f64 = r.harmonic_levels.iter().map(|(_, a)| a * a).sum();
        r.thd_pct = (harm_pow.sqrt() / denom) * 100.0;
    }
}

/// Apply mic-curve correction to a gated (quasi-anechoic) frequency
/// response in place — the frequency-domain route #285 requires for
/// `plot_ir`. Reuses [`apply_mic_curve_db_f64`]'s subtraction on
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
    apply_mic_curve_db_f64(curve, &freqs, &mut mags);
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
    fn flat_curve_uniform_offset_on_spectrum_db_f64() {
        let curve = parse_mic_curve(&flat_curve_text(32, 3.0), None).unwrap();
        let freqs: Vec<f64> = (1..=10).map(|i| 100.0 * i as f64).collect();
        let mut mags: Vec<f64> = vec![-20.0; freqs.len()];
        apply_mic_curve_db_f64(&curve, &freqs, &mut mags);
        // Mic over-reads by 3 dB everywhere → corrected reads -23 dB.
        for &m in &mags {
            assert!((m - -23.0).abs() < 0.01, "got {m}");
        }
    }

    #[test]
    fn flat_curve_uniform_scale_on_spectrum_linear_f64() {
        let curve = parse_mic_curve(&flat_curve_text(32, 3.0), None).unwrap();
        let freqs: Vec<f64> = (1..=10).map(|i| 100.0 * i as f64).collect();
        let mut amps: Vec<f64> = vec![0.1; freqs.len()];
        amps[3] = f64::NAN;
        amps[4] = f64::INFINITY;
        apply_mic_curve_linear_f64(&curve, &freqs, &mut amps);
        // Mic over-reads by 3 dB everywhere → 0.1 · 10^(-3/20) = 0.07079.
        let expected = 0.1 * 10f64.powf(-3.0 / 20.0);
        for (i, &a) in amps.iter().enumerate() {
            match i {
                3 => assert!(a.is_nan(), "NaN must stay NaN, got {a}"),
                4 => assert_eq!(a, f64::INFINITY, "inf must stay inf"),
                _ => assert!((a - expected).abs() < 1e-6, "amps[{i}] got {a}"),
            }
        }
    }

    #[test]
    fn analysis_result_corrected_in_place() {
        // Curve has +2 dB at 1 kHz, +5 dB at 2 kHz. A real `thd::analyze`
        // result (linear `spectrum` and `harmonic_levels`, as production
        // produces them — #167) for a -10 dBFS 1 kHz sine with a 1 %
        // 2nd harmonic: fundamental_dbfs drops 2 dB, the spectrum and H2
        // amplitudes scale by 10^(-2/20) and 10^(-5/20), and THD% is
        // recomputed from the scaled amplitudes.
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
        let curve = parse_mic_curve(&text, None).unwrap();

        let sr = 48_000u32;
        let fund_amp = 10f64.powf(-10.0 / 20.0);
        let samples: Vec<f32> = (0..sr as usize)
            .map(|i| {
                let t = i as f64 / sr as f64;
                (fund_amp * (2.0 * std::f64::consts::PI * 1000.0 * t).sin()
                    + 0.01 * fund_amp * (2.0 * std::f64::consts::PI * 2000.0 * t).sin())
                    as f32
            })
            .collect();
        let mut r = ac_core::measurement::thd::analyze(&samples, sr, 1000.0, 10).unwrap();
        let orig = r.clone();
        assert!(
            orig.spectrum.iter().all(|a| *a >= 0.0),
            "fixture precondition: thd::analyze spectrum is linear amplitude"
        );

        super::apply_mic_curve_to_analysis(&curve, &mut r);

        assert!(
            (r.fundamental_dbfs - (orig.fundamental_dbfs - 2.0)).abs() < 0.01,
            "fund: got {} from {}",
            r.fundamental_dbfs,
            orig.fundamental_dbfs
        );

        // Spectrum bin at 1 kHz scaled by 10^(-2/20).
        let k1 = orig
            .freqs
            .iter()
            .position(|&f| (f - 1000.0).abs() < 1e-9)
            .expect("1 kHz bin");
        let want = orig.spectrum[k1] * 10f64.powf(-2.0 / 20.0);
        assert!(
            ((r.spectrum[k1] - want) / want).abs() < 1e-6,
            "spec@1k: got {} want {want}",
            r.spectrum[k1]
        );
        // Every bin scaled by the curve at its own frequency — never shifted.
        for (i, (&a, &a0)) in r.spectrum.iter().zip(&orig.spectrum).enumerate() {
            let want = a0 * 10f64.powf(-(curve.correction_at(orig.freqs[i] as f32) as f64) / 20.0);
            assert!((a - want).abs() <= 1e-12 + 1e-9 * want.abs(), "spec[{i}]");
        }

        // H2 amplitude scaled by 10^(-5/20).
        assert_eq!(r.harmonic_levels[0].0, 2000.0);
        let h2_want = orig.harmonic_levels[0].1 * 10f64.powf(-5.0 / 20.0);
        assert!(
            ((r.harmonic_levels[0].1 - h2_want) / h2_want).abs() < 1e-6,
            "h2: got {} want {h2_want}",
            r.harmonic_levels[0].1
        );

        // THD recomputed from the scaled amplitudes over the total output
        // at the fundamental's correction (same-terminal ratio, #167).
        let scaled: Vec<f64> = orig
            .harmonic_levels
            .iter()
            .map(|&(f, a)| a * 10f64.powf(-(curve.correction_at(f as f32) as f64) / 20.0))
            .collect();
        let harm_rss = scaled.iter().map(|a| a * a).sum::<f64>().sqrt();
        let s1 = 10f64.powf(-(curve.correction_at(1000.0) as f64) / 20.0);
        let thd_want = harm_rss / (orig.total_output_rms * s1) * 100.0;
        assert!(
            (r.thd_pct - thd_want).abs() < 1e-9,
            "thd_pct: got {} want {thd_want}",
            r.thd_pct
        );
        assert!(
            r.thd_pct > 0.65 && r.thd_pct < 0.75,
            "thd_pct: got {}, expected ≈ 0.71 % (1 % H2, harmonic 3 dB above fundamental)",
            r.thd_pct
        );

        // The rejected numerator-only rule (corrected harmonics over the raw
        // total) is biased by the curve at the fundamental: here 10^(2/20).
        let rejected_numerator_only = harm_rss / orig.total_output_rms * 100.0;
        assert!(
            (r.thd_pct - rejected_numerator_only).abs() > 0.1 * r.thd_pct,
            "thd_pct must not match the rejected numerator-only value \
             {rejected_numerator_only}: got {}",
            r.thd_pct
        );

        // The rejected pre-#167 implementation: subtract dB offsets from
        // the linear harmonic amplitudes, then read them back as dB.
        let rejected_db_on_linear = orig
            .harmonic_levels
            .iter()
            .map(|&(f, a)| {
                let db = a - curve.correction_at(f as f32) as f64;
                10f64.powf(db / 10.0)
            })
            .sum::<f64>()
            .sqrt()
            / orig.total_output_rms
            * 100.0;
        assert!(
            rejected_db_on_linear > 10.0 * r.thd_pct,
            "the dB-on-linear path ({rejected_db_on_linear} %) must be far from the \
             corrected one ({} %)",
            r.thd_pct
        );

        // Untouched fields stay untouched.
        assert_eq!(r.thdn_pct, orig.thdn_pct);
        assert_eq!(r.noise_floor_dbfs, orig.noise_floor_dbfs);
        assert_eq!(r.linear_rms, orig.linear_rms);
        assert_eq!(r.total_output_rms, orig.total_output_rms);
    }

    #[test]
    fn flat_curve_leaves_thd_pct_invariant() {
        // THD is a same-terminal ratio (IEC 60268-3 §15.12.3.2 e), #167): a
        // flat curve is a pure sensitivity error and must cancel. Fixture
        // from thd.rs `distortion_ratios_are_referenced_to_total_output`
        // (U2/f1 = √1.25), where total-referenced and re-fundamental THD
        // differ enough to tell apart.
        let sr = 48_000u32;
        let samples: Vec<f32> = (0..sr as usize)
            .map(|i| {
                let t = i as f64 / sr as f64;
                (0.5 * (2.0 * std::f64::consts::PI * 1000.0 * t).sin()
                    + 0.25 * (2.0 * std::f64::consts::PI * 2000.0 * t).sin()) as f32
            })
            .collect();
        let mut r = ac_core::measurement::thd::analyze(&samples, sr, 1000.0, 10).unwrap();
        let orig = r.clone();
        let curve = parse_mic_curve(&flat_curve_text(32, 3.0), None).unwrap();

        super::apply_mic_curve_to_analysis(&curve, &mut r);

        assert!(
            ((r.thd_pct - orig.thd_pct) / orig.thd_pct).abs() < 1e-9,
            "flat curve must leave thd_pct unchanged: got {} from {}",
            r.thd_pct,
            orig.thd_pct
        );

        let s1 = 10f64.powf(-3.0 / 20.0);
        let harm_rss = r
            .harmonic_levels
            .iter()
            .map(|(_, a)| a * a)
            .sum::<f64>()
            .sqrt();
        // Rejected: corrected harmonics over the raw total (≈ 31.6 %).
        let rejected_numerator_only = orig.thd_pct * s1;
        // Rejected: corrected harmonics over the corrected fundamental (≈ 50 %).
        let orig_fund_amp = 10f64.powf(orig.fundamental_dbfs / 20.0);
        let rejected_re_fundamental = harm_rss / (orig_fund_amp * s1) * 100.0;
        for (name, rejected) in [
            ("numerator-only", rejected_numerator_only),
            ("re-fundamental", rejected_re_fundamental),
        ] {
            assert!(
                (r.thd_pct - rejected).abs() > 0.1 * r.thd_pct,
                "thd_pct {} must not match the rejected {name} value {rejected}",
                r.thd_pct
            );
        }
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
    /// inherits `apply_mic_curve_db_f64`'s non-finite-bin skip
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
