//! H1 transfer function estimator via Welch averaging.
//!
//! Direct port of `ac/transfer.py`.  Returns `freqs`, `magnitude_db`,
//! `phase_deg`, `coherence`, `delay_samples`, and `delay_ms`.

use std::f64::consts::PI;

use realfft::num_complex::Complex;
use realfft::RealFftPlanner;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct TransferResult {
    pub freqs:         Vec<f64>,
    pub magnitude_db:  Vec<f64>,
    pub phase_deg:     Vec<f64>,
    pub coherence:     Vec<f64>,
    /// Complex H(ω) — real part. Parallel to `freqs`. `unified.md`
    /// Phase 3 — needed by Tier 2 views that consume H directly
    /// (Nyquist locus, IR via IFFT, group-delay-from-complex).
    /// Existing magnitude_db / phase_deg are derived from this same
    /// h1 complex value so the three views are guaranteed consistent.
    pub re:            Vec<f64>,
    /// Complex H(ω) — imaginary part. Parallel to `re`.
    pub im:            Vec<f64>,
    pub delay_samples: i64,
    pub delay_ms:      f64,
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn hann_window(n: usize) -> Vec<f64> {
    (0..n)
        .map(|i| 0.5 * (1.0 - (2.0 * PI * i as f64 / (n - 1) as f64).cos()))
        .collect()
}

/// Apply Hann window to `seg` and return the complex spectrum (n/2+1 bins).
fn fft_windowed(
    seg: &[f64],
    window: &[f64],
    planner: &mut RealFftPlanner<f64>,
) -> Vec<Complex<f64>> {
    let n = seg.len();
    let fft = planner.plan_fft_forward(n);
    let mut buf: Vec<f64> = seg.iter().zip(window.iter()).map(|(&s, &w)| s * w).collect();
    let mut out = fft.make_output_vec();
    fft.process(&mut buf, &mut out).ok();
    out
}

/// Welch joint estimate: returns `(Gxx, Gyy, Gxy)` accumulated from a single
/// per-segment FFT pair. Computing the three quantities together halves the
/// FFT count vs calling separate `welch_psd(x) + welch_psd(y) + welch_csd(x,y)`
/// because each segment's FFTs are reused across all three accumulators.
fn welch_all(
    x: &[f64],
    y: &[f64],
    nperseg: usize,
    noverlap: usize,
    window: &[f64],
    planner: &mut RealFftPlanner<f64>,
) -> (Vec<f64>, Vec<f64>, Vec<Complex<f64>>) {
    let nfft = nperseg / 2 + 1;
    let step = nperseg - noverlap;
    let mut gxx = vec![0.0_f64; nfft];
    let mut gyy = vec![0.0_f64; nfft];
    let mut gxy = vec![Complex::new(0.0, 0.0); nfft];
    let mut n_seg = 0usize;
    let mut pos = 0;
    let len = x.len().min(y.len());
    while pos + nperseg <= len {
        let fx = fft_windowed(&x[pos..pos + nperseg], window, planner);
        let fy = fft_windowed(&y[pos..pos + nperseg], window, planner);
        for k in 0..nfft {
            let cx = fx[k];
            let cy = fy[k];
            gxx[k] += cx.norm_sqr();
            gyy[k] += cy.norm_sqr();
            gxy[k] += cx.conj() * cy;
        }
        n_seg += 1;
        pos += step;
    }
    if n_seg > 0 {
        let inv = 1.0 / n_seg as f64;
        for k in 0..nfft {
            gxx[k] *= inv;
            gyy[k] *= inv;
            gxy[k] *= inv;
        }
    }
    (gxx, gyy, gxy)
}

/// Delay estimation via FFT-based cross-correlation. Exposed so callers that
/// drive `h1_estimate` in a tight loop (e.g. `transfer_stream`) can estimate
/// once on warmup and reuse the result via [`h1_estimate_with_delay`] — the
/// ref↔meas path delay is physically constant during a streaming session.
pub fn estimate_delay_samples(ref_sig: &[f32], meas: &[f32], sr: u32) -> i64 {
    let r: Vec<f64> = ref_sig.iter().map(|&x| x as f64).collect();
    let m: Vec<f64> = meas.iter().map(|&x| x as f64).collect();
    estimate_delay(&r, &m, sr)
}

fn estimate_delay(ref_sig: &[f64], meas: &[f64], sr: u32) -> i64 {
    let corr_len = ref_sig.len().min(meas.len()).min(4 * sr as usize);
    let r = &ref_sig[..corr_len];
    let m = &meas[..corr_len];
    let max_lag = (sr as usize).min(corr_len / 2);

    // Zero-pad to next power of 2 for efficient FFT
    let fft_len = (2 * corr_len).next_power_of_two();
    let mut rp: Vec<f64> = r.to_vec();
    rp.resize(fft_len, 0.0);
    let mut mp: Vec<f64> = m.to_vec();
    mp.resize(fft_len, 0.0);

    let mut planner = RealFftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(fft_len);
    let mut fr = fft.make_output_vec();
    let mut fm = fft.make_output_vec();
    fft.process(&mut rp, &mut fr).ok();
    fft.process(&mut mp, &mut fm).ok();

    // Cross-spectrum: conj(fr) * fm
    let mut cross: Vec<Complex<f64>> = fr.iter().zip(fm.iter()).map(|(a, b)| a.conj() * b).collect();

    let ifft = planner.plan_fft_inverse(fft_len);
    let mut corr = ifft.make_output_vec();
    ifft.process(&mut cross, &mut corr).ok();
    let norm = fft_len as f64;
    for v in corr.iter_mut() {
        *v /= norm;
    }

    // Find peak within ±max_lag
    let mut best_lag = 0i64;
    let mut best_val = f64::NEG_INFINITY;
    for (lag, &c) in corr.iter().enumerate().take(max_lag + 1) {
        let v = c.abs();
        if v > best_val {
            best_val = v;
            best_lag = lag as i64;
        }
    }
    for lag in 1..=max_lag {
        let idx = fft_len - lag;
        if idx < corr.len() {
            let v = corr[idx].abs();
            if v > best_val {
                best_val = v;
                best_lag = -(lag as i64);
            }
        }
    }
    best_lag
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// H1 transfer function estimate.
///
/// * `ref_sig` — reference channel (the stimulus; input to DUT)
/// * `meas`    — measurement channel (the output of DUT)
/// * `sr`      — sample rate in Hz
pub fn h1_estimate(ref_sig: &[f32], meas: &[f32], sr: u32) -> TransferResult {
    let r: Vec<f64> = ref_sig.iter().map(|&x| x as f64).collect();
    let m: Vec<f64> = meas.iter().map(|&x| x as f64).collect();
    let delay_samples = estimate_delay(&r, &m, sr);
    h1_estimate_core(&r, &m, sr, delay_samples)
}

/// Variant of [`h1_estimate`] that skips the O(N log N) delay estimation and
/// uses a caller-supplied `delay_samples`. The streaming transfer worker
/// estimates the delay once on warmup (the ref↔meas path is physically
/// constant while a session is running) and feeds it in on every tick,
/// cutting ~12–15 ms of per-frame FFT work at 2.5 s ring length and
/// 48 kHz — the difference between 8.5 Hz choppy and 20 Hz smooth
/// transfer-view refresh.
pub fn h1_estimate_with_delay(
    ref_sig: &[f32],
    meas: &[f32],
    sr: u32,
    delay_samples: i64,
) -> TransferResult {
    let r: Vec<f64> = ref_sig.iter().map(|&x| x as f64).collect();
    let m: Vec<f64> = meas.iter().map(|&x| x as f64).collect();
    h1_estimate_core(&r, &m, sr, delay_samples)
}

fn h1_estimate_core(r: &[f64], m: &[f64], sr: u32, delay_samples: i64) -> TransferResult {
    assert_eq!(r.len(), m.len(), "ref and meas must have equal length");

    let nperseg  = sr as usize; // 1 Hz resolution
    let noverlap = nperseg / 2;
    let window   = hann_window(nperseg);

    let delay_ms = delay_samples as f64 / sr as f64 * 1000.0;

    let mut planner = RealFftPlanner::<f64>::new();
    let (gxx, gyy, gxy) = welch_all(r, m, nperseg, noverlap, &window, &mut planner);

    let nfft  = nperseg / 2 + 1;
    let freqs: Vec<f64> = (0..nfft).map(|k| k as f64 * sr as f64 / nperseg as f64).collect();

    // Delay compensation: Gxy_comp = Gxy * exp(j * 2π * f * delay / sr)
    let gxy_comp: Vec<Complex<f64>> = gxy.iter().enumerate().map(|(k, &g)| {
        let phase = 2.0 * PI * freqs[k] * delay_samples as f64 / sr as f64;
        g * Complex::new(phase.cos(), phase.sin())
    }).collect();

    // H1 = Gxy_comp / Gxx — preserve the complex value so re/im are
    // consistent with magnitude_db / phase_deg (all three derived
    // from the same h1).
    let mut magnitude_db = vec![0.0f64; nfft];
    let mut phase_deg    = vec![0.0f64; nfft];
    let mut re           = vec![0.0f64; nfft];
    let mut im           = vec![0.0f64; nfft];
    for k in 0..nfft {
        let gxx_safe = gxx[k].max(1e-30);
        let h1       = gxy_comp[k] / gxx_safe;
        let mag      = h1.norm().max(1e-6); // floor at −120 dB
        magnitude_db[k] = 20.0 * mag.log10();
        phase_deg[k]    = h1.arg().to_degrees();
        re[k]           = h1.re;
        im[k]           = h1.im;
    }

    // Coherence = |Gxy|² / (Gxx × Gyy)
    let coherence: Vec<f64> = (0..nfft).map(|k| {
        let denom = gxx[k] * gyy[k];
        let coh   = if denom > 0.0 { gxy[k].norm_sqr() / denom } else { 0.0 };
        coh.clamp(0.0, 1.0)
    }).collect();

    TransferResult { freqs, magnitude_db, phase_deg, coherence, re, im, delay_samples, delay_ms }
}

/// Inverse FFT of a complex H(ω) (in `re`, `im` parallel arrays from a
/// `TransferResult`) into a time-domain impulse response h(t).
///
/// Returns `Vec<f32>` of length `(re.len() - 1) * 2`. For the
/// `h1_estimate_core` Welch path, that's `nperseg = sr` samples = 1 s
/// of IR — plenty of visual range for typical room / DUT responses.
///
/// h(t) is centred via `fftshift`-style rotation so the dominant peak
/// (DC bin energy + linear-phase pre-roll) sits at `t = 0` in the
/// middle of the array. Caller treats indices `[0, n/2)` as
/// pre-causal taps, `[n/2, n)` as causal. Empty / mismatched / too-
/// short inputs return `Vec::new()`.
///
/// `unified.md` Phase 4b. Daemon-side IFFT — UI gets a downsampled
/// time-series and just plots it (no UI-side FFT plumbing needed).
pub fn impulse_response_from_h(re: &[f64], im: &[f64]) -> Vec<f32> {
    if re.is_empty() || re.len() != im.len() || re.len() < 2 {
        return Vec::new();
    }
    let nfft = re.len();
    let n_time = (nfft - 1) * 2;
    let mut planner = RealFftPlanner::<f64>::new();
    let ifft = planner.plan_fft_inverse(n_time);
    let mut spectrum: Vec<Complex<f64>> = re
        .iter()
        .zip(im.iter())
        .map(|(&r, &i)| Complex::new(r, i))
        .collect();
    // realfft inverse requires DC (bin 0) and Nyquist (bin n-1) to
    // have zero imaginary part — they describe real-valued frequency
    // components in any real-input → complex-output forward FFT, so
    // their inverse must hold the same constraint. Welch H₁ from
    // real signal pairs *should* give real values at these bins
    // (real/real = real), but Welch averaging + float noise leaves
    // tiny non-zero imaginary residue that realfft refuses. Zero
    // them so the IFFT proceeds cleanly. The discarded residue is
    // sub-1e-10 in normal operation and reflects numerical noise,
    // not signal content.
    if let Some(first) = spectrum.first_mut() {
        first.im = 0.0;
    }
    if let Some(last) = spectrum.last_mut() {
        last.im = 0.0;
    }
    let mut time = ifft.make_output_vec();
    if ifft.process(&mut spectrum, &mut time).is_err() {
        return Vec::new();
    }
    // Realfft inverse doesn't normalise — divide by n_time so the
    // recovered impulse magnitude matches the H(ω) amplitudes.
    let norm = n_time as f64;
    // Center via fftshift-style swap so the user sees the IR peak at
    // mid-cell instead of at the array edge (where pre-causal taps
    // wrap around to indices near n_time-1 in the un-shifted output).
    let half = n_time / 2;
    let mut out = Vec::<f32>::with_capacity(n_time);
    for k in 0..n_time {
        // Source index: k = 0 → src n/2 (the t=0 IR peak); k = n-1 →
        // src n/2 - 1 (the wraparound point).
        let src = (k + half) % n_time;
        out.push((time[src] / norm) as f32);
    }
    out
}

/// Number of capture seconds needed for `n_averages` Welch segments at `sr`.
pub fn capture_duration(n_averages: usize, sr: u32) -> f64 {
    let nperseg  = sr as usize;
    let noverlap = nperseg / 2;
    let step     = nperseg - noverlap;
    let total    = nperseg + step * (n_averages - 1);
    total as f64 / sr as f64
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;
    use rand::rngs::StdRng;
    use rand::SeedableRng;
    use rand_distr::{Distribution, Normal};

    const SR: u32 = 48_000;
    const N: usize = 3 * SR as usize; // 3 s → 5 Welch segments

    fn white_noise(n: usize, amplitude: f64, seed: u64) -> Vec<f32> {
        let mut rng = StdRng::seed_from_u64(seed);
        let dist = Normal::new(0.0, amplitude).unwrap();
        (0..n).map(|_| dist.sample(&mut rng) as f32).collect()
    }

    // ---- capture_duration ----

    #[test]
    fn capture_duration_arithmetic() {
        assert_relative_eq!(capture_duration(1, SR), 1.0, epsilon = 1e-12);
        assert_relative_eq!(capture_duration(5, SR), 3.0, epsilon = 1e-12);
        assert_relative_eq!(capture_duration(10, SR), 5.5, epsilon = 1e-12);
    }

    // ---- Unity / delay / filter ----

    #[test]
    fn unity_loopback() {
        let sig = white_noise(N, 0.5, 42);
        let r = h1_estimate(&sig, &sig, SR);

        assert_eq!(r.delay_samples, 0);
        for k in 20..=20_000 {
            assert!(
                r.magnitude_db[k].abs() < 0.1,
                "bin {k}: mag {:.3} dB", r.magnitude_db[k]
            );
            assert!(
                r.phase_deg[k].abs() < 1.0,
                "bin {k}: phase {:.3}°", r.phase_deg[k]
            );
            assert!(
                r.coherence[k] > 0.999,
                "bin {k}: coh {:.4}", r.coherence[k]
            );
        }
    }

    /// unified.md Phase 3: re/im are populated parallel to mag/phase
    /// and consistent with them. Unity loopback should give Re ≈ 1,
    /// Im ≈ 0 (within Welch noise) at every bin in the audio band.
    #[test]
    fn unity_loopback_re_im_consistent() {
        let sig = white_noise(N, 0.5, 42);
        let r = h1_estimate(&sig, &sig, SR);

        assert_eq!(r.re.len(), r.magnitude_db.len(), "re len mismatch");
        assert_eq!(r.im.len(), r.magnitude_db.len(), "im len mismatch");
        for k in 20..=20_000 {
            // Round-trip check: |H| from re/im matches |H| from db.
            let mag_lin_re_im = (r.re[k].powi(2) + r.im[k].powi(2)).sqrt();
            let mag_lin_db    = 10.0_f64.powf(r.magnitude_db[k] / 20.0);
            assert_relative_eq!(mag_lin_re_im, mag_lin_db, epsilon = 1e-9);
            // Phase round-trip: atan2(im, re) matches phase_deg.
            let p_re_im = r.im[k].atan2(r.re[k]).to_degrees();
            assert_relative_eq!(p_re_im, r.phase_deg[k], epsilon = 1e-9);
        }
        // Unity-gain expectation: Re ≈ 1, Im ≈ 0 in the audio band.
        for k in 200..=2_000 {
            assert!(
                (r.re[k] - 1.0).abs() < 0.05,
                "bin {k}: Re {:.4} (expected ≈ 1)", r.re[k]
            );
            assert!(
                r.im[k].abs() < 0.05,
                "bin {k}: Im {:.4} (expected ≈ 0)", r.im[k]
            );
        }
    }

    #[test]
    fn delay_only_path() {
        let sig = white_noise(N, 0.5, 42);
        let delay: usize = 100;

        let mut meas = vec![0.0f32; N];
        meas[delay..].copy_from_slice(&sig[..N - delay]);

        let r = h1_estimate(&sig, &meas, SR);

        assert_eq!(r.delay_samples, delay as i64);
        let expected_ms = delay as f64 / SR as f64 * 1000.0;
        assert_relative_eq!(r.delay_ms, expected_ms, epsilon = 0.01);

        for k in 100..=20_000 {
            assert!(
                r.magnitude_db[k].abs() < 0.5,
                "bin {k}: mag {:.3} dB", r.magnitude_db[k]
            );
            assert!(
                r.coherence[k] > 0.95,
                "bin {k}: coh {:.4}", r.coherence[k]
            );
        }
    }

    #[test]
    fn single_pole_lowpass() {
        let ref_sig = white_noise(N, 0.5, 42);

        let fc = 2000.0_f64;
        let a = 1.0 - (-2.0 * PI * fc / SR as f64).exp();

        // Apply IIR: y[n] = a*x[n] + (1-a)*y[n-1]
        let mut meas = vec![0.0f32; N];
        let mut prev = 0.0_f64;
        for i in 0..N {
            let y = a * ref_sig[i] as f64 + (1.0 - a) * prev;
            meas[i] = y as f32;
            prev = y;
        }

        let r = h1_estimate(&ref_sig, &meas, SR);

        // Analytical: H(z) = a / (1 - (1-a)*z^{-1})
        let spot_checks: &[(f64, f64)] = &[
            (200.0, 0.5),
            (2000.0, 0.5),
            (10000.0, 1.0),
            (20000.0, 1.5),
        ];
        for &(freq, tol) in spot_checks {
            let w = 2.0 * PI * freq / SR as f64;
            let z_inv = Complex::new(w.cos(), -w.sin());
            let denom = Complex::new(1.0, 0.0) - z_inv * (1.0 - a);
            let h = Complex::new(a, 0.0) / denom;
            let expected_db = 20.0 * h.norm().log10();
            let k = freq.round() as usize;
            assert!(
                (r.magnitude_db[k] - expected_db).abs() < tol,
                "f={freq}: got {:.2} dB, expected {:.2} dB",
                r.magnitude_db[k], expected_db
            );
        }
    }

    // ---- Noise & coherence ----

    #[test]
    fn noise_robustness() {
        let ref_sig = white_noise(N, 0.5, 42);
        let noise = white_noise(N, 0.05, 99);
        let meas: Vec<f32> = ref_sig.iter().zip(&noise).map(|(&s, &n)| s + n).collect();

        let r = h1_estimate(&ref_sig, &meas, SR);

        let range = 50..=20_000;
        let count = range.clone().count() as f64;
        let mean_mag_err: f64 =
            range.clone().map(|k| r.magnitude_db[k].abs()).sum::<f64>() / count;
        assert!(
            mean_mag_err < 0.5,
            "mean |mag error| {:.3} dB", mean_mag_err
        );
        let mean_coh: f64 =
            range.map(|k| r.coherence[k]).sum::<f64>() / count;
        assert!(mean_coh > 0.95, "mean coherence {:.4}", mean_coh);
    }

    #[test]
    fn coherence_uncorrelated() {
        let a = white_noise(N, 0.5, 42);
        let b = white_noise(N, 0.5, 99);
        let r = h1_estimate(&a, &b, SR);

        let mean_coh: f64 =
            r.coherence[1..].iter().sum::<f64>() / (r.coherence.len() - 1) as f64;
        assert!(
            mean_coh < 0.4,
            "uncorrelated signals should have low coherence, got {:.4}",
            mean_coh
        );
    }

    /// Phase 4b round-trip: a flat-spectrum H(ω) (Re ≡ 1, Im ≡ 0)
    /// represents an ideal unit-impulse system. The IFFT must recover
    /// a time-domain h(t) with a single positive peak centred at the
    /// middle of the array (after the centring shift) and ~zero
    /// energy elsewhere.
    #[test]
    fn impulse_response_recovers_unit_impulse() {
        // 4097 freq bins → 8192-sample IR (1 s at 8 kHz, etc.).
        let nfft = 4097;
        let re = vec![1.0; nfft];
        let im = vec![0.0; nfft];
        let ir = impulse_response_from_h(&re, &im);
        assert_eq!(ir.len(), (nfft - 1) * 2);
        let n = ir.len();
        let mid = n / 2;
        // The peak must be at the centre.
        let (peak_idx, peak_val) = ir
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
            .unwrap();
        assert_eq!(peak_idx, mid, "peak index {peak_idx}, expected {mid}");
        assert!(*peak_val > 0.0, "peak value should be positive, got {peak_val}");
        // Off-peak energy must be ~zero (Re=1 IFFT is a Dirac delta).
        for (i, v) in ir.iter().enumerate() {
            if i != mid {
                assert!(
                    v.abs() < 1e-3,
                    "non-peak bin {i} = {v} (expected ~0)",
                );
            }
        }
    }

    /// Empty / mismatched inputs are defensive returns of Vec::new(),
    /// not panics — the daemon emits IR sidecar frames every tick and
    /// must not crash on edge cases (empty re/im on cold start, etc.).
    #[test]
    fn impulse_response_empty_inputs_yield_empty() {
        assert!(impulse_response_from_h(&[], &[]).is_empty());
        assert!(impulse_response_from_h(&[1.0], &[]).is_empty());
        assert!(impulse_response_from_h(&[1.0, 2.0], &[0.0]).is_empty());
        // Single-bin input is too short to IFFT meaningfully.
        assert!(impulse_response_from_h(&[1.0], &[0.0]).is_empty());
    }
}
