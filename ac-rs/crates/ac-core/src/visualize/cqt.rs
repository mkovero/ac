//! Constant-Q transform — Brown 1991 / Schörkhuber–Klapuri 2010 pragmatic
//! variant.
//!
//! Produces one CQT column (one magnitude per log-spaced bin, sampled at the
//! right-edge of the analysis buffer) by direct time-domain dot product of
//! the input against a precomputed bank of Hann-windowed complex
//! exponentials. Kernel length scales with `Q · sample_rate / f_k` so every
//! bin sees the same number of cycles → constant Q across the band.
//!
//! ## Calibration
//!
//! Magnitudes are absolute dBFS, normalized to match [`crate::analysis`]
//! and [`super::cwt`]: a pure cosine of amplitude `A` at a bin centre
//! produces `20 · log10(A)` at that bin. Each kernel is scaled by
//! `2 / Σ w[i]` (where `w` is the Hann window) so the dot product yields
//! `A` directly for a unit-cycle-aligned tone.
//!
//! ## Buffer requirements
//!
//! The lowest bin's kernel needs `Q · sr / f_min` samples in `buf`; bins
//! whose kernel exceeds the buffer length are NaN-padded by `cqt_into`.
//! Use [`min_supported_f`] to derive a feasible `f_min` from the available
//! buffer length, or pass [`build_kernels`] a `max_n` cap to truncate
//! oversize kernels (with a small sub-Q penalty at the low end).
//!
//! ## Performance
//!
//! Scalar implementation only — no SIMD, no parallel rayon iter. The
//! totals at the default 24 bpo / 30 Hz–0.9·Nyquist grid land around
//! ~25 M f64 mul-adds/s at 50 Hz tick rate, comfortably under what one
//! core handles. The `cwt.rs`-style AVX2 + rayon treatment can land later
//! if a user-visible bottleneck shows up; tracked as a follow-up.

use std::f64::consts::PI;

use realfft::num_complex::Complex;

/// Default bins per octave. 24 = quarter-tones, fine enough for visual
/// resolution, coarse enough to keep kernel counts modest.
pub const DEFAULT_BPO: u32 = 24;

/// Default low edge of the CQT frequency axis (Hz). 30 Hz gives a kernel
/// length of `Q · sr / 30 ≈ 55 k` samples at 48 kHz — too long for the
/// 0.15 s CWT ring, so the daemon worker sizes its CQT ring to 1 s.
pub const DEFAULT_F_MIN: f32 = 30.0;

/// Default high edge as a fraction of Nyquist. Same 0.9 as CWT to keep
/// the highest kernel from bleeding against the band edge.
pub const DEFAULT_F_MAX_NYQUIST_FRACTION: f32 = 0.9;

/// Default high edge in Hz for a given sample rate.
pub fn default_f_max(sample_rate: u32) -> f32 {
    (sample_rate as f32 / 2.0) * DEFAULT_F_MAX_NYQUIST_FRACTION
}

/// CQT quality factor for `bpo` bins per octave.
///
/// Standard derivation: adjacent bin centres are spaced by `2^(1/B)` so the
/// fractional bandwidth is `2^(1/B) - 1`, and `Q = 1 / Δf_rel`.
pub fn cqt_q(bpo: u32) -> f64 {
    assert!(bpo >= 1, "bpo must be ≥ 1");
    1.0 / (2.0_f64.powf(1.0 / bpo as f64) - 1.0)
}

/// Lowest frequency whose Q-invariant kernel fits in `buf_len` samples.
pub fn min_supported_f(buf_len: usize, sample_rate: u32, bpo: u32) -> f32 {
    assert!(buf_len > 0 && sample_rate > 0);
    let q = cqt_q(bpo);
    ((q * sample_rate as f64) / buf_len as f64) as f32
}

/// Geometric (log₂) frequency grid: `f_k = f_min · 2^(k/B)` up to the last
/// `f_k ≤ f_max`.
pub fn log_freqs(f_min: f32, f_max: f32, bpo: u32) -> Vec<f32> {
    assert!(f_min > 0.0 && f_max > f_min, "invalid range {f_min}..{f_max}");
    assert!(bpo >= 1);
    let step = (2.0_f64).powf(1.0 / bpo as f64);
    let mut out = Vec::new();
    let mut f = f_min as f64;
    while (f as f32) <= f_max {
        out.push(f as f32);
        f *= step;
    }
    out
}

/// Precomputed kernel bank — one Hann-windowed complex exponential per bin.
pub struct CqtKernels {
    pub bpo:         u32,
    pub q:           f64,
    pub sample_rate: u32,
    pub freqs:       Vec<f32>,
    kernels:         Vec<Kernel>,
}

struct Kernel {
    /// Pre-conjugated, pre-windowed, pre-normalised samples. `dot(x, k)`
    /// directly yields the analytic-signal coefficient for the bin's tone.
    samples: Vec<Complex<f64>>,
    /// Number of input samples this kernel consumes (equal to `samples.len()`,
    /// surfaced for clarity in the dot-product loop).
    n: usize,
}

impl CqtKernels {
    pub fn n_bins(&self) -> usize { self.freqs.len() }
    pub fn max_kernel_len(&self) -> usize {
        self.kernels.iter().map(|k| k.n).max().unwrap_or(0)
    }
}

/// Build the per-bin kernel bank.
///
/// Each kernel's length is `clamp(round(Q · sr / f_k), 16, max_n)`. Bins
/// whose ideal length exceeds `max_n` get a truncated kernel with a small
/// sub-Q penalty (resolution slightly worse than constant-Q at the low
/// end); the alternative — emitting NaN — would leave a hole in the
/// spectrum, which is worse for visualisation.
pub fn build_kernels(
    freqs: &[f32],
    sample_rate: u32,
    bpo: u32,
    max_n: usize,
) -> CqtKernels {
    assert!(sample_rate > 0);
    assert!(max_n >= 16, "max_n must be ≥ 16, got {max_n}");
    let q = cqt_q(bpo);
    let sr = sample_rate as f64;

    let kernels = freqs.iter().map(|&f| {
        let f = f as f64;
        let ideal = (q * sr / f).round() as usize;
        let n = ideal.clamp(16, max_n);

        // Hann window over n samples.
        let mut wsum = 0.0;
        let mut win = vec![0.0_f64; n];
        for (i, w) in win.iter_mut().enumerate() {
            *w = 0.5 - 0.5 * ((2.0 * PI * i as f64) / (n as f64 - 1.0)).cos();
            wsum += *w;
        }
        // Normalise so a unit cosine at f produces |X| = 1.
        let scale = 2.0 / wsum;

        // Conjugate kernel: e^{-j 2π f i / sr}, windowed and scaled.
        let omega = 2.0 * PI * f / sr;
        let samples: Vec<Complex<f64>> = win.iter().enumerate().map(|(i, &w)| {
            let phase = -omega * i as f64;
            Complex::new(w * scale * phase.cos(), w * scale * phase.sin())
        }).collect();

        Kernel { samples, n }
    }).collect();

    CqtKernels { bpo, q, sample_rate, freqs: freqs.to_vec(), kernels }
}

/// Compute one CQT column. Magnitudes are written to `mags_out` (resized to
/// `kernels.n_bins()`) in absolute dBFS, with `f32::NEG_INFINITY` reserved
/// for empty bins (`n = 0`).
///
/// The right edge of `buf` is treated as the column's sampling instant —
/// each kernel reads its last `n` samples from `buf[buf.len() - n ..]`.
/// Bins whose kernel is longer than `buf` produce `f32::NAN`.
pub fn cqt_into(buf: &[f32], kernels: &CqtKernels, mags_out: &mut Vec<f32>) {
    mags_out.clear();
    mags_out.reserve(kernels.n_bins());
    let buf_len = buf.len();

    for k in &kernels.kernels {
        if k.n == 0 {
            mags_out.push(f32::NEG_INFINITY);
            continue;
        }
        if k.n > buf_len {
            mags_out.push(f32::NAN);
            continue;
        }
        let start = buf_len - k.n;
        let mut acc_re = 0.0_f64;
        let mut acc_im = 0.0_f64;
        for i in 0..k.n {
            let x = buf[start + i] as f64;
            let kk = k.samples[i];
            acc_re += x * kk.re;
            acc_im += x * kk.im;
        }
        let mag = (acc_re * acc_re + acc_im * acc_im).sqrt();
        let db = if mag > 1e-12 {
            20.0 * mag.log10()
        } else {
            -240.0
        };
        mags_out.push(db as f32);
    }
}

/// Convenience wrapper: allocates and returns a fresh magnitude vector.
pub fn cqt(buf: &[f32], kernels: &CqtKernels) -> Vec<f32> {
    let mut out = Vec::with_capacity(kernels.n_bins());
    cqt_into(buf, kernels, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cosine(amp: f32, freq: f32, n: usize, sr: u32) -> Vec<f32> {
        let two_pi = 2.0 * std::f32::consts::PI;
        (0..n).map(|i| amp * (two_pi * freq * i as f32 / sr as f32).cos()).collect()
    }

    #[test]
    fn q_matches_brown() {
        // B=12: Q ≈ 16.817; B=24: Q ≈ 34.127; B=1: Q = 1.0 (one octave-wide bin).
        assert!((cqt_q(12) - 16.817).abs() < 0.01);
        assert!((cqt_q(24) - 34.127).abs() < 0.01);
        assert!((cqt_q(1) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn log_freqs_geometric() {
        let f = log_freqs(100.0, 800.0, 12);
        // 12 bins/octave, 3 octaves = 36 steps + endpoint = 37 bins.
        assert_eq!(f.len(), 37);
        assert!((f[0] - 100.0).abs() < 1e-3);
        assert!((f[12] - 200.0).abs() < 0.1);
        assert!((f[24] - 400.0).abs() < 0.1);
        assert!((f[36] - 800.0).abs() < 0.5);
    }

    #[test]
    fn unit_cosine_calibration() {
        // Cosine at a bin centre, amplitude 0.5 → -6 dBFS at that bin.
        let sr = 48_000;
        let bpo = 24;
        let f0 = 1000.0_f32;
        let freqs = vec![f0];
        let buf_len = 8192;                                   // > Q·sr/f0 ≈ 1639
        let kernels = build_kernels(&freqs, sr, bpo, buf_len);
        let buf = cosine(0.5, f0, buf_len, sr);
        let mags = cqt(&buf, &kernels);
        let expected = 20.0 * 0.5_f32.log10();                // -6.02 dBFS
        assert!(
            (mags[0] - expected).abs() < 0.5,
            "got {} dBFS, expected ≈ {} dBFS",
            mags[0], expected
        );
    }

    #[test]
    fn equal_energy_at_constant_q() {
        // Same amplitude at three frequencies that all sit on a 24-bpo grid
        // anchored at 100 Hz: each should report the same dBFS within a few
        // tenths of a dB (Hann window leakage at the bin centre is small but
        // non-zero because Q·sr/f rounds to an integer).
        let sr = 48_000;
        let bpo = 24;
        let amp = 0.25_f32;
        let f_min = 100.0_f32;
        let freqs = log_freqs(f_min, 8000.0, bpo);
        let buf_len = (cqt_q(bpo) * sr as f64 / f_min as f64).ceil() as usize + 256;
        let kernels = build_kernels(&freqs, sr, bpo, buf_len);
        let expected = 20.0 * amp.log10();

        for &test_f in &[200.0_f32, 1000.0, 5000.0] {
            // Snap test_f to the nearest grid bin so we measure the kernel,
            // not the leakage between adjacent kernels.
            let (idx, _) = freqs.iter().enumerate()
                .min_by(|a, b| {
                    (a.1 - test_f).abs().partial_cmp(&(b.1 - test_f).abs()).unwrap()
                })
                .unwrap();
            let f_grid = freqs[idx];
            let buf = cosine(amp, f_grid, buf_len, sr);
            let mags = cqt(&buf, &kernels);
            assert!(
                (mags[idx] - expected).abs() < 0.6,
                "bin {idx} (f={f_grid}) got {} dBFS, expected ≈ {}",
                mags[idx], expected
            );
        }
    }

    #[test]
    fn log_sweep_peak_tracks_diagonal() {
        // Stationary tones at three points along a log-frequency axis: the
        // CQT column for each should peak at (close to) the corresponding
        // bin. This is the static analogue of "sweep peak tracks diagonally
        // on a log-f waterfall" without the time axis dependency.
        let sr = 48_000;
        let bpo = 24;
        let freqs = log_freqs(100.0, 8000.0, bpo);
        let buf_len = 8192;
        let kernels = build_kernels(&freqs, sr, bpo, buf_len);

        for &probe_f in &[150.0_f32, 750.0, 3500.0] {
            let buf = cosine(0.5, probe_f, buf_len, sr);
            let mags = cqt(&buf, &kernels);
            let (peak_idx, _) = mags.iter().enumerate()
                .filter(|(_, &m)| m.is_finite())
                .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
                .unwrap();
            let peak_f = freqs[peak_idx];
            // Probe is mid-bin: peak must be in one of the two flanking bins.
            let rel_err = (peak_f / probe_f).log2().abs() * bpo as f32;
            assert!(
                rel_err < 1.5,
                "probe {probe_f} Hz peaked at bin {peak_idx} (f={peak_f}, err={rel_err:.2} bins)"
            );
        }
    }

    #[test]
    fn min_supported_f_round_trip() {
        let sr = 48_000;
        let bpo = 24;
        let buf_len = 7200;                                   // 0.15 s
        let f_min = min_supported_f(buf_len, sr, bpo);
        // Build kernels at exactly f_min and ensure the lowest one fits.
        let freqs = vec![f_min, f_min * 2.0];
        let kernels = build_kernels(&freqs, sr, bpo, buf_len);
        assert!(kernels.max_kernel_len() <= buf_len);
        // And the lowest bin's kernel is essentially full-length.
        let lo_n = (cqt_q(bpo) * sr as f64 / f_min as f64).round() as usize;
        assert!((lo_n as i64 - buf_len as i64).abs() <= 2);
    }

    #[test]
    fn oversize_kernel_truncated_not_nan() {
        // f below the buffer's supported minimum: build_kernels truncates
        // to max_n (sub-Q at the low end) instead of returning a NaN bin.
        let sr = 48_000;
        let bpo = 24;
        let buf_len = 1024;
        let f0 = 30.0;
        let kernels = build_kernels(&[f0], sr, bpo, buf_len);
        assert_eq!(kernels.max_kernel_len(), buf_len);
        let buf = cosine(0.5, f0, buf_len, sr);
        let mags = cqt(&buf, &kernels);
        assert!(mags[0].is_finite(), "got {}", mags[0]);
    }
}
