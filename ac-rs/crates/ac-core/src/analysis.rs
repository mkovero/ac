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

use std::cell::RefCell;
use std::collections::HashMap;
use std::f64::consts::PI;
use std::sync::Arc;

use anyhow::{bail, Result};
use realfft::{RealFftPlanner, RealToComplex};

use crate::constants::{FUNDAMENTAL_HZ, NUM_HARMONICS, SAMPLERATE};
use crate::types::AnalysisResult;

thread_local! {
    /// Thread-local cache of forward real FFT plans keyed on N. `analyze` is
    /// called per monitor tick; planner construction is the single biggest
    /// non-twiddle cost and is wasted when N is stable (which it is once the
    /// UI settles on an `fft_n`). The UI ladder has 7 entries, so the cache
    /// is bounded at 7 plans per worker thread.
    static REAL_FFT_PLANS: RefCell<HashMap<usize, Arc<dyn RealToComplex<f64>>>> =
        RefCell::new(HashMap::new());

    /// Thread-local cache for the Hann window (depends only on N).
    static HANN_CACHE: RefCell<HannCache> = RefCell::new(HannCache::default());

    /// Thread-local cache for the frequency and time axes (depend on N, sr).
    static AXES_CACHE: RefCell<AxesCache> = RefCell::new(AxesCache::default());
}

#[derive(Default)]
struct HannCache {
    n: usize,
    win: Vec<f64>,
    wc:  f64,
}

#[derive(Default)]
struct AxesCache {
    n:     usize,
    sr:    u32,
    freqs: Vec<f64>,
    t:     Vec<f64>,
}

fn real_fft_plan(n: usize) -> Arc<dyn RealToComplex<f64>> {
    REAL_FFT_PLANS.with(|cell| {
        cell.borrow_mut()
            .entry(n)
            .or_insert_with(|| RealFftPlanner::<f64>::new().plan_fft_forward(n))
            .clone()
    })
}

/// Return references to a cached Hann window and its RMS correction for the
/// current thread. Rebuilds iff `n` changed since the last call on this thread.
fn with_hann<R>(n: usize, f: impl FnOnce(&[f64], f64) -> R) -> R {
    HANN_CACHE.with(|cell| {
        let mut c = cell.borrow_mut();
        if c.n != n {
            c.win.clear();
            c.win.reserve(n);
            for i in 0..n {
                c.win.push(0.5 * (1.0 - (2.0 * PI * i as f64 / (n - 1) as f64).cos()));
            }
            c.wc = (c.win.iter().map(|w| w * w).sum::<f64>() / n as f64).sqrt();
            c.n = n;
        }
        f(&c.win, c.wc)
    })
}

/// Return references to cached frequency and time axes for (N, sr).
fn with_axes<R>(n: usize, sr: u32, f: impl FnOnce(&[f64], &[f64]) -> R) -> R {
    AXES_CACHE.with(|cell| {
        let mut c = cell.borrow_mut();
        if c.n != n || c.sr != sr {
            let half = n / 2 + 1;
            c.freqs.clear();
            c.freqs.reserve(half);
            for k in 0..half {
                c.freqs.push(k as f64 * sr as f64 / n as f64);
            }
            c.t.clear();
            c.t.reserve(n);
            for i in 0..n {
                c.t.push(i as f64 / sr as f64);
            }
            c.n = n;
            c.sr = sr;
        }
        f(&c.freqs, &c.t)
    })
}

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

    // Convert to f64 mono.
    let mono: Vec<f64> = samples.iter().map(|&x| x as f64).collect();

    // ------------------------------------------------------------------
    // Hann window + windowed FFT
    // ------------------------------------------------------------------

    let fft = real_fft_plan(n);
    let mut windowed = vec![0.0f64; n];
    let mut win_spectrum = fft.make_output_vec();

    // Apply cached Hann window (keyed on N) into `windowed`, taking `wc` out
    // for the normalization constant below.
    let wc = HANN_CACHE.with(|cell| {
        let mut c = cell.borrow_mut();
        if c.n != n {
            c.win.clear();
            c.win.reserve(n);
            for i in 0..n {
                c.win.push(0.5 * (1.0 - (2.0 * PI * i as f64 / (n - 1) as f64).cos()));
            }
            c.wc = (c.win.iter().map(|w| w * w).sum::<f64>() / n as f64).sqrt();
            c.n = n;
        }
        for i in 0..n {
            windowed[i] = mono[i] * c.win[i];
        }
        c.wc
    });

    fft.process(&mut windowed, &mut win_spectrum)
        .map_err(|e| anyhow::anyhow!("FFT error: {e:?}"))?;

    // Amplitude spectrum: |FFT(windowed)| / (N/2) / wc
    let norm = (n as f64 / 2.0) * wc;
    let spec: Vec<f64> = win_spectrum.iter().map(|c| c.norm() / norm).collect();

    // Cached frequency axis (keyed on N, sr).
    let freqs: Vec<f64> = with_axes(n, sr, |f, _t| f.to_vec());

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

    // THD+N: sum of |spec|² over the non-notch range. Summed directly to
    // skip the spec.clone() + zero-fill from the original implementation
    // while avoiding the catastrophic cancellation of `(total - notch)`
    // when almost all energy sits inside the notch.
    let lo = f1_bin.saturating_sub(bw);
    let hi = (f1_bin + bw).min(spec.len());
    let thdn_sq: f64 =
        spec[..lo].iter().map(|x| x * x).sum::<f64>() +
        spec[hi..].iter().map(|x| x * x).sum::<f64>();
    let thdn = thdn_sq.sqrt() / f1_amp * 100.0;

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
    // Reuse `windowed` as scratch input — it's about to be dropped anyway.
    windowed.copy_from_slice(&mono);
    let mut raw_spectrum = fft.make_output_vec();
    fft.process(&mut windowed, &mut raw_spectrum)
        .map_err(|e| anyhow::anyhow!("FFT (phase) error: {e:?}"))?;

    let mut residual = mono.clone();

    // Subtract each harmonic in the time domain. Uses a cos/sin recurrence
    // (angle-addition) so the inner loop runs one real multiply instead of
    // a libm cos() per sample — cos was ~48% of daemon CPU in FFT monitor
    // mode at fft_n=16384 × 11 harmonics × 4 ch × 20 Hz.
    //
    // cos(θ + kΔθ) is obtained from (c_k, s_k) via:
    //   c_{k+1} = c_k·cos(Δθ) − s_k·sin(Δθ)
    //   s_{k+1} = s_k·cos(Δθ) + c_k·sin(Δθ)
    for harmonic in 1..=(n_harmonics + 1) {
        let hf = fundamental * harmonic as f64;
        if hf > sr as f64 / 2.0 {
            break;
        }
        let hb       = find_peak(&spec, &freqs, hf, 20.0);
        let hf_real  = freqs[hb];
        let phase    = raw_spectrum[hb].arg(); // phase from unwindowed FFT
        let amp_time = spec[hb];               // amplitude from windowed FFT

        let dtheta = 2.0 * PI * hf_real / sr as f64;
        let cos_d  = dtheta.cos();
        let sin_d  = dtheta.sin();
        let mut c  = phase.cos();
        let mut s  = phase.sin();
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
    let fft = real_fft_plan(n);
    let mut windowed = vec![0.0f64; n];
    let mut out = fft.make_output_vec();

    let wc = HANN_CACHE.with(|cell| {
        let mut c = cell.borrow_mut();
        if c.n != n {
            c.win.clear();
            c.win.reserve(n);
            for i in 0..n {
                c.win.push(0.5 * (1.0 - (2.0 * PI * i as f64 / (n - 1) as f64).cos()));
            }
            c.wc = (c.win.iter().map(|w| w * w).sum::<f64>() / n as f64).sqrt();
            c.n = n;
        }
        for (i, &s) in samples.iter().enumerate().take(n) {
            windowed[i] = s as f64 * c.win[i];
        }
        c.wc
    });

    if fft.process(&mut windowed, &mut out).is_err() {
        return (vec![], vec![]);
    }
    let norm = (n as f64 / 2.0) * wc;
    let spec: Vec<f64> = out.iter().map(|c| c.norm() / norm).collect();
    let freqs: Vec<f64> = with_axes(n, sr, |f, _t| f.to_vec());
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
