use ac_core::analysis::analyze;
use std::time::Instant;

fn main() {
    let sr = 48_000u32;
    for &n in &[1024usize, 8192, 65536] {
        let samples: Vec<f32> = (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / sr as f32).sin())
            .collect();
        let _ = analyze(&samples, sr, 1000.0, 10);
        let iters = if n >= 65536 { 200 } else { 1000 };
        let t0 = Instant::now();
        for _ in 0..iters {
            let _ = analyze(&samples, sr, 1000.0, 10);
        }
        let e = t0.elapsed();
        let per = e.as_secs_f64() * 1000.0 / iters as f64;
        println!("n={:>6} iters={} avg={:.3} ms/call", n, iters, per);
    }
}
