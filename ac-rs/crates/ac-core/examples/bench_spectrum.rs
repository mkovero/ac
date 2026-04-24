use ac_core::visualize::spectrum::spectrum_only;
use std::time::Instant;

fn main() {
    let sr = 48_000u32;
    let iters: usize = std::env::var("AC_BENCH_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2000);

    for &n in &[1024usize, 4096, 8192, 16384, 32768, 65536] {
        let samples: Vec<f32> = (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / sr as f32).sin())
            .collect();
        let _ = spectrum_only(&samples, sr); // warmup

        let t0 = Instant::now();
        for _ in 0..iters {
            let _ = spectrum_only(&samples, sr);
        }
        let e = t0.elapsed();
        let per_ms = e.as_secs_f64() * 1000.0 / iters as f64;
        println!("n={:>6} avg={:.4} ms/call", n, per_ms);
    }
}
