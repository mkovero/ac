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

/// FFT length at stage 0, and at every stage under [`Speed::Detail`]. The
/// deeper stages' length is the speed preset's ([`Speed::deep_nfft`], #714):
/// a shorter one trades their resolution for update rate and settling.
pub const NFFT: usize = 4096;

/// The ladder's speed preset (#714): the deeper stages' FFT length. Stage 0
/// is the same in all three.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Speed {
    /// 4096 everywhere: 0.98 Hz at the bottom, settling 2.43 s (96 kHz).
    #[default]
    Detail,
    /// 2048 below stage 0: 1.95 Hz, settling 1.22 s.
    Live,
    /// 1024 below stage 0: 3.9 Hz, settling 0.61 s.
    Follow,
}

impl Speed {
    pub const ALL: [Speed; 3] = [Speed::Detail, Speed::Live, Speed::Follow];

    /// FFT length of every stage below stage 0.
    pub fn deep_nfft(self) -> usize {
        match self {
            Speed::Detail => NFFT,
            Speed::Live => NFFT / 2,
            Speed::Follow => NFFT / 4,
        }
    }

    /// The wire and request tag.
    pub fn tag(self) -> &'static str {
        match self {
            Speed::Detail => "detail",
            Speed::Live => "live",
            Speed::Follow => "follow",
        }
    }

    pub fn from_tag(tag: &str) -> Option<Speed> {
        Speed::ALL.into_iter().find(|s| s.tag() == tag)
    }

    /// The operator-facing name.
    pub fn label(self) -> &'static str {
        match self {
            Speed::Detail => "Detail",
            Speed::Live => "Live",
            Speed::Follow => "Follow",
        }
    }

    /// The preset whose deep stages run `nfft`-point FFTs.
    pub fn for_deep_nfft(nfft: usize) -> Option<Speed> {
        Speed::ALL.into_iter().find(|s| s.deep_nfft() == nfft)
    }

    /// Detail → Live → Follow → Detail.
    pub fn next(self) -> Speed {
        match self {
            Speed::Detail => Speed::Live,
            Speed::Live => Speed::Follow,
            Speed::Follow => Speed::Detail,
        }
    }
}

/// Stage 0's segment hop in samples — 50% overlap, matching the Hann window
/// the estimator uses. The deeper stages hop more often ([`HOP_DIVISORS`]).
pub const HOP: usize = NFFT / 2;

/// `NFFT / hop` per stage, shallowest first: 50 %, 75 % and 87.5 % overlap.
///
/// A deeper stage's window is longer, and at 50 % overlap it moved only once
/// per half window: the bottom rung (1.024 s) every 0.51 s and settled a
/// change in 2.56 s — the operator's "completely unusable" for live
/// monitoring below ~250 Hz (rig, 2026-09-28). Overlap buys update rate
/// without touching resolution: the bottom now moves every 0.128 s, the
/// middle every 0.085 s (#699). Overlapping blocks share samples, so the
/// deeper stages average more of them ([`STAGE_BLOCKS`]).
pub const HOP_DIVISORS: [usize; 3] = [2, 4, 8];

/// Blocks each stage averages, as a multiple `num/den` of the session's
/// base `N`, shallowest first: 4, 6 and 12 blocks at `N = 4`.
///
/// Overlap alone would have shortened what a deeper stage averages over,
/// and a coherence estimate is only as good as its distinct data: at 87.5 %
/// overlap and 4 blocks, two unrelated signals read 0.68 on average. These
/// counts keep the floor where 50 % × 4 put it — measured 0.244, 0.266,
/// 0.268 on uncorrelated inputs, against 0.44 at the bottom with half the
/// blocks (`each_stage_keeps_the_uniform_ladders_coherence_floor`) — so the
/// deeper stages move more often without their coherence meaning less.
/// Settling is then about as before: 0.768 s in the middle, 2.432 s at the
/// bottom. Settling faster needs fewer blocks (a higher floor) or a shorter
/// bottom window (coarser resolution); neither is taken here.
pub const STAGE_BLOCKS: [(usize, usize); 3] = [(1, 1), (3, 2), (3, 1)];

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
/// job: at 4 kHz the bottom settles in 2.56 s at 50 % overlap (2.43 s since
/// #699), matching today, and every step
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
    /// This stage's FFT length: [`NFFT`] at stage 0, the speed preset's
    /// [`Speed::deep_nfft`] below it.
    pub nfft: usize,
    /// Bin width, `rate / nfft`.
    pub df: f64,
    /// Analysis window length in seconds, `nfft / rate`.
    pub window_s: f64,
    /// Segment hop in samples at this stage's rate, `nfft / HOP_DIVISORS[i]`.
    pub hop: usize,
    /// [`STAGE_BLOCKS`] for this stage: its block count is
    /// `ceil(N · num / den)` for the session's base `N`.
    pub blocks_factor: (usize, usize),
    /// Segment hop in seconds, `hop / rate`.
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

impl Stage {
    /// Blocks this stage averages for the session's base `n_blocks`.
    pub fn blocks(&self, n_blocks: usize) -> usize {
        let (num, den) = self.blocks_factor;
        (n_blocks.max(1) * num).div_ceil(den)
    }
}

/// The full stage list for one sample rate, shallowest (full rate) first.
#[derive(Clone, Debug, PartialEq)]
pub struct Ladder {
    pub sr: u32,
    /// The speed preset the stages were laid out for (#714).
    pub speed: Speed,
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
    /// A stage's served band (to `blend_top`) passes [`MAX_TOP_FRACTION`] of
    /// its decimated rate: its anti-alias filter would have no transition
    /// band. Checked at every boundary (#714), not only the first.
    BandTooWide {
        stage: usize,
        percent: u32,
    },
    /// `sr` is high enough that stage 1's served band would not fit inside its
    /// decimator's passband, and inserting stages did not resolve it.
    RateTooHigh(u32),
}

impl fmt::Display for LadderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LadderError::ZeroRate => write!(f, "sample rate must be non-zero"),
            LadderError::BandTooWide { stage, percent } => write!(
                f,
                "ladder stage {stage} would serve {percent} % of its decimated rate (max {:.0} %)",
                MAX_TOP_FRACTION * 100.0
            ),
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
    layout_for(sr, Speed::Detail)
}

/// [`layout`] under a speed preset (#714): the stage rates and stage 0 are
/// the same; the deeper stages take [`Speed::deep_nfft`], and every
/// crossover follows the validity edges that sets.
pub fn layout_for(sr: u32, speed: Speed) -> Result<Ladder, LadderError> {
    layout_with(sr, speed, speed.deep_nfft())
}

/// [`layout_for`] with the deep FFT length given, so the boundary guard can
/// be tested on a length no preset uses.
fn layout_with(sr: u32, speed: Speed, deep_nfft: usize) -> Result<Ladder, LadderError> {
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
        let nfft = if i == 0 { NFFT } else { deep_nfft };
        let df = rate / nfft as f64;
        let f_valid = k * df;
        // A stage no finer than the one above serves nothing: its band
        // would run from its validity edge up to the same edge. Follow's
        // 12 kHz rung at ≤ 48 kHz (1024 points: the 4096-point Δf at 48 kHz)
        // is one (#714). It is left out; the rung below hands over directly.
        if let Some(above) = stages.last() {
            if df >= above.df {
                continue;
            }
        }
        // Stage 0 runs to Nyquist and blends with nothing above it.
        let (f_top, blend_top) = match stages.last() {
            None => (nyquist, nyquist),
            Some(above) => (above.f_valid, above.f_valid * blend_ratio),
        };
        let depth = i.min(HOP_DIVISORS.len() - 1);
        let hop = nfft / HOP_DIVISORS[depth];
        if i > 0 && blend_top > MAX_TOP_FRACTION * rate {
            return Err(LadderError::BandTooWide {
                stage: i,
                percent: (100.0 * blend_top / rate).ceil() as u32,
            });
        }
        stages.push(Stage {
            decim: m,
            rate,
            nfft,
            df,
            window_s: nfft as f64 / rate,
            hop,
            blocks_factor: STAGE_BLOCKS[depth],
            hop_s: hop as f64 / rate,
            f_valid,
            f_top,
            blend_top,
        });
    }

    Ok(Ladder { sr, speed, stages })
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

    /// #714: every preset lays out at every supported rate, with stage 0
    /// unchanged and the deep stages at the preset's FFT length; resolution
    /// coarsens monotonically with frequency; the table in ZMQ.md holds at
    /// 96 kHz.
    #[test]
    fn every_preset_lays_out_and_matches_the_documented_table() {
        for sr in [44_100u32, 48_000, 96_000, 192_000] {
            for speed in Speed::ALL {
                let l = layout_for(sr, speed).unwrap_or_else(|e| panic!("{sr} {speed:?}: {e}"));
                assert_eq!(l.speed, speed);
                assert_eq!(l.stages[0].nfft, NFFT);
                assert!(l.stages[1..].iter().all(|s| s.nfft == speed.deep_nfft()));
                for w in l.stages.windows(2) {
                    assert!(w[1].df < w[0].df, "{sr} {speed:?}: Δf not coarser upward");
                }
            }
        }
        let l = |s| layout_for(96_000, s).unwrap();
        let settle = |s: &Stage| s.window_s + s.hop_s * (s.blocks(4) - 1) as f64;
        for (speed, df, hop_mid, hop_bot, set_mid, set_bot) in [
            (Speed::Detail, 0.977, 0.0853, 0.128, 0.768, 2.432),
            (Speed::Live, 1.953, 0.0427, 0.064, 0.384, 1.216),
            (Speed::Follow, 3.906, 0.0213, 0.032, 0.192, 0.608),
        ] {
            let st = &l(speed).stages;
            assert!((st[2].df - df).abs() < 1e-3, "{speed:?} Δf {}", st[2].df);
            assert!((st[1].hop_s - hop_mid).abs() < 1e-3, "{speed:?}");
            assert!((st[2].hop_s - hop_bot).abs() < 1e-3, "{speed:?}");
            assert!((settle(&st[1]) - set_mid).abs() < 1e-3, "{speed:?}");
            assert!((settle(&st[2]) - set_bot).abs() < 1e-3, "{speed:?}");
        }
    }

    /// #714: the boundary guard fires on the case it names — a deep FFT so
    /// short that the bottom stage would serve most of its decimated rate
    /// (256 points: stage 2's band tops out near 4 kHz at a 4 kHz rate).
    #[test]
    fn the_boundary_guard_refuses_a_band_its_filter_cannot_pass() {
        match layout_with(96_000, Speed::Follow, 256) {
            Err(LadderError::BandTooWide { stage, .. }) => assert_eq!(stage, 2),
            other => panic!("guard did not fire: {other:?}"),
        }
        assert!(layout_with(96_000, Speed::Follow, 1024).is_ok());
    }
}
