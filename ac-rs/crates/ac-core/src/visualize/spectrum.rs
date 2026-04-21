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
}
