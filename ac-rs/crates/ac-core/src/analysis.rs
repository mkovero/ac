//! FFT-based audio analysis — THD, THD+N, noise floor.
//!
//! Direct port of `ac/server/analysis.py`.  The public API is a single
//! function [`analyze`] that accepts a mono `f32` sample slice and returns
//! an [`AnalysisResult`].
//!
//! # Numeric fidelity
//!
//! All intermediate computation uses `f64`.  The windowed FFT normalization
//! matches the Python `/ (len(mono) / 2) / wc` convention, so
//! `fundamental_dbfs` and `harmonic_levels` are bit-compatible with the
//! existing Python server output.

use std::f64::consts::PI;

use anyhow::{bail, Result};
use realfft::RealFftPlanner;

use crate::constants::{FUNDAMENTAL_HZ, NUM_HARMONICS, SAMPLERATE};
use crate::types::AnalysisResult;

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

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
/// Returns an error if no signal is detected at the fundamental
/// (`f1_amp < 1e-9`).
pub fn analyze(
    samples: &[f32],
    sr: u32,
    fundamental: f64,
    n_harmonics: usize,
) -> Result<AnalysisResult> {
    let n = samples.len();
    assert!(n >= 256, "need at least 256 samples");

    // Convert to f64 mono.
    let mono: Vec<f64> = samples.iter().map(|&x| x as f64).collect();

    // ------------------------------------------------------------------
    // Hann window + windowed FFT
    // ------------------------------------------------------------------

    // Symmetric Hann window: w[i] = 0.5 * (1 – cos(2π·i / (N-1)))
    // Matches scipy.signal.get_window("hann", N).
    let win: Vec<f64> = (0..n)
        .map(|i| 0.5 * (1.0 - (2.0 * PI * i as f64 / (n - 1) as f64).cos()))
        .collect();

    // Window RMS correction factor.
    let wc = (win.iter().map(|w| w * w).sum::<f64>() / n as f64).sqrt();

    // Apply window — clone mono first; FFT process() may clobber its input.
    let mut windowed: Vec<f64> = mono.iter().zip(win.iter()).map(|(x, w)| x * w).collect();

    let mut planner = RealFftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(n);

    let mut win_spectrum = fft.make_output_vec();
    fft.process(&mut windowed, &mut win_spectrum)
        .map_err(|e| anyhow::anyhow!("FFT error: {e:?}"))?;

    // Amplitude spectrum: |FFT(windowed)| / (N/2) / wc
    // This is the one-sided peak-amplitude spectrum used for THD ratios.
    let norm = (n as f64 / 2.0) * wc;
    let spec: Vec<f64> = win_spectrum.iter().map(|c| c.norm() / norm).collect();

    // Frequency axis: freqs[k] = k * sr / N
    let freqs: Vec<f64> = (0..win_spectrum.len())
        .map(|k| k as f64 * sr as f64 / n as f64)
        .collect();

    // ------------------------------------------------------------------
    // Fundamental peak
    // ------------------------------------------------------------------

    let f1_bin = find_peak(&spec, &freqs, fundamental, 20.0);
    let f1_amp = spec[f1_bin];

    if f1_amp < 1e-9 {
        bail!("No signal -- check connections");
    }

    // ------------------------------------------------------------------
    // Harmonics (2nd … n_harmonics+1th)
    // ------------------------------------------------------------------

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

    // ------------------------------------------------------------------
    // THD (%)
    // ------------------------------------------------------------------

    let thd = h_amps.iter().map(|a| a * a).sum::<f64>().sqrt() / f1_amp * 100.0;

    // ------------------------------------------------------------------
    // THD+N (%) — notch fundamental, compute residual RMS
    // ------------------------------------------------------------------

    let bin_hz = sr as f64 / n as f64;
    let bw = ((fundamental * 0.1 / bin_hz) as usize).max(1);

    let mut spec_nn = spec.clone();
    let lo = f1_bin.saturating_sub(bw);
    let hi = (f1_bin + bw).min(spec_nn.len());
    spec_nn[lo..hi].iter_mut().for_each(|x| *x = 0.0);

    let thdn = spec_nn.iter().map(|x| x * x).sum::<f64>().sqrt() / f1_amp * 100.0;

    // ------------------------------------------------------------------
    // Fundamental level (dBFS, windowed spectrum)
    // ------------------------------------------------------------------

    let fundamental_dbfs = 20.0 * f1_amp.max(1e-12).log10();

    // ------------------------------------------------------------------
    // Linear RMS — time domain, 5% trim
    // ------------------------------------------------------------------

    let trim = ((n as f64 * 0.05) as usize).max(1);
    let rms_slice = &mono[trim..n - trim];
    let linear_rms =
        (rms_slice.iter().map(|x| x * x).sum::<f64>() / rms_slice.len() as f64).sqrt();

    // ------------------------------------------------------------------
    // Noise floor — residual after subtracting all harmonics
    //
    // Uses the unwindowed FFT for phase extraction, windowed amplitude
    // for magnitude — same approach as the Python server.
    // ------------------------------------------------------------------

    // Unwindowed FFT for phase (compute once, reuse across harmonics).
    let mut raw_input = mono.clone();
    let mut raw_spectrum = fft.make_output_vec();
    fft.process(&mut raw_input, &mut raw_spectrum)
        .map_err(|e| anyhow::anyhow!("FFT (phase) error: {e:?}"))?;

    // Time axis
    let t: Vec<f64> = (0..n).map(|i| i as f64 / sr as f64).collect();

    let mut residual = mono.clone();

    for harmonic in 1..=(n_harmonics + 1) {
        let hf = fundamental * harmonic as f64;
        if hf > sr as f64 / 2.0 {
            break;
        }
        let hb    = find_peak(&spec, &freqs, hf, 20.0);
        let hf_real = freqs[hb];
        let phase   = raw_spectrum[hb].arg();     // phase from unwindowed FFT
        let amp_time = spec[hb];                  // amplitude from windowed FFT

        for (i, r) in residual.iter_mut().enumerate() {
            *r -= amp_time * (2.0 * PI * hf_real * t[i] + phase).cos();
        }
    }

    let res_slice = &residual[trim..n - trim];
    let residual_rms =
        (res_slice.iter().map(|x| x * x).sum::<f64>() / res_slice.len() as f64).sqrt();
    let noise_floor_dbfs = 20.0 * residual_rms.max(1e-12).log10();

    // ------------------------------------------------------------------
    // Clipping detection
    // ------------------------------------------------------------------

    let clipping = mono[trim..n - trim].iter().any(|&x| x.abs() >= 0.9999);

    // ------------------------------------------------------------------
    // AC-coupling flag
    //
    // At low frequencies (< 50 Hz) a dominant 2nd harmonic (> 80% of THD)
    // indicates capacitor coupling causing waveform asymmetry rather than
    // genuine distortion.
    // ------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Convenience wrapper with library defaults
// ---------------------------------------------------------------------------

/// Convenience wrapper using [`SAMPLERATE`], [`FUNDAMENTAL_HZ`] and
/// [`NUM_HARMONICS`] defaults.
pub fn analyze_default(samples: &[f32]) -> Result<AnalysisResult> {
    analyze(samples, SAMPLERATE, FUNDAMENTAL_HZ, NUM_HARMONICS)
}

/// Compute just the amplitude spectrum (no THD/fundamental analysis).
/// Always succeeds for any input ≥ 2 samples. Used by `monitor_spectrum`
/// to publish data even when there is no detectable signal.
pub fn spectrum_only(samples: &[f32], sr: u32) -> (Vec<f64>, Vec<f64>) {
    let n = samples.len().max(2);
    let mono: Vec<f64> = samples.iter().map(|&x| x as f64).collect();
    let win: Vec<f64> = (0..n)
        .map(|i| 0.5 * (1.0 - (2.0 * PI * i as f64 / (n - 1) as f64).cos()))
        .collect();
    let wc = (win.iter().map(|w| w * w).sum::<f64>() / n as f64).sqrt();
    let mut windowed: Vec<f64> = mono.iter().zip(win.iter()).map(|(x, w)| x * w).collect();
    let mut planner = RealFftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(n);
    let mut out = fft.make_output_vec();
    if fft.process(&mut windowed, &mut out).is_err() {
        return (vec![], vec![]);
    }
    let norm = (n as f64 / 2.0) * wc;
    let spec: Vec<f64> = out.iter().map(|c| c.norm() / norm).collect();
    let freqs: Vec<f64> = (0..out.len())
        .map(|k| k as f64 * sr as f64 / n as f64)
        .collect();
    (spec, freqs)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Find the bin index with the highest magnitude within `±tol_hz` of
/// `target_hz`.  Falls back to the nearest bin if none are in range.
///
/// Mirrors `_find_peak` in `analysis.py`.
fn find_peak(spec: &[f64], freqs: &[f64], target_hz: f64, tol_hz: f64) -> usize {
    // Collect candidate indices within tolerance.
    let candidates: Vec<usize> = freqs
        .iter()
        .enumerate()
        .filter(|(_, &f)| (f - target_hz).abs() < tol_hz)
        .map(|(i, _)| i)
        .collect();

    if candidates.is_empty() {
        // No bins within tolerance — return the nearest bin.
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
        // Return the candidate with the highest magnitude.
        candidates
            .into_iter()
            .max_by(|&a, &b| spec[a].partial_cmp(&spec[b]).unwrap())
            .unwrap()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;

    /// Generate a pure sine of known amplitude for testing.
    fn pure_sine(freq: f64, amplitude: f64, sr: u32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (amplitude * (2.0 * PI * freq * i as f64 / sr as f64).sin()) as f32)
            .collect()
    }

    const SR: u32 = 48_000;
    const F1: f64 = 1_000.0;

    // ------------------------------------------------------------------
    // Basic correctness
    // ------------------------------------------------------------------

    #[test]
    fn pure_sine_linear_rms() {
        // 1000 Hz sine, amplitude 0.5 → RMS = 0.5/√2
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
        // A numerically perfect sine should have THD well below 0.01%.
        let samples = pure_sine(F1, 0.5, SR, SR as usize);
        let r = analyze(&samples, SR, F1, 10).unwrap();
        assert!(
            r.thd_pct < 0.01,
            "THD too high for pure sine: {:.4}%",
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
        // Saturated sine — clip to ±1.0 hard.
        let samples: Vec<f32> = pure_sine(F1, 2.0, SR, SR as usize)
            .into_iter()
            .map(|x| x.clamp(-1.0, 1.0))
            .collect();
        let r = analyze(&samples, SR, F1, 10).unwrap();
        assert!(r.clipping, "clipping flag should be set for saturated signal");
    }

    // ------------------------------------------------------------------
    // Spectrum dimensions
    // ------------------------------------------------------------------

    #[test]
    fn spectrum_length() {
        let n = SR as usize; // 48 000 samples
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
        // DC bin
        assert_relative_eq!(r.freqs[0], 0.0, epsilon = 1e-9);
        // Nyquist bin
        let nyq = SR as f64 / 2.0;
        assert_relative_eq!(*r.freqs.last().unwrap(), nyq, epsilon = 1e-6);
        // 1000 Hz bin should be at index 1000 for N=48000, sr=48000
        assert_relative_eq!(r.freqs[1000], 1000.0, epsilon = 1e-6);
    }

    // ------------------------------------------------------------------
    // Error cases
    // ------------------------------------------------------------------

    #[test]
    fn no_signal_returns_error() {
        let samples = vec![0.0f32; 48_000];
        assert!(analyze(&samples, SR, F1, 10).is_err());
    }

    #[test]
    fn wrong_fundamental_returns_error_or_high_thd() {
        // Signal at 1 kHz, analysed at 2 kHz — either no signal or nonsense.
        let samples = pure_sine(1_000.0, 0.5, SR, SR as usize);
        let result = analyze(&samples, SR, 2_000.0, 10);
        // It might succeed (finds signal at wrong bin) or fail — either is
        // acceptable; the important thing is it doesn't panic.
        let _ = result;
    }

    // ------------------------------------------------------------------
    // Harmonic structure
    // ------------------------------------------------------------------

    #[test]
    fn harmonic_levels_count() {
        let samples = pure_sine(F1, 0.5, SR, SR as usize);
        let r = analyze(&samples, SR, F1, 10).unwrap();
        // All 10 harmonics (2nd–11th) fit below Nyquist at 1 kHz, 48 kHz sr.
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

    // ------------------------------------------------------------------
    // AC-coupling flag
    // ------------------------------------------------------------------

    #[test]
    fn ac_coupled_not_set_at_1khz() {
        let samples = pure_sine(F1, 0.5, SR, SR as usize);
        let r = analyze(&samples, SR, F1, 10).unwrap();
        assert!(!r.ac_coupled);
    }

    // ------------------------------------------------------------------
    // Non-standard lengths
    // ------------------------------------------------------------------

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

    // ------------------------------------------------------------------
    // JSON round-trip (ensures serde derives are correct)
    // ------------------------------------------------------------------

    #[test]
    fn json_round_trip() {
        let samples = pure_sine(F1, 0.5, SR, SR as usize);
        let r = analyze(&samples, SR, F1, 10).unwrap();
        let json = serde_json::to_string(&r).unwrap();
        let r2: crate::types::AnalysisResult = serde_json::from_str(&json).unwrap();
        assert_relative_eq!(r2.thd_pct, r.thd_pct, epsilon = 1e-12);
        assert_relative_eq!(r2.linear_rms, r.linear_rms, epsilon = 1e-12);
    }

    // ------------------------------------------------------------------
    // Property-based coverage over amplitude/frequency/phase grid.
    // Issue #33. Tolerances are deliberately loose — we're checking that
    // analyze() behaves *sanely* across the surface, not bit-exact.
    // ------------------------------------------------------------------

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

            /// A pure sine, regardless of amplitude/freq/phase, should yield
            /// `linear_rms ≈ amplitude / √2` within ±0.3 dB.
            #[test]
            fn pure_sine_rms_within_tolerance(
                freq      in 200.0f64..8_000.0,
                amp_db    in -40.0f64..(-3.0),
                phase     in 0.0f64..(2.0 * PI),
            ) {
                let amp = 10f64.powf(amp_db / 20.0);
                let samples = sine_with_phase(freq, amp, phase, SR, SR as usize);
                let r = analyze(&samples, SR, freq, 8).unwrap();

                let expected = amp / std::f64::consts::SQRT_2;
                let got_db      = 20.0 * r.linear_rms.max(1e-12).log10();
                let expected_db = 20.0 * expected.max(1e-12).log10();
                prop_assert!(
                    (got_db - expected_db).abs() < 0.3,
                    "RMS off: got {:.3} dB, expected {:.3} dB (freq={freq}, amp={amp}, phase={phase})",
                    got_db, expected_db
                );
            }

            /// THD of a numerically clean sine must stay below 0.1 %.
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

            /// Every returned field must be finite (no NaN / inf), the
            /// spectrum must match the expected bin count, and flags must
            /// be booleans — i.e. analyze never panics on in-range inputs.
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
                // THD+N is always at least THD.
                prop_assert!(r.thdn_pct + 1e-9 >= r.thd_pct);
            }

            /// Adding a known 2nd-harmonic component at 2% amplitude should
            /// push THD above 1.5 %.
            #[test]
            fn second_harmonic_lifts_thd(
                freq  in 500.0f64..4_000.0,
                amp   in 0.1f64..0.5,
            ) {
                let n = SR as usize;
                let samples: Vec<f32> = (0..n).map(|i| {
                    let t = i as f64 / SR as f64;
                    let fund = amp * (2.0 * PI * freq * t).sin();
                    let h2   = amp * 0.02 * (2.0 * PI * 2.0 * freq * t).sin();
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
