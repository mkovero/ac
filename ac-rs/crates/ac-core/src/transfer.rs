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

/// Welch auto-power spectral density.
fn welch_psd(
    sig: &[f64],
    nperseg: usize,
    noverlap: usize,
    window: &[f64],
    planner: &mut RealFftPlanner<f64>,
) -> Vec<f64> {
    let nfft = nperseg / 2 + 1;
    let step = nperseg - noverlap;
    let mut psd = vec![0.0f64; nfft];
    let mut n_seg = 0usize;
    let mut pos = 0;
    while pos + nperseg <= sig.len() {
        let spec = fft_windowed(&sig[pos..pos + nperseg], window, planner);
        for (k, c) in spec.iter().enumerate() {
            psd[k] += c.norm_sqr();
        }
        n_seg += 1;
        pos += step;
    }
    if n_seg > 0 {
        for v in psd.iter_mut() {
            *v /= n_seg as f64;
        }
    }
    psd
}

/// Welch cross-power spectral density: E[FFT(x)*.FFT(y)].
fn welch_csd(
    x: &[f64],
    y: &[f64],
    nperseg: usize,
    noverlap: usize,
    window: &[f64],
    planner: &mut RealFftPlanner<f64>,
) -> Vec<Complex<f64>> {
    let nfft = nperseg / 2 + 1;
    let step = nperseg - noverlap;
    let mut acc = vec![Complex::new(0.0, 0.0); nfft];
    let mut n_seg = 0usize;
    let mut pos = 0;
    let len = x.len().min(y.len());
    while pos + nperseg <= len {
        let fx = fft_windowed(&x[pos..pos + nperseg], window, planner);
        let fy = fft_windowed(&y[pos..pos + nperseg], window, planner);
        for (k, (cx, cy)) in fx.iter().zip(fy.iter()).enumerate() {
            acc[k] += cx.conj() * cy;
        }
        n_seg += 1;
        pos += step;
    }
    if n_seg > 0 {
        for v in acc.iter_mut() {
            *v /= n_seg as f64;
        }
    }
    acc
}

/// Delay estimation via FFT-based cross-correlation.
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
    for lag in 0..=max_lag {
        let v = corr[lag].abs();
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
    assert_eq!(ref_sig.len(), meas.len(), "ref and meas must have equal length");

    let r: Vec<f64> = ref_sig.iter().map(|&x| x as f64).collect();
    let m: Vec<f64> = meas.iter().map(|&x| x as f64).collect();

    let nperseg  = sr as usize; // 1 Hz resolution
    let noverlap = nperseg / 2;
    let window   = hann_window(nperseg);

    let delay_samples = estimate_delay(&r, &m, sr);
    let delay_ms      = delay_samples as f64 / sr as f64 * 1000.0;

    let mut planner = RealFftPlanner::<f64>::new();
    let gxx = welch_psd(&r, nperseg, noverlap, &window, &mut planner);
    let gyy = welch_psd(&m, nperseg, noverlap, &window, &mut planner);
    let gxy = welch_csd(&r, &m, nperseg, noverlap, &window, &mut planner);

    let nfft  = nperseg / 2 + 1;
    let freqs: Vec<f64> = (0..nfft).map(|k| k as f64 * sr as f64 / nperseg as f64).collect();

    // Delay compensation: Gxy_comp = Gxy * exp(j * 2π * f * delay / sr)
    let gxy_comp: Vec<Complex<f64>> = gxy.iter().enumerate().map(|(k, &g)| {
        let phase = 2.0 * PI * freqs[k] * delay_samples as f64 / sr as f64;
        g * Complex::new(phase.cos(), phase.sin())
    }).collect();

    // H1 = Gxy_comp / Gxx
    let mut magnitude_db = vec![0.0f64; nfft];
    let mut phase_deg    = vec![0.0f64; nfft];
    for k in 0..nfft {
        let gxx_safe = gxx[k].max(1e-30);
        let h1       = gxy_comp[k] / gxx_safe;
        let mag      = h1.norm().max(1e-6); // floor at −120 dB
        magnitude_db[k] = 20.0 * mag.log10();
        phase_deg[k]    = h1.arg().to_degrees();
    }

    // Coherence = |Gxy|² / (Gxx × Gyy)
    let coherence: Vec<f64> = (0..nfft).map(|k| {
        let denom = gxx[k] * gyy[k];
        let coh   = if denom > 0.0 { gxy[k].norm_sqr() / denom } else { 0.0 };
        coh.min(1.0).max(0.0)
    }).collect();

    TransferResult { freqs, magnitude_db, phase_deg, coherence, delay_samples, delay_ms }
}

/// Number of capture seconds needed for `n_averages` Welch segments at `sr`.
pub fn capture_duration(n_averages: usize, sr: u32) -> f64 {
    let nperseg  = sr as usize;
    let noverlap = nperseg / 2;
    let step     = nperseg - noverlap;
    let total    = nperseg + step * (n_averages - 1);
    total as f64 / sr as f64
}
