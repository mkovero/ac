//! Stimulus helpers shared by the loudness submodules' unit tests.

use std::f64::consts::PI;

pub(super) const FS: u32 = 48_000;

/// Generate `n` samples of an N-dBFS sine at `f_hz`.
pub(super) fn sine_samples(n: usize, f_hz: f64, amp_dbfs: f64, fs: u32) -> Vec<f32> {
    let amp = 10.0_f64.powf(amp_dbfs / 20.0);
    let w = 2.0 * PI * f_hz / fs as f64;
    (0..n)
        .map(|i| (amp * (w * i as f64).sin()) as f32)
        .collect()
}
