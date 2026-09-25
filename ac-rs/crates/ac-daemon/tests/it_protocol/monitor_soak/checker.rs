//! Incremental per-frame checker for the `monitor_spectrum` temporal soak
//! (#189). Pure: no daemon, no sockets, no wall clock. The runner in
//! `mod.rs` feeds it every published frame in arrival order; the self-tests
//! at the bottom feed it synthetic sequences built from the same
//! [`SoakFrame`] shape, so the checker the soak runs is the one tested here.
//!
//! **Clock.** Time is counted in *frames*, not seconds. With one channel the
//! daemon publishes one spectrum frame per tick, so a frame index is the
//! daemon's tick count, and the LF band's cadence is defined in ticks
//! (`hop_ticks`, supplied by the caller from the ack). A loaded test host
//! makes the daemon produce fewer frames per second, not fewer frames per
//! LF change, so no invariant here can go red from scheduler jitter. What it
//! cannot see is the tick itself drifting from `interval` in wall time.
//!
//! Invariants, judged on every frame, first violation wins, in priority
//! order bounded > continuity > liveness > rate > plausibility:
//! - **I4-t** (bounded), from frame 0: no NaN, no `+inf`, no value above
//!   0 dBFS + [`BOUNDED_TOL_DB`]. `-inf` (an exact-zero amplitude on the
//!   wire) is legal here; a collapsed LF band is I5b's to catch.
//! - **I2-t** (continuity), post-settle, split at `crossover_hz`: LF/HF
//!   splice step within
//!   [`CONTINUITY_TOL_DB`], violation only after [`CONTINUITY_STREAK`]
//!   consecutive out-of-tolerance frames.
//! - **I5a** (liveness), post-settle: LF slice bit-identical for more than
//!   [`LIVENESS_HOP_MULTIPLE`] × `hop_ticks` consecutive frames.
//! - **I5c** (rate), post-settle: mean frames between LF changes within
//!   [`RATE_TOLERANCE_FACTOR`] of `hop_ticks`, either direction.
//! - **I5b** (plausibility), post-settle: LF power-mean against its own
//!   baseline; out of [`PLAUSIBILITY_TOL_DB`] in a majority of a rolling
//!   window.
//!
//!
//! I5a, I5b and I5c judge the **LF-only slice**, `freqs < lf_edge_hz`, not
//! everything below the crossover: the columns in `[lf_edge_hz,
//! crossover_hz)` are cross-faded with HF, which is recomputed every tick, so
//! they change on every frame of a healthy daemon. See [`lf_only_edge_hz`].
//!
//! The constants are ported from the `ac-ui --headless-test` I5 soak
//! (c9523d9, removed with the ac-ui detach in ec4c6db); their rationale is
//! kept on each.

use std::collections::VecDeque;

/// I4-t: headroom above 0 dBFS. Same value and rationale as
/// `monitor_spectrum_fake_noise_stays_bounded`: a single bin can read a few
/// hundredths of a dB above nominal from window-leakage summation of random
/// phase; a gain or clamping bug produces far more.
pub const BOUNDED_TOL_DB: f64 = 1.0;

/// I2-t: LF/HF splice step tolerance. Generous relative to the documented
/// per-band sigma (HF ~0.7-2.4 dB, LF post-EMA target ~2x that): the two
/// bands are independent estimates that need not agree bin-for-bin, only
/// avoid a gross jump.
pub const CONTINUITY_TOL_DB: f64 = 8.0;

/// I2-t: columns power-averaged on each side of the crossover before the
/// step is taken, so one noisy column cannot carry the comparison.
pub const SPLICE_COLS: usize = 3;

/// I2-t: consecutive out-of-tolerance frames before a violation. A lone
/// crossing is expected from broadband-noise chi-squared tails with no bug
/// present (c9523d9 recorded a lone 8.16 dB step in 327 frames against an
/// 8 dB tolerance); the failure modes this soak exists for are sustained.
///
/// That point came from non-overlapping HF windows (0.2 s interval). At this
/// soak's interval (hop/4 = 34.1 ms) consecutive HF windows share about 80 %
/// of their samples. Measured there on a healthy `--fake-audio` daemon
/// (#189, three runs, bit-identical readouts): max step 8.66 dB, one frame
/// over tolerance in 440, longest over-tolerance streak 1. The healthy
/// maximum is over [`CONTINUITY_TOL_DB`], so the margin rests on this streak
/// rule, not on the tolerance.
pub const CONTINUITY_STREAK: u32 = 3;

/// I5a: LF slice unchanged for more than this many expected hops is
/// `frozen`. The 2x is the issue's ("not frozen beyond ~2x expected hop").
pub const LIVENESS_HOP_MULTIPLE: f64 = 2.0;

/// I5c: minimum LF-change intervals before the mean interval is judged;
/// fewer make the mean too noisy to call.
pub const RATE_MIN_SAMPLES: u32 = 5;

/// I5c: mean frames between LF changes must stay within this factor of
/// `hop_ticks`, either direction. Loose relative to I5a because it catches
/// a band that *keeps* updating too fast (recomputing every tick), which I5a
/// cannot see. The slow side is checked too, but I5a reaches it first: a mean
/// above 3x hop needs an interval above 3x hop, and I5a fires at 2x.
pub const RATE_TOLERANCE_FACTOR: f64 = 3.0;

/// I5b: post-settle frames averaged into the plausibility baseline.
pub const BASELINE_FRAMES: usize = 5;

/// I5b: LF power-mean tolerance against the baseline. Generous relative to
/// the ~2.2-2.4 dB post-EMA sigma #173 tuned for, so it catches
/// collapse/garbage/drift rather than EMA's expected residual.
pub const PLAUSIBILITY_TOL_DB: f64 = 6.0;

/// I5b: rolling window and the out-of-tolerance count within it that is a
/// violation. A count rather than a strict streak, because a band updating
/// with wrong values can dip back into tolerance between bad frames and
/// would reset a streak forever.
///
/// The window is **not** a vote over independent frames. The LF-only slice
/// is bit-identical between recomputes, so its power-mean repeats for
/// `hop_ticks + 1` frames: at the soak's `hop_ticks` = 4 the 10 frames cover
/// two LF recomputes, and one out-of-tolerance recompute fills 5 slots and
/// fires by itself. What keeps that from being a false red is the healthy
/// margin, which the soak prints ([`SoakStats`]): measured 0.67 dB max
/// |cur − baseline| against the 6 dB tolerance (#189, three runs,
/// bit-identical readouts).
pub const PLAUSIBILITY_WINDOW: usize = 10;
pub const PLAUSIBILITY_WINDOW_MIN_VIOLATIONS: usize = 5;

/// Half-width, in octaves, of the band across which the daemon cross-fades
/// LF into HF around `crossover_hz`. Published wire contract: ZMQ.md →
/// `### Dual-resolution low-frequency path (#142)` ("cross-faded linearly in
/// linear amplitude across a ±1/6-octave band at the crossover"); the value
/// behind it is `ac-core`'s private `BLEND_HALF_OCTAVE`. The soak is a wire
/// consumer, so it takes the width from the document, not the crate.
///
/// If the blend band widens, blend columns fall inside the LF-only slice and
/// I5c goes red on every run — loud. If it narrows, the slice stays LF-only
/// and only loses a little coverage. The coupling is recorded by
/// `blend_columns_outside_lf_edge_do_not_count_as_lf_changes`.
pub const BLEND_HALF_OCTAVE_WIRE: f64 = 1.0 / 6.0;

/// Upper edge of the LF-only slice: the low end of the blend band,
/// `crossover_hz / 2^BLEND_HALF_OCTAVE_WIRE`.
pub fn lf_only_edge_hz(crossover_hz: f64) -> f64 {
    crossover_hz / 2f64.powf(BLEND_HALF_OCTAVE_WIRE)
}

/// One published spectrum frame: display columns and their dBFS values.
#[derive(Clone, Debug)]
pub struct SoakFrame {
    pub freqs: Vec<f64>,
    pub spectrum: Vec<f64>,
}

/// Which invariant tripped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Invariant {
    /// I4-t
    Bounded,
    /// I2-t
    Continuity,
    /// I5a
    Liveness,
    /// I5c
    Rate,
    /// I5b
    Plausibility,
}

impl Invariant {
    pub fn label(self) -> &'static str {
        match self {
            Invariant::Bounded => "I4-t bounded",
            Invariant::Continuity => "I2-t continuity",
            Invariant::Liveness => "I5a liveness",
            Invariant::Rate => "I5c rate",
            Invariant::Plausibility => "I5b plausibility",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Violation {
    pub invariant: Invariant,
    /// Failure-mode word for the dump: garbage / level-jump / frozen /
    /// wrong-rate / drift.
    pub class: &'static str,
    /// Index of the frame that tripped it (0-based, in checker order).
    pub frame_idx: usize,
    pub detail: String,
}

/// Frames on which each invariant was actually judged. The runner fails the
/// soak if any count is short, so a soak whose LF band never engaged cannot
/// pass vacuously.
#[derive(Clone, Copy, Debug, Default)]
pub struct CheckCounts {
    pub bounded: usize,
    pub continuity: usize,
    pub liveness: usize,
    pub rate: usize,
    pub plausibility: usize,
    /// Post-settle LF-slice changes seen.
    pub lf_changes: usize,
}

/// Healthy-margin statistics for the assumed I2-t and I5b tolerances,
/// information only: the runner prints them, nothing asserts on them. They
/// are what shows how far a healthy daemon sits from each tolerance at the
/// soak's own configuration (overlapping HF windows, `hop_ticks` = 4).
#[derive(Clone, Copy, Debug)]
pub struct SoakStats {
    /// Largest I2-t splice step judged, dB.
    pub continuity_max_step_db: f64,
    /// Frames whose splice step exceeded [`CONTINUITY_TOL_DB`].
    pub continuity_over_tol: usize,
    /// Longest run of consecutive over-tolerance splice steps.
    pub continuity_longest_streak: u32,
    /// Largest |LF power-mean − baseline| judged by I5b, dB.
    pub plausibility_max_err_db: f64,
}

impl Default for SoakStats {
    fn default() -> Self {
        Self {
            continuity_max_step_db: f64::NEG_INFINITY,
            continuity_over_tol: 0,
            continuity_longest_streak: 0,
            plausibility_max_err_db: f64::NEG_INFINITY,
        }
    }
}

impl SoakStats {
    pub fn readout(&self) -> String {
        format!(
            "I2-t max step {:.2} dB (tol {CONTINUITY_TOL_DB}), {} frames over tol, longest \
             over-tol streak {} (violation at {CONTINUITY_STREAK}); I5b max |cur - baseline| \
             {:.2} dB (tol {PLAUSIBILITY_TOL_DB})",
            self.continuity_max_step_db,
            self.continuity_over_tol,
            self.continuity_longest_streak,
            self.plausibility_max_err_db
        )
    }
}

/// Split index: `freqs[..split]` below `edge_hz`, `freqs[split..]` at or
/// above it.
pub fn lf_split(freqs: &[f64], edge_hz: f64) -> usize {
    freqs
        .iter()
        .position(|&f| f >= edge_hz)
        .unwrap_or(freqs.len())
}

/// Power-domain mean of a dBFS slice, `10·log10(mean(10^(v/10)))` — the
/// same convention as the daemon's EMA, not a bare mean of dB values.
pub fn power_mean_db(vals: &[f64]) -> f64 {
    if vals.is_empty() {
        return f64::NEG_INFINITY;
    }
    let mean_pow = vals.iter().map(|&v| 10f64.powf(v / 10.0)).sum::<f64>() / vals.len() as f64;
    10.0 * mean_pow.log10()
}

/// I4-t on one frame, independent of any checker state. `-inf` is legal: it
/// is an exact-zero amplitude, which the wire can carry. NaN (a negative or
/// non-numeric amplitude) and `+inf` are garbage.
pub fn check_bounded(sf: &SoakFrame, frame_idx: usize) -> Option<Violation> {
    for (i, &v) in sf.spectrum.iter().enumerate() {
        if v.is_nan() || v == f64::INFINITY {
            return Some(Violation {
                invariant: Invariant::Bounded,
                class: "garbage",
                frame_idx,
                detail: format!("non-finite value {v:?} at column {i}"),
            });
        }
        if v > BOUNDED_TOL_DB {
            return Some(Violation {
                invariant: Invariant::Bounded,
                class: "level-jump",
                frame_idx,
                detail: format!(
                    "value {v:.3} dBFS at column {i} ({:.1} Hz) exceeds 0 dBFS + \
                     {BOUNDED_TOL_DB} dB",
                    sf.freqs.get(i).copied().unwrap_or(f64::NAN)
                ),
            });
        }
    }
    None
}

pub struct SoakChecker {
    crossover_hz: f64,
    lf_edge_hz: f64,
    hop_ticks: f64,
    settle_frames: usize,

    next_idx: usize,
    last_lf: Option<Vec<f64>>,
    last_change_idx: usize,
    last_change_post_settle: bool,
    continuity_streak: u32,
    rate_interval_count: u32,
    rate_interval_sum: usize,
    baseline_samples: Vec<f64>,
    baseline_mean_db: Option<f64>,
    plausibility_window: VecDeque<bool>,

    counts: CheckCounts,
    stats: SoakStats,
}

impl SoakChecker {
    /// `crossover_hz`: where I2-t splits LF from HF.
    /// `lf_edge_hz`: upper edge of the LF-only slice I5a/I5b/I5c judge
    /// (normally [`lf_only_edge_hz`]`(crossover_hz)`).
    /// `hop_ticks`: expected frames between LF recomputes, unrounded.
    /// `settle_frames`: frames judged by I4-t only before the rest engage.
    pub fn new(crossover_hz: f64, lf_edge_hz: f64, hop_ticks: f64, settle_frames: usize) -> Self {
        Self {
            crossover_hz,
            lf_edge_hz,
            hop_ticks,
            settle_frames,
            next_idx: 0,
            last_lf: None,
            last_change_idx: 0,
            last_change_post_settle: false,
            continuity_streak: 0,
            rate_interval_count: 0,
            rate_interval_sum: 0,
            baseline_samples: Vec::with_capacity(BASELINE_FRAMES),
            baseline_mean_db: None,
            plausibility_window: VecDeque::with_capacity(PLAUSIBILITY_WINDOW),
            counts: CheckCounts::default(),
            stats: SoakStats::default(),
        }
    }

    pub fn counts(&self) -> CheckCounts {
        self.counts
    }

    pub fn stats(&self) -> SoakStats {
        self.stats
    }

    /// Judge the next frame. Returns the first violation it trips, if any.
    pub fn check_frame(&mut self, sf: &SoakFrame) -> Option<Violation> {
        let idx = self.next_idx;
        self.next_idx += 1;

        self.counts.bounded += 1;
        if let Some(v) = check_bounded(sf, idx) {
            return Some(v);
        }

        let settled = idx >= self.settle_frames;
        let split = lf_split(&sf.freqs, self.crossover_hz).min(sf.spectrum.len());

        if settled && split >= SPLICE_COLS && sf.spectrum.len() - split >= SPLICE_COLS {
            self.counts.continuity += 1;
            let lf_edge = power_mean_db(&sf.spectrum[split - SPLICE_COLS..split]);
            let hf_edge = power_mean_db(&sf.spectrum[split..split + SPLICE_COLS]);
            let step = (hf_edge - lf_edge).abs();
            self.stats.continuity_max_step_db = self.stats.continuity_max_step_db.max(step);
            if step > CONTINUITY_TOL_DB {
                self.continuity_streak += 1;
                self.stats.continuity_over_tol += 1;
                self.stats.continuity_longest_streak = self
                    .stats
                    .continuity_longest_streak
                    .max(self.continuity_streak);
                if self.continuity_streak >= CONTINUITY_STREAK {
                    return Some(Violation {
                        invariant: Invariant::Continuity,
                        class: "level-jump",
                        frame_idx: idx,
                        detail: format!(
                            "LF/HF splice step {step:.2} dB > {CONTINUITY_TOL_DB} dB for \
                             {} consecutive frames — LF edge ({SPLICE_COLS} cols to \
                             {:.1} Hz) {lf_edge:.2} dBFS, HF edge ({SPLICE_COLS} cols from \
                             {:.1} Hz) {hf_edge:.2} dBFS",
                            self.continuity_streak,
                            sf.freqs[split - 1],
                            sf.freqs[split],
                        ),
                    });
                }
            } else {
                self.continuity_streak = 0;
            }
        }

        let lf_only = lf_split(&sf.freqs, self.lf_edge_hz).min(sf.spectrum.len());
        let lf = &sf.spectrum[..lf_only];
        if lf.is_empty() {
            return None;
        }

        let changed = self.last_lf.as_deref() != Some(lf);
        if changed {
            let prev_idx = self.last_change_idx;
            let prev_post_settle = self.last_change_post_settle;
            self.last_lf = Some(lf.to_vec());
            self.last_change_idx = idx;
            self.last_change_post_settle = settled;
            if settled {
                self.counts.lf_changes += 1;
                if prev_post_settle {
                    self.rate_interval_sum += idx - prev_idx;
                    self.rate_interval_count += 1;
                }
            }
        }

        if !settled {
            return None;
        }

        // I5a liveness.
        self.counts.liveness += 1;
        let stale = idx - self.last_change_idx;
        let bound = LIVENESS_HOP_MULTIPLE * self.hop_ticks;
        if stale as f64 > bound {
            return Some(Violation {
                invariant: Invariant::Liveness,
                class: "frozen",
                frame_idx: idx,
                detail: format!(
                    "LF slice bit-identical for {stale} frames (> {bound:.2} = \
                     {LIVENESS_HOP_MULTIPLE}x expected hop {:.2} frames), last change at \
                     frame {}",
                    self.hop_ticks, self.last_change_idx
                ),
            });
        }

        // I5c rate.
        if self.rate_interval_count >= RATE_MIN_SAMPLES {
            self.counts.rate += 1;
            let mean = self.rate_interval_sum as f64 / self.rate_interval_count as f64;
            let ratio = mean / self.hop_ticks;
            if !(1.0 / RATE_TOLERANCE_FACTOR..=RATE_TOLERANCE_FACTOR).contains(&ratio) {
                return Some(Violation {
                    invariant: Invariant::Rate,
                    class: "wrong-rate",
                    frame_idx: idx,
                    detail: format!(
                        "mean {mean:.2} frames between LF changes over {} intervals vs \
                         expected hop {:.2} frames (ratio {ratio:.2}x, tolerance \
                         {RATE_TOLERANCE_FACTOR}x either way)",
                        self.rate_interval_count, self.hop_ticks
                    ),
                });
            }
        }

        // I5b plausibility.
        let cur = power_mean_db(lf);
        match self.baseline_mean_db {
            None => {
                self.baseline_samples.push(cur);
                if self.baseline_samples.len() >= BASELINE_FRAMES {
                    let mean = self.baseline_samples.iter().sum::<f64>()
                        / self.baseline_samples.len() as f64;
                    self.baseline_mean_db = Some(mean);
                }
            }
            Some(baseline) => {
                self.counts.plausibility += 1;
                let err = (cur - baseline).abs();
                self.stats.plausibility_max_err_db = self.stats.plausibility_max_err_db.max(err);
                self.plausibility_window
                    .push_back(err > PLAUSIBILITY_TOL_DB);
                while self.plausibility_window.len() > PLAUSIBILITY_WINDOW {
                    self.plausibility_window.pop_front();
                }
                let bad = self.plausibility_window.iter().filter(|&&b| b).count();
                if self.plausibility_window.len() >= PLAUSIBILITY_WINDOW
                    && bad >= PLAUSIBILITY_WINDOW_MIN_VIOLATIONS
                {
                    return Some(Violation {
                        invariant: Invariant::Plausibility,
                        class: "drift",
                        frame_idx: idx,
                        detail: format!(
                            "LF power-mean {cur:.2} dBFS departs from post-settle baseline \
                             {baseline:.2} dBFS by {err:.2} dB (tol {PLAUSIBILITY_TOL_DB} dB) — \
                             {bad}/{} recent frames out of tolerance",
                            self.plausibility_window.len()
                        ),
                    });
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    //! Synthetic-sequence self-tests: each proves one invariant goes red on
    //! the fault it exists for, and that a clean stream stays green.

    use super::*;

    const CROSSOVER_HZ: f64 = 750.0;
    const LF_COLS: usize = 7;
    const HF_COLS: usize = 7;

    /// LF columns at 100..700 Hz (below the crossover), HF at 800..1400 Hz.
    /// With the wire blend width the LF-only edge is ≈668 Hz, so the 700 Hz
    /// column is a blend column and 100..600 Hz is the LF-only slice.
    fn frame(lf: &[f64], hf: &[f64]) -> SoakFrame {
        let mut freqs = Vec::new();
        let mut spectrum = Vec::new();
        for (i, &v) in lf.iter().enumerate() {
            freqs.push(100.0 + i as f64 * 100.0);
            spectrum.push(v);
        }
        for (i, &v) in hf.iter().enumerate() {
            freqs.push(800.0 + i as f64 * 100.0);
            spectrum.push(v);
        }
        SoakFrame { freqs, spectrum }
    }

    /// LF content for the recompute numbered `k`: varies with `k`, like
    /// noise-driven content, and sits within ±2 dB of `base`.
    fn lf_content(k: usize, base: f64) -> Vec<f64> {
        (0..LF_COLS)
            .map(|c| base + ((k as f64) * 0.7 + c as f64).sin() * 2.0)
            .collect()
    }

    /// Feed `n` frames from `make(i)`; return the first violation.
    fn run(
        hop_ticks: f64,
        settle: usize,
        n: usize,
        make: impl Fn(usize) -> SoakFrame,
    ) -> (Option<Violation>, CheckCounts) {
        let mut c = SoakChecker::new(
            CROSSOVER_HZ,
            lf_only_edge_hz(CROSSOVER_HZ),
            hop_ticks,
            settle,
        );
        for i in 0..n {
            if let Some(v) = c.check_frame(&make(i)) {
                return (Some(v), c.counts());
            }
        }
        (None, c.counts())
    }

    /// A healthy stream: LF recomputes every `period` frames (the daemon's
    /// actual `hop_ticks + 1`), HF tracks the LF level.
    fn healthy(i: usize, period: usize) -> SoakFrame {
        frame(&lf_content(i / period, -40.0), &[-40.0; HF_COLS])
    }

    #[test]
    fn clean_stream_passes_and_every_invariant_is_exercised() {
        let (v, counts) = run(4.0, 20, 400, |i| healthy(i, 5));
        assert!(v.is_none(), "clean stream flagged: {v:?}");
        assert_eq!(counts.bounded, 400);
        assert_eq!(counts.continuity, 380);
        assert_eq!(counts.liveness, 380);
        assert!(counts.rate > 300, "{counts:?}");
        assert!(counts.plausibility > 300, "{counts:?}");
        assert!(counts.lf_changes >= 70, "{counts:?}");
    }

    #[test]
    fn late_freeze_is_frozen_not_before_it_starts() {
        const FREEZE_AT: usize = 100; // after settle (20) and baseline (+5)
        let (v, _) = run(4.0, 20, 300, |i| healthy(i.min(FREEZE_AT), 5));
        let v = v.expect("late freeze not caught");
        assert_eq!(v.invariant, Invariant::Liveness, "{v:?}");
        assert_eq!(v.class, "frozen");
        assert!(v.frame_idx > FREEZE_AT, "fired before the freeze: {v:?}");
        // No later than 2x hop past the last change it could have seen.
        assert!(v.frame_idx <= FREEZE_AT + 9, "fired late: {v:?}");
    }

    #[test]
    fn freeze_during_settle_is_not_judged() {
        // Frozen only inside the settle window, healthy after: no violation.
        let (v, _) = run(4.0, 50, 200, |i| {
            if i < 50 {
                healthy(0, 5)
            } else {
                healthy(i, 5)
            }
        });
        assert!(v.is_none(), "{v:?}");
    }

    #[test]
    fn changing_every_frame_at_wrong_level_is_plausibility() {
        // hop_ticks 1 and a change every frame: rate is right, liveness is
        // fine. After the baseline the level drops 20 dB — HF follows so the
        // splice stays continuous. Only I5b can see it.
        let (v, _) = run(1.0, 20, 200, |i| {
            let base = if i < 60 { -40.0 } else { -60.0 };
            frame(&lf_content(i, base), &[base; HF_COLS])
        });
        let v = v.expect("wrong-level drift not caught");
        assert_eq!(v.invariant, Invariant::Plausibility, "{v:?}");
        assert!(v.frame_idx > 60, "{v:?}");
    }

    #[test]
    fn changing_every_frame_seven_times_too_fast_is_rate_not_liveness() {
        let (v, _) = run(7.0, 20, 200, |i| healthy(i, 1));
        let v = v.expect("7x-too-fast LF not caught");
        assert_eq!(v.invariant, Invariant::Rate, "{v:?}");
        assert_eq!(v.class, "wrong-rate");
    }

    #[test]
    fn under_refreshing_is_caught_by_liveness() {
        // A mean interval above 3x hop needs some interval above 3x hop,
        // and I5a fires at 2x — so the slow side of I5c is covered by I5a,
        // which judges first. 13 frames per change against hop 4 (3.25x).
        let (v, _) = run(4.0, 0, 200, |i| healthy(i, 13));
        let v = v.expect("under-refreshing LF not caught");
        assert_eq!(v.invariant, Invariant::Liveness, "{v:?}");
    }

    /// The splice step the two continuity tests inject, measured the same
    /// way the checker measures it — so a change to `lf_content` that shrank
    /// the step below tolerance would fail here, not make the lone-step
    /// test pass for the wrong reason.
    fn injected_step_db() -> f64 {
        let lf = lf_content(100 / 5, -40.0);
        let lf_edge = power_mean_db(&lf[LF_COLS - SPLICE_COLS..]);
        (-31.0 - lf_edge).abs()
    }

    #[test]
    fn lone_splice_step_is_not_a_violation() {
        let step = injected_step_db();
        assert!(
            step > CONTINUITY_TOL_DB && step < CONTINUITY_TOL_DB + 1.0,
            "injected step {step:.2} dB must sit just over tolerance"
        );
        let (v, _) = run(4.0, 20, 200, |i| {
            let hf = if i == 100 { -31.0 } else { -40.0 };
            frame(&lf_content(i / 5, -40.0), &[hf; HF_COLS])
        });
        assert!(v.is_none(), "lone {step:.2} dB step flagged: {v:?}");
    }

    /// The healthy-margin readout counts what it says: a lone step and a
    /// pair of steps give 3 frames over tolerance, longest streak 2, and the
    /// max step is the injected one. All three sit in one LF recompute
    /// (frames 100-104), so each step is exactly [`injected_step_db`].
    #[test]
    fn stats_readout_records_over_tolerance_steps_and_streaks() {
        let step = injected_step_db();
        let mut c = SoakChecker::new(CROSSOVER_HZ, lf_only_edge_hz(CROSSOVER_HZ), 4.0, 20);
        for i in 0..200 {
            let hf = if [100, 102, 103].contains(&i) {
                -31.0
            } else {
                -40.0
            };
            let v = c.check_frame(&frame(&lf_content(i / 5, -40.0), &[hf; HF_COLS]));
            assert!(v.is_none(), "frame {i} flagged: {v:?}");
        }
        let s = c.stats();
        assert_eq!(s.continuity_over_tol, 3, "{s:?}");
        assert_eq!(s.continuity_longest_streak, 2, "{s:?}");
        assert!(
            (s.continuity_max_step_db - step).abs() < 1e-9,
            "max step {} vs injected {step}",
            s.continuity_max_step_db
        );
        assert!(
            s.plausibility_max_err_db.is_finite()
                && s.plausibility_max_err_db < PLAUSIBILITY_TOL_DB,
            "{s:?}"
        );
    }

    #[test]
    fn three_consecutive_splice_steps_are_continuity() {
        let (v, _) = run(4.0, 20, 200, |i| {
            let hf = if (100..103).contains(&i) {
                -31.0
            } else {
                -40.0
            };
            frame(&lf_content(i / 5, -40.0), &[hf; HF_COLS])
        });
        let v = v.expect("sustained splice step not caught");
        assert_eq!(v.invariant, Invariant::Continuity, "{v:?}");
        assert_eq!(v.frame_idx, 102);
    }

    #[test]
    fn over_range_bin_in_warm_up_is_bounded() {
        let (v, _) = run(4.0, 1000, 10, |i| {
            let mut f = healthy(i, 5);
            if i == 1 {
                f.spectrum[3] = 2.0;
            }
            f
        });
        let v = v.expect("+2 dBFS bin in warm-up not caught");
        assert_eq!(v.invariant, Invariant::Bounded, "{v:?}");
        assert_eq!(v.class, "level-jump");
        assert_eq!(v.frame_idx, 1);
    }

    #[test]
    fn exact_zero_amplitude_is_legal_for_bounded() {
        // -inf dBFS is an exact-zero amplitude on the wire, not garbage.
        let mut f = healthy(0, 5);
        f.spectrum[9] = f64::NEG_INFINITY;
        assert!(check_bounded(&f, 0).is_none());
        f.spectrum[9] = f64::INFINITY;
        let v = check_bounded(&f, 0).expect("+inf not caught");
        assert_eq!(v.class, "garbage");
    }

    #[test]
    fn collapsed_lf_band_is_plausibility() {
        // From frame 60 the LF-only slice (100..600 Hz) is exact zero
        // amplitude, -inf dBFS; the 700 Hz blend column stays live. I4-t
        // lets -inf through; I5b needs 5 bad frames and I5a needs more than
        // 2 x 4, so the collapse is reported as plausibility, not a freeze.
        const COLLAPSE_AT: usize = 60;
        let (v, _) = run(4.0, 20, 200, |i| {
            let mut lf = lf_content(i / 4, -40.0);
            if i >= COLLAPSE_AT {
                for x in lf.iter_mut().take(LF_COLS - 1) {
                    *x = f64::NEG_INFINITY;
                }
            }
            frame(&lf, &[-40.0; HF_COLS])
        });
        let v = v.expect("collapsed LF band not caught");
        assert_eq!(v.invariant, Invariant::Plausibility, "{v:?}");
        assert_eq!(v.class, "drift");
        assert!(v.frame_idx >= COLLAPSE_AT, "{v:?}");
    }

    #[test]
    fn blend_columns_outside_lf_edge_do_not_count_as_lf_changes() {
        // LF-only columns change once every hop_ticks frames; the blend
        // column (700 Hz, in [edge, crossover)) changes every frame, as HF
        // does on the daemon. The checker built with the wire blend width
        // must stay green; the rejected slice (edge = crossover_hz), computed
        // here, must read the blend churn as LF changing every frame.
        const HOP: usize = 4;
        let make = |i: usize| {
            let mut lf = lf_content(i / HOP, -40.0);
            lf[LF_COLS - 1] = -40.0 + (i as f64 * 1.3).sin();
            frame(&lf, &[-40.0; HF_COLS])
        };
        let edge = lf_only_edge_hz(CROSSOVER_HZ);
        assert!(
            edge > 600.0 && edge < 700.0,
            "LF-only edge {edge:.1} Hz must separate the 600 Hz LF column from \
             the 700 Hz blend column"
        );

        let mut good = SoakChecker::new(CROSSOVER_HZ, edge, HOP as f64, 20);
        let mut rejected = SoakChecker::new(CROSSOVER_HZ, CROSSOVER_HZ, HOP as f64, 20);
        let mut rejected_v = None;
        for i in 0..300 {
            let f = make(i);
            if let Some(v) = good.check_frame(&f) {
                panic!("LF-edge checker flagged a healthy stream: {v:?}");
            }
            if rejected_v.is_none() {
                rejected_v = rejected.check_frame(&f);
            }
        }
        assert!(good.counts().rate > 200, "{:?}", good.counts());
        let v = rejected_v.expect("slice at crossover_hz did not go red on blend churn");
        assert_eq!(v.invariant, Invariant::Rate, "{v:?}");
        assert_eq!(v.class, "wrong-rate");
    }

    #[test]
    fn nan_is_bounded_garbage() {
        let (v, _) = run(4.0, 1000, 10, |i| {
            let mut f = healthy(i, 5);
            if i == 4 {
                f.spectrum[9] = f64::NAN;
            }
            f
        });
        let v = v.expect("NaN not caught");
        assert_eq!(v.invariant, Invariant::Bounded);
        assert_eq!(v.class, "garbage");
    }

    #[test]
    fn no_lf_columns_leaves_lf_invariants_unexercised() {
        // Crossover below every column: LF slice empty. The counts must show
        // it, so the runner's coverage assertion fails rather than passes
        // vacuously.
        let mut c = SoakChecker::new(10.0, lf_only_edge_hz(10.0), 4.0, 0);
        for i in 0..100 {
            assert!(c.check_frame(&healthy(i, 5)).is_none());
        }
        let k = c.counts();
        assert_eq!(k.bounded, 100);
        assert_eq!(k.continuity, 0);
        assert_eq!(k.liveness, 0);
        assert_eq!(k.rate, 0);
        assert_eq!(k.plausibility, 0);
        assert_eq!(k.lf_changes, 0);
    }
}
