use ac_core::measurement::sweep::{deconvolve_full, extract_irs, inverse_sweep, log_sweep, SweepParams};
use std::time::Instant;

fn main() {
    let sr = 48_000u32;
    let iters: usize = std::env::var("AC_BENCH_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(100);

    for &duration_s in &[1.0, 3.0, 10.0] {
        let p = SweepParams { f1_hz: 20.0, f2_hz: 20_000.0, duration_s, sample_rate: sr };
        let sweep = log_sweep(&p).expect("log_sweep");
        let inv   = inverse_sweep(&p).expect("inverse_sweep");

        // log_sweep / inverse_sweep timing
        let t0 = Instant::now();
        for _ in 0..iters {
            let _ = log_sweep(&p);
        }
        let log_ms = t0.elapsed().as_secs_f64() * 1000.0 / iters as f64;

        let t0 = Instant::now();
        for _ in 0..iters {
            let _ = inverse_sweep(&p);
        }
        let inv_ms = t0.elapsed().as_secs_f64() * 1000.0 / iters as f64;

        // deconvolve_full timing — simulate a captured recording = sweep + tail.
        let tail = (0.5 * sr as f64) as usize;
        let mut captured: Vec<f32> = sweep.clone();
        captured.extend(std::iter::repeat(0.0).take(tail));
        let _ = deconvolve_full(&captured, &inv);
        let t0 = Instant::now();
        for _ in 0..iters {
            let _ = deconvolve_full(&captured, &inv);
        }
        let deconv_ms = t0.elapsed().as_secs_f64() * 1000.0 / iters as f64;

        // extract_irs timing (windowing over the deconvolved buffer).
        let full = deconvolve_full(&captured, &inv);
        let t0 = Instant::now();
        for _ in 0..iters {
            let _ = extract_irs(&full, &p, 5, 4096).expect("extract_irs");
        }
        let extract_ms = t0.elapsed().as_secs_f64() * 1000.0 / iters as f64;

        println!(
            "dur={:>4.1}s  n={:>6}  log_sweep={:.3}  inverse={:.3}  deconv={:.3}  extract={:.3} ms/call",
            duration_s, sweep.len(), log_ms, inv_ms, deconv_ms, extract_ms
        );
    }
}
