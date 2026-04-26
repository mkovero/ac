//! Auger–Flandrin reassigned STFT spectrogram (Auger & Flandrin 1995).
//!
//! For each FFT bin we compute a "reassigned" centre frequency using two
//! auxiliary STFTs (one weighted by the window's derivative `dh/dt`, one
//! weighted by `t · h(t)`); energy is then reassigned from the bin's
//! coordinate `f_k = k·sr/N` to its instantaneous frequency `f̂_k`. The
//! result is a sharper spectrogram than the plain STFT — chirps no longer
//! smear across multiple bins, and tones closer than the FFT's main-lobe
//! width can resolve as separate peaks.
//!
//! ## Single-column simplification
//!
//! The full reassignment formula also gives a reassigned time `t̂_k`; for
//! a streaming `monitor_spectrum`-style column we evaluate at a fixed
//! buffer-centre instant and discard `t̂_k`. The frequency coordinate
//! does the heavy lifting visually.
//!
//! ## Calibration
//!
//! Magnitudes are absolute dBFS, normalised to match [`crate::analysis`],
//! [`super::cwt`], and [`super::cqt`]: a pure cosine of amplitude `A`
//! that lands cleanly inside one FFT bin reports `20·log10(A)` at the
//! containing output bin.
//!
//! ## Performance
//!
//! Three forward real FFTs per column, plus an O(N) reassignment loop.
//! At N = 4096 this is sub-millisecond on a single core. As with `cqt`
//! and `cwt`, scalar only on first cut — perf treatment can land later
//! if a hot path appears.

use std::cell::RefCell;
use std::f64::consts::PI;
use std::sync::Arc;

use realfft::{num_complex::Complex, RealFftPlanner, RealToComplex};

thread_local! {
    /// Per-thread scratch buffers reused across `reassigned_into` calls.
    /// Without these the hot path allocates 3 input vecs + 3 spectrum vecs
    /// + 1 FFT scratch + 1 power accumulator on every column — about
    /// 200 KB at the default 4096-sample / 1024-bin geometry. Resizing in
    /// place keeps allocator traffic out of the hot path; the vecs only
    /// shrink-by-resize never deallocate.
    static SCRATCH_X:    RefCell<Vec<f64>>            = const { RefCell::new(Vec::new()) };
    static SCRATCH_IN_H: RefCell<Vec<f64>>            = const { RefCell::new(Vec::new()) };
    static SCRATCH_IN_T: RefCell<Vec<f64>>            = const { RefCell::new(Vec::new()) };
    static SCRATCH_IN_D: RefCell<Vec<f64>>            = const { RefCell::new(Vec::new()) };
    static SCRATCH_SP_H: RefCell<Vec<Complex<f64>>>   = const { RefCell::new(Vec::new()) };
    static SCRATCH_SP_T: RefCell<Vec<Complex<f64>>>   = const { RefCell::new(Vec::new()) };
    static SCRATCH_SP_D: RefCell<Vec<Complex<f64>>>   = const { RefCell::new(Vec::new()) };
    static SCRATCH_FFT:  RefCell<Vec<Complex<f64>>>   = const { RefCell::new(Vec::new()) };
    static SCRATCH_POW:  RefCell<Vec<f64>>            = const { RefCell::new(Vec::new()) };
}

/// Default FFT length. 4096 → ~11.7 Hz bin width at 48 kHz, with the
/// reassignment step concentrating energy well below that.
pub const DEFAULT_N: usize = 4096;

/// Default number of log-spaced output bins (the reassigned column the UI
/// receives). Higher than the FFT bin count so reassignment has somewhere
/// to put closely-spaced peaks.
pub const DEFAULT_N_OUT_BINS: usize = 1024;

/// Default low edge of the output frequency axis. Same 20 Hz as CWT.
pub const DEFAULT_F_MIN: f32 = 20.0;

/// Default high edge as a fraction of Nyquist. Matches CWT/CQT.
pub const DEFAULT_F_MAX_NYQUIST_FRACTION: f32 = 0.9;

/// Default high edge in Hz for a given sample rate.
pub fn default_f_max(sample_rate: u32) -> f32 {
    (sample_rate as f32 / 2.0) * DEFAULT_F_MAX_NYQUIST_FRACTION
}

/// Magnitude floor (dB below the column's peak) below which a bin's
/// reassignment is discarded — the f̂ formula divides by `|X_h|²`,
/// which goes wild for noise-floor bins. 60 dB is conservative.
pub const DEFAULT_NOISE_FLOOR_DB: f32 = -60.0;

/// Precomputed plan and windows for a single output column shape.
pub struct ReassignedKernels {
    pub n:           usize,
    pub sample_rate: u32,
    pub freqs_out:   Vec<f32>,
    h:               Vec<f64>,
    th:              Vec<f64>,
    dh:              Vec<f64>,
    h_norm:          f64,
    /// Equivalent noise bandwidth of `h`, in bins. Reassignment funnels a
    /// tone's spread sidelobes back to one output bin, and Parseval says
    /// the sum of one-sided `|Y·amp_scale|²` over those bins exceeds the
    /// tone's `A²` by exactly this factor (≈ 1.5 for Hann). Dividing the
    /// accumulated power by `enbw` recovers `A²`.
    enbw:            f64,
    fft_plan:        Arc<dyn RealToComplex<f64>>,
    /// Output-bin index for each FFT bin's nominal centre frequency,
    /// precomputed so the hot-path reassignment loop does index math
    /// instead of `log()` per bin.
    nominal_out_idx: Vec<usize>,
}

/// Build the kernel bundle. The output grid is log-spaced from `f_min`
/// to `f_max` with `n_out_bins` entries. Both endpoints are included.
pub fn build_kernels(
    n:           usize,
    sample_rate: u32,
    n_out_bins:  usize,
    f_min:       f32,
    f_max:       f32,
) -> ReassignedKernels {
    assert!(n >= 256, "n must be ≥ 256, got {n}");
    assert!(n.is_power_of_two(), "n must be a power of two for FFT efficiency, got {n}");
    assert!(n_out_bins >= 16, "n_out_bins must be ≥ 16, got {n_out_bins}");
    assert!(sample_rate > 0);
    assert!(f_min > 0.0 && f_max > f_min);

    // Hann window and its time-weighted / derivative variants.
    // Time origin at the buffer centre so `th` is antisymmetric — keeps
    // the time-reassignment formula sign-correct.
    let centre = (n as f64 - 1.0) / 2.0;
    let mut h  = vec![0.0_f64; n];
    let mut th = vec![0.0_f64; n];
    let mut wsum = 0.0;
    let mut wsum_sq = 0.0;
    for i in 0..n {
        let w = 0.5 - 0.5 * (2.0 * PI * i as f64 / (n as f64 - 1.0)).cos();
        h[i]  = w;
        th[i] = (i as f64 - centre) * w;
        wsum    += w;
        wsum_sq += w * w;
    }
    let enbw = n as f64 * wsum_sq / (wsum * wsum);
    // Central-difference derivative; edges use forward/backward differences.
    let mut dh = vec![0.0_f64; n];
    dh[0]     = h[1] - h[0];
    dh[n - 1] = h[n - 1] - h[n - 2];
    for i in 1..(n - 1) {
        dh[i] = (h[i + 1] - h[i - 1]) * 0.5;
    }

    let fft_plan = RealFftPlanner::<f64>::new().plan_fft_forward(n);

    // Log-spaced output grid.
    let log_min = (f_min as f64).ln();
    let log_max = (f_max as f64).ln();
    let step = (log_max - log_min) / (n_out_bins as f64 - 1.0);
    let mut freqs_out = Vec::with_capacity(n_out_bins);
    for i in 0..n_out_bins {
        freqs_out.push((log_min + step * i as f64).exp() as f32);
    }

    // Map each FFT bin's nominal centre to its closest log-grid index.
    // Used as a fallback when reassignment is suppressed by the noise gate.
    let n_half = n / 2 + 1;
    let mut nominal_out_idx = Vec::with_capacity(n_half);
    for k in 0..n_half {
        let f = k as f64 * sample_rate as f64 / n as f64;
        nominal_out_idx.push(out_bin_idx(f as f32, &freqs_out));
    }

    ReassignedKernels {
        n,
        sample_rate,
        freqs_out,
        h,
        th,
        dh,
        h_norm: wsum,
        enbw,
        fft_plan,
        nominal_out_idx,
    }
}

/// Map a frequency to its closest log-grid output-bin index.
fn out_bin_idx(f: f32, grid: &[f32]) -> usize {
    if !f.is_finite() || f <= grid[0] { return 0; }
    if f >= grid[grid.len() - 1] { return grid.len() - 1; }
    // Binary search on monotonically increasing log grid.
    let mut lo = 0usize;
    let mut hi = grid.len() - 1;
    while lo + 1 < hi {
        let mid = (lo + hi) / 2;
        if grid[mid] <= f { lo = mid; } else { hi = mid; }
    }
    if (f - grid[lo]).abs() <= (grid[hi] - f).abs() { lo } else { hi }
}

/// Compute one reassigned spectrogram column.
///
/// Reads the last `kernels.n` samples of `buf` (panics if shorter), runs
/// three Hann-windowed FFTs (standard, time-weighted, derivative-weighted),
/// reassigns each bin's energy to its instantaneous frequency `f̂_k`,
/// accumulates power into the log-spaced output grid, and writes dBFS
/// magnitudes into `mags_out` (resized to `kernels.freqs_out.len()`).
///
/// Bins below `kernels`'s noise gate (60 dB below the column peak) keep
/// their nominal frequency — reassignment becomes meaningless there
/// because `1/|X_h|²` blows up.
pub fn reassigned_into(
    buf:      &[f32],
    kernels:  &ReassignedKernels,
    mags_out: &mut Vec<f32>,
) {
    let n = kernels.n;
    assert!(buf.len() >= n, "buf shorter than kernel.n: {} < {n}", buf.len());
    let n_half = n / 2 + 1;
    let n_out  = kernels.freqs_out.len();
    let start  = buf.len() - n;

    SCRATCH_X.with(|x_cell|
    SCRATCH_IN_H.with(|h_cell|
    SCRATCH_IN_T.with(|t_cell|
    SCRATCH_IN_D.with(|d_cell|
    SCRATCH_SP_H.with(|sh_cell|
    SCRATCH_SP_T.with(|st_cell|
    SCRATCH_SP_D.with(|sd_cell|
    SCRATCH_FFT.with(|fft_cell|
    SCRATCH_POW.with(|pow_cell| {
        let mut x      = x_cell.borrow_mut();
        let mut in_h   = h_cell.borrow_mut();
        let mut in_th  = t_cell.borrow_mut();
        let mut in_dh  = d_cell.borrow_mut();
        let mut sp_h   = sh_cell.borrow_mut();
        let mut sp_th  = st_cell.borrow_mut();
        let mut sp_dh  = sd_cell.borrow_mut();
        let mut fft_sc = fft_cell.borrow_mut();
        let mut acc    = pow_cell.borrow_mut();

        x.resize(n, 0.0);
        in_h.resize(n, 0.0);
        in_th.resize(n, 0.0);
        in_dh.resize(n, 0.0);
        sp_h.resize(n_half, Complex::new(0.0, 0.0));
        sp_th.resize(n_half, Complex::new(0.0, 0.0));
        sp_dh.resize(n_half, Complex::new(0.0, 0.0));
        let need_scratch = kernels.fft_plan.get_scratch_len();
        if fft_sc.len() < need_scratch {
            fft_sc.resize(need_scratch, Complex::new(0.0, 0.0));
        }
        acc.clear();
        acc.resize(n_out, 0.0);

        // f32 → f64 once per call. The inner per-window loops then read
        // from `x` (already-converted) instead of re-casting per bin.
        for (dst, &src) in x.iter_mut().zip(buf[start..].iter()) {
            *dst = src as f64;
        }
        for i in 0..n {
            in_h[i]  = x[i] * kernels.h[i];
            in_th[i] = x[i] * kernels.th[i];
            in_dh[i] = x[i] * kernels.dh[i];
        }

        kernels.fft_plan.process_with_scratch(
            &mut in_h, &mut sp_h, &mut fft_sc[..need_scratch],
        ).expect("realfft length contract");
        kernels.fft_plan.process_with_scratch(
            &mut in_th, &mut sp_th, &mut fft_sc[..need_scratch],
        ).expect("realfft length contract");
        kernels.fft_plan.process_with_scratch(
            &mut in_dh, &mut sp_dh, &mut fft_sc[..need_scratch],
        ).expect("realfft length contract");

        // Two-sided amplitude correction: real FFT returns one-sided spectrum,
        // so peak amplitude for a cosine A·cos(2πfₖt) is (A/2)·sum(h). Scale
        // by 2/sum(h) → |X_h[k]|·(2/sum(h)) = A directly.
        let amp_scale = 2.0 / kernels.h_norm;
        let amp_scale_sq = amp_scale * amp_scale;

        // First pass: find peak |X_h|² to anchor the noise gate.
        let mut peak_mag2 = 0.0_f64;
        for k in 1..(n_half - 1) {
            let m2 = sp_h[k].norm_sqr();
            if m2 > peak_mag2 { peak_mag2 = m2; }
        }
        let gate_pow = peak_mag2 * 10.0_f64.powf(DEFAULT_NOISE_FLOOR_DB as f64 / 10.0);

        // Reassign bin-by-bin.
        let sr = kernels.sample_rate as f64;
        let inv_two_pi = 1.0 / (2.0 * PI);
        let inv_n = 1.0 / n as f64;
        for k in 1..(n_half - 1) {
            let xh = sp_h[k];
            let mag2 = xh.norm_sqr();
            if mag2 < 1e-30 { continue; }
            let amp_sq = mag2 * amp_scale_sq;
            let target = if mag2 < gate_pow {
                kernels.nominal_out_idx[k]
            } else {
                let xdh = sp_dh[k];
                let im_a_conj_b = xdh.re * xh.im - xdh.im * xh.re;
                let f_k   = k as f64 * sr * inv_n;
                let f_hat = f_k + (im_a_conj_b / mag2) * sr * inv_two_pi;
                if f_hat.is_finite() {
                    out_bin_idx(f_hat as f32, &kernels.freqs_out)
                } else {
                    kernels.nominal_out_idx[k]
                }
            };
            acc[target] += amp_sq;
        }

        // ENBW-corrected amplitude² → dBFS.
        mags_out.clear();
        mags_out.reserve(n_out);
        let inv_enbw = 1.0 / kernels.enbw;
        for &p in acc.iter() {
            let amp_sq = p * inv_enbw;
            let db = if amp_sq > 1e-30 {
                10.0 * amp_sq.log10()
            } else {
                -240.0
            };
            mags_out.push(db as f32);
        }
    })))))))));
}

/// Convenience wrapper.
pub fn reassigned(buf: &[f32], kernels: &ReassignedKernels) -> Vec<f32> {
    let mut out = Vec::with_capacity(kernels.freqs_out.len());
    reassigned_into(buf, kernels, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cosine(amp: f32, freq: f32, n: usize, sr: u32) -> Vec<f32> {
        let two_pi = 2.0 * std::f32::consts::PI;
        (0..n).map(|i| amp * (two_pi * freq * i as f32 / sr as f32).cos()).collect()
    }

    fn linear_chirp(amp: f32, f_start: f32, f_end: f32, n: usize, sr: u32) -> Vec<f32> {
        // x(t) = A·cos(2π·(f_start·t + 0.5·k·t²)) with k = (f_end - f_start)/T.
        let two_pi = 2.0 * std::f32::consts::PI;
        let t_total = n as f32 / sr as f32;
        let k_chirp = (f_end - f_start) / t_total;
        (0..n).map(|i| {
            let t = i as f32 / sr as f32;
            amp * (two_pi * (f_start * t + 0.5 * k_chirp * t * t)).cos()
        }).collect()
    }

    /// Sum two cosines.
    fn two_tones(amp: f32, f1: f32, f2: f32, n: usize, sr: u32) -> Vec<f32> {
        let a = cosine(amp, f1, n, sr);
        let b = cosine(amp, f2, n, sr);
        a.iter().zip(b.iter()).map(|(x, y)| x + y).collect()
    }

    #[test]
    fn unit_cosine_calibration() {
        let sr = 48_000;
        let n  = 4096;
        let f0 = 1000.0_f32;
        let kernels = build_kernels(n, sr, 1024, 20.0, default_f_max(sr));
        let buf = cosine(0.5, f0, n, sr);
        let mags = reassigned(&buf, &kernels);
        // Find the bin with the strongest reading.
        let (peak_idx, peak_db) = mags.iter().enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap()).unwrap();
        let peak_f = kernels.freqs_out[peak_idx];
        let expected = 20.0 * 0.5_f32.log10();              // -6.02 dBFS
        assert!(
            (peak_db - expected).abs() < 1.5,
            "peak {peak_db:.2} dBFS at {peak_f} Hz, expected ≈ {expected:.2} dBFS"
        );
        assert!(
            (peak_f - f0).abs() < 50.0,
            "peak at {peak_f} Hz, expected ≈ {f0} Hz"
        );
    }

    #[test]
    fn adjacent_tones_resolved_better_than_fft() {
        // Two cosines 12 Hz apart at 48 kHz with N=4096 → FFT bin width
        // = 11.72 Hz, so the tones land in adjacent bins. Plain FFT
        // smears them into one ridge; reassignment should split them.
        let sr = 48_000;
        let n  = 4096;
        let f1 = 1000.0_f32;
        let f2 = 1012.0_f32;
        let buf = two_tones(0.25, f1, f2, n, sr);

        let kernels = build_kernels(n, sr, 2048, 20.0, default_f_max(sr));
        let mags = reassigned(&buf, &kernels);

        // Search a ±50 Hz window around the test pair for distinct peaks.
        let lo = (kernels.freqs_out.partition_point(|&f| f < 950.0)).saturating_sub(1);
        let hi = kernels.freqs_out.partition_point(|&f| f < 1060.0);
        let local: Vec<(usize, f32)> = (lo..hi).map(|i| (i, mags[i])).collect();

        // Count "peaks" = bins higher than both immediate neighbours and
        // within 12 dB of the local maximum (so we don't flag noise floor).
        let local_max = local.iter().fold(f32::NEG_INFINITY, |m, &(_, v)| m.max(v));
        let threshold = local_max - 12.0;
        let peaks: Vec<(f32, f32)> = (1..local.len() - 1).filter_map(|i| {
            let v = local[i].1;
            if v > local[i - 1].1 && v > local[i + 1].1 && v > threshold {
                Some((kernels.freqs_out[local[i].0], v))
            } else {
                None
            }
        }).collect();

        assert!(
            peaks.len() >= 2,
            "expected ≥ 2 peaks in [950, 1060] Hz, got {peaks:?}"
        );
        // The two strongest peaks should sit on either side of the midline
        // and differ by something in the ballpark of the actual 12 Hz spacing.
        let mut sorted = peaks.clone();
        sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        let dom: Vec<f32> = sorted.iter().take(2).map(|&(f, _)| f).collect();
        let split = (dom[0] - dom[1]).abs();
        assert!(
            (4.0..30.0).contains(&split),
            "two strongest peaks at {dom:?} (split {split:.1} Hz) — expected ≈ 12 Hz"
        );
    }

    #[test]
    fn chirp_ridge_sharper_than_fft() {
        // Linear chirp: classic STFT smears across many bins because the
        // frequency moves during the analysis window. Reassignment maps
        // each bin to its instantaneous frequency, concentrating energy.
        let sr = 48_000;
        let n  = 4096;
        let buf = linear_chirp(0.5, 800.0, 1600.0, n, sr);
        let kernels = build_kernels(n, sr, 2048, 20.0, default_f_max(sr));
        let mags = reassigned(&buf, &kernels);

        // Plain Hann-windowed FFT for comparison.
        let mut fft_in: Vec<f64> = (0..n).map(|i| (buf[i] as f64) * kernels.h[i]).collect();
        let mut fft_out = vec![Complex::new(0.0, 0.0); n / 2 + 1];
        let scratch_len = kernels.fft_plan.get_scratch_len();
        let mut scratch = vec![Complex::new(0.0, 0.0); scratch_len];
        kernels.fft_plan.process_with_scratch(&mut fft_in, &mut fft_out, &mut scratch).unwrap();
        let amp_scale = 2.0 / kernels.h_norm;
        let fft_db: Vec<f32> = fft_out.iter().map(|c| {
            let amp = c.norm() * amp_scale;
            if amp > 1e-12 { (20.0 * amp.log10()) as f32 } else { -240.0 }
        }).collect();

        // Compare effective bandwidth — count how many bins sit within 6 dB
        // of the peak. Reassignment should concentrate energy more tightly.
        fn bandwidth_count(spec: &[f32]) -> usize {
            let peak = spec.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
            let cutoff = peak - 6.0;
            spec.iter().filter(|&&v| v >= cutoff).count()
        }

        // Restrict to the chirp's frequency band so noise floor outside
        // doesn't pollute the ratio.
        let lo = kernels.freqs_out.partition_point(|&f| f < 700.0);
        let hi = kernels.freqs_out.partition_point(|&f| f < 1700.0);
        let reass_band = &mags[lo..hi];
        let fft_lo_k = (700.0 * n as f32 / sr as f32).round() as usize;
        let fft_hi_k = (1700.0 * n as f32 / sr as f32).round() as usize;
        let fft_band = &fft_db[fft_lo_k..fft_hi_k];

        let bw_reass = bandwidth_count(reass_band);
        let bw_fft   = bandwidth_count(fft_band);
        // FFT band has fewer bins absolute (linear vs log), so compare
        // fraction of bins within -6 dB instead. Reassignment should be
        // notably tighter relative to its grid.
        let frac_reass = bw_reass as f32 / reass_band.len() as f32;
        let frac_fft   = bw_fft   as f32 / fft_band.len()   as f32;
        assert!(
            frac_reass < frac_fft,
            "reassigned chirp ridge not sharper: \
             reass {bw_reass}/{} ({:.1}%) vs fft {bw_fft}/{} ({:.1}%)",
            reass_band.len(), frac_reass * 100.0,
            fft_band.len(),   frac_fft   * 100.0
        );
    }

    #[test]
    fn empty_input_produces_floor() {
        let sr = 48_000;
        let n  = 4096;
        let kernels = build_kernels(n, sr, 256, 20.0, default_f_max(sr));
        let buf = vec![0.0_f32; n];
        let mags = reassigned(&buf, &kernels);
        // All bins should be at the very-low floor sentinel (-240) — no
        // NaN / Inf escapes.
        assert_eq!(mags.len(), 256);
        for v in &mags {
            assert!(v.is_finite(), "got non-finite {v}");
            assert!(*v <= -200.0, "silent input produced {v}");
        }
    }

    #[test]
    fn out_bin_idx_clamps_endpoints() {
        let grid: Vec<f32> = (0..10).map(|i| 100.0 * 2.0_f32.powf(i as f32)).collect();
        assert_eq!(out_bin_idx(50.0, &grid), 0);
        assert_eq!(out_bin_idx(grid[0], &grid), 0);
        assert_eq!(out_bin_idx(grid[9], &grid), 9);
        assert_eq!(out_bin_idx(99_999.0, &grid), 9);
        // Mid-point: snap to nearest log neighbour.
        let mid = (grid[3] * grid[4]).sqrt();
        let idx = out_bin_idx(mid, &grid);
        assert!(idx == 3 || idx == 4, "got {idx}");
    }
}
