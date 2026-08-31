use ac_core::measurement::loudness::{GatingBlock, KWeighting, LoudnessState};
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

    {
        // The path the daemon actually runs: planar push into a
        // multi-channel state, filter + tiling + true-peak in one call.
        let mut st = LoudnessState::new_mono(sr).expect("state");
        st.push(&[&samples]).expect("push");
        st.reset();
        let t0 = Instant::now();
        for _ in 0..iters {
            st.push(&[&samples]).expect("push");
        }
        let per_ms = t0.elapsed().as_secs_f64() * 1000.0 / iters as f64;
        println!(
            "LoudnessState::push n={} avg={:.4} ms/call ({:.2}× realtime)",
            n,
            per_ms,
            (n as f64 / sr as f64) * 1000.0 / per_ms
        );
    }

    {
        // Query cost against history depth. These are read once per emit
        // tick per channel, so their cost must not track session length:
        // the histogram makes them O(bins), and the numbers below should
        // stay flat as the simulated session grows.
        for minutes in [1u32, 10, 60] {
            let mut st = LoudnessState::new_mono(sr).expect("state");
            for _ in 0..(minutes * 60) {
                st.push(&[&samples]).expect("push");
            }
            let reps = 2_000;
            let t0 = Instant::now();
            let mut sink = 0.0_f64;
            for _ in 0..reps {
                sink += st.integrated() + st.loudness_range() + st.gated_duration_s();
            }
            let per_us = t0.elapsed().as_secs_f64() * 1e6 / reps as f64;
            println!("query @ {minutes:>2} min history  avg={per_us:.2} µs/emit (sink {sink:.3})");
        }
    }
}
