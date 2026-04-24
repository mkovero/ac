use ac_core::visualize::cwt::{default_f_max, log_scales, morlet_cwt, DEFAULT_F_MIN, DEFAULT_SIGMA};
use std::time::Instant;

fn main() {
    let sr = 48_000u32;
    let n = (sr as f64 * 0.15) as usize;
    let samples: Vec<f32> = (0..n).map(|i| (i as f32 * 0.01).sin()).collect();
    let (scales, _) = log_scales(DEFAULT_F_MIN, default_f_max(sr), 512, sr, DEFAULT_SIGMA);
    let _ = morlet_cwt(&samples, sr, &scales, DEFAULT_SIGMA);
    let iters: usize = std::env::var("AC_BENCH_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(100);
    let t0 = Instant::now();
    for _ in 0..iters {
        let _ = morlet_cwt(&samples, sr, &scales, DEFAULT_SIGMA);
    }
    let e = t0.elapsed();
    let per_call_ms = e.as_secs_f64() * 1000.0 / iters as f64;
    println!(
        "n={} scales={} avg={:.3} ms/call",
        n,
        scales.len(),
        per_call_ms
    );
}
