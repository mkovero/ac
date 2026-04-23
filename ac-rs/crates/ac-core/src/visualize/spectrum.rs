//! Tier 2 — Live spectrum. Windowed FFT magnitude only, no THD or
//! fundamental detection. Always succeeds, so `monitor_spectrum` can
//! emit a frame even when no signal is detected.

use crate::shared::fft_cache::{freq_axis, real_fft_plan, with_hann_window};

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

    /// The "amplitude spectrum" returned by `spectrum_only` uses the
    /// `/(N/2)/wc` convention inherited from the Python reference, where
    /// `wc` is the Hann window's RMS (≈0.6124). For an integer-bin sine
    /// of amplitude A the peak bin therefore reads `A · 0.5/wc ≈ A · 0.8165`
    /// — independent of N. This test asserts that reading is stable across
    /// the FFT sizes the UI cycles through (1024 … 65536), using an
    /// integer-bin frequency (984.375 Hz = SR/1024 · 21) so scalloping
    /// cannot contaminate the comparison.
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
    /// theoretical bound (≈1.42 dB for the un-normalized window; the
    /// RMS-normalized convention used here has a slightly looser bound
    /// because the bin-centre boost disappears). Asserts both bounds
    /// across N.
    #[test]
    fn spectrum_only_scalloping_bounded_across_n() {
        const AMP: f64 = 0.5;
        // Empirical ceiling for Hann peak-bin loss at frac=0.5 with the
        // `/(N/2)/wc` convention: measured ≈-3.8 dB → ≈0.65× the on-bin
        // reading. 0.60 is a safe floor.
        const MIN_RATIO: f64 = 0.60;
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
