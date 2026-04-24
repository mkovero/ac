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

use std::cell::RefCell;
use std::collections::HashMap;
use std::f64::consts::PI;
use std::sync::Arc;

use rayon::prelude::*;
use realfft::{num_complex::Complex, RealFftPlanner, RealToComplex};

thread_local! {
    /// Forward real→complex FFT plans keyed on N. CWT is called per monitor
    /// tick and N is fixed at `ring_cap` once warm, so this caches the plan
    /// for the whole run. Keeps the CWT tick hot path out of the planner.
    /// Real-input FFT halves the butterfly work vs a general complex FFT.
    static CWT_FFT_PLANS: RefCell<HashMap<usize, Arc<dyn RealToComplex<f64>>>> =
        RefCell::new(HashMap::new());

    /// Scratch buffers reused across calls: `input` holds the real f64 copy
    /// of the incoming f32 samples; `spectrum` holds the N/2+1 positive-
    /// frequency bins. Kept thread-local so the monitor worker reuses them
    /// across every CWT tick and the allocator never shows up in profiles.
    static CWT_SCRATCH_INPUT: RefCell<Vec<f64>> = const { RefCell::new(Vec::new()) };
    static CWT_SCRATCH_SPECTRUM: RefCell<Vec<Complex<f64>>> = const { RefCell::new(Vec::new()) };
    /// Scratch buffer for `RealToComplex::process_with_scratch`. Without this,
    /// `process()` allocates and zeros a scratch Vec<Complex<f64>> on every
    /// call — showed up as ~44% `copy_nonoverlapping` in v5 profiles.
    static CWT_SCRATCH_FFT: RefCell<Vec<Complex<f64>>> = const { RefCell::new(Vec::new()) };
}

fn cwt_fft_plan(n: usize) -> Arc<dyn RealToComplex<f64>> {
    CWT_FFT_PLANS.with(|cell| {
        cell.borrow_mut()
            .entry(n)
            .or_insert_with(|| RealFftPlanner::<f64>::new().plan_fft_forward(n))
            .clone()
    })
}

// --- MAC inner loop ---------------------------------------------------------
//
// Each kernel computes `acc = Σ spectrum[k_lo + i] * h[i]` over a few dozen
// bins and returns `(acc.re, acc.im)`. At 512 scales × ~20–60 bins this is
// the single hottest loop in the CWT path, so we dispatch to an AVX2+FMA
// implementation when the CPU supports it and fall back to scalar otherwise.
//
// The AVX path relies on two layout invariants guaranteed by `CachedKernel`
// and the scratch setup above:
//  1. `h_dup` is `[h0, h0, h1, h1, …]` padded to a multiple of 4 f64s, so
//     each 256-bit load yields two complex-bin weights without any shuffle.
//  2. The `Complex<f64>` spectrum scratch is padded with one extra zero
//     complex so the last AVX load of a maxed-out kernel never reads uninit.

fn mac_scalar(spectrum: &[Complex<f64>], h_dup: &[f64], k_lo: usize) -> (f64, f64) {
    let mut re = 0.0_f64;
    let mut im = 0.0_f64;
    let n = h_dup.len() / 2;
    for j in 0..n {
        let c = spectrum[k_lo + j];
        let h = h_dup[2 * j];
        re += c.re * h;
        im += c.im * h;
    }
    (re, im)
}

#[cfg(target_arch = "x86_64")]
fn avx2_fma_available() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static CACHED: AtomicU8 = AtomicU8::new(0); // 0 unknown, 1 yes, 2 no
    match CACHED.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let yes = is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma");
            CACHED.store(if yes { 1 } else { 2 }, Ordering::Relaxed);
            yes
        }
    }
}

#[cfg(not(target_arch = "x86_64"))]
fn avx2_fma_available() -> bool { false }

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn mac_avx2_fma(spec_f64: *const f64, h_dup: &[f64], k_lo: usize) -> (f64, f64) {
    use std::arch::x86_64::*;
    // Four independent accumulators so consecutive `vfmadd231pd` instructions
    // do not chain on the same register — hides the ~4-cycle FMA latency and
    // keeps one FMA port busy per cycle on Skylake/Alder Lake. Single-acc
    // version was latency-bound and no faster than scalar.
    let mut a0 = _mm256_setzero_pd();
    let mut a1 = _mm256_setzero_pd();
    let mut a2 = _mm256_setzero_pd();
    let mut a3 = _mm256_setzero_pd();
    let spec = spec_f64.add(k_lo * 2);
    let h = h_dup.as_ptr();
    let len = h_dup.len();
    let full = len & !15; // multiple of 16 f64s
    let mut i = 0;
    while i < full {
        a0 = _mm256_fmadd_pd(_mm256_loadu_pd(spec.add(i)),      _mm256_loadu_pd(h.add(i)),      a0);
        a1 = _mm256_fmadd_pd(_mm256_loadu_pd(spec.add(i + 4)),  _mm256_loadu_pd(h.add(i + 4)),  a1);
        a2 = _mm256_fmadd_pd(_mm256_loadu_pd(spec.add(i + 8)),  _mm256_loadu_pd(h.add(i + 8)),  a2);
        a3 = _mm256_fmadd_pd(_mm256_loadu_pd(spec.add(i + 12)), _mm256_loadu_pd(h.add(i + 12)), a3);
        i += 16;
    }
    // Tail: h_dup is padded to mult-of-4, so this runs 0..=3 more iters.
    while i < len {
        a0 = _mm256_fmadd_pd(_mm256_loadu_pd(spec.add(i)), _mm256_loadu_pd(h.add(i)), a0);
        i += 4;
    }
    // Reduce the four 256-bit accumulators to one [re_total, im_total] pair.
    let s01 = _mm256_add_pd(a0, a1);
    let s23 = _mm256_add_pd(a2, a3);
    let acc = _mm256_add_pd(s01, s23);
    let lo = _mm256_castpd256_pd128(acc);
    let hi = _mm256_extractf128_pd(acc, 1);
    let s128 = _mm_add_pd(lo, hi);
    let re = _mm_cvtsd_f64(s128);
    let im = _mm_cvtsd_f64(_mm_unpackhi_pd(s128, s128));
    (re, im)
}

// Non-x86 fallback so the function name resolves; never called because
// `avx2_fma_available` returns false on non-x86.
#[cfg(not(target_arch = "x86_64"))]
unsafe fn mac_avx2_fma(_spec_f64: *const f64, _h_dup: &[f64], _k_lo: usize) -> (f64, f64) {
    (0.0, 0.0)
}

/// Per-scale Gaussian kernel, keyed on `(n, sigma, scale)` via
/// [`KernelCache`]. The twiddle-folded Gaussian is stored pre-duplicated as
/// `[h0, h0, h1, h1, …]` with a trailing zero pad to a multiple of four f64s.
/// This layout lines up exactly with the interleaved `Complex<f64>` spectrum
/// produced by `realfft`, so one AVX2 `vfmadd231pd` per four f64s processes
/// two complex bins at a time without any shuffles or a split pass.
struct CachedKernel {
    k_lo:  usize,
    h_dup: Vec<f64>,
}

#[derive(Default)]
struct KernelCache {
    n:         usize,
    sigma_bits: u32,
    scales:    Vec<u32>, // f32::to_bits to dodge Eq on f32
    kernels:   Vec<CachedKernel>,
}

impl KernelCache {
    fn matches(&self, n: usize, sigma: f32, scales: &[f32]) -> bool {
        self.n == n
            && self.sigma_bits == sigma.to_bits()
            && self.scales.len() == scales.len()
            && self.scales.iter().zip(scales).all(|(a, b)| *a == b.to_bits())
    }

    fn rebuild(&mut self, n: usize, sigma: f32, scales: &[f32]) {
        let omega0 = sigma as f64;
        let two_pi_over_n = 2.0 * PI / n as f64;
        let half = n / 2;
        const CUTOFF: f64 = 5.5;

        self.n = n;
        self.sigma_bits = sigma.to_bits();
        self.scales = scales.iter().map(|s| s.to_bits()).collect();
        self.kernels = scales
            .par_iter()
            .map(|&scale| {
                let a = scale as f64;
                let k_center = omega0 / (a * two_pi_over_n);
                let k_width  = CUTOFF / (a * two_pi_over_n);
                let k_lo = ((k_center - k_width).floor() as isize).max(0) as usize;
                let k_hi = ((k_center + k_width).ceil() as isize).min(half as isize) as usize;
                // Fold the (-1)^k twiddle (single-point IFFT at t = N/2) into
                // the kernel: negate h[i] when (k_lo + i) is odd. Lets the
                // hot MAC loop skip a full N-wide pass over `spectrum`.
                let h_len = k_hi - k_lo + 1;
                let dup_len = (2 * h_len + 3) & !3; // pad to mult of 4 f64s
                let mut h_dup = vec![0.0_f64; dup_len];
                for (i, k) in (k_lo..=k_hi).enumerate() {
                    let arg = a * (two_pi_over_n * k as f64) - omega0;
                    let g = (-0.5 * arg * arg).exp();
                    let g = if k & 1 != 0 { -g } else { g };
                    h_dup[2 * i]     = g;
                    h_dup[2 * i + 1] = g;
                }
                CachedKernel { k_lo, h_dup }
            })
            .collect();
    }
}

thread_local! {
    static KERNEL_CACHE: RefCell<KernelCache> = RefCell::new(KernelCache::default());
}

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
    sample_rate: u32,
    scales: &[f32],
    sigma: f32,
) -> Vec<f32> {
    let mut out = Vec::with_capacity(scales.len());
    morlet_cwt_into(samples, sample_rate, scales, sigma, &mut out);
    out
}

/// In-place variant of [`morlet_cwt`]: writes the dBFS magnitudes into
/// `out`, resizing it to `scales.len()`. The allocated capacity is retained
/// across calls — the live monitor worker reuses one `Vec<f32>` across
/// every tick, which keeps the per-call allocator cost (seen as `madvise`
/// in profiles) out of the hot path.
pub fn morlet_cwt_into(
    samples: &[f32],
    _sample_rate: u32,
    scales: &[f32],
    sigma: f32,
    out: &mut Vec<f32>,
) {
    let n = samples.len();
    assert!(n >= 256, "need at least 256 samples, got {n}");
    assert!(!scales.is_empty(), "need at least one scale");
    assert!(sigma > 0.0, "sigma must be positive");

    let fft = cwt_fft_plan(n);

    CWT_SCRATCH_INPUT.with(|in_cell| {
        CWT_SCRATCH_SPECTRUM.with(|sp_cell| {
            CWT_SCRATCH_FFT.with(|sc_cell| {
                let mut input = in_cell.borrow_mut();
                let mut spectrum = sp_cell.borrow_mut();
                let mut scratch = sc_cell.borrow_mut();
                input.resize(n, 0.0);
                spectrum.resize(n / 2 + 1, Complex::new(0.0, 0.0));
                let need_scratch = fft.get_scratch_len();
                if scratch.len() < need_scratch {
                    scratch.resize(need_scratch, Complex::new(0.0, 0.0));
                }
                for (dst, &src) in input.iter_mut().zip(samples.iter()) {
                    *dst = src as f64;
                }
                // realfft only writes the positive-frequency half (N/2 + 1 bins);
                // kernels already clamp k_hi to N/2 so the stride into `spectrum`
                // is safe without further bounds work.
                fft.process_with_scratch(&mut input, &mut spectrum, &mut scratch[..need_scratch])
                    .expect("realfft: input/output/scratch lengths match plan");

            // (-1)^k twiddle is folded into each cached kernel's h_dup[], so
            // we feed `spectrum` to the MAC loop unchanged.
            // Log-domain: |mag|·inv_n·2 → 20·log10 = (re²+im²)·inv_n²·4 → 10·log10.
            // Saves a `hypot` per scale.
            let scale_const = 4.0 / (n as f64 * n as f64);

            // AVX2 reads 4 f64s (= 2 complex bins) at a time and the kernel's
            // last h_dup chunk may extend past the valid N/2+1 spectrum bins
            // by up to one complex. Pad the scratch with one zero complex so
            // the overread sees 0.0 × 0.0 = 0.0 and does not touch uninit mem.
            spectrum.push(Complex::new(0.0, 0.0));

            KERNEL_CACHE.with(|cell| {
                let mut cache = cell.borrow_mut();
                if !cache.matches(n, sigma, scales) {
                    cache.rebuild(n, sigma, scales);
                }
                out.clear();
                out.reserve(cache.kernels.len());
                let spec_f64: *const f64 = spectrum.as_ptr() as *const f64;
                // Serial — rayon was a loss here (see commit 33ba79b). Each
                // kernel is a few dozen MACs and a full tick totals ~12k MACs
                // (sub-millisecond). Waking the global rayon pool for that
                // dwarfs the math with sched_yield/futex/epoch-pin cost. If
                // the workload grows (n_scales ≫ 1024 or per-kernel width
                // ≫ 500 bins), reintroduce a *dedicated* small pool — never
                // the global one.
                let use_avx2 = avx2_fma_available();
                for kernel in &cache.kernels {
                    let (re, im) = if use_avx2 {
                        // SAFETY: avx2_fma_available returned true; pointers
                        // come from live borrows of `spectrum` / kernel.h_dup;
                        // h_dup.len() is a multiple of 4 and spectrum has the
                        // trailing zero-complex pad asserted above.
                        unsafe { mac_avx2_fma(spec_f64, &kernel.h_dup, kernel.k_lo) }
                    } else {
                        mac_scalar(&spectrum, &kernel.h_dup, kernel.k_lo)
                    };
                    let mag_sq = (re * re + im * im) * scale_const;
                    out.push((10.0 * mag_sq.max(1e-24).log10()) as f32);
                }
            });
            });
        });
    });
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
