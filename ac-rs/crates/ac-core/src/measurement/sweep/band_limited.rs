//! The arrival of a deconvolved impulse response read off the IR
//! high-passed at [`ARRIVAL_HIGH_PASS_CORNER_HZ`] (#537).
//!
//! A room mode can put the broadband maximum of a speaker capture well
//! after the direct sound: on pupu at 2 m a ~55 Hz mode's first swing sat
//! about 16 ms after the direct sound and 24 dB above the direct HF, so
//! `argmax |h|` ([`super::ir_peak`]) read a stable, plausible and wrong
//! arrival. High-passing the IR before picking removes such a component
//! (the mode sits more than four octaves below the corner) while leaving
//! the direct sound's HF edge in place.
//!
//! The filter is zero-phase: a 4th-order Butterworth high-pass run
//! forward over the IR, then forward over the time-reversed result, so the
//! phase of the two passes cancels and the magnitude is squared (−6 dB at
//! the corner). ISO 3382-1:2009 §A.3.4 warns that a filtered IR's start
//! carries the filter's delay, and allows determining the start "from the
//! broadband or high frequency impulse responses and the measured delay of
//! the filters". A zero-phase filter makes that delay zero, so the
//! band-limited peak of a pure delay lands on the same sample as the
//! broadband peak, and it pairs with a peak-picked τ exactly as
//! [`super::ir_peak`] does (see `peak.rs`'s pairing rule and its test).

use crate::measurement::filterbank::{apply_df2t, Biquad};

use super::ir_peak;

/// Corner of the high-pass the arrival is picked through, in Hz.
///
/// Provenance: measured. On pupu (2026-09-18, Genelec 1083 at 2 m) a
/// 1–10 kHz sweep read the arrival at +634/+635 samples against about 598
/// expected for 2 m, while the default 20 Hz–20 kHz sweep's broadband peak
/// read +2137/+2138 — the maximum of a ~55 Hz room mode, more than four
/// octaves below this corner.
pub const ARRIVAL_HIGH_PASS_CORNER_HZ: f64 = 1000.0;

/// The band-limited arrival needs the stimulus to reach at least this
/// multiple of the corner — one octave above it. Below that the high-passed
/// IR holds little of the sweep's own energy, and the arrival falls back to
/// the broadband peak.
///
/// Provenance: assumed (#537 architect decision).
pub const BAND_LIMIT_MIN_F2_RATIO: f64 = 2.0;

/// Butterworth Q of each biquad section of a 4th-order design:
/// `1 / (2·sin((2k − 1)·π / 8))` for `k = 1, 2`.
const BUTTERWORTH_4_Q: [f64; 2] = [1.306_562_964_876_376_7, 0.541_196_100_146_197];

/// Settle length of the padding at each end, in periods of the corner.
/// The slowest section (Q ≈ 1.31) decays with a time constant of
/// `2Q / (2π·f_c)` ≈ 0.42 corner periods, so ten periods leave a start-up
/// transient below 1e-10 of its initial size before the IR's first sample.
const PAD_CORNER_PERIODS: f64 = 10.0;

/// The highest frequency the stimulus put into the IR: the payload's `f2_hz`,
/// capped at Nyquist.
pub fn band_limit_top_hz(sample_rate_hz: u32, f2_hz: f64) -> f64 {
    f2_hz.min(sample_rate_hz as f64 * 0.5)
}

/// Whether the arrival can be picked from the IR high-passed at `corner_hz`:
/// the stimulus must reach [`BAND_LIMIT_MIN_F2_RATIO`] times the corner
/// ([`band_limit_top_hz`]), and the corner must sit below Nyquist.
pub fn band_limit_available(sample_rate_hz: u32, f2_hz: f64, corner_hz: f64) -> bool {
    let nyquist = sample_rate_hz as f64 * 0.5;
    corner_hz > 0.0
        && corner_hz < nyquist
        && band_limit_top_hz(sample_rate_hz, f2_hz) >= BAND_LIMIT_MIN_F2_RATIO * corner_hz
}

/// `linear_ir` high-passed at `corner_hz` with zero phase: a 4th-order
/// Butterworth run forward, then over the reversed result, so the output
/// is aligned sample-for-sample with the input and −6 dB at the corner.
/// Same length as `linear_ir`.
///
/// Both ends are padded by an odd extension (`2·x[0] − x[k]`, as
/// `scipy.signal.filtfilt` does) of [`PAD_CORNER_PERIODS`] corner periods,
/// continued at a constant level when the IR is shorter than the pad, so
/// the filter's start-up transient decays before it reaches the IR and a
/// DC offset passes through as nothing rather than as an edge.
///
/// `corner_hz` must be positive and below Nyquist
/// ([`band_limit_available`]); otherwise the input is returned unchanged.
pub fn zero_phase_high_pass(linear_ir: &[f64], sample_rate_hz: u32, corner_hz: f64) -> Vec<f64> {
    let fs = sample_rate_hz as f64;
    if linear_ir.is_empty() || !(corner_hz > 0.0 && corner_hz < fs * 0.5) {
        return linear_ir.to_vec();
    }
    let sos = butterworth_4_high_pass(fs, corner_hz);
    let pad = (PAD_CORNER_PERIODS * fs / corner_hz).ceil() as usize;
    let n = linear_ir.len();

    let mut padded = Vec::with_capacity(n + 2 * pad);
    padded.extend((1..=pad).rev().map(|k| odd_extension(linear_ir, k, true)));
    padded.extend_from_slice(linear_ir);
    padded.extend((1..=pad).map(|k| odd_extension(linear_ir, k, false)));

    run_cascade(&sos, &mut padded);
    padded.reverse();
    run_cascade(&sos, &mut padded);
    padded.reverse();
    padded[pad..pad + n].to_vec()
}

/// Index and magnitude of the largest-magnitude sample of `linear_ir`
/// high-passed at `corner_hz` ([`zero_phase_high_pass`]), with
/// [`ir_peak`]'s tie and NaN rules. `None` when [`band_limit_available`]
/// excludes the band — the caller falls back to the broadband peak.
pub fn band_limited_peak(
    linear_ir: &[f64],
    sample_rate_hz: u32,
    f2_hz: f64,
    corner_hz: f64,
) -> Option<(usize, f64)> {
    if linear_ir.is_empty() || !band_limit_available(sample_rate_hz, f2_hz, corner_hz) {
        return None;
    }
    Some(ir_peak(&zero_phase_high_pass(
        linear_ir,
        sample_rate_hz,
        corner_hz,
    )))
}

/// Sample `k` (1-based) of the odd extension before (`before`) or after the
/// IR. Beyond `n − 1` samples out the reflection has no source sample, so
/// the extension holds its outermost value.
fn odd_extension(x: &[f64], k: usize, before: bool) -> f64 {
    let n = x.len();
    let k = k.min(n - 1);
    if before {
        2.0 * x[0] - x[k]
    } else {
        2.0 * x[n - 1] - x[n - 1 - k]
    }
}

fn run_cascade(sos: &[Biquad], x: &mut [f64]) {
    let mut state = vec![[0.0_f64; 2]; sos.len()];
    for v in x.iter_mut() {
        let mut y = *v;
        for (bq, z) in sos.iter().zip(state.iter_mut()) {
            y = apply_df2t(bq, z, y);
        }
        *v = y;
    }
}

/// 4th-order Butterworth high-pass as two biquads, bilinear-transformed
/// with the corner prewarped (the RBJ cookbook high-pass section at each
/// Butterworth Q). −3 dB at `corner_hz` for one pass.
fn butterworth_4_high_pass(fs: f64, corner_hz: f64) -> Vec<Biquad> {
    let w0 = 2.0 * std::f64::consts::PI * corner_hz / fs;
    let (sin_w0, cos_w0) = w0.sin_cos();
    BUTTERWORTH_4_Q
        .iter()
        .map(|&q| {
            let alpha = sin_w0 / (2.0 * q);
            let a0 = 1.0 + alpha;
            Biquad {
                b0: (1.0 + cos_w0) / 2.0 / a0,
                b1: -(1.0 + cos_w0) / a0,
                b2: (1.0 + cos_w0) / 2.0 / a0,
                a1: -2.0 * cos_w0 / a0,
                a2: (1.0 - alpha) / a0,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 96_000;

    fn tone(freq_hz: f64, n: usize) -> Vec<f64> {
        (0..n)
            .map(|i| (2.0 * std::f64::consts::PI * freq_hz * i as f64 / SR as f64).sin())
            .collect()
    }

    fn rms(x: &[f64]) -> f64 {
        (x.iter().map(|v| v * v).sum::<f64>() / x.len() as f64).sqrt()
    }

    /// Two passes of a Butterworth: −6.02 dB at the corner, flat an octave
    /// above it within 0.1 dB, and more than 90 dB down four octaves below
    /// (where the rig's 55 Hz room mode sat against the 1 kHz corner).
    #[test]
    fn magnitude_is_squared_butterworth() {
        let n = SR as usize;
        let mid = n / 4..3 * n / 4;
        let gain_db = |f: f64| {
            let x = tone(f, n);
            let y = zero_phase_high_pass(&x, SR, ARRIVAL_HIGH_PASS_CORNER_HZ);
            20.0 * (rms(&y[mid.clone()]) / rms(&x[mid.clone()])).log10()
        };
        let at_corner = gain_db(1000.0);
        assert!((at_corner + 6.02).abs() < 0.05, "corner gain {at_corner}");
        let above = gain_db(4000.0);
        assert!(above.abs() < 0.1, "4 kHz gain {above}");
        let mode = gain_db(55.0);
        assert!(mode < -90.0, "55 Hz gain {mode}");
    }

    /// Zero phase: a symmetric pulse stays centred on the same sample, and
    /// a DC offset (the fixtures' flat "noise") passes as nothing.
    #[test]
    fn a_spike_stays_on_its_sample_and_dc_is_removed() {
        let mut x = vec![0.25_f64; 4096];
        x[1500] = 1.25;
        let y = zero_phase_high_pass(&x, SR, 1000.0);
        assert_eq!(y.len(), x.len());
        assert_eq!(ir_peak(&y).0, 1500);
        // Symmetric about the spike.
        for k in 1..200 {
            assert!((y[1500 - k] - y[1500 + k]).abs() < 1e-9, "k={k}");
        }
        // The DC level is gone at both ends, including the edges.
        assert!(y[0].abs() < 1e-9 && y[4095].abs() < 1e-9);
        assert!(y[..1000].iter().all(|v| v.abs() < 1e-6));
    }

    /// An IR shorter than the pad still filters (the extension holds its
    /// outermost value) and keeps its length.
    #[test]
    fn a_short_ir_keeps_its_length() {
        let x = vec![0.0, 0.1, 1.0, 0.1, 0.0];
        let y = zero_phase_high_pass(&x, 48_000, 1000.0);
        assert_eq!(y.len(), 5);
        assert_eq!(ir_peak(&y).0, 2);
        assert!(zero_phase_high_pass(&[], 48_000, 1000.0).is_empty());
    }

    /// The band rule: one octave above the corner, measured to the lower
    /// of `f2_hz` and Nyquist.
    #[test]
    fn band_limit_needs_an_octave_above_the_corner() {
        assert!(band_limit_available(96_000, 20_000.0, 1000.0));
        assert!(band_limit_available(96_000, 2_000.0, 1000.0));
        assert!(!band_limit_available(96_000, 1_999.0, 1000.0));
        assert!(!band_limit_available(96_000, 500.0, 1000.0));
        // Nyquist caps the band top.
        assert!(!band_limit_available(3_000, 20_000.0, 1000.0));
        assert!(band_limit_available(4_000, 20_000.0, 1000.0));
        assert_eq!(
            band_limited_peak(&[0.0, 1.0, 0.0], 96_000, 500.0, 1000.0),
            None
        );
        assert_eq!(band_limited_peak(&[], 96_000, 20_000.0, 1000.0), None);
    }
}
