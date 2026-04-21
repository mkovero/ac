//! Signal generation — sine and pink noise.
//!
//! Returns `Vec<f32>` sample buffers ready to hand to the audio backend.
//! No audio I/O here; this is pure math so it can be unit-tested without
//! any hardware.

use std::f64::consts::PI;

/// Generate one buffer of a sine wave.
///
/// - `freq`: frequency in Hz
/// - `amplitude`: peak amplitude (0.0–1.0 for full scale)
/// - `sample_rate`: samples per second
/// - `n_samples`: length of the returned buffer
///
/// The buffer is suitable for looping (starts at phase 0).
pub fn generate_sine(freq: f64, amplitude: f64, sample_rate: u32, n_samples: usize) -> Vec<f32> {
    (0..n_samples)
        .map(|i| {
            let t = i as f64 / sample_rate as f64;
            (amplitude * (2.0 * PI * freq * t).sin()) as f32
        })
        .collect()
}

/// Generate a cycle-aligned 1-second sine buffer at the given frequency.
///
/// Mirrors `JackEngine::set_tone` in the Python server: the output buffer is
/// exactly `sample_rate` samples long so the loop point is phase-continuous
/// as long as the JACK block size divides `sample_rate`.
pub fn generate_sine_1s(freq: f64, amplitude: f64, sample_rate: u32) -> Vec<f32> {
    generate_sine(freq, amplitude, sample_rate, sample_rate as usize)
}

/// dBFS peak amplitude → linear peak amplitude (0 dBFS = 1.0).
pub fn dbfs_to_amplitude(dbfs: f64) -> f64 {
    10.0_f64.powf(dbfs / 20.0)
}

/// Generate a pink noise buffer of length `4 * sample_rate` samples,
/// normalised to the requested amplitude (peak-referenced, like a sine).
///
/// Algorithm: white noise → FFT → 1/√f shaping → band-limit to ≥ 20 Hz →
/// IFFT → RMS-normalise.  Matches `JackEngine::set_pink_noise` in Python.
pub fn generate_pink_noise(amplitude: f64, sample_rate: u32) -> Vec<f32> {
    use rand::Rng;
    use rand_distr::StandardNormal;
    use realfft::RealFftPlanner;

    let n = 4 * sample_rate as usize;
    let bin_hz = sample_rate as f64 / n as f64;
    let mut rng = rand::thread_rng();

    // White noise
    let mut white: Vec<f64> = (0..n).map(|_| rng.sample(StandardNormal)).collect();

    // Forward FFT
    let mut planner = RealFftPlanner::<f64>::new();
    let fft_fwd = planner.plan_fft_forward(n);
    let mut spectrum = fft_fwd.make_output_vec();
    fft_fwd.process(&mut white, &mut spectrum).unwrap();

    // 1/√f shaping; zero out sub-20 Hz bins (DC + very low freq).
    for (k, s) in spectrum.iter_mut().enumerate() {
        let freq = if k == 0 { 1.0 } else { k as f64 * bin_hz };
        if freq < 20.0 {
            s.re = 0.0;
            s.im = 0.0;
        } else {
            let scale = 1.0 / freq.sqrt();
            s.re *= scale;
            s.im *= scale;
        }
    }

    // Inverse FFT
    let fft_inv = planner.plan_fft_inverse(n);
    let mut pink_f64 = fft_inv.make_output_vec();
    fft_inv.process(&mut spectrum, &mut pink_f64).unwrap();

    // realfft inverse does not normalise — divide by N.
    pink_f64.iter_mut().for_each(|x| *x /= n as f64);

    // Amplitude normalisation: amplitude is peak-referenced (same as sine).
    // Pink RMS target = amplitude / √2.
    let rms = (pink_f64.iter().map(|x| x * x).sum::<f64>() / n as f64).sqrt();
    if rms > 0.0 {
        let scale = amplitude / (rms * std::f64::consts::SQRT_2);
        pink_f64.iter_mut().for_each(|x| *x *= scale);
    }

    pink_f64.iter().map(|&x| x as f32).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;

    #[test]
    fn sine_rms_matches_amplitude() {
        let sr = 48_000u32;
        let freq = 1_000.0f64;
        let amp = 0.5f64;
        let buf = generate_sine_1s(freq, amp, sr);

        let rms = (buf.iter().map(|x| (*x as f64).powi(2)).sum::<f64>() / buf.len() as f64).sqrt();
        let expected_rms = amp / std::f64::consts::SQRT_2;
        assert_relative_eq!(rms, expected_rms, epsilon = 1e-4);
    }

    #[test]
    fn sine_starts_at_zero() {
        let buf = generate_sine_1s(1000.0, 1.0, 48_000);
        assert!(buf[0].abs() < 1e-6);
    }

    #[test]
    fn dbfs_to_amplitude_unity() {
        assert_relative_eq!(dbfs_to_amplitude(0.0), 1.0, epsilon = 1e-12);
        assert_relative_eq!(dbfs_to_amplitude(-6.0), 10.0_f64.powf(-0.3), epsilon = 1e-10);
    }

    #[test]
    fn pink_noise_length_and_rms() {
        let sr = 48_000u32;
        let amp = 0.5f64;
        let buf = generate_pink_noise(amp, sr);

        assert_eq!(buf.len(), 4 * sr as usize);

        // RMS should be ≈ amplitude / √2 (within 10% — random noise)
        let rms = (buf.iter().map(|x| (*x as f64).powi(2)).sum::<f64>() / buf.len() as f64).sqrt();
        let expected = amp / std::f64::consts::SQRT_2;
        assert!((rms - expected).abs() / expected < 0.1,
            "pink noise RMS {rms:.4} too far from {expected:.4}");
    }

    #[test]
    fn pink_noise_crest_factor_reasonable() {
        // Pink noise has high crest factor (~4–6 dB above RMS).
        // With amplitude=0.5, RMS ≈ 0.354; peaks of several × RMS are normal.
        // We just check nothing has gone catastrophically wrong (e.g. no NaNs,
        // and peaks are within a physically plausible range).
        let buf = generate_pink_noise(0.5, 48_000);
        assert!(buf.iter().all(|x| x.is_finite()), "pink noise contains NaN/inf");
        let peak = buf.iter().map(|x| x.abs()).fold(0.0f32, f32::max);
        assert!(peak < 10.0, "pink noise peak {peak} is implausibly large");
    }
}
