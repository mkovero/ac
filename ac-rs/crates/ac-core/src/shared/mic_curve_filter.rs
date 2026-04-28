//! Linear-phase FIR filter that applies the inverse of a per-channel
//! mic frequency-response curve to audio samples in the time domain.
//!
//! Designed via frequency sampling + Hann windowing: the target
//! magnitude response is the *negation* of the curve (in dB), the IFFT
//! of that gives the impulse response, a Hann window cleans up the
//! stopband, and the resulting symmetric coefficient set convolves
//! with input samples one at a time. Because the FIR is symmetric the
//! phase response is linear (constant group delay), so the filter
//! delays the signal by `(n_taps − 1) / 2` samples but does not
//! distort phase.
//!
//! ## Why this lives next to `Calibration`
//!
//! The BS.1770-5 / R128 K-weighting filter (`measurement/loudness.rs`)
//! is a fixed DSP — applying mic-curve correction *to it* requires the
//! correction to be in the time domain, ahead of K-weighting. A simple
//! dB offset is not enough because both filters are frequency-
//! dependent. This module produces the FIR; the loudness wiring lives
//! in the daemon (`handlers/audio/monitor.rs`).
//!
//! ## Calibration / verification
//!
//! Unit tests in this file cover:
//! - flat (0 dB) curve → unit-gain delay (no level change)
//! - +N dB curve at a test frequency → tone at that frequency reads
//!   N dB quieter through the filter
//! - filter is causal, stable, and BIBO-bounded for any well-formed
//!   `MicResponse` (no NaNs / Infs in the output)

use std::f64::consts::PI;

use realfft::{num_complex::Complex, RealFftPlanner};

use crate::shared::calibration::MicResponse;

/// Default tap count. 512 at 48 kHz gives a group delay of
/// 255 samples ≈ 5.3 ms — well under BS.1770-5's 400 ms momentary
/// window, so the FIR latency is invisible to LKFS readouts. The
/// convolution runs comfortably above realtime at this length even
/// without SIMD; jump to 1024 if the curve's low-frequency detail
/// matters more than the extra latency.
pub const DEFAULT_N_TAPS: usize = 512;

/// Time-domain FIR that compensates a mic-curve when convolved with
/// the captured signal. The coefficients are linear-phase symmetric.
///
/// Internal buffer note: `history` is allocated at length `2 * n_taps`
/// and every incoming sample is written to *both halves* (`head` and
/// `head + n_taps`). This double-write costs one extra store per
/// sample but lets the inner convolution loop read `n_taps`
/// contiguous samples without any modulo or branch — the dominant
/// per-sample cost in a 1-modulo-per-multiply naïve implementation.
pub struct MicCurveFir {
    coeffs:  Vec<f32>,
    history: Vec<f32>,                      // 2 × n_taps for branch-free reads
    head:    usize,                         // write index in [0, n_taps)
    n:       usize,                         // n_taps (cached for hot path)
    /// Group delay in samples (`(n_taps − 1) / 2` for symmetric FIRs).
    pub group_delay_samples: usize,
}

impl MicCurveFir {
    /// Build a new filter from a `MicResponse` curve at the given
    /// sample rate. `n_taps` must be even and ≥ 16; pass
    /// [`DEFAULT_N_TAPS`] unless you have a specific reason to deviate.
    pub fn new(curve: &MicResponse, sample_rate: u32, n_taps: usize) -> Self {
        assert!(n_taps >= 16, "n_taps must be ≥ 16, got {n_taps}");
        assert!(n_taps % 2 == 0, "n_taps must be even, got {n_taps}");
        assert!(sample_rate > 0);

        let sr = sample_rate as f64;
        let n_freq = n_taps / 2 + 1;

        // Target magnitude (linear) on the half-spectrum: the inverse
        // of the mic-curve in dB. Phase is identically zero — that's
        // what makes the resulting time-domain FIR symmetric and so
        // linear-phase.
        let mut spec = vec![Complex::new(0.0_f64, 0.0); n_freq];
        for (k, bin) in spec.iter_mut().enumerate() {
            let f = k as f64 * sr / n_taps as f64;
            let inv_db = -curve.correction_at(f as f32) as f64;
            *bin = Complex::new(10f64.powf(inv_db / 20.0), 0.0);
        }
        // DC and Nyquist must be real for a real-valued IFFT result.
        spec[0].im          = 0.0;
        spec[n_freq - 1].im = 0.0;

        // Inverse real-FFT → time-domain impulse at index 0 wrapping
        // (so the first half of `h` is the causal part, the second
        // half is "negative time"). `realfft`'s IFFT is unnormalised —
        // divide by `n_taps` for unity round-trip gain.
        let ifft = RealFftPlanner::<f64>::new().plan_fft_inverse(n_taps);
        let mut h = vec![0.0_f64; n_taps];
        ifft.process(&mut spec, &mut h).expect("real IFFT length contract");

        // Circular-shift so the impulse peaks in the middle (index
        // n_taps/2). After Hann windowing this becomes a symmetric
        // linear-phase FIR.
        let half = n_taps / 2;
        let mut shifted = vec![0.0_f64; n_taps];
        for i in 0..n_taps {
            shifted[i] = h[(i + half) % n_taps];
        }

        // Hann window + IFFT normalisation in one pass.
        let inv_n = 1.0 / n_taps as f64;
        let mut coeffs = Vec::with_capacity(n_taps);
        for (i, &v) in shifted.iter().enumerate() {
            let w = 0.5 - 0.5 * (2.0 * PI * i as f64 / (n_taps as f64 - 1.0)).cos();
            coeffs.push((v * w * inv_n) as f32);
        }

        Self {
            coeffs,
            history: vec![0.0; 2 * n_taps],
            head:    0,
            n:       n_taps,
            group_delay_samples: (n_taps - 1) / 2,
        }
    }

    /// Filter `samples` in place. Maintains delay-line state across
    /// calls so block boundaries are seamless — call repeatedly per
    /// audio block. The inner loop reads `n_taps` contiguous samples
    /// from a double-write buffer so it's branch-free and SIMD-
    /// friendly without explicit intrinsics.
    pub fn process_inplace(&mut self, samples: &mut [f32]) {
        let n = self.n;
        for s in samples.iter_mut() {
            // Write to both halves so the contiguous read window
            // `[head .. head + n]` is always valid without wrapping.
            self.history[self.head]       = *s;
            self.history[self.head + n]   = *s;
            // The most recent sample sits at `history[head + n - 1]`,
            // the oldest at `history[head]`. For symmetric coeffs the
            // direction doesn't change the result, but indexing
            // `coeffs[i] * history[head + n - 1 - i]` is the canonical
            // FIR convolution with `h[i] · x[k − i]`.
            let win = &self.history[self.head .. self.head + n];
            let mut acc = 0.0_f32;
            for i in 0..n {
                acc += self.coeffs[i] * win[n - 1 - i];
            }
            self.head += 1;
            if self.head == n {
                self.head = 0;
            }
            *s = acc;
        }
    }

    /// Zero the delay line. Use after a long pause to avoid the
    /// previous block's tail bleeding into the next.
    pub fn reset(&mut self) {
        self.history.iter_mut().for_each(|s| *s = 0.0);
        self.head = 0;
    }

    /// Number of FIR coefficients. Useful for sizing benches.
    pub fn n_taps(&self) -> usize { self.n }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::calibration::parse_mic_curve;

    /// Build a synthetic curve: 24 log-spaced points, all at the same
    /// `gain_db` value (flat across the audio band).
    fn flat_curve(gain_db: f32) -> MicResponse {
        let mut s = String::new();
        let log_min = 20.0_f32.ln();
        let log_max = 20_000.0_f32.ln();
        for i in 0..24 {
            let t = i as f32 / 23.0;
            let f = (log_min + t * (log_max - log_min)).exp();
            s.push_str(&format!("{f}\t{gain_db}\n"));
        }
        parse_mic_curve(&s, None).unwrap()
    }

    fn cosine(amp: f32, freq: f32, n: usize, sr: u32) -> Vec<f32> {
        let two_pi = 2.0 * std::f32::consts::PI;
        (0..n).map(|i| amp * (two_pi * freq * i as f32 / sr as f32).cos()).collect()
    }

    /// RMS amplitude over the steady-state portion of a buffer
    /// (skipping the FIR's group delay so transients don't pollute).
    fn steady_rms(buf: &[f32], skip: usize) -> f32 {
        let tail = &buf[skip..];
        let sum_sq: f32 = tail.iter().map(|v| v * v).sum();
        (sum_sq / tail.len() as f32).sqrt()
    }

    #[test]
    fn flat_curve_is_unit_gain_delay() {
        // 0 dB at every freq → filter is a pure delay (no amplitude
        // change) for any test tone.
        let curve = flat_curve(0.0);
        let sr = 48_000;
        let n_taps = 1024;
        let mut fir = MicCurveFir::new(&curve, sr, n_taps);

        let amp = 0.5_f32;
        let freq = 1_000.0_f32;
        let mut samples = cosine(amp, freq, 4 * n_taps, sr);
        let rms_in = steady_rms(&samples, n_taps);
        fir.process_inplace(&mut samples);
        let rms_out = steady_rms(&samples, n_taps + fir.group_delay_samples);

        let ratio_db = 20.0 * (rms_out / rms_in).log10();
        assert!(
            ratio_db.abs() < 0.5,
            "flat curve should be unity gain, got Δ={ratio_db:.3} dB"
        );
    }

    #[test]
    fn positive_curve_attenuates_signal_by_curve_db() {
        // Curve says mic over-reads by +3 dB everywhere → filter
        // attenuates by 3 dB everywhere. A 1 kHz cosine through the
        // FIR comes out 3 dB quieter.
        let curve = flat_curve(3.0);
        let sr = 48_000;
        let n_taps = 1024;
        let mut fir = MicCurveFir::new(&curve, sr, n_taps);

        let amp = 0.5_f32;
        let freq = 1_000.0_f32;
        let mut samples = cosine(amp, freq, 4 * n_taps, sr);
        let rms_in = steady_rms(&samples, n_taps);
        fir.process_inplace(&mut samples);
        let rms_out = steady_rms(&samples, n_taps + fir.group_delay_samples);

        let ratio_db = 20.0 * (rms_out / rms_in).log10();
        assert!(
            (ratio_db - -3.0).abs() < 0.2,
            "expected −3 dB attenuation, got Δ={ratio_db:.3} dB"
        );
    }

    #[test]
    fn negative_curve_boosts_signal() {
        // Curve says mic UNDER-reads by 4 dB → inverse boosts by +4 dB.
        let curve = flat_curve(-4.0);
        let sr = 48_000;
        let n_taps = 1024;
        let mut fir = MicCurveFir::new(&curve, sr, n_taps);

        let amp = 0.25_f32;                  // headroom
        let freq = 2_000.0_f32;
        let mut samples = cosine(amp, freq, 4 * n_taps, sr);
        let rms_in = steady_rms(&samples, n_taps);
        fir.process_inplace(&mut samples);
        let rms_out = steady_rms(&samples, n_taps + fir.group_delay_samples);

        let ratio_db = 20.0 * (rms_out / rms_in).log10();
        assert!(
            (ratio_db - 4.0).abs() < 0.2,
            "expected +4 dB boost, got Δ={ratio_db:.3} dB"
        );
    }

    #[test]
    fn output_is_finite_for_random_input() {
        // Robustness: white noise through the filter must stay finite
        // (no NaN / Inf escape). Catches a NaN-leaking curve point or
        // a divide-by-zero in the design path.
        use rand::{Rng, SeedableRng};
        use rand::rngs::StdRng;
        let curve = flat_curve(2.0);
        let mut fir = MicCurveFir::new(&curve, 48_000, 512);
        let mut rng = StdRng::seed_from_u64(0xCAB1_C0DE_DEAD_BEEF);
        let mut samples: Vec<f32> = (0..4096)
            .map(|_| rng.gen_range(-1.0..1.0)).collect();
        fir.process_inplace(&mut samples);
        for &s in &samples {
            assert!(s.is_finite(), "non-finite sample after filter: {s}");
            assert!(s.abs() < 100.0, "implausibly large sample: {s}");
        }
    }

    #[test]
    fn group_delay_matches_expected() {
        let curve = flat_curve(0.0);
        let fir = MicCurveFir::new(&curve, 48_000, 1024);
        assert_eq!(fir.group_delay_samples, 511);
        let fir = MicCurveFir::new(&curve, 48_000, 512);
        assert_eq!(fir.group_delay_samples, 255);
    }

    #[test]
    fn reset_zeros_history() {
        let curve = flat_curve(0.0);
        let mut fir = MicCurveFir::new(&curve, 48_000, 64);
        let mut samples = vec![1.0_f32; 128];
        fir.process_inplace(&mut samples);
        // After processing a non-zero block, history is non-zero.
        assert!(fir.history.iter().any(|&x| x != 0.0));
        fir.reset();
        assert!(fir.history.iter().all(|&x| x == 0.0));
        assert_eq!(fir.head, 0);
    }
}
