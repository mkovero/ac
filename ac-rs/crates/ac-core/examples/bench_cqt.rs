use ac_core::visualize::cqt::{
    build_kernels, cqt, default_f_max, log_freqs, min_supported_f, DEFAULT_BPO, DEFAULT_F_MIN,
};
use std::time::Instant;

fn main() {
    let sr = 48_000u32;
    let buf_len = sr as usize;                                              // 1.0 s ring
    let samples: Vec<f32> = (0..buf_len).map(|i| (i as f32 * 0.01).sin()).collect();
    let f_min = DEFAULT_F_MIN.max(min_supported_f(buf_len, sr, DEFAULT_BPO));
    let freqs = log_freqs(f_min, default_f_max(sr), DEFAULT_BPO);
    let kernels = build_kernels(&freqs, sr, DEFAULT_BPO, buf_len);
    let _ = cqt(&samples, &kernels);                                        // warm scratch
    let iters: usize = std::env::var("AC_BENCH_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(100);
    let t0 = Instant::now();
    for _ in 0..iters {
        let _ = cqt(&samples, &kernels);
    }
    let e = t0.elapsed();
    let per_call_ms = e.as_secs_f64() * 1000.0 / iters as f64;
    println!(
        "n={} bins={} bpo={} avg={:.3} ms/call",
        buf_len,
        freqs.len(),
        DEFAULT_BPO,
        per_call_ms,
    );
}
