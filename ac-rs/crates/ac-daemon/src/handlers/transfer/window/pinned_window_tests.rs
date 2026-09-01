//! The positive control #208 was closed without.
//!
//! `work/planning/state-live-spectrum.md` records the gap in as many words:
//! the A/B used a 6 s level step, which is longer than the analysis window,
//! so its edge gives a monotone ramp on *both* builds and cannot excite the
//! symptom. These tests use a burst **shorter than one Welch block** and
//! score only the ticks where it sits entirely inside the analysis window —
//! there the total energy is constant, so a sound estimator must report a
//! flat line and every dB of spread is artifact.
//!
//! Both drains run on the same stream in the same test, because "pinned, not
//! sliding" is only a claim if the rejected implementation is measured next
//! to it.

use super::{drain_to_block_lattice, Window};

const SR: u32 = 8_000;
const BURST_START_S: f64 = 1.0;
const BURST_LEN_S: f64 = 0.25; // shorter than the 1 s block — the whole point

/// The production geometry, from the production type — not a second
/// copy of the arithmetic. A window this test derived for itself could
/// stay green while the session's own moved out from under it.
fn params() -> Window {
    Window::new(SR, 4)
}

/// Deterministic broadband burst in silence. No rng dependency: a
/// fixed-seed LCG keeps this test reproducible across toolchains.
fn burst_stream(total_s: f64) -> Vec<f32> {
    let n = (total_s * SR as f64) as usize;
    let mut x = vec![0.0f32; n];
    let a = (BURST_START_S * SR as f64) as usize;
    let b = ((BURST_START_S + BURST_LEN_S) * SR as f64) as usize;
    let mut s: u32 = 0x1234_5678;
    for v in x[a..b].iter_mut() {
        s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        *v = ((s >> 8) as f32 / 8_388_608.0 - 1.0) * 0.1;
    }
    x
}

/// The drain this replaced: trim to an exact length every tick, which
/// slides the block grid across the audio.
fn drain_exact(ring: &mut Vec<f32>, target_total: usize, _step: usize) {
    if ring.len() > target_total {
        let d = ring.len() - target_total;
        ring.drain(..d);
    }
}

/// Broadband level of one published frame, in dB. Uses the real
/// estimator, not a reimplementation of it.
fn level_db(ring: &[f32]) -> f64 {
    let r = ac_core::visualize::transfer::h1_estimate_with_delay(ring, ring, SR, 0);
    let p: f64 = r.meas_amp.iter().map(|a| a * a).sum();
    10.0 * p.max(1e-30).log10()
}

/// Run the worker's ring loop with `drain`, returning `(t_s, level_db)`
/// for every tick that would have published a frame.
fn run(drain: fn(&mut Vec<f32>, usize, usize)) -> Vec<(f64, f64)> {
    let w = params();
    let (nperseg, step, n_averages, target_total) =
        (w.nperseg, w.step, w.n_averages, w.target_total());
    let chunk = (0.05 * SR as f64) as usize;
    let x = burst_stream(6.0);
    let mut ring: Vec<f32> = Vec::new();
    let mut out = Vec::new();
    let mut t = 0usize;
    while t + chunk <= x.len() {
        ring.extend_from_slice(&x[t..t + chunk]);
        drain(&mut ring, target_total, step);
        t += chunk;
        // The production warmup gate, verbatim: one segment.
        if ring.len() < nperseg {
            continue;
        }
        // Score only frames at full N. The artifact under test is
        // re-weighting at a *constant* block count; a frame from a
        // still-filling window legitimately reports a different level, and
        // mixing those in would let a real defect hide behind honest
        // settling. Mirrors the frame's own `n_averages`.
        if (ring.len() - nperseg) / step + 1 != n_averages {
            continue;
        }
        out.push((t as f64 / SR as f64, level_db(&ring)));
    }
    out
}

/// dB spread over the ticks where the burst is wholly inside the window.
fn spread_while_fully_inside(series: &[(f64, f64)]) -> (f64, usize) {
    let win_s = params().target_total() as f64 / SR as f64;
    // Window covers [t - win_s, t]; burst is inside once t has passed its
    // end and until t - win_s passes its start. Trim a tick off each edge
    // so quantisation of the chunk grid is not scored.
    let lo = BURST_START_S + BURST_LEN_S + 0.05;
    let hi = BURST_START_S + win_s - 0.05;
    let v: Vec<f64> = series
        .iter()
        .filter(|(t, _)| *t >= lo && *t <= hi)
        .map(|(_, d)| *d)
        .collect();
    if v.len() < 3 {
        return (f64::NAN, v.len());
    }
    let max = v.iter().cloned().fold(f64::MIN, f64::max);
    let min = v.iter().cloned().fold(f64::MAX, f64::min);
    (max - min, v.len())
}

/// The fix: a burst held entirely inside the window reports one level.
#[test]
fn pinned_grid_holds_a_stationary_burst_at_a_constant_level() {
    let (spread, n) = spread_while_fully_inside(&run(drain_to_block_lattice));
    assert!(
        n >= 10,
        "only {n} scored frames — the control is not running"
    );
    assert!(
        spread < 0.05,
        "pinned grid still moved a constant burst by {spread:.2} dB over {n} frames"
    );
}

/// The control. If this ever stops failing, the burst has become
/// incapable of exciting the defect and the test above proves nothing.
#[test]
fn exact_trim_drain_reweights_the_same_burst() {
    let (spread, n) = spread_while_fully_inside(&run(drain_exact));
    assert!(
        n >= 10,
        "only {n} scored frames — the control is not running"
    );
    assert!(
        spread > 3.0,
        "the discarded exact-trim drain moved the burst by only {spread:.2} dB \
         over {n} frames; this stimulus can no longer excite #208, so the \
         pinned-grid test above is not evidence of anything"
    );
}

/// The drain's own contract: length lands in `[target_total,
/// target_total + step)`, which is what fits exactly `n_averages` blocks.
#[test]
fn drain_keeps_the_ring_on_the_block_lattice() {
    let w = params();
    let (nperseg, step, n_avg, target_total) = (w.nperseg, w.step, w.n_averages, w.target_total());
    let mut ring: Vec<f32> = Vec::new();
    for _ in 0..400 {
        ring.extend(std::iter::repeat_n(0.0f32, 137)); // coprime with step
        drain_to_block_lattice(&mut ring, target_total, step);
        if ring.len() >= target_total {
            assert!(
                ring.len() < target_total + step,
                "ring grew to {} beyond the window",
                ring.len()
            );
            let blocks = (ring.len() - nperseg) / step + 1;
            assert_eq!(blocks, n_avg, "ring of {} fits {blocks} blocks", ring.len());
        }
    }
}
