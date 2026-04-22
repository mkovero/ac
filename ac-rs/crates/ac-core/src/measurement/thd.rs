//! Tier 1 — THD / THD+N / noise-floor analysis of a stepped-sine capture.
//!
//! Citation for `MeasurementReport`s produced from this analyser is
//! provided by [`citation`]. The `verified` flag stays `false` until a
//! human cross-checks the emitted clause numbers against the published
//! IEC 60268-3:2018 text.
//!
//! The public entry point is [`analyze`]: mono `f32` PCM in, a fully
//! populated [`AnalysisResult`] out. All intermediate DSP is `f64`. The
//! windowed FFT normalization matches the Python reference server's
//! `/ (len/2) / wc` convention so `fundamental_dbfs` and
//! `harmonic_levels` are bit-compatible across implementations.

use std::f64::consts::PI;

use anyhow::{bail, Result};

use crate::measurement::report::StandardsCitation;
use crate::shared::constants::{FUNDAMENTAL_HZ, NUM_HARMONICS, SAMPLERATE};
use crate::shared::fft_cache::{freq_axis, real_fft_plan, with_hann_window};
use crate::shared::types::AnalysisResult;

/// Citation for a `MeasurementReport` populated from [`analyze`] output.
///
/// The clause number (§14.12) corresponds to the THD definition in
/// IEC 60268-3:2018 "Sound system equipment — Part 3: Amplifiers" per
/// widely-cited secondary sources; `verified` stays `false` until the
/// published PDF is checked in person.
pub fn citation() -> StandardsCitation {
    StandardsCitation {
        standard: "IEC 60268-3:2018".into(),
        clause: "§14.12 Total harmonic distortion".into(),
        verified: false,
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

    let wc = with_hann_window(n, |win, wc| {
        for i in 0..n {
            windowed[i] = mono[i] * win[i];
        }
        wc
    });

    fft.process(&mut windowed, &mut win_spectrum)
        .map_err(|e| anyhow::anyhow!("FFT error: {e:?}"))?;

    let norm = (n as f64 / 2.0) * wc;
    let spec: Vec<f64> = win_spectrum.iter().map(|c| c.norm() / norm).collect();
    let freqs = freq_axis(n, sr);

    let f1_bin = find_peak(&spec, &freqs, fundamental, 20.0);
    let f1_amp = spec[f1_bin];

    if f1_amp < 1e-9 {
        bail!("No signal -- check connections");
    }

    let mut h_amps: Vec<f64> = Vec::with_capacity(n_harmonics);
    let mut harmonic_levels: Vec<(f64, f64)> = Vec::with_capacity(n_harmonics);

    for harmonic in 2..=(n_harmonics + 1) {
        let hf = fundamental * harmonic as f64;
        if hf > sr as f64 / 2.0 {
            break;
        }
        let hb = find_peak(&spec, &freqs, hf, 20.0);
        h_amps.push(spec[hb]);
        harmonic_levels.push((hf, spec[hb]));
    }

    let thd = h_amps.iter().map(|a| a * a).sum::<f64>().sqrt() / f1_amp * 100.0;

    // THD+N: notch the fundamental and sum |spec|² outside. Direct sum
    // beats `total − notch` when nearly all energy sits in the notch.
    let bin_hz = sr as f64 / n as f64;
    let bw = ((fundamental * 0.1 / bin_hz) as usize).max(1);
    let lo = f1_bin.saturating_sub(bw);
    let hi = (f1_bin + bw).min(spec.len());
    let thdn_sq: f64 =
        spec[..lo].iter().map(|x| x * x).sum::<f64>() +
        spec[hi..].iter().map(|x| x * x).sum::<f64>();
    let thdn = thdn_sq.sqrt() / f1_amp * 100.0;

    let fundamental_dbfs = 20.0 * f1_amp.max(1e-12).log10();

    let trim = ((n as f64 * 0.05) as usize).max(1);
    let rms_slice = &mono[trim..n - trim];
    let linear_rms =
        (rms_slice.iter().map(|x| x * x).sum::<f64>() / rms_slice.len() as f64).sqrt();

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
        let hb = find_peak(&spec, &freqs, hf, 20.0);
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
    let noise_floor_dbfs = 20.0 * residual_rms.max(1e-12).log10();

    let clipping = mono[trim..n - trim].iter().any(|&x| x.abs() >= 0.9999);

    // AC-coupling heuristic: at < 50 Hz a dominant 2nd harmonic (> 80 % of
    // THD) indicates capacitor-coupling asymmetry rather than real distortion.
    let ac_coupled = if fundamental < 50.0 && !h_amps.is_empty() && thd > 0.0 {
        let h2_pct = h_amps[0] / f1_amp * 100.0;
        (h2_pct / thd) > 0.80
    } else {
        false
    };

    Ok(AnalysisResult {
        fundamental_hz: fundamental,
        fundamental_dbfs,
        linear_rms,
        thd_pct: thd,
        thdn_pct: thdn,
        harmonic_levels,
        noise_floor_dbfs,
        spectrum: spec,
        freqs,
        clipping,
        ac_coupled,
    })
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
        assert!(r.thd_pct < 0.01, "THD too high for pure sine: {:.4}%", r.thd_pct);
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
        assert!(r.clipping, "clipping flag should be set for saturated signal");
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
        let samples = pure_sine(1_000.0, 0.5, SR, SR as usize);
        let result = analyze(&samples, SR, 2_000.0, 10);
        let _ = result;
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
                .map(|i| (amplitude * (2.0 * PI * freq * i as f64 / sr as f64 + phase).sin()) as f32)
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
                prop_assert!(r.thdn_pct + 1e-9 >= r.thd_pct);
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
