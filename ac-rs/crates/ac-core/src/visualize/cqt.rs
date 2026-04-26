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
//! Hot path uses an AVX2 + FMA dispatch on x86_64 with a scalar fallback.
//! The kernel bank is split-of-arrays (`re` and `im` in separate
//! `Vec<f64>`) to keep both inner dot products SIMD-friendly, and the
//! `f32` → `f64` cast of the input window is hoisted into a thread-local
//! scratch so it runs once per column instead of once per bin.

use std::cell::RefCell;
use std::f64::consts::PI;

#[cfg(target_arch = "x86_64")]
use std::sync::OnceLock;

thread_local! {
    /// Per-thread input-conversion scratch. Sized to the longest kernel
    /// the active `CqtKernels` uses; the `f32` → `f64` cast of the right-
    /// edge buffer slice runs into here once per `cqt_into` call, then
    /// every bin reads its tail of this vec instead of re-casting
    /// per element. Reused across calls — only resizes upward.
    static CQT_SCRATCH_X: RefCell<Vec<f64>> = const { RefCell::new(Vec::new()) };
}

#[cfg(target_arch = "x86_64")]
fn avx2_fma_available() -> bool {
    static CACHED: OnceLock<bool> = OnceLock::new();
    *CACHED.get_or_init(|| {
        is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma")
    })
}

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
    /// Real part of the pre-conjugated, pre-windowed, pre-normalised
    /// kernel. Length = `n`. SOA layout (paired with `im`) keeps each
    /// inner dot product a clean `f64` stream that vectorises directly.
    re: Vec<f64>,
    /// Imaginary part of the kernel, length = `n`.
    im: Vec<f64>,
    /// Number of input samples this kernel consumes.
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

        // Conjugate kernel: e^{-j 2π f i / sr}, windowed and scaled. Split
        // into two `Vec<f64>`s so the AVX inner loop streams each
        // independently.
        let omega = 2.0 * PI * f / sr;
        let mut re = Vec::with_capacity(n);
        let mut im = Vec::with_capacity(n);
        for (i, &w) in win.iter().enumerate() {
            let phase = -omega * i as f64;
            re.push(w * scale * phase.cos());
            im.push(w * scale * phase.sin());
        }
        Kernel { re, im, n }
    }).collect();

    CqtKernels { bpo, q, sample_rate, freqs: freqs.to_vec(), kernels }
}

// --- MAC inner loop ---------------------------------------------------------
//
// Each kernel computes `acc = Σ x[i] * k_re[i]` and `Σ x[i] * k_im[i]` over
// `n` `f64`s. The dispatcher picks AVX2+FMA on x86_64 when the CPU advertises
// it, falling back to scalar on every other arch and the no-AVX boot path.
// All slices are padded so a 4-wide load never reads outside the kernel's
// valid range — the tail past `n / 4 * 4` is finished scalar to avoid
// per-bin tail-zero padding.

#[inline]
fn mac_scalar(x: &[f64], k_re: &[f64], k_im: &[f64], n: usize) -> (f64, f64) {
    let mut re = 0.0_f64;
    let mut im = 0.0_f64;
    for i in 0..n {
        re += x[i] * k_re[i];
        im += x[i] * k_im[i];
    }
    (re, im)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn mac_avx2_fma(x: &[f64], k_re: &[f64], k_im: &[f64], n: usize) -> (f64, f64) {
    use std::arch::x86_64::*;
    let chunks = n / 4;
    let mut acc_re = _mm256_setzero_pd();
    let mut acc_im = _mm256_setzero_pd();
    for c in 0..chunks {
        let off = c * 4;
        let xv  = _mm256_loadu_pd(x.as_ptr().add(off));
        let krv = _mm256_loadu_pd(k_re.as_ptr().add(off));
        let kiv = _mm256_loadu_pd(k_im.as_ptr().add(off));
        acc_re  = _mm256_fmadd_pd(xv, krv, acc_re);
        acc_im  = _mm256_fmadd_pd(xv, kiv, acc_im);
    }
    let hsum = |v: __m256d| -> f64 {
        let lo = _mm256_castpd256_pd128(v);
        let hi = _mm256_extractf128_pd::<1>(v);
        let s  = _mm_add_pd(lo, hi);
        let s  = _mm_hadd_pd(s, s);
        _mm_cvtsd_f64(s)
    };
    let mut re = hsum(acc_re);
    let mut im = hsum(acc_im);
    let tail_start = chunks * 4;
    for i in tail_start..n {
        re += x[i] * k_re[i];
        im += x[i] * k_im[i];
    }
    (re, im)
}

#[inline]
fn mac_dispatch(x: &[f64], k_re: &[f64], k_im: &[f64], n: usize) -> (f64, f64) {
    #[cfg(target_arch = "x86_64")]
    {
        if avx2_fma_available() {
            // Safety: gated on a runtime cpuid check and the slices have
            // length ≥ n by the caller's contract.
            return unsafe { mac_avx2_fma(x, k_re, k_im, n) };
        }
    }
    mac_scalar(x, k_re, k_im, n)
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
    let max_n = kernels.max_kernel_len();

    CQT_SCRATCH_X.with(|cell| {
        let mut x = cell.borrow_mut();
        // Hoist the f32 → f64 conversion: cast `min(buf_len, max_n)`
        // samples from the right edge of `buf` once into thread-local
        // scratch, then each kernel reads its tail.
        let read_n = max_n.min(buf_len);
        x.resize(read_n, 0.0);
        let src_start = buf_len - read_n;
        for (dst, &src) in x.iter_mut().zip(buf[src_start..].iter()) {
            *dst = src as f64;
        }

        for k in &kernels.kernels {
            if k.n == 0 {
                mags_out.push(f32::NEG_INFINITY);
                continue;
            }
            if k.n > buf_len {
                mags_out.push(f32::NAN);
                continue;
            }
            // x's last `k.n` samples align with the kernel's domain.
            let off = read_n - k.n;
            let (re, im) = mac_dispatch(&x[off..off + k.n], &k.re, &k.im, k.n);
            let mag2 = re * re + im * im;
            let db = if mag2 > 1e-24 {
                10.0 * mag2.log10()                          // 10·log10(|x|²) = 20·log10(|x|)
            } else {
                -240.0
            };
            mags_out.push(db as f32);
        }
    });
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
