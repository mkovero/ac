//! Benchmark for the mic-curve FIR (#104). Measures the per-block
//! convolution cost so a future change can spot regressions.

use std::time::Instant;

use ac_core::shared::calibration::parse_mic_curve;
use ac_core::shared::mic_curve_filter::{MicCurveFir, DEFAULT_N_TAPS};

fn main() {
    let sr = 48_000_u32;
    // Synthetic 24-point flat curve at +2 dB.
    let mut text = String::new();
    let log_min = 20.0_f32.ln();
    let log_max = 20_000.0_f32.ln();
    for i in 0..24 {
        let t = i as f32 / 23.0;
        let f = (log_min + t * (log_max - log_min)).exp();
        text.push_str(&format!("{f}\t2.0\n"));
    }
    let curve = parse_mic_curve(&text, None).unwrap();

    let block_samples: usize = std::env::var("AC_BENCH_BLOCK")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2400);                              // ~50 ms @ 48 kHz
    let iters: usize = std::env::var("AC_BENCH_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(200);

    let mut fir = MicCurveFir::new(&curve, sr, DEFAULT_N_TAPS);
    let mut block: Vec<f32> = (0..block_samples)
        .map(|i| (i as f32 * 0.001).sin())
        .collect();

    // Warm.
    fir.process_inplace(&mut block);
    fir.reset();

    let t0 = Instant::now();
    for _ in 0..iters {
        fir.process_inplace(&mut block);
    }
    let e = t0.elapsed();
    let per_block_ms = e.as_secs_f64() * 1000.0 / iters as f64;
    let block_seconds = block_samples as f64 / sr as f64;
    let realtime_factor = block_seconds * 1000.0 / per_block_ms;
    println!(
        "n_taps={} block={} samples ({:.1} ms audio) avg={:.3} ms/block ({:.0}× realtime)",
        DEFAULT_N_TAPS, block_samples, block_seconds * 1000.0, per_block_ms, realtime_factor,
    );
}
