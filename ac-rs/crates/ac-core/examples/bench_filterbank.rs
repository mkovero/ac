use ac_core::measurement::filterbank::Filterbank;
use std::time::Instant;

fn main() {
    let sr = 48_000u32;
    let iters: usize = std::env::var("AC_BENCH_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(200);

    let n = sr as usize; // 1 s of signal
    let samples: Vec<f32> = (0..n)
        .map(|i| (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / sr as f32).sin())
        .collect();

    for &bpo in &[1usize, 3, 6, 12] {
        let bank = Filterbank::new(sr, bpo, 20.0, 20_000.0).expect("filterbank");
        let n_bands = bank.centres_hz().len();
        let _ = bank.process(&samples); // warmup
        let t0 = Instant::now();
        for _ in 0..iters {
            let _ = bank.process(&samples);
        }
        let per_ms = t0.elapsed().as_secs_f64() * 1000.0 / iters as f64;
        println!(
            "Filterbank bpo={:>2} bands={:>3} n={} avg={:.3} ms/call",
            bpo, n_bands, n, per_ms
        );
    }
}
