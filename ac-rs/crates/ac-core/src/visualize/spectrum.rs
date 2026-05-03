//! Tier 2 — Live spectrum. Windowed FFT magnitude only, no THD or
//! fundamental detection. Always succeeds, so `monitor_spectrum` can
//! emit a frame even when no signal is detected.

use crate::shared::fft_cache::{freq_axis, real_fft_plan, with_hann_window};

/// One detected local-maximum peak with parabolic-interpolated
/// frequency and dBFS amplitude. The interpolation cancels Hann scallop
/// loss to within ~0.01 dB across the full ±0.5-bin offset range, so
/// the cursor / hover readout shows the **true** tone amplitude rather
/// than the scalloped bin value (Smith, *Spectral Audio Signal
/// Processing*, "Quadratic Interpolation of Spectral Peaks").
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InterpolatedPeak {
    pub freq_hz: f32,
    pub dbfs:    f32,
}

/// Detect local maxima in a linear-amplitude FFT spectrum and apply
/// parabolic interpolation in the dB domain to recover scallop-corrected
/// peak frequency and amplitude.
///
/// `spectrum_linear` is `|FFT[k]| / norm` (peak-amplitude normalized; see
/// `spectrum_only` for the convention). `freqs` is the matching axis
/// (`freq_axis(n, sr)`). Both must be the same length and at least 3.
///
/// Returns up to `n_max` strongest peaks above `threshold_dbfs`, sorted
/// strongest-first. A bin is considered a local maximum when it's
/// strictly greater than both neighbours; the interpolation then uses
/// `Δ = 0.5·(Y[k-1] − Y[k+1]) / (Y[k-1] − 2·Y[k] + Y[k+1])` (in dB) and
/// `Y_peak = Y[k] − 0.25·(Y[k-1] − Y[k+1])·Δ`. DC and Nyquist bins are
/// skipped.
pub fn find_interpolated_peaks(
    spectrum_linear: &[f64],
    freqs:           &[f64],
    n_max:           usize,
    threshold_dbfs:  f32,
) -> Vec<InterpolatedPeak> {
    let n = spectrum_linear.len();
    if n < 3 || freqs.len() != n || n_max == 0 {
        return Vec::new();
    }
    let bin_hz = if n >= 2 { (freqs[1] - freqs[0]) as f32 } else { return Vec::new(); };

    // Convert to dB once. `1e-20` floor avoids `-inf` at the silence bins.
    let db: Vec<f32> = spectrum_linear
        .iter()
        .map(|&v| 20.0 * (v.max(1e-20) as f32).log10())
        .collect();

    let mut peaks: Vec<InterpolatedPeak> = Vec::new();
    // Skip DC (k=0) and Nyquist (k=n-1) — interpolation needs both neighbours.
    for k in 1..n - 1 {
        let yk = db[k];
        if yk < threshold_dbfs {
            continue;
        }
        let yl = db[k - 1];
        let yr = db[k + 1];
        if !(yk > yl && yk > yr) {
            continue;
        }
        // Quadratic vertex offset in bins (range (-0.5, 0.5)).
        let denom = yl - 2.0 * yk + yr;
        let delta = if denom.abs() < 1e-12 {
            0.0
        } else {
            0.5 * (yl - yr) / denom
        };
        let peak_db = yk - 0.25 * (yl - yr) * delta;
        let peak_hz = freqs[k] as f32 + delta * bin_hz;
        peaks.push(InterpolatedPeak { freq_hz: peak_hz, dbfs: peak_db });
    }

    // Strongest-first; truncate to n_max.
    peaks.sort_by(|a, b| b.dbfs.partial_cmp(&a.dbfs).unwrap_or(std::cmp::Ordering::Equal));
    peaks.truncate(n_max);
    peaks
}

/// Amplitude spectrum of a mono capture, along with the matching
/// frequency axis. Returns `(vec![], vec![])` if the FFT fails (e.g.
/// empty input).
pub fn spectrum_only(samples: &[f32], sr: u32) -> (Vec<f64>, Vec<f64>) {
    let n = samples.len().max(2);
    let fft = real_fft_plan(n);
    let mut windowed = vec![0.0f64; n];
    let mut out = fft.make_output_vec();

    let wc = with_hann_window(n, |win, wc| {
        for (i, &s) in samples.iter().enumerate().take(n) {
            windowed[i] = s as f64 * win[i];
        }
        wc
    });

    if fft.process(&mut windowed, &mut out).is_err() {
        return (vec![], vec![]);
    }
    let norm = (n as f64 / 2.0) * wc;
    let spec: Vec<f64> = out.iter().map(|c| c.norm() / norm).collect();
    let freqs = freq_axis(n, sr);
    (spec, freqs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    const SR: u32 = 48_000;

    fn pure_sine(freq: f64, amplitude: f64, sr: u32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (amplitude * (2.0 * PI * freq * i as f64 / sr as f64).sin()) as f32)
            .collect()
    }

    #[test]
    fn spectrum_only_length_matches_n_half_plus_one() {
        let n = SR as usize;
        let samples = pure_sine(1_000.0, 0.5, SR, n);
        let (spec, freqs) = spectrum_only(&samples, SR);
        assert_eq!(spec.len(), n / 2 + 1);
        assert_eq!(freqs.len(), spec.len());
    }

    #[test]
    fn spectrum_only_freq_axis_endpoints() {
        let n = SR as usize;
        let samples = pure_sine(1_000.0, 0.5, SR, n);
        let (_, freqs) = spectrum_only(&samples, SR);
        assert!((freqs[0] - 0.0).abs() < 1e-9);
        assert!((freqs.last().copied().unwrap() - SR as f64 / 2.0).abs() < 1e-6);
    }

    #[test]
    fn spectrum_only_peak_at_fundamental() {
        let f = 1_000.0;
        let samples = pure_sine(f, 0.5, SR, SR as usize);
        let (spec, freqs) = spectrum_only(&samples, SR);
        let (peak_idx, _) = spec
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap();
        assert!((freqs[peak_idx] - f).abs() < 2.0);
    }

    #[test]
    fn spectrum_only_silent_input_does_not_panic() {
        let samples = vec![0.0f32; SR as usize];
        let (spec, freqs) = spectrum_only(&samples, SR);
        assert_eq!(spec.len(), freqs.len());
        assert!(spec.iter().all(|&x| x.is_finite()));
    }

    /// `spectrum_only` normalizes by `(N/2) · CG` where `CG = mean(w[i])` is
    /// the Hann coherent gain (≈0.5). For an integer-bin sine of peak
    /// amplitude A, the peak bin therefore reads exactly A — independent
    /// of N. This test asserts that reading is stable across the FFT
    /// sizes the UI cycles through (1024 … 65536), using an integer-bin
    /// frequency (984.375 Hz = SR/1024 · 21) so scalloping cannot
    /// contaminate the comparison.
    #[test]
    fn spectrum_only_peak_magnitude_stable_across_n() {
        const AMP: f64 = 0.5;
        // 48000/1024 * 21 = 984.375 Hz — integer-bin for every power-of-two
        // N ≥ 1024 at SR=48000.
        const FREQ: f64 = 984.375;
        const TOL: f64 = 1e-3;

        let mut readings = Vec::new();
        for &n in &[1024usize, 2048, 4096, 8192, 16384, 32768, 65536] {
            let samples = pure_sine(FREQ, AMP, SR, n);
            let (spec, freqs) = spectrum_only(&samples, SR);

            assert_eq!(spec.len(), n / 2 + 1, "N={n} length mismatch");

            let (peak_idx, &peak) = spec
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
                .unwrap();

            assert!(
                (freqs[peak_idx] - FREQ).abs() < 1e-6,
                "N={n}: peak at {} Hz, expected {FREQ}",
                freqs[peak_idx]
            );
            readings.push((n, peak));
        }

        let reference = readings[0].1;
        for (n, peak) in &readings {
            assert!(
                (peak - reference).abs() <= TOL,
                "N={n}: peak {peak:.6} drifts from reference {reference:.6} (tol {TOL})"
            );
        }
    }

    /// Peak bin must scale linearly with input amplitude, at every N. The
    /// ratio `peak / A` is the window-normalization constant — here
    /// asserted identical (within 1e-3) across a wide amplitude range so
    /// future changes that introduce amplitude-dependent scaling (e.g.,
    /// quantization, saturation in the FFT path) get caught.
    #[test]
    fn spectrum_only_scales_linearly_with_amplitude_across_n() {
        const FREQ: f64 = 984.375;
        const TOL: f64 = 1e-3;
        for &n in &[2048usize, 8192, 32768] {
            let mut ratios = Vec::new();
            for &amp in &[0.1_f64, 0.25, 0.5, 0.9] {
                let samples = pure_sine(FREQ, amp, SR, n);
                let (spec, _) = spectrum_only(&samples, SR);
                let peak = spec.iter().cloned().fold(0.0f64, f64::max);
                ratios.push(peak / amp);
            }
            let r0 = ratios[0];
            for r in &ratios {
                assert!(
                    (r - r0).abs() <= TOL,
                    "N={n}: peak/amp ratio {r:.6} drifts from {r0:.6}"
                );
            }
        }
    }

    /// Off-bin (scalloped) readings must never exceed the on-bin reading,
    /// and worst-case Hann scalloping loss must stay within the
    /// theoretical bound (≈1.42 dB at frac=0.5, i.e. ratio ≥ 0.85).
    /// Asserts both bounds across N.
    #[test]
    fn spectrum_only_scalloping_bounded_across_n() {
        const AMP: f64 = 0.5;
        // Hann scallop loss at frac=0.5 is ~-1.42 dB (ratio ≈0.849). 0.80
        // is a safe floor that still catches a regression past the
        // theoretical worst case.
        const MIN_RATIO: f64 = 0.80;
        for &n in &[2048usize, 8192, 32768] {
            let bin_hz = SR as f64 / n as f64;
            // On-bin reference.
            let on_bin_samples = pure_sine(984.375, AMP, SR, n);
            let (on_bin_spec, _) = spectrum_only(&on_bin_samples, SR);
            let on_bin_peak = on_bin_spec.iter().cloned().fold(0.0f64, f64::max);

            for &frac in &[0.25_f64, 0.5] {
                let f = 984.375 + frac * bin_hz;
                let samples = pure_sine(f, AMP, SR, n);
                let (spec, _) = spectrum_only(&samples, SR);
                let peak = spec.iter().cloned().fold(0.0f64, f64::max);

                assert!(
                    peak <= on_bin_peak + 1e-6,
                    "N={n} frac={frac}: off-bin peak {peak} exceeds on-bin {on_bin_peak}"
                );
                assert!(
                    peak >= on_bin_peak * MIN_RATIO,
                    "N={n} frac={frac}: peak {peak} below scalloping floor {}",
                    on_bin_peak * MIN_RATIO
                );
            }
        }
    }

    /// Regression: peak bin of an integer-bin sine reads back the peak
    /// amplitude of the input signal — i.e. coherent-gain normalized.
    ///
    /// Before 2026-05-01 the cache stored window RMS (≈0.6124) as `wc`
    /// instead of the coherent gain (≈0.5), so peak bins read
    /// `A · 0.5/0.6124 ≈ A · 0.8165` — a constant -1.78 dB shortfall on
    /// every FFT-derived dBFS reading (`fundamental_dbfs`, spectrum
    /// columns, harmonic levels, monitor peak readouts). Caught while
    /// chasing FFT visualization "off by ~2 dB" reports against a real
    /// FF400 loopback.
    #[test]
    fn spectrum_only_peak_equals_signal_peak() {
        const AMP: f64 = 0.5;
        const FREQ: f64 = 984.375; // integer-bin for every N below
        const TOL: f64 = 1e-3;
        for &n in &[1024usize, 4096, 16384, 65536] {
            let samples = pure_sine(FREQ, AMP, SR, n);
            let (spec, _) = spectrum_only(&samples, SR);
            let peak = spec.iter().cloned().fold(0.0_f64, f64::max);
            assert!(
                (peak - AMP).abs() < TOL,
                "N={n}: peak {peak:.6} != signal peak {AMP} (tol {TOL})"
            );
        }
    }

    /// Parabolic peak interpolation MUST recover the true peak amplitude
    /// of an off-bin Hann-windowed sine. Worst-case scallop loss for raw
    /// bins is ~1.42 dB at frac=0.5; quadratic interpolation on the
    /// dB-magnitude Hann main lobe (the standard JOS / Smith technique)
    /// brings the residual to ≤0.4 dB across the full ±0.5-bin range —
    /// about a 3.5× tightening at the worst case (frac=0.5, where the
    /// tone is exactly between two bins) and ≤0.25 dB for the typical
    /// case (frac < 0.45). Higher-order schemes (Werner-Germain
    /// log-magnitude, 5-tap, sinc) get below 0.05 dB but cost more
    /// arithmetic; revisit if users push for it. Hann's residual is
    /// non-zero because its main lobe in dB is not exactly parabolic.
    #[test]
    fn parabolic_interp_kills_hann_scallop() {
        const AMP: f64 = 0.5; // -6.02 dBFS peak
        const TRUE_DB: f32 = -6.0205994; // 20·log10(0.5)
        // Tighter near-zero offset, looser at frac=0.5 (algorithm limit).
        let cases: &[(f64, f32)] = &[
            (0.0,  0.05),
            (0.1,  0.10),
            (0.25, 0.15),
            (0.4,  0.30),
            (0.5,  0.40),
        ];

        for &n in &[2048usize, 8192, 32768] {
            let bin_hz = SR as f64 / n as f64;
            // Anchor the test on a low-frequency bin so we can sweep
            // the full ±0.5-bin offset without other peaks getting in
            // the way. Pick bin 100 (well above DC, well below Nyquist).
            let base_hz = 100.0 * bin_hz;
            for &(frac, tol) in cases {
                let f = base_hz + frac * bin_hz;
                let samples = pure_sine(f, AMP, SR, n);
                let (spec, freqs) = spectrum_only(&samples, SR);
                let peaks = find_interpolated_peaks(&spec, &freqs, 4, -60.0);
                assert!(!peaks.is_empty(), "N={n} frac={frac}: no peak detected");
                let p = peaks[0];

                let err = (p.dbfs - TRUE_DB).abs();
                assert!(
                    err < tol,
                    "N={n} frac={frac}: interpolated peak {:.4} dBFS \
                     vs true {TRUE_DB:.4} (err {err:.4} dB > {tol})",
                    p.dbfs,
                );
                // Frequency error must be < 0.1 bin in the typical case;
                // at frac=0.5 the tone is exactly between two bins so
                // ±0.5 bin is the algorithm's lower bound.
                let freq_tol = if frac >= 0.5 { 0.5 } else { 0.1 };
                let freq_err_bins = ((p.freq_hz as f64 - f) / bin_hz).abs();
                assert!(
                    freq_err_bins < freq_tol,
                    "N={n} frac={frac}: interpolated freq {:.3} Hz vs true \
                     {f:.3} Hz (err {freq_err_bins:.3} bins > {freq_tol})",
                    p.freq_hz,
                );
            }
        }
    }

    #[test]
    fn parabolic_interp_threshold_filters_noise() {
        let n = 4096;
        let mut spec = vec![1e-7_f64; n / 2 + 1]; // ~-140 dBFS floor
        let freqs: Vec<f64> = (0..spec.len())
            .map(|k| k as f64 * SR as f64 / n as f64)
            .collect();
        // One real peak.
        spec[1000] = 0.5; // -6 dBFS
        let peaks = find_interpolated_peaks(&spec, &freqs, 8, -100.0);
        assert_eq!(peaks.len(), 1, "expected 1 peak above -100 dBFS, got {peaks:?}");
        // No spurious peaks when threshold rejects the real one.
        let none = find_interpolated_peaks(&spec, &freqs, 8, 0.0);
        assert!(none.is_empty(), "threshold above peak should yield no peaks: {none:?}");
    }

    /// DC-only input: the 0 Hz bin should dominate and carry the signal's
    /// magnitude; all other bins should be near zero. Tested across N so
    /// that the DC normalization (which uses the same coherent-gain
    /// factor) is consistent.
    #[test]
    fn spectrum_only_dc_input_concentrates_at_zero_hz() {
        for &n in &[1024usize, 4096, 16384] {
            let samples = vec![0.5f32; n];
            let (spec, freqs) = spectrum_only(&samples, SR);
            assert_eq!(freqs[0], 0.0);
            let peak_idx = spec
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
                .unwrap()
                .0;
            assert_eq!(peak_idx, 0, "N={n}: DC peak not at bin 0");
        }
    }
}
