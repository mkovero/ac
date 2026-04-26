use ac_core::visualize::reassigned::{
    build_kernels, default_f_max, reassigned, DEFAULT_F_MIN, DEFAULT_N, DEFAULT_N_OUT_BINS,
};
use std::time::Instant;

fn main() {
    let sr = 48_000u32;
    let n  = DEFAULT_N;
    let samples: Vec<f32> = (0..n).map(|i| (i as f32 * 0.01).sin()).collect();
    let kernels = build_kernels(n, sr, DEFAULT_N_OUT_BINS, DEFAULT_F_MIN, default_f_max(sr));
    let _ = reassigned(&samples, &kernels);                                 // warm scratch
    let iters: usize = std::env::var("AC_BENCH_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(500);
    let t0 = Instant::now();
    for _ in 0..iters {
        let _ = reassigned(&samples, &kernels);
    }
    let e = t0.elapsed();
    let per_call_ms = e.as_secs_f64() * 1000.0 / iters as f64;
    println!(
        "n={} bins_out={} avg={:.3} ms/call",
        n,
        kernels.freqs_out.len(),
        per_call_ms,
    );
}
