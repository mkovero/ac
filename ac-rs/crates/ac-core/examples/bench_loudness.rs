use ac_core::measurement::loudness::{GatingBlock, KWeighting};
use std::time::Instant;

fn main() {
    let sr = 48_000u32;
    let iters: usize = std::env::var("AC_BENCH_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1000);

    // One second of a 1 kHz sine — representative of a live monitor push.
    let n = sr as usize;
    let samples: Vec<f32> = (0..n)
        .map(|i| (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / sr as f32).sin())
        .collect();

    {
        let mut kw = KWeighting::new(sr).expect("k-weighting");
        let _ = kw.apply(&samples); // warmup
        let t0 = Instant::now();
        for _ in 0..iters {
            let _ = kw.apply(&samples);
        }
        let per_ms = t0.elapsed().as_secs_f64() * 1000.0 / iters as f64;
        println!(
            "KWeighting::apply  n={} avg={:.4} ms/call ({:.2}× realtime)",
            n,
            per_ms,
            (n as f64 / sr as f64) * 1000.0 / per_ms
        );
    }

    {
        let mut gb = GatingBlock::new(sr).expect("gating");
        let _ = gb.push(&samples);
        gb.reset();
        let t0 = Instant::now();
        for _ in 0..iters {
            let _ = gb.push(&samples);
            gb.reset();
        }
        let per_ms = t0.elapsed().as_secs_f64() * 1000.0 / iters as f64;
        println!(
            "GatingBlock::push  n={} avg={:.4} ms/call ({:.2}× realtime)",
            n,
            per_ms,
            (n as f64 / sr as f64) * 1000.0 / per_ms
        );
    }
}
