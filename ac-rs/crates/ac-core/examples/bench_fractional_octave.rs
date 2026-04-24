use ac_core::visualize::cwt::{
    default_f_max, log_scales, morlet_cwt, DEFAULT_F_MIN, DEFAULT_N_SCALES, DEFAULT_SIGMA,
};
use ac_core::visualize::fractional_octave::cwt_to_fractional_octave;
use std::time::Instant;

fn main() {
    let sr = 48_000u32;
    let f_max = default_f_max(sr);
    let (scales, freqs) = log_scales(DEFAULT_F_MIN, f_max, DEFAULT_N_SCALES, sr, DEFAULT_SIGMA);

    // Realistic column: take an actual CWT of a synthetic signal so the dB
    // values span a useful dynamic range instead of being uniform.
    let n = (sr as f64 * 0.15) as usize;
    let samples: Vec<f32> = (0..n).map(|i| (i as f32 * 0.01).sin()).collect();
    let col = morlet_cwt(&samples, sr, &scales, DEFAULT_SIGMA);

    let iters: usize = std::env::var("AC_BENCH_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2000);

    // Warmup.
    for &bpo in &[1usize, 3, 6, 12, 24] {
        let _ = cwt_to_fractional_octave(&col, &freqs, bpo, DEFAULT_F_MIN, f_max);
    }

    for &bpo in &[1usize, 3, 6, 12, 24] {
        let t0 = Instant::now();
        for _ in 0..iters {
            let _ = cwt_to_fractional_octave(&col, &freqs, bpo, DEFAULT_F_MIN, f_max);
        }
        let e = t0.elapsed();
        let per_ms = e.as_secs_f64() * 1000.0 / iters as f64;
        let (centres, _) =
            cwt_to_fractional_octave(&col, &freqs, bpo, DEFAULT_F_MIN, f_max);
        println!(
            "bpo={:>2} bands={:>3} scales={} avg={:.4} ms/call",
            bpo,
            centres.len(),
            scales.len(),
            per_ms
        );
    }
}
