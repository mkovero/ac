//! Tier 1 — THD / THD+N / noise-floor analysis of a stepped-sine capture.
//!
//! Citation for `MeasurementReport`s produced from this analyser is
//! provided by [`citation`] and tracks IEC 60268-3:2018 §15.12.3.2
//! ("Total harmonic distortion under standard measuring conditions"),
//! verified against the full text at
//! `stddocs/iec-full/Sound system equipment_ Amplifiers … 2018 …pdf`.
//! §15.12.3.2 defines the ratio as `d_tot = (U2'/U2) × 100 %` or
//! `L_d,tot = 20·lg(U2'/U2)` dB, which matches the THD+N output. The
//! harmonic-only THD numerator follows §15.12.5 and uses the same total-output
//! denominator. Residual power is integrated over the full analysis band up
//! to Nyquist; the AES17 20 kHz low-pass filter is not applied here.
//!
//! The public entry point is [`analyze`]: mono `f32` PCM in, a fully
//! populated [`AnalysisResult`] out. All intermediate DSP is `f64`. The
//! windowed FFT normalization matches the Python reference server's
//! `/ (len/2) / wc` convention so `fundamental_dbfs` and
//! `harmonic_levels` are bit-compatible across implementations.

use std::f64::consts::PI;
use std::ops::Range;

use anyhow::{bail, Result};

use crate::measurement::report::StandardsCitation;
use crate::shared::constants::{FUNDAMENTAL_HZ, NUM_HARMONICS, SAMPLERATE};
use crate::shared::fft_cache::{freq_axis, real_fft_plan, with_hann_window};
use crate::shared::reference_levels::amplitude_to_dbfs;
use crate::shared::types::AnalysisResult;

/// Relative headroom for the `thdn_pct >= thd_pct` physical-law check.
///
/// `thd_pct` sums harmonic **peak-bin amplitudes**; `thdn_pct` sums
/// **ENBW-normalized bin power** over the residual region (which is a
/// superset of the harmonic bins). Both are true ratios to the same
/// `total_output_rms` denominator, so the inequality holds physically, but
/// the two numerators are formed by different summations and disagree by a
/// small numerical residue once no `.max()` clamp forces the order. Measured
/// residue on the coherent 1 % H2 fixture (`thdn_is_never_below_thd_for_coherent_harmonic`,
/// 48 000-sample FFT): ~1.9e-9 relative. This constant is ~500x that
/// headroom — enough to absorb float accumulation, not enough to hide a real
/// estimator inversion, which is percent-scale. Used at every site that
/// compares the two fields; do not raise it to fit a failing fixture — that
/// is an estimator defect, not a tolerance shortfall.
pub const THDN_GE_THD_REL_TOL: f64 = 1e-6;

/// Citation for a `MeasurementReport` populated from [`analyze`] output.
///
/// IEC 60268-3:2018 "Sound system equipment — Part 3: Amplifiers". THD is
/// defined in §15.12 "Amplitude non-linearity"; §15.12.3.2 is the specific
/// total-distortion measurement that matches this analyser's THD+N output.
/// Verified against the full text at
/// `stddocs/iec-full/Sound system equipment_ Amplifiers … 2018 …pdf`:
/// §15.12.3.2 gives the ratio and dB formulae implemented here.
pub fn citation() -> StandardsCitation {
    StandardsCitation {
        standard: "IEC 60268-3:2018".into(),
        clause: "§15.12.3.2 Total harmonic distortion under standard measuring conditions".into(),
        verified: true,
    }
}

/// Analyse a mono audio capture and return THD, THD+N, noise floor, spectrum.
///
/// # Arguments
///
/// * `samples`     — mono `f32` PCM, any length ≥ 256 samples
/// * `sr`          — sample rate in Hz (typically 48 000)
/// * `fundamental` — expected fundamental frequency in Hz
/// * `n_harmonics` — number of harmonics to track (2nd … n+1th)
///
/// # Errors
///
/// Returns an error if `samples.len() < 256` or if no signal is detected
/// at the fundamental (`f1_amp < 1e-9`).
pub fn analyze(
    samples: &[f32],
    sr: u32,
    fundamental: f64,
    n_harmonics: usize,
) -> Result<AnalysisResult> {
    let n = samples.len();
    if n < 256 {
        bail!("need at least 256 samples, got {n}");
    }

    let mono: Vec<f64> = samples.iter().map(|&x| x as f64).collect();

    let fft = real_fft_plan(n);
    let mut windowed = vec![0.0f64; n];
    let mut win_spectrum = fft.make_output_vec();

    let (wc, enbw_bins) = with_hann_window(n, |win, wc| {
        for i in 0..n {
            windowed[i] = mono[i] * win[i];
        }
        let window_power = win.iter().map(|w| w * w).sum::<f64>() / n as f64;
        (wc, window_power / (wc * wc))
    });

    fft.process(&mut windowed, &mut win_spectrum)
        .map_err(|e| anyhow::anyhow!("FFT error: {e:?}"))?;

    let norm = (n as f64 / 2.0) * wc;
    let spec: Vec<f64> = win_spectrum.iter().map(|c| c.norm() / norm).collect();
    let freqs = freq_axis(n, sr);

    let bin_hz = sr as f64 / n as f64;
    let fundamental_bin = fundamental / bin_hz;
    let f1_bin = find_fundamental_peak(&spec, fundamental_bin);
    let f1_amp = spec[f1_bin];

    if f1_amp < 1e-9 {
        bail!("No signal -- check connections");
    }

    // A Hann tone's main lobe extends two bins either side of its peak.
    let bw = ((fundamental * 0.1 / bin_hz) as usize).max(2);
    let notch = symmetric_bin_range(f1_bin, bw, spec.len());

    let mut h_amps: Vec<f64> = Vec::with_capacity(n_harmonics);
    let mut harmonic_levels: Vec<(f64, f64)> = Vec::with_capacity(n_harmonics);

    for harmonic in 2..=(n_harmonics + 1) {
        let hf = fundamental * harmonic as f64;
        if hf > sr as f64 / 2.0 {
            break;
        }
        let amp = find_harmonic_peak(&spec, fundamental_bin, notch.end, harmonic)
            .map(|bin| spec[bin])
            .unwrap_or(0.0);
        h_amps.push(amp);
        harmonic_levels.push((hf, amp));
    }

    let harmonic_power = h_amps.iter().map(|a| a * a).sum::<f64>();

    // THD+N: notch the fundamental and sum |spec|² outside. Direct sum
    // beats `total − notch` when nearly all energy sits in the notch.
    let has_nyquist = n.is_multiple_of(2);
    let thdn_sq = one_sided_power(&spec, 0..notch.start, has_nyquist)
        + one_sided_power(&spec, notch.end..spec.len(), has_nyquist);
    // `spec` is coherent-gain normalized for tone amplitudes. Dividing its
    // power by the Hann equivalent noise bandwidth converts the broadband
    // bin sum to the same RMS ratio used by IEC 60268-3.
    let residual_power = thdn_sq / enbw_bins;
    let total_amp = (f1_amp * f1_amp + residual_power).sqrt();
    let thd = (harmonic_power.sqrt() / total_amp * 100.0).clamp(0.0, 100.0);
    let thdn = (residual_power.sqrt() / total_amp * 100.0).clamp(0.0, 100.0);

    let fundamental_dbfs = amplitude_to_dbfs(f1_amp);

    let trim = ((n as f64 * 0.05) as usize).max(1);
    let rms_slice = &mono[trim..n - trim];
    let linear_rms = (rms_slice.iter().map(|x| x * x).sum::<f64>() / rms_slice.len() as f64).sqrt();

    // Noise floor = residual after subtracting all harmonics, reconstructed
    // with amplitude from the windowed FFT and phase from the unwindowed
    // FFT (matches the Python reference). Uses a cos/sin recurrence
    // (angle-addition) so the inner loop runs one real multiply instead of
    // a libm cos() per sample.
    windowed.copy_from_slice(&mono);
    let mut raw_spectrum = fft.make_output_vec();
    fft.process(&mut windowed, &mut raw_spectrum)
        .map_err(|e| anyhow::anyhow!("FFT (phase) error: {e:?}"))?;

    let mut residual = mono.clone();
    for harmonic in 1..=(n_harmonics + 1) {
        let hf = fundamental * harmonic as f64;
        if hf > sr as f64 / 2.0 {
            break;
        }
        let hb = if harmonic == 1 {
            Some(f1_bin)
        } else {
            find_harmonic_peak(&spec, fundamental_bin, notch.end, harmonic)
        };
        let Some(hb) = hb else {
            continue;
        };
        let hf_real = freqs[hb];
        let phase = raw_spectrum[hb].arg();
        let amp_time = spec[hb];

        let dtheta = 2.0 * PI * hf_real / sr as f64;
        let cos_d = dtheta.cos();
        let sin_d = dtheta.sin();
        let mut c = phase.cos();
        let mut s = phase.sin();
        for r in residual.iter_mut() {
            *r -= amp_time * c;
            let c_new = c * cos_d - s * sin_d;
            let s_new = s * cos_d + c * sin_d;
            c = c_new;
            s = s_new;
        }
    }
    let res_slice = &residual[trim..n - trim];
    let residual_rms =
        (res_slice.iter().map(|x| x * x).sum::<f64>() / res_slice.len() as f64).sqrt();
    let noise_floor_dbfs = amplitude_to_dbfs(residual_rms);

    let clipping = mono[trim..n - trim].iter().any(|&x| x.abs() >= 0.9999);

    // AC-coupling heuristic: at < 50 Hz a 2nd harmonic carrying > 80 % of
    // harmonic power indicates capacitor-coupling asymmetry.
    let ac_coupled = if fundamental < 50.0 && !h_amps.is_empty() && harmonic_power > 0.0 {
        (h_amps[0] * h_amps[0] / harmonic_power) > 0.80
    } else {
        false
    };

    Ok(AnalysisResult {
        fundamental_hz: fundamental,
        fundamental_dbfs,
        linear_rms,
        thd_pct: thd,
        thdn_pct: thdn,
        total_output_rms: total_amp,
        harmonic_levels,
        noise_floor_dbfs,
        spectrum: spec,
        freqs,
        clipping,
        ac_coupled,
    })
}

fn symmetric_bin_range(center: usize, radius: usize, len: usize) -> Range<usize> {
    center.saturating_sub(radius)..center.saturating_add(radius).saturating_add(1).min(len)
}

/// Locate the fundamental in a relative window that never includes DC.
fn find_fundamental_peak(spec: &[f64], fundamental_bin: f64) -> usize {
    let center = (fundamental_bin.round() as usize).min(spec.len() - 1);
    let radius = (fundamental_bin * 0.1).floor().max(1.0) as usize;
    let mut range = symmetric_bin_range(center, radius, spec.len());
    range.start = range.start.max(1).min(range.end.saturating_sub(1));
    range
        .max_by(|&a, &b| spec[a].partial_cmp(&spec[b]).unwrap())
        .unwrap_or(center)
}

fn one_sided_power(spec: &[f64], range: Range<usize>, has_nyquist: bool) -> f64 {
    range
        .map(|bin| {
            let endpoint_weight = if bin == 0 || (has_nyquist && bin + 1 == spec.len()) {
                0.5
            } else {
                1.0
            };
            endpoint_weight * spec[bin] * spec[bin]
        })
        .sum()
}

/// Locate a harmonic in a fundamental-relative bin window. A nominal 10 %
/// radius (at least one bin) is clamped at neighboring-harmonic midpoints and
/// outside the fundamental notch, so the search regions cannot overlap.
fn find_harmonic_peak(
    spec: &[f64],
    fundamental_bin: f64,
    fundamental_notch_end: usize,
    harmonic: usize,
) -> Option<usize> {
    let center = (fundamental_bin * harmonic as f64).round() as usize;
    let center = center.min(spec.len() - 1);
    let radius = (fundamental_bin * 0.1).floor().max(1.0) as usize;
    let lower_midpoint = (fundamental_bin * (harmonic as f64 - 0.5)).ceil() as usize;
    let upper_midpoint = (fundamental_bin * (harmonic as f64 + 0.5)).ceil() as usize;
    let mut range = symmetric_bin_range(center, radius, spec.len());
    range.start = range.start.max(lower_midpoint);
    range.end = range.end.min(upper_midpoint);
    if harmonic == 2 {
        range.start = range.start.max(fundamental_notch_end);
    }

    range.max_by(|&a, &b| spec[a].partial_cmp(&spec[b]).unwrap())
}

/// Convenience wrapper using [`SAMPLERATE`], [`FUNDAMENTAL_HZ`] and
/// [`NUM_HARMONICS`] defaults.
pub fn analyze_default(samples: &[f32]) -> Result<AnalysisResult> {
    analyze(samples, SAMPLERATE, FUNDAMENTAL_HZ, NUM_HARMONICS)
}

/// Find the bin with the highest magnitude within `±tol_hz` of `target_hz`.
/// Falls back to the nearest bin if none are in range.
pub fn find_peak(spec: &[f64], freqs: &[f64], target_hz: f64, tol_hz: f64) -> usize {
    let candidates: Vec<usize> = freqs
        .iter()
        .enumerate()
        .filter(|(_, &f)| (f - target_hz).abs() < tol_hz)
        .map(|(i, _)| i)
        .collect();

    if candidates.is_empty() {
        freqs
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                ((*a - target_hz).abs())
                    .partial_cmp(&((*b - target_hz).abs()))
                    .unwrap()
            })
            .map(|(i, _)| i)
            .unwrap_or(0)
    } else {
        candidates
            .into_iter()
            .max_by(|&a, &b| spec[a].partial_cmp(&spec[b]).unwrap())
            .unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;

    fn pure_sine(freq: f64, amplitude: f64, sr: u32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (amplitude * (2.0 * PI * freq * i as f64 / sr as f64).sin()) as f32)
            .collect()
    }

    fn deterministic_noise(n: usize, rms: f64) -> Vec<f64> {
        let mut state = 0x426_u64;
        let mut noise: Vec<f64> = (0..n)
            .map(|_| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1);
                (state >> 32) as f64 / u32::MAX as f64 * 2.0 - 1.0
            })
            .collect();
        let mean = noise.iter().sum::<f64>() / n as f64;
        for sample in &mut noise {
            *sample -= mean;
        }
        let measured_rms = (noise.iter().map(|x| x * x).sum::<f64>() / n as f64).sqrt();
        for sample in &mut noise {
            *sample *= rms / measured_rms;
        }
        noise
    }

    const SR: u32 = 48_000;
    const F1: f64 = 1_000.0;

    #[test]
    fn pure_sine_linear_rms() {
        let samples = pure_sine(F1, 0.5, SR, SR as usize);
        let r = analyze(&samples, SR, F1, 10).unwrap();
        let expected = 0.5 / std::f64::consts::SQRT_2;
        assert_relative_eq!(r.linear_rms, expected, epsilon = 1e-4);
    }

    #[test]
    fn pure_sine_fundamental_detected() {
        let samples = pure_sine(F1, 0.5, SR, SR as usize);
        let r = analyze(&samples, SR, F1, 10).unwrap();
        assert_relative_eq!(r.fundamental_hz, F1, epsilon = 1.0);
    }

    #[test]
    fn pure_sine_low_thd() {
        let samples = pure_sine(F1, 0.5, SR, SR as usize);
        let r = analyze(&samples, SR, F1, 10).unwrap();
        assert!(
            r.thd_pct < 0.01,
            "THD too high for pure sine: {:.4}%",
            r.thd_pct
        );
    }

    #[test]
    fn broadband_noise_uses_hann_enbw_normalization() {
        let n = SR as usize;
        let noise = deterministic_noise(n, 0.001);
        let samples: Vec<f32> = pure_sine(F1, 0.5, SR, n)
            .into_iter()
            .zip(noise)
            .map(|(tone, noise)| tone + noise as f32)
            .collect();

        let r = analyze(&samples, SR, F1, 10).unwrap();
        let expected_pct = 0.001 / (0.5 / std::f64::consts::SQRT_2) * 100.0;
        assert_relative_eq!(r.thdn_pct, expected_pct, epsilon = 0.005);
        assert_relative_eq!(r.noise_floor_dbfs, -60.0, epsilon = 0.2);
    }

    #[test]
    fn pure_20_hz_harmonic_search_excludes_fundamental_leakage() {
        let samples = pure_sine(20.0, 0.5, SR, SR as usize);
        let r = analyze(&samples, SR, 20.0, 10).unwrap();
        assert!(r.thd_pct < 0.01, "20 Hz THD was {:.4}%", r.thd_pct);
        assert!(r.thdn_pct < 0.01, "20 Hz THD+N was {:.4}%", r.thdn_pct);
        assert!(
            r.thdn_pct + THDN_GE_THD_REL_TOL * r.thd_pct.abs() >= r.thd_pct,
            "THD+N {:.12}% was below THD {:.12}% at 20 Hz",
            r.thdn_pct,
            r.thd_pct
        );
    }

    #[test]
    fn fundamental_search_at_20_hz_excludes_dc_offset() {
        let tone_amp = 0.25_f64;
        let samples: Vec<f32> = pure_sine(20.0, tone_amp, SR, SR as usize)
            .into_iter()
            .map(|sample| sample + 0.5)
            .collect();

        let r = analyze(&samples, SR, 20.0, 10).unwrap();
        assert_relative_eq!(
            r.fundamental_dbfs,
            amplitude_to_dbfs(tone_amp),
            epsilon = 0.001
        );
        assert!(r.thd_pct < 0.01, "20 Hz THD was {:.4}%", r.thd_pct);
    }

    #[test]
    fn pure_50_hz_has_low_thd_and_noise() {
        let samples = pure_sine(50.0, 0.5, SR, SR as usize);
        let r = analyze(&samples, SR, 50.0, 10).unwrap();
        assert!(r.thd_pct < 0.01, "50 Hz THD was {:.4}%", r.thd_pct);
        assert!(r.thdn_pct < 0.01, "50 Hz THD+N was {:.4}%", r.thdn_pct);
    }

    #[test]
    fn pure_100_hz_has_low_thd_and_noise() {
        let samples = pure_sine(100.0, 0.5, SR, SR as usize);
        let r = analyze(&samples, SR, 100.0, 10).unwrap();
        assert!(r.thd_pct < 0.01, "100 Hz THD was {:.4}%", r.thd_pct);
        assert!(r.thdn_pct < 0.01, "100 Hz THD+N was {:.4}%", r.thdn_pct);
    }

    #[test]
    fn minimum_256_sample_1khz_capture_rejects_fundamental_skirt() {
        let samples = pure_sine(F1, 0.5, SR, 256);
        let r = analyze(&samples, SR, F1, 10).unwrap();
        assert!(r.thd_pct < 1.0, "256-sample THD was {:.4}%", r.thd_pct);
        assert!(r.thdn_pct < 2.0, "256-sample THD+N was {:.4}%", r.thdn_pct);
        assert!(
            r.thdn_pct + THDN_GE_THD_REL_TOL * r.thd_pct.abs() >= r.thd_pct,
            "THD+N {:.12}% was below THD {:.12}% at 256 samples",
            r.thdn_pct,
            r.thd_pct
        );
    }

    #[test]
    fn symmetric_fundamental_notch_excludes_both_edge_bins() {
        assert_eq!(symmetric_bin_range(10, 2, 32), 8..13);
    }

    #[test]
    fn dc_residual_uses_time_domain_rms() {
        let n = SR as usize;
        let residual_rms = 0.01_f64;
        let samples: Vec<f32> = pure_sine(F1, 0.5, SR, n)
            .into_iter()
            .map(|sample| sample + residual_rms as f32)
            .collect();

        let r = analyze(&samples, SR, F1, 10).unwrap();
        let residual_amp = residual_rms * std::f64::consts::SQRT_2;
        let expected_pct = residual_amp / (0.5_f64.powi(2) + residual_amp.powi(2)).sqrt() * 100.0;
        assert_relative_eq!(r.thdn_pct, expected_pct, epsilon = 0.001);
    }

    #[test]
    fn nyquist_residual_uses_time_domain_rms() {
        let n = SR as usize;
        let residual_rms = 0.01_f64;
        let samples: Vec<f32> = pure_sine(F1, 0.5, SR, n)
            .into_iter()
            .enumerate()
            .map(|(i, sample)| {
                let nyquist = if i.is_multiple_of(2) {
                    residual_rms
                } else {
                    -residual_rms
                };
                sample + nyquist as f32
            })
            .collect();

        let r = analyze(&samples, SR, F1, 10).unwrap();
        let residual_amp = residual_rms * std::f64::consts::SQRT_2;
        let expected_pct = residual_amp / (0.5_f64.powi(2) + residual_amp.powi(2)).sqrt() * 100.0;
        assert_relative_eq!(r.thdn_pct, expected_pct, epsilon = 0.001);
    }

    #[test]
    fn distortion_ratios_are_referenced_to_total_output() {
        let fundamental_amp = 0.5_f64;
        let harmonic_amp = 0.25_f64;
        let samples: Vec<f32> = (0..SR as usize)
            .map(|i| {
                let t = i as f64 / SR as f64;
                (fundamental_amp * (2.0 * PI * F1 * t).sin()
                    + harmonic_amp * (2.0 * PI * 2.0 * F1 * t).sin()) as f32
            })
            .collect();

        let r = analyze(&samples, SR, F1, 10).unwrap();
        let expected_pct =
            harmonic_amp / (fundamental_amp.powi(2) + harmonic_amp.powi(2)).sqrt() * 100.0;
        let rejected_re_fundamental = harmonic_amp / fundamental_amp * 100.0;
        assert_relative_eq!(expected_pct, 44.721_359_550, epsilon = 1e-9);
        assert_relative_eq!(rejected_re_fundamental, 50.0, epsilon = 1e-12);
        assert!((r.thd_pct - rejected_re_fundamental).abs() > 1.0);
        assert_relative_eq!(r.thd_pct, expected_pct, epsilon = 0.001);
        assert_relative_eq!(r.thdn_pct, expected_pct, epsilon = 0.001);
    }

    #[test]
    fn harmonic_bin_window_cannot_select_fundamental_skirt() {
        let mut spec = vec![0.0; 101];
        spec[21] = 1.0;
        spec[40] = 0.01;
        assert_eq!(find_harmonic_peak(&spec, 20.0, 23, 2), Some(40));
    }

    #[test]
    fn harmonic_bin_window_clamps_at_dc_and_nyquist() {
        let spec = vec![0.0; 129];
        assert_eq!(find_harmonic_peak(&spec, 0.6, 3, 2), None);
        let nyquist_bin = find_harmonic_peak(&spec, 64.0, 67, 2).unwrap();
        assert!((122..129).contains(&nyquist_bin));
    }

    #[test]
    fn thdn_is_never_below_thd_for_coherent_harmonic() {
        let samples: Vec<f32> = (0..SR as usize)
            .map(|i| {
                let t = i as f64 / SR as f64;
                (0.5 * (2.0 * PI * F1 * t).sin() + 0.005 * (2.0 * PI * 2.0 * F1 * t).sin()) as f32
            })
            .collect();
        let r = analyze(&samples, SR, F1, 10).unwrap();
        assert!(
            r.thdn_pct + THDN_GE_THD_REL_TOL * r.thd_pct.abs() >= r.thd_pct,
            "THD+N {:.12}% was below THD {:.12}%",
            r.thdn_pct,
            r.thd_pct
        );
    }

    #[test]
    fn pure_sine_no_clipping() {
        let samples = pure_sine(F1, 0.5, SR, SR as usize);
        let r = analyze(&samples, SR, F1, 10).unwrap();
        assert!(!r.clipping);
    }

    #[test]
    fn clipping_detected() {
        let samples: Vec<f32> = pure_sine(F1, 2.0, SR, SR as usize)
            .into_iter()
            .map(|x| x.clamp(-1.0, 1.0))
            .collect();
        let r = analyze(&samples, SR, F1, 10).unwrap();
        assert!(
            r.clipping,
            "clipping flag should be set for saturated signal"
        );
    }

    #[test]
    fn spectrum_length() {
        let n = SR as usize;
        let samples = pure_sine(F1, 0.5, SR, n);
        let r = analyze(&samples, SR, F1, 10).unwrap();
        let expected_len = n / 2 + 1;
        assert_eq!(r.spectrum.len(), expected_len);
        assert_eq!(r.freqs.len(), expected_len);
    }

    #[test]
    fn freqs_axis_correct() {
        let n = SR as usize;
        let samples = pure_sine(F1, 0.5, SR, n);
        let r = analyze(&samples, SR, F1, 10).unwrap();
        assert_relative_eq!(r.freqs[0], 0.0, epsilon = 1e-9);
        let nyq = SR as f64 / 2.0;
        assert_relative_eq!(*r.freqs.last().unwrap(), nyq, epsilon = 1e-6);
        assert_relative_eq!(r.freqs[1000], 1000.0, epsilon = 1e-6);
    }

    #[test]
    fn no_signal_returns_error() {
        let samples = vec![0.0f32; 48_000];
        assert!(analyze(&samples, SR, F1, 10).is_err());
    }

    #[test]
    fn wrong_fundamental_returns_error_or_high_thd() {
        // Signal is a pure 1 kHz tone, but we tell `analyze` the fundamental
        // is 2 kHz. The 2 kHz bin holds no real tone, so either the
        // fundamental-energy guard trips (Err) or the THD ratio blows up.
        let samples = pure_sine(1_000.0, 0.5, SR, SR as usize);
        // An `Err` (fundamental-energy guard tripped) is an acceptable
        // outcome; if it does return Ok, the THD ratio must be blown out.
        if let Ok(r) = analyze(&samples, SR, 2_000.0, 10) {
            assert!(
                r.thd_pct > 10.0,
                "wrong fundamental should yield high THD, got {:.4}%",
                r.thd_pct
            );
        }
    }

    #[test]
    fn harmonic_levels_count() {
        let samples = pure_sine(F1, 0.5, SR, SR as usize);
        let r = analyze(&samples, SR, F1, 10).unwrap();
        assert_eq!(r.harmonic_levels.len(), 10);
    }

    #[test]
    fn harmonic_frequencies_are_multiples() {
        let samples = pure_sine(F1, 0.5, SR, SR as usize);
        let r = analyze(&samples, SR, F1, 10).unwrap();
        for (i, &(freq, _)) in r.harmonic_levels.iter().enumerate() {
            let expected = F1 * (i + 2) as f64;
            assert_relative_eq!(freq, expected, epsilon = 1.0);
        }
    }

    #[test]
    fn ac_coupled_not_set_at_1khz() {
        let samples = pure_sine(F1, 0.5, SR, SR as usize);
        let r = analyze(&samples, SR, F1, 10).unwrap();
        assert!(!r.ac_coupled);
    }

    #[test]
    fn half_second_capture() {
        let n = SR as usize / 2;
        let samples = pure_sine(F1, 0.5, SR, n);
        let r = analyze(&samples, SR, F1, 10).unwrap();
        assert!(r.thd_pct < 0.1);
    }

    #[test]
    fn ten_second_capture() {
        let n = SR as usize * 10;
        let samples = pure_sine(F1, 0.5, SR, n);
        let r = analyze(&samples, SR, F1, 10).unwrap();
        assert!(r.thd_pct < 0.01);
    }

    #[test]
    fn json_round_trip() {
        let samples = pure_sine(F1, 0.5, SR, SR as usize);
        let r = analyze(&samples, SR, F1, 10).unwrap();
        let json = serde_json::to_string(&r).unwrap();
        let r2: AnalysisResult = serde_json::from_str(&json).unwrap();
        assert_relative_eq!(r2.thd_pct, r.thd_pct, epsilon = 1e-12);
        assert_relative_eq!(r2.linear_rms, r.linear_rms, epsilon = 1e-12);
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        fn sine_with_phase(freq: f64, amplitude: f64, phase: f64, sr: u32, n: usize) -> Vec<f32> {
            (0..n)
                .map(|i| {
                    (amplitude * (2.0 * PI * freq * i as f64 / sr as f64 + phase).sin()) as f32
                })
                .collect()
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(64))]

            #[test]
            fn pure_sine_rms_within_tolerance(
                freq   in 200.0f64..8_000.0,
                amp_db in -40.0f64..(-3.0),
                phase  in 0.0f64..(2.0 * PI),
            ) {
                let amp = 10f64.powf(amp_db / 20.0);
                let samples = sine_with_phase(freq, amp, phase, SR, SR as usize);
                let r = analyze(&samples, SR, freq, 8).unwrap();

                let expected = amp / std::f64::consts::SQRT_2;
                let got_db = 20.0 * r.linear_rms.max(1e-12).log10();
                let expected_db = 20.0 * expected.max(1e-12).log10();
                prop_assert!(
                    (got_db - expected_db).abs() < 0.3,
                    "RMS off: got {:.3} dB, expected {:.3} dB (freq={freq}, amp={amp}, phase={phase})",
                    got_db, expected_db
                );
            }

            #[test]
            fn pure_sine_thd_is_low(
                freq  in 300.0f64..6_000.0,
                amp   in 0.05f64..0.8,
                phase in 0.0f64..(2.0 * PI),
            ) {
                let samples = sine_with_phase(freq, amp, phase, SR, SR as usize);
                let r = analyze(&samples, SR, freq, 8).unwrap();
                prop_assert!(
                    r.thd_pct < 0.1,
                    "THD too high: {:.4}% (freq={freq}, amp={amp})", r.thd_pct
                );
            }

            #[test]
            fn analyze_is_total_on_sensible_inputs(
                freq  in 100.0f64..10_000.0,
                amp   in 0.01f64..0.9,
                phase in 0.0f64..(2.0 * PI),
                n_hp  in 1usize..12,
            ) {
                let samples = sine_with_phase(freq, amp, phase, SR, SR as usize);
                let r = analyze(&samples, SR, freq, n_hp).unwrap();

                prop_assert!(r.fundamental_dbfs.is_finite());
                prop_assert!(r.thd_pct.is_finite());
                prop_assert!(r.thdn_pct.is_finite());
                prop_assert!(r.noise_floor_dbfs.is_finite());
                prop_assert!(r.linear_rms.is_finite());
                prop_assert_eq!(r.spectrum.len(), SR as usize / 2 + 1);
                prop_assert_eq!(r.freqs.len(), r.spectrum.len());
                prop_assert!(r.thdn_pct + THDN_GE_THD_REL_TOL * r.thd_pct.abs() >= r.thd_pct);
            }

            #[test]
            fn second_harmonic_lifts_thd(
                freq in 500.0f64..4_000.0,
                amp  in 0.1f64..0.5,
            ) {
                let n = SR as usize;
                let samples: Vec<f32> = (0..n).map(|i| {
                    let t = i as f64 / SR as f64;
                    let fund = amp * (2.0 * PI * freq * t).sin();
                    let h2 = amp * 0.02 * (2.0 * PI * 2.0 * freq * t).sin();
                    (fund + h2) as f32
                }).collect();
                let r = analyze(&samples, SR, freq, 8).unwrap();
                prop_assert!(
                    r.thd_pct > 1.5,
                    "expected THD > 1.5% from 2% 2nd harmonic, got {:.3}%", r.thd_pct
                );
            }
        }
    }
}
