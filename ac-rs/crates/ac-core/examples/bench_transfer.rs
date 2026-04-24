use ac_core::visualize::transfer::{
    capture_duration, estimate_delay_samples, h1_estimate, h1_estimate_with_delay,
};
use std::time::Instant;

fn main() {
    let sr = 48_000u32;
    let iters: usize = std::env::var("AC_BENCH_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);

    for &n_averages in &[1usize, 4, 8, 16] {
        let secs = capture_duration(n_averages, sr);
        let n = (secs * sr as f64) as usize;
        let mut state = 0x1234_5678_u32;
        let r: Vec<f32> = (0..n)
            .map(|_| {
                state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                ((state >> 16) as i16) as f32 / 32768.0 * 0.3
            })
            .collect();
        let delay_samples = (sr as f64 * 0.002) as usize;
        let m: Vec<f32> = (0..n)
            .map(|i| {
                let src = if i >= delay_samples { r[i - delay_samples] } else { 0.0 };
                state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                let noise = ((state >> 16) as i16) as f32 / 32768.0 * 0.01;
                src + noise
            })
            .collect();

        let _ = h1_estimate(&r, &m, sr); // warmup
        let t0 = Instant::now();
        for _ in 0..iters {
            let _ = h1_estimate(&r, &m, sr);
        }
        let per_ms = t0.elapsed().as_secs_f64() * 1000.0 / iters as f64;

        // Simulate the transfer_stream hot loop: delay pre-computed once.
        let d = estimate_delay_samples(&r, &m, sr);
        let _ = h1_estimate_with_delay(&r, &m, sr, d);
        let t0 = Instant::now();
        for _ in 0..iters {
            let _ = h1_estimate_with_delay(&r, &m, sr, d);
        }
        let hot_ms = t0.elapsed().as_secs_f64() * 1000.0 / iters as f64;

        println!(
            "avgs={:>2} capture={:.2}s n={:>6}  h1_estimate={:.2}  with_delay(hot)={:.2} ms/call",
            n_averages, secs, n, per_ms, hot_ms
        );
    }
}
