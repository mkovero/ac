//! Morlet continuous wavelet transform.
//!
//! Produces one column of a CWT waterfall (one magnitude per scale, sampled at
//! the centre time of the analysed buffer) via frequency-domain convolution:
//!
//! 1. forward complex FFT of the real input (once),
//! 2. per scale: dot-product of `X[k] · H[k] · (-1)^k` over positive
//!    frequencies — this evaluates the IFFT at exactly `t = N/2` without
//!    computing the full inverse transform.
//!
//! The single-point IFFT trick makes this O(n_scales · N) instead of
//! O(n_scales · N log N), which is the difference between smooth real-time
//! at 256+ scales and dropping frames.
//!
//! Internal DSP is `f64` (matches [`crate::analysis`]). The public API yields
//! `f32` to keep wire frames compact — the absolute precision is bounded by
//! the `20 * log10` cast anyway and the difference is visually invisible.
//!
//! The output is absolute-dBFS calibrated: a pure cosine of amplitude `A`
//! feeding [`morlet_cwt`] at the scale whose peak matches the tone's
//! frequency produces `20 * log10(A)` at that scale. Levels therefore line
//! up with the existing FFT spectrum waterfall — the user can switch modes
//! and the same tone sits at the same dBFS line.

use std::f64::consts::PI;

use rustfft::{num_complex::Complex, FftPlanner};

/// Morlet wavelet shape parameter `ω₀` (sometimes written `σ` in the
/// literature). Controls the time/frequency resolution trade-off:
///
/// - lower values (≈ 5.0) → wider in frequency, narrower in time: better for
///   transients, percussive content;
/// - higher values (≈ 7.0–8.0) → narrower in frequency, wider in time:
///   better for sustained tones and room modes.
///
/// `6.0` matches `scipy.signal.morlet2` and most audio/room-measurement
/// literature; it's a good default for mixed program material.
///
/// Note: analytic (one-sided) approximation requires `ω₀ ≳ 5`; below that the
/// zero-mean correction term becomes non-negligible. Keep ≥ 5.0.
pub const DEFAULT_SIGMA: f32 = 12.0;

/// Default number of log-spaced scales per CWT column. 512 log-spaced bins
/// across 3 decades gives ~170 per decade / ~17 per octave — enough to fill
/// a 1080p-height waterfall without visible banding while staying fast with
/// the single-point IFFT dot-product path.
pub const DEFAULT_N_SCALES: usize = 512;

/// Default low edge of the CWT frequency axis (Hz). 20 Hz is the
/// conventional audio low end.
pub const DEFAULT_F_MIN: f32 = 20.0;

/// Default high edge of the CWT frequency axis as a fraction of Nyquist.
/// 0.9 keeps the highest-frequency Morlet kernel from bleeding against the
/// band edge.
pub const DEFAULT_F_MAX_NYQUIST_FRACTION: f32 = 0.9;

/// Compute the default high edge in Hz for a given sample rate.
pub fn default_f_max(sample_rate: u32) -> f32 {
    (sample_rate as f32 / 2.0) * DEFAULT_F_MAX_NYQUIST_FRACTION
}

/// Build `n_scales` log-spaced CWT scales covering the frequency range
/// `[f_min, f_max]` Hz.
///
/// Returns `(scales, frequencies)` where:
/// - `scales[i]` is the CWT dilation used by [`morlet_cwt`],
/// - `frequencies[i]` is the peak frequency of the kernel at that scale (Hz).
///
/// The mapping is `scale = sigma * sample_rate / (2π · freq)`, derived from
/// the Morlet peak at normalized angular frequency `ω₀/a`.
pub fn log_scales(
    f_min: f32,
    f_max: f32,
    n_scales: usize,
    sample_rate: u32,
    sigma: f32,
) -> (Vec<f32>, Vec<f32>) {
    assert!(n_scales >= 2, "need at least 2 scales");
    assert!(
        f_min > 0.0 && f_max > f_min,
        "invalid frequency range: {f_min}..{f_max}"
    );
    assert!(sigma > 0.0, "sigma must be positive");

    let log_min = f_min.ln();
    let log_max = f_max.ln();
    let step = (log_max - log_min) / (n_scales - 1) as f32;
    let two_pi = 2.0 * std::f32::consts::PI;

    let mut scales = Vec::with_capacity(n_scales);
    let mut freqs = Vec::with_capacity(n_scales);
    for i in 0..n_scales {
        let f = (log_min + step * i as f32).exp();
        let a = sigma * sample_rate as f32 / (two_pi * f);
        scales.push(a);
        freqs.push(f);
    }
    (scales, freqs)
}

/// Compute one Morlet CWT column — the magnitude (dBFS) at each scale,
/// sampled at the centre of `samples`.
///
/// Uses the single-point IFFT trick: we only need `y[N/2]`, and the
/// twiddle at m = N/2 is `(-1)^k`, so each scale reduces to a dot product
/// over the frequency-domain bins where the Gaussian kernel is non-negligible.
///
/// The Gaussian `exp(-0.5 * x²)` drops below `~1e-7` for `|x| > 5.5`, so
/// each scale only touches the bins near its peak frequency — typically
/// a few dozen bins for low-frequency scales, more for high-frequency ones.
/// This sparse evaluation makes the cost proportional to the sum of kernel
/// widths rather than `n_scales × N/2`.
///
/// Magnitudes are already in **dBFS**. A pure cosine of amplitude `A`
/// yields `20 * log10(A)` at the matching scale — same calibration as
/// [`crate::analysis::analyze`].
///
/// # Panics
///
/// Panics if `samples.len() < 256` or `scales` is empty.
pub fn morlet_cwt(
    samples: &[f32],
    _sample_rate: u32,
    scales: &[f32],
    sigma: f32,
) -> Vec<f32> {
    let n = samples.len();
    assert!(n >= 256, "need at least 256 samples, got {n}");
    assert!(!scales.is_empty(), "need at least one scale");
    assert!(sigma > 0.0, "sigma must be positive");

    let mut planner = FftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(n);

    let mut spectrum: Vec<Complex<f64>> = samples
        .iter()
        .map(|&x| Complex::new(x as f64, 0.0))
        .collect();
    fft.process(&mut spectrum);

    // Pre-apply the (-1)^k sign so the inner loop is a plain MAC.
    for (k, bin) in spectrum.iter_mut().enumerate() {
        if k & 1 != 0 {
            *bin = -*bin;
        }
    }

    let omega0 = sigma as f64;
    let half = n / 2;
    let inv_n = 1.0 / n as f64;
    let two_pi_over_n = 2.0 * PI / n as f64;

    // Gaussian cutoff: |arg| > CUTOFF → exp(-0.5 * arg²) < ~1e-7.
    const CUTOFF: f64 = 5.5;

    let mut out = Vec::with_capacity(scales.len());

    for &scale in scales {
        let a = scale as f64;

        // Kernel peak is at k_peak = ω₀ · N / (2π · a).
        // Support: k where |a · ω_k - ω₀| < CUTOFF, i.e.
        //   k ∈ [ (ω₀ - CUTOFF) · N / (2π · a),  (ω₀ + CUTOFF) · N / (2π · a) ]
        let k_center = omega0 / (a * two_pi_over_n);
        let k_width = CUTOFF / (a * two_pi_over_n);
        let k_lo = ((k_center - k_width).floor() as isize).max(0) as usize;
        let k_hi = ((k_center + k_width).ceil() as isize).min(half as isize) as usize;

        let mut acc = Complex::new(0.0, 0.0);
        for k in k_lo..=k_hi {
            let omega_k = two_pi_over_n * k as f64;
            let arg = a * omega_k - omega0;
            let h = (-0.5 * arg * arg).exp();
            acc += spectrum[k] * h;
        }

        let mag = acc.norm() * inv_n * 2.0;
        let dbfs = 20.0 * mag.max(1e-12).log10();
        out.push(dbfs as f32);
    }

    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;

    fn pure_cosine(freq_hz: f64, amp: f64, sr: u32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| {
                let t = i as f64 / sr as f64;
                (amp * (2.0 * PI * freq_hz * t).cos()) as f32
            })
            .collect()
    }

    #[test]
    fn test_cwt_dc() {
        let samples: Vec<f32> = vec![0.5; 4096];
        let (scales, _freqs) = log_scales(20.0, 20_000.0, 64, 48_000, DEFAULT_SIGMA);
        let mags = morlet_cwt(&samples, 48_000, &scales, DEFAULT_SIGMA);
        for (i, &db) in mags.iter().enumerate() {
            assert!(
                db < -60.0,
                "DC input leaked into scale {i}: {db} dB (expected < -60)"
            );
        }
    }

    #[test]
    fn test_cwt_sine_localization() {
        // Bin-aligned test tone avoids FFT leakage: k · sr / N, k integer.
        let n = 4096;
        let sr = 48_000;
        let k = 85;
        let f_test = k as f64 * sr as f64 / n as f64; // ≈ 996.09 Hz
        let amp = 10f64.powf(-6.0 / 20.0);
        let samples = pure_cosine(f_test, amp, sr, n);

        // Place the centre scale exactly on `f_test`, plus ±15 % neighbours
        // to verify localization (the centre should dominate).
        let sigma = DEFAULT_SIGMA as f64;
        let two_pi = 2.0 * PI;
        let a_centre = sigma * sr as f64 / (two_pi * f_test);
        let scales = [
            (a_centre * 1.15) as f32, // peak at lower freq
            a_centre as f32,
            (a_centre / 1.15) as f32, // peak at higher freq
        ];

        let mags = morlet_cwt(&samples, sr, &scales, DEFAULT_SIGMA);
        let argmax = mags
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap()
            .0;
        assert_eq!(argmax, 1, "centre scale did not dominate: {mags:?}");

        // Centre magnitude must match the -6 dBFS test tone within 0.5 dB.
        assert_relative_eq!(mags[1] as f64, -6.0, epsilon = 0.5);
    }

    #[test]
    fn test_scale_normalization() {
        // Equal-amplitude sines at very different frequencies must land at
        // the same dBFS. This pins the peak-1 Gaussian + 2× analytic factor
        // convention: no extra `1/sqrt(a)` is needed for level consistency.
        let n = 4096;
        let sr = 48_000;
        let sigma = DEFAULT_SIGMA as f64;
        let two_pi = 2.0 * PI;
        let amp = 10f64.powf(-6.0 / 20.0);

        let bin_low = 9_i32; // ≈ 105.47 Hz
        let bin_high = 427_i32; // ≈ 5004.1 Hz
        let f_low = bin_low as f64 * sr as f64 / n as f64;
        let f_high = bin_high as f64 * sr as f64 / n as f64;

        let mut peaks = Vec::new();
        for &f in &[f_low, f_high] {
            let samples = pure_cosine(f, amp, sr, n);
            let a = sigma * sr as f64 / (two_pi * f);
            let mags = morlet_cwt(&samples, sr, &[a as f32], DEFAULT_SIGMA);
            peaks.push(mags[0] as f64);
        }

        for &p in &peaks {
            assert_relative_eq!(p, -6.0, epsilon = 0.5);
        }
        assert!(
            (peaks[0] - peaks[1]).abs() < 0.5,
            "scale-dependent level: {peaks:?}"
        );
    }

    #[test]
    fn test_log_scales_bounds() {
        let (scales, freqs) = log_scales(20.0, 20_000.0, 96, 48_000, DEFAULT_SIGMA);
        assert_eq!(scales.len(), 96);
        assert_eq!(freqs.len(), 96);

        assert_relative_eq!(freqs[0], 20.0, epsilon = 1e-3);
        assert_relative_eq!(freqs[95], 20_000.0, epsilon = 1e-2);

        // Monotone increasing frequencies, monotone decreasing scales.
        for w in freqs.windows(2) {
            assert!(w[1] > w[0], "frequencies not monotonic: {w:?}");
        }
        for w in scales.windows(2) {
            assert!(w[1] < w[0], "scales not monotonic: {w:?}");
        }

        // Log spacing: ratio of consecutive frequencies is constant.
        let ratio0 = (freqs[1] / freqs[0]) as f64;
        for w in freqs.windows(2) {
            let r = (w[1] / w[0]) as f64;
            assert_relative_eq!(r, ratio0, epsilon = 1e-4);
        }

        // Spot-check scale↔freq inverse: for each bin, scale * freq ≈
        // sigma * sr / (2π). Constant, independent of i.
        let expected = DEFAULT_SIGMA as f64 * 48_000.0 / (2.0 * PI);
        for (a, f) in scales.iter().zip(freqs.iter()) {
            assert_relative_eq!(*a as f64 * *f as f64, expected, epsilon = 0.1);
        }
    }
}
