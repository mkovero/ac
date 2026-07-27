//! Stage layout for the multi-time-window ladder.
//!
//! Every number here is derived from `sr`. There are no rate-specific
//! constants and no tabulated decimation factors: the stages are specified by
//! **target decimated rate**, and the factors fall out of `round(sr / target)`.
//! Specifying by rate rather than by factor is what makes the ladder behave
//! identically at 48 and 96 kHz — a fixed 4x step does not.
//!
//! # Octave convention (deliverable 7)
//!
//! The log-frequency grid here is built on `2^(1/P)` — the MTW convention,
//! where an octave is exactly a factor of two. IEC 61260-1 uses the base-ten
//! octave ratio `G = 10^(3/10) = 1.99526`, which this crate keeps in
//! [`crate::shared::constants::G_OCTAVE`] for the Tier 1 filterbank.
//!
//! **The two differ by 0.24% and must not be unified.** They are not two
//! spellings of one constant: `G_OCTAVE` is normative for a standards-claiming
//! filterbank, and `2` is what a decimating ladder physically does — each stage
//! halves (here, quarters) a rate, and no amount of convention can make that
//! land on `10^(3/10)`. Rewriting the expressions below in terms of `G_OCTAVE`
//! would silently move every crossover and every column edge by 0.24% while
//! looking like a tidy-up.

use std::fmt;

/// FFT length at every stage. Fixed across the ladder so a stage's resolution
/// and window are determined entirely by its decimated rate.
pub const NFFT: usize = 4096;

/// Segment hop in samples — 50% overlap, matching the Hann window the
/// estimator uses. [`crate::visualize::mtw::average::HANN_50_RHO`] is the block
/// correlation this overlap implies, and the two must move together.
pub const HOP: usize = NFFT / 2;

/// The density the ladder is **built** to support, in points per octave.
///
/// This is a property of the ladder, not of the display. The live view's
/// points-per-octave is a separate parameter (see
/// [`column_edges`](crate::visualize::mtw::ladder::column_edges)) and may be
/// higher or lower; where it asks for more than the serving stage can deliver,
/// the column grid thins out rather than the crossovers moving.
///
/// Keeping these two apart is load-bearing. If the crossovers were derived
/// from the *display* density, changing a display setting would change the
/// window and averaging behind every column in the midrange, and two
/// screenshots taken at different densities would not be comparable.
pub const P_REF: f64 = 48.0;

/// Target decimated rates below full rate, shallowest first.
///
/// Fixed rates rather than fixed factors: stage 1 is 12 kHz whether the
/// interface runs at 48 or 96 kHz, so its resolution, window and validity edge
/// are identical at both.
///
/// The bottom rung is 4 kHz, giving 0.98 Hz resolution honest to 67.6 Hz — the
/// same reach the full-rate estimator has today. Going deeper is bench mode's
/// job: at 4 kHz the bottom settles in 2.56 s, matching today, and every step
/// finer costs settling time in the one mode that cannot afford it.
pub const TARGET_RATES: [f64; 2] = [12_000.0, 4_000.0];

/// Width of the crossover blend, in octaves.
pub const BLEND_OCTAVES: f64 = 1.0 / 3.0;

/// Largest fraction of a stage's decimated rate its served band may reach.
///
/// The anti-alias filter has to pass everything the stage serves and reach
/// full stopband by `rate - served_top`, so `served_top` must stay well under
/// half the decimated rate or the transition band vanishes. 0.45 leaves the
/// filter designable at every rate this ladder is used at.
const MAX_TOP_FRACTION: f64 = 0.45;

/// Cap on stages inserted above [`TARGET_RATES`] before giving up. Reached
/// only at absurd sample rates; exists so a bad `sr` cannot spin.
const MAX_INSERTED_STAGES: usize = 6;

/// Frequency-to-resolution ratio at which a log grid of `ppo` points per
/// octave still has at least one FFT bin per column.
///
/// A column centred at `f` spans `f · (2^(1/2P) − 2^(−1/2P))`, so it holds a
/// bin only for `f ≥ Δf · kappa(P)`. `kappa(48) = 69.2488`, which is why a
/// 1 Hz-resolution estimate can honestly fill a 1/48-octave grid only above
/// 69.25 Hz.
pub fn kappa(ppo: f64) -> f64 {
    let e = 1.0 / (2.0 * ppo);
    1.0 / (2f64.powf(e) - 2f64.powf(-e))
}

/// One rung of the ladder.
#[derive(Clone, Debug, PartialEq)]
pub struct Stage {
    /// Decimation factor from full rate. 1 for stage 0.
    pub decim: usize,
    /// Decimated sample rate, `sr / decim`.
    pub rate: f64,
    /// Bin width, `rate / NFFT`.
    pub df: f64,
    /// Analysis window length in seconds, `NFFT / rate`.
    pub window_s: f64,
    /// Segment hop in seconds, `HOP / rate`.
    pub hop_s: f64,
    /// Lowest frequency at which this stage supports [`P_REF`] points per
    /// octave — its **validity edge**. Below this its columns are wider than
    /// the reference grid asks for.
    pub f_valid: f64,
    /// Frequency at which this stage begins handing over to the stage above
    /// it. Equal to the shallower stage's `f_valid`; the ladder's Nyquist for
    /// stage 0.
    pub f_top: f64,
    /// Top of the blend region. Above this the shallower stage is used alone.
    /// `f_top · 2^BLEND_OCTAVES`, and the highest frequency this stage is ever
    /// read at.
    pub blend_top: f64,
}

/// The full stage list for one sample rate, shallowest (full rate) first.
#[derive(Clone, Debug, PartialEq)]
pub struct Ladder {
    pub sr: u32,
    pub stages: Vec<Stage>,
}

/// Which stage(s) serve a display frequency, and with what weight.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Source {
    /// Deeper (longer-window, finer-resolution) stage. Always present.
    pub deep: usize,
    /// Shallower stage, present only inside a crossover blend.
    pub shallow: Option<usize>,
    /// Weight of `shallow` in `[0, 1]`. Zero when `shallow` is `None`.
    pub w_shallow: f64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LadderError {
    ZeroRate,
    /// `sr` is high enough that stage 1's served band would not fit inside its
    /// decimator's passband, and inserting stages did not resolve it.
    RateTooHigh(u32),
}

impl fmt::Display for LadderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LadderError::ZeroRate => write!(f, "sample rate must be non-zero"),
            LadderError::RateTooHigh(sr) => write!(
                f,
                "sample rate {sr} Hz needs more than {MAX_INSERTED_STAGES} inserted ladder stages"
            ),
        }
    }
}

impl std::error::Error for LadderError {}

/// Derive the stage list for `sr`.
///
/// Stage 0 is always full rate. Each subsequent stage decimates to the nearest
/// achievable approximation of its target rate; a target that would round to
/// the same factor as the stage above it is dropped rather than duplicated, so
/// low sample rates get a shorter ladder instead of degenerate rungs.
///
/// The number of stages is not fixed at three. Above roughly 319 kHz stage 1's
/// served band no longer fits inside its own decimator's passband, and an
/// intermediate stage is inserted. That is why this returns a `Vec` and a
/// `Result` rather than a `[Stage; 3]`: a fixed-length ladder passes every
/// rate in use today and breaks silently on the first faster device.
pub fn layout(sr: u32) -> Result<Ladder, LadderError> {
    if sr == 0 {
        return Err(LadderError::ZeroRate);
    }
    let srf = f64::from(sr);
    // Stage 1's top edge is set by stage 0's resolution, which is set by `sr`.
    // This is the quantity the passband guard is about.
    let f_top_stage1 = kappa(P_REF) * srf / NFFT as f64;

    let mut targets: Vec<f64> = TARGET_RATES.to_vec();
    let mut inserted = 0usize;
    let decims = loop {
        let decims = derive_decims(srf, &targets);
        // Only the stage-0 -> stage-1 boundary can violate the guard: every
        // other boundary steps by the fixed 4x between adjacent targets, which
        // puts the top edge at 6.8% of the decimated rate by construction.
        let fits = match decims.get(1) {
            None => true,
            Some(&m1) => f_top_stage1 <= MAX_TOP_FRACTION * (srf / m1 as f64),
        };
        if fits {
            break decims;
        }
        if inserted >= MAX_INSERTED_STAGES {
            return Err(LadderError::RateTooHigh(sr));
        }
        targets.insert(0, targets[0] * 4.0);
        inserted += 1;
    };

    let k = kappa(P_REF);
    let blend_ratio = 2f64.powf(BLEND_OCTAVES);
    let nyquist = srf / 2.0;

    let mut stages: Vec<Stage> = Vec::with_capacity(decims.len());
    for (i, &m) in decims.iter().enumerate() {
        let rate = srf / m as f64;
        let df = rate / NFFT as f64;
        let f_valid = k * df;
        // Stage 0 runs to Nyquist and blends with nothing above it.
        let (f_top, blend_top) = if i == 0 {
            (nyquist, nyquist)
        } else {
            let top = stages[i - 1].f_valid;
            (top, top * blend_ratio)
        };
        stages.push(Stage {
            decim: m,
            rate,
            df,
            window_s: NFFT as f64 / rate,
            hop_s: HOP as f64 / rate,
            f_valid,
            f_top,
            blend_top,
        });
    }

    Ok(Ladder { sr, stages })
}

/// `[1, round(sr/t)...]`, dropping targets that would not deepen the ladder.
fn derive_decims(srf: f64, targets: &[f64]) -> Vec<usize> {
    let mut decims = vec![1usize];
    for &t in targets {
        let m = (srf / t).round().max(1.0) as usize;
        if m > *decims.last().expect("seeded with stage 0") {
            decims.push(m);
        }
    }
    decims
}

impl Ladder {
    /// The deepest stage — the one with the finest resolution and the longest
    /// window.
    pub fn deepest(&self) -> &Stage {
        self.stages.last().expect("layout always yields stage 0")
    }

    /// Lowest frequency the ladder supports at [`P_REF`] points per octave.
    /// Below this the column grid widens; nothing is synthesised.
    pub fn f_valid_min(&self) -> f64 {
        self.deepest().f_valid
    }

    /// Which stage(s) serve display frequency `f`.
    ///
    /// Searched deepest-first: a frequency is served by the deepest stage that
    /// still reaches it, and only handed up where the shallower stage has
    /// become valid. Inside a crossover the shallower stage ramps in over
    /// [`BLEND_OCTAVES`], starting **at** its own validity edge — never below
    /// it, so no column is ever drawn from a stage that cannot support it.
    pub fn source_at(&self, f: f64) -> Source {
        for i in (1..self.stages.len()).rev() {
            let s = &self.stages[i];
            // Inclusive at `blend_top`: the blend must *complete* there, handing
            // fully to the shallower stage. Excluding it drops the weight back to
            // zero for one column and puts a notch at every crossover.
            if f > s.blend_top {
                continue;
            }
            if f <= s.f_top {
                return Source {
                    deep: i,
                    shallow: None,
                    w_shallow: 0.0,
                };
            }
            // Inside [f_top, blend_top]: cosine ramp in log frequency.
            let t = (f / s.f_top).log2() / BLEND_OCTAVES;
            let w = 0.5 - 0.5 * (std::f64::consts::PI * t.clamp(0.0, 1.0)).cos();
            return Source {
                deep: i,
                shallow: Some(i - 1),
                w_shallow: w,
            };
        }
        Source {
            deep: 0,
            shallow: None,
            w_shallow: 0.0,
        }
    }

    /// Widest bin width among the stages serving `f` — the resolution a
    /// display column at `f` has to respect for **every** contributor to hold
    /// at least one bin.
    pub fn df_at(&self, f: f64) -> f64 {
        let src = self.source_at(f);
        let mut df = self.stages[src.deep].df;
        if let Some(sh) = src.shallow {
            df = df.max(self.stages[sh].df);
        }
        df
    }
}

/// Display column edges between `f_min` and `f_max` at `ppo` points per
/// octave — **honest density**.
///
/// Each column is the wider of the requested log width and the serving stage's
/// bin width, so every emitted column spans at least one bin of every stage
/// that feeds it. Where the requested density exceeds the available
/// resolution the columns widen and the count drops; nothing is interpolated
/// and no column is synthesised from its neighbours.
///
/// Returns `n + 1` edges for `n` columns, or an empty vec for a degenerate
/// range.
// Negated `>` comparisons are intentional NaN-aware guards: `!(f_min > 0.0)`
// is true for NaN, zero and negative inputs, all of which must short-circuit.
#[allow(clippy::neg_cmp_op_on_partial_ord)]
pub fn column_edges(ladder: &Ladder, f_min: f64, f_max: f64, ppo: f64) -> Vec<f64> {
    if !(f_min > 0.0) || !(f_max > f_min) || !(ppo > 0.0) {
        return Vec::new();
    }
    let step = 2f64.powf(1.0 / ppo);
    let mut edges = vec![f_min];
    let mut f = f_min;
    while f < f_max {
        // `df_at` is non-decreasing in frequency (a shallower stage has the
        // coarser resolution), so the binding constraint for a column is the
        // requirement at its *upper* edge, not its lower one. A column that
        // straddles a crossover has to be wide enough for the stage it is
        // handing over to, or the blend would read a stage with no bin in it.
        // Two or three passes settle it — `df_at` takes at most one value per
        // stage.
        let mut next = (f * step).max(f + ladder.df_at(f));
        for _ in 0..8 {
            let cand = (f * step).max(f + ladder.df_at(next));
            if cand <= next + 1e-12 {
                break;
            }
            next = cand;
        }
        if next >= f_max {
            // The remainder is too narrow to stand as its own column: widen
            // the last one to reach `f_max` rather than emitting a column
            // that no stage can back.
            if f_max - f >= ladder.df_at(f_max) {
                edges.push(f_max);
            } else if edges.len() >= 2 {
                *edges.last_mut().expect("len >= 2") = f_max;
            } else {
                // The whole range is narrower than one bin. There is no honest
                // column to emit.
                return Vec::new();
            }
            break;
        }
        edges.push(next);
        f = next;
    }
    if edges.len() < 2 {
        return Vec::new();
    }
    edges
}

/// Geometric centre of each column, from [`column_edges`] output.
pub fn column_centres(edges: &[f64]) -> Vec<f64> {
    edges.windows(2).map(|w| (w[0] * w[1]).sqrt()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The constant the handoff's own arithmetic implies. At `Δf = 1 Hz` the
    /// validity edge lands at 69.25 Hz, and `48·log2(69.2488/20) = 86.01` —
    /// the "86 columns are invented" figure. Independently re-derived here
    /// rather than copied from a comment.
    #[test]
    fn kappa_matches_the_reference_density() {
        assert!((kappa(48.0) - 69.2488).abs() < 1e-3, "{}", kappa(48.0));
        let invented = 48.0 * (kappa(48.0) / 20.0).log2();
        assert!((invented - 86.0).abs() < 0.05, "{invented}");
        // Denser grids need proportionally more resolution.
        assert!(kappa(96.0) > kappa(48.0));
    }

    /// Criterion 2: the layout derives from `sr` with no rate-specific
    /// constants. Expected factors and rates are written out per rate because
    /// a test that recomputed them with the implementation's own expression
    /// would assert nothing.
    #[test]
    fn ladder_derives_from_sr_at_every_supported_rate() {
        for (sr, want_decims, want_rates) in [
            (
                44_100u32,
                vec![1usize, 4, 11],
                vec![44_100.0, 11_025.0, 4_009.090_909_090_909],
            ),
            (48_000, vec![1, 4, 12], vec![48_000.0, 12_000.0, 4_000.0]),
            (96_000, vec![1, 8, 24], vec![96_000.0, 12_000.0, 4_000.0]),
            (192_000, vec![1, 16, 48], vec![192_000.0, 12_000.0, 4_000.0]),
        ] {
            let l = layout(sr).expect("layout");
            let decims: Vec<usize> = l.stages.iter().map(|s| s.decim).collect();
            assert_eq!(decims, want_decims, "decimation factors at {sr}");
            for (s, want) in l.stages.iter().zip(want_rates.iter()) {
                assert!((s.rate - want).abs() < 1e-6, "rate at {sr}: {s:?}");
            }
        }
    }

    /// The two deep stages are rate-independent by construction — that is the
    /// entire reason the ladder is specified by decimated rate. 44.1 kHz is
    /// the accepted exception (4009 Hz, 0.23% off target).
    #[test]
    fn deep_stages_are_identical_at_48_96_and_192_khz() {
        let mut seen: Vec<(f64, f64)> = Vec::new();
        for sr in [48_000u32, 96_000, 192_000] {
            let l = layout(sr).unwrap();
            seen.push((l.stages[1].window_s, l.stages[2].window_s));
        }
        for w in &seen {
            assert!((w.0 - seen[0].0).abs() < 1e-12, "stage 1 window moved");
            assert!((w.1 - seen[0].1).abs() < 1e-12, "stage 2 window moved");
        }
        // And the accepted 44.1 kHz variance, stated rather than discovered:
        // 4009 Hz against the 4000 Hz target, 0.23% off.
        let l = layout(44_100).unwrap();
        assert!(
            (l.stages[2].rate - 4_009.090_9).abs() < 1e-3,
            "{:?}",
            l.stages[2]
        );
        assert!((l.stages[2].window_s - 1.021_678).abs() < 1e-5);
    }

    /// Independently re-derived validity edges and windows: `f_valid = κ·Δf`,
    /// `Δf = rate/4096`, `W = 4096/rate`.
    #[test]
    fn validity_edges_and_windows_match_the_analytic_values() {
        let l = layout(48_000).unwrap();
        let want = [(811.51, 0.085_333), (202.88, 0.341_333), (67.63, 1.024_000)];
        for (s, (f_valid, window)) in l.stages.iter().zip(want.iter()) {
            assert!((s.f_valid - f_valid).abs() < 0.02, "{s:?}");
            assert!((s.window_s - window).abs() < 1e-5, "{s:?}");
        }
        // Crossovers are the shallower stage's validity edge, not a constant.
        assert!((l.stages[1].f_top - l.stages[0].f_valid).abs() < 1e-12);
        assert!((l.stages[2].f_top - l.stages[1].f_valid).abs() < 1e-12);
        // At 96 kHz stage 0 resolves further down, so the crossover moves with
        // `sr` — a fixed fraction-of-Nyquist crossover would not.
        let l96 = layout(96_000).unwrap();
        assert!(
            (l96.stages[1].f_top - 1_623.0).abs() < 0.05,
            "{:?}",
            l96.stages[1]
        );
    }

    /// The served band must sit inside the decimator's passband at every rate,
    /// with 192 kHz — the highest rate the acceptance criteria require — the
    /// tightest case.
    #[test]
    fn served_band_stays_inside_every_decimators_passband() {
        for sr in [44_100u32, 48_000, 96_000, 192_000] {
            let l = layout(sr).unwrap();
            for s in l.stages.iter().skip(1) {
                assert!(
                    s.blend_top < MAX_TOP_FRACTION * s.rate,
                    "sr {sr}: stage serves to {} of rate {}",
                    s.blend_top,
                    s.rate
                );
            }
        }
    }

    /// The rate ceiling is real and handled by inserting a stage, not by
    /// silently producing an undesignable filter.
    #[test]
    fn very_high_rates_gain_a_stage_rather_than_breaking() {
        let l = layout(384_000).unwrap();
        assert_eq!(
            l.stages.len(),
            4,
            "384 kHz must not fit the three-stage ladder: {:?}",
            l.stages.iter().map(|s| s.rate).collect::<Vec<_>>()
        );
        assert!((l.stages[1].rate - 48_000.0).abs() < 1e-6);
        for s in l.stages.iter().skip(1) {
            assert!(s.blend_top < MAX_TOP_FRACTION * s.rate, "{s:?}");
        }
    }

    /// Low rates get a shorter ladder rather than duplicate rungs.
    #[test]
    fn low_rates_drop_degenerate_stages() {
        let l = layout(8_000).unwrap();
        let decims: Vec<usize> = l.stages.iter().map(|s| s.decim).collect();
        assert_eq!(decims, vec![1, 2], "12 kHz target cannot deepen 8 kHz");
        assert_eq!(layout(0), Err(LadderError::ZeroRate));
    }

    /// A blend must never draw on a stage below its own validity edge —
    /// criterion 1, at the one place the crossover could break it.
    #[test]
    fn blend_starts_at_the_shallower_stages_validity_edge() {
        let l = layout(48_000).unwrap();
        for i in 1..l.stages.len() {
            let shallow = &l.stages[i - 1];
            // Just inside the blend, the shallower stage has weight > 0 and is
            // at or above its own validity edge.
            let f = l.stages[i].f_top * 1.001;
            let src = l.source_at(f);
            assert_eq!(src.shallow, Some(i - 1), "at {f}");
            assert!(src.w_shallow > 0.0);
            assert!(
                f >= shallow.f_valid,
                "blend at {f} reaches below stage {}'s validity edge {}",
                i - 1,
                shallow.f_valid
            );
        }
    }

    /// Blend weight runs 0 -> 1 across the crossover and is monotone, so the
    /// splice cannot double back on itself.
    #[test]
    fn blend_weight_ramps_monotonically_from_zero_to_one() {
        let l = layout(48_000).unwrap();
        let s = &l.stages[1];
        assert_eq!(l.source_at(s.f_top).w_shallow, 0.0);
        let mut prev = 0.0;
        for i in 0..=40 {
            let f = s.f_top * 2f64.powf(BLEND_OCTAVES * i as f64 / 40.0);
            let w = l.source_at(f).w_shallow;
            assert!(w >= prev - 1e-12, "weight fell at {f}: {prev} -> {w}");
            prev = w;
        }
        assert!((prev - 1.0).abs() < 1e-9, "blend never completes: {prev}");
        // Above the blend the shallower stage is used alone.
        assert_eq!(l.source_at(s.blend_top * 1.01).deep, 0);
    }

    /// Criterion 1, structurally: every column spans at least one bin of every
    /// stage that feeds it, at every rate and at densities either side of the
    /// ladder's reference.
    #[test]
    fn every_column_spans_at_least_one_bin() {
        for sr in [44_100u32, 48_000, 96_000, 192_000] {
            let l = layout(sr).unwrap();
            for ppo in [12.0, 48.0, 96.0, 192.0] {
                let edges = column_edges(&l, 20.0, f64::from(sr) / 2.0, ppo);
                assert!(edges.len() > 2, "sr {sr} ppo {ppo}");
                for w in edges.windows(2) {
                    let (lo, hi) = (w[0], w[1]);
                    // At the upper edge: `df_at` is non-decreasing, so this is
                    // the widest resolution any contributor to the column has.
                    let df = l.df_at(hi);
                    assert!(
                        hi - lo >= df - 1e-9,
                        "sr {sr} ppo {ppo}: column [{lo}, {hi}] is narrower than Δf {df}"
                    );
                }
            }
        }
    }

    /// Density follows resolution: raising the display density adds columns
    /// only where the ladder can back them, and never moves a crossover.
    #[test]
    fn density_is_a_parameter_but_crossovers_are_not() {
        let l = layout(48_000).unwrap();
        let coarse = column_edges(&l, 20.0, 24_000.0, 48.0);
        let fine = column_edges(&l, 20.0, 24_000.0, 192.0);
        assert!(fine.len() > coarse.len(), "denser grid must add columns");

        // Below the deepest validity edge both grids are Δf-limited, so the
        // extra density buys nothing — which is the honest outcome.
        let below = |e: &[f64]| e.iter().filter(|&&f| f < l.f_valid_min()).count();
        assert_eq!(
            below(&coarse),
            below(&fine),
            "columns below the validity edge must not multiply with density"
        );

        // Crossovers are untouched by the density change.
        let tops: Vec<f64> = l.stages.iter().map(|s| s.f_top).collect();
        let l2 = layout(48_000).unwrap();
        for (a, b) in tops.iter().zip(l2.stages.iter().map(|s| s.f_top)) {
            assert!((a - b).abs() < 1e-12);
        }
    }

    /// The Δf-limited region is linear, not log — the visible consequence of
    /// dropping interpolation, and the shape a reviewer should expect.
    #[test]
    fn low_frequency_columns_are_delta_f_limited() {
        let l = layout(48_000).unwrap();
        let edges = column_edges(&l, 20.0, 24_000.0, 48.0);
        let df = l.deepest().df;
        // Widths just above f_min are one bin, not the (much narrower) log
        // width the density asks for.
        let log_width = 20.0 * (2f64.powf(1.0 / 48.0) - 1.0);
        assert!(log_width < df, "premise: 48 ppo at 20 Hz is finer than Δf");
        assert!((edges[1] - edges[0] - df).abs() < 1e-9, "{:?}", &edges[..3]);
        let n_below: usize = edges.iter().filter(|&&f| f < l.f_valid_min()).count();
        // ~(67.6 - 20)/0.977 columns, against the 84 a 1/48-octave grid would
        // have claimed there.
        assert!(
            (44..=54).contains(&n_below),
            "expected ~49 honest columns below the validity edge, got {n_below}"
        );
    }
}
