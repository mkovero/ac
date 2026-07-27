//! Column assembly: per-stage band aggregation, then the crossover blend.
//!
//! Two reductions happen here and the order matters.
//!
//! 1. **Within a stage**, bins are summed as cross-spectra and divided once,
//!    per column. Summing `Sxx`, `Syy`, `Sxy` and dividing at the end is the
//!    same discipline [`super::average`] applies in time, applied in frequency:
//!    a column's coherence is a statement about the bins it contains, and
//!    averaging per-bin coherences would not be one.
//! 2. **Across stages**, `H1` and `γ²` are blended. Not the cross-spectra:
//!    `Sxy` from stage `b` carries `|Hdec,b|²` and its own normalisation, so
//!    blending those would mean deconvolving the decimator at exactly the
//!    frequencies the crossover exists to handle — reintroducing synthesis in
//!    a new place. `H1` and `γ²` are already dimensionless and
//!    stage-independent, which is the whole point of the cancellation
//!    argument, so they blend directly.
//!
//! A complex-`H1` blend nulls if the two stages disagree in phase. They must
//! not: both estimate the same `H1` from the same aligned pair. That is left
//! unguarded on purpose — a null at a crossover is a correct and loud symptom
//! of an alignment or phase-lock bug, and worth more than a magnitude-only
//! blend that would hide it.

use realfft::num_complex::Complex;

use super::ladder::Ladder;

/// One display column and everything needed to interpret it.
///
/// The per-column `df` / `window_s` / `n` are not decoration: without them
/// a screenshot of a multi-rate display is not interpretable, because
/// neighbouring columns can come from windows 12x apart, and coherence from
/// uncorrelated inputs floats near `1/n`.
#[derive(Clone, Debug, PartialEq)]
pub struct Column {
    /// Geometric centre of the column.
    pub freq: f64,
    pub lo: f64,
    pub hi: f64,
    pub h1: Complex<f64>,
    pub coherence: f64,
    /// Bin width of the stage this column is reported against.
    pub df: f64,
    /// Analysis window of that stage, in seconds.
    pub window_s: f64,
    /// Blocks averaged behind this column.
    pub n: usize,
    /// Index of the deeper contributing stage.
    pub stage: usize,
    /// Weight of the shallower stage, `0.0` outside a crossover.
    pub blend: f64,
    /// Source bins summed for this column, across all contributors. Never
    /// zero: the grid guarantees at least one, and criterion 1 is that
    /// guarantee made observable.
    pub bins: usize,
}

/// Sum of one stage's bins over `[lo, hi)`, divided once.
///
/// Returns `None` when the stage has no bin in the column. The column grid
/// from [`super::ladder::column_edges`] makes that unreachable for a
/// contributing stage; it is handled rather than asserted so a future grid
/// change degrades to "this stage does not contribute" instead of to a
/// fabricated value.
fn stage_column(
    st: &StageSpectra<'_>,
    df: f64,
    lo: f64,
    hi: f64,
) -> Option<(Complex<f64>, f64, usize)> {
    let (sxx, syy, sxy) = (st.sxx, st.syy, st.sxy);
    let k_lo = (lo / df).ceil().max(0.0) as usize;
    let k_hi = (hi / df).ceil().min(sxx.len() as f64) as usize;
    if k_hi <= k_lo {
        return None;
    }
    let mut axx = 0.0;
    let mut ayy = 0.0;
    let mut axy = Complex::new(0.0, 0.0);
    for k in k_lo..k_hi {
        axx += sxx[k];
        ayy += syy[k];
        axy += sxy[k];
    }
    let h1 = if axx > 0.0 {
        axy / axx
    } else {
        Complex::new(0.0, 0.0)
    };
    let denom = axx * ayy;
    let coh = if denom > 0.0 {
        (axy.norm_sqr() / denom).clamp(0.0, 1.0)
    } else {
        0.0
    };
    Some((h1, coh, k_hi - k_lo))
}

/// Raw per-stage accumulator contents, in ladder order (stage 0 first).
pub struct StageSpectra<'a> {
    pub sxx: &'a [f64],
    pub syy: &'a [f64],
    pub sxy: &'a [Complex<f64>],
    pub n: usize,
}

/// Assemble display columns from the ladder's per-stage accumulators.
///
/// `edges` comes from [`super::ladder::column_edges`], so every column already
/// spans at least one bin of every stage that feeds it.
///
/// A stage is `None` until it has settled — it has completed fewer than `N`
/// blocks, so it has no estimate at the agreed averaging depth. Columns whose
/// contributors are not **all** settled are omitted entirely rather than drawn
/// from a shallower average. That is what keeps `N` uniform across every
/// emitted column, which is the whole reason the coherence bias is the same
/// either side of a crossover; emitting an under-averaged column would put
/// back exactly the step this design exists to remove, and would do it at a
/// frequency that moves as the ladder warms.
///
/// The consequence is that the display fills **downward** as the rungs come
/// good — top band first, bottom band last — rather than staying blank until
/// the deepest one settles.
pub fn assemble(
    ladder: &Ladder,
    stages: &[Option<StageSpectra<'_>>],
    edges: &[f64],
) -> Vec<Column> {
    if edges.len() < 2 || stages.len() != ladder.stages.len() {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(edges.len() - 1);
    for w in edges.windows(2) {
        let (lo, hi) = (w[0], w[1]);
        let freq = (lo * hi).sqrt();
        let src = ladder.source_at(freq);

        let deep_stage = &ladder.stages[src.deep];
        let Some(deep) = stages[src.deep].as_ref() else {
            continue;
        };
        // Inside a crossover the shallower stage must be settled too, or the
        // blend would mix one settled estimate with nothing.
        if src.shallow.is_some_and(|si| stages[si].is_none()) {
            continue;
        }
        let Some((h_deep, c_deep, n_deep)) = stage_column(deep, deep_stage.df, lo, hi) else {
            continue;
        };

        let (mut h1, mut coherence, mut bins) = (h_deep, c_deep, n_deep);
        let mut blend = 0.0;
        if let (Some(si), w_sh) = (src.shallow, src.w_shallow) {
            let sh_stage = &ladder.stages[si];
            let sh = stages[si].as_ref().expect("checked settled above");
            if let Some((h_sh, c_sh, n_sh)) = stage_column(sh, sh_stage.df, lo, hi) {
                h1 = h_deep * (1.0 - w_sh) + h_sh * w_sh;
                coherence = c_deep * (1.0 - w_sh) + c_sh * w_sh;
                bins += n_sh;
                blend = w_sh;
            }
        }

        out.push(Column {
            freq,
            lo,
            hi,
            h1,
            coherence,
            // The deeper stage's window bounds how stale the column can be, and
            // is monotone in frequency across a crossover — reporting the
            // dominant stage's instead would make the reported window jump
            // back and forth mid-blend and read as a data artifact.
            df: deep_stage.df,
            window_s: deep_stage.window_s,
            n: deep.n,
            stage: src.deep,
            blend,
            bins,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::ladder::{self, column_edges, NFFT};
    use super::*;

    struct Fixture {
        sxx: Vec<Vec<f64>>,
        syy: Vec<Vec<f64>>,
        sxy: Vec<Vec<Complex<f64>>>,
        n: Vec<usize>,
    }

    impl Fixture {
        /// A flat, fully coherent `H1 = h` in every stage, plus enough
        /// uncorrelated power to bring coherence to `gamma2`.
        fn new(ladder: &Ladder, h: Complex<f64>, gamma2: f64, n: usize) -> Self {
            let bins = NFFT / 2 + 1;
            let nstages = ladder.stages.len();
            Self {
                sxx: vec![vec![1.0; bins]; nstages],
                syy: vec![vec![h.norm_sqr() / gamma2; bins]; nstages],
                sxy: vec![vec![h; bins]; nstages],
                n: vec![n; nstages],
            }
        }

        fn view(&self) -> Vec<Option<StageSpectra<'_>>> {
            (0..self.sxx.len())
                .map(|i| {
                    Some(StageSpectra {
                        sxx: &self.sxx[i],
                        syy: &self.syy[i],
                        sxy: &self.sxy[i],
                        n: self.n[i],
                    })
                })
                .collect()
        }

        /// Same, with the listed stages not yet settled.
        fn view_settled(&self, settled: &[bool]) -> Vec<Option<StageSpectra<'_>>> {
            (0..self.sxx.len())
                .map(|i| {
                    settled[i].then(|| StageSpectra {
                        sxx: &self.sxx[i],
                        syy: &self.syy[i],
                        sxy: &self.sxy[i],
                        n: self.n[i],
                    })
                })
                .collect()
        }
    }

    /// Criterion 1, as an output property rather than a grid property: every
    /// emitted column maps to at least one real source bin.
    #[test]
    fn every_emitted_column_is_backed_by_bins() {
        for sr in [44_100u32, 48_000, 96_000, 192_000] {
            let l = ladder::layout(sr).unwrap();
            let f = Fixture::new(&l, Complex::new(0.5, 0.0), 0.8, 4);
            let edges = column_edges(&l, 20.0, f64::from(sr) / 2.0, 48.0);
            let cols = assemble(&l, &f.view(), &edges);
            assert_eq!(cols.len(), edges.len() - 1, "sr {sr}: columns were dropped");
            for c in &cols {
                assert!(c.bins >= 1, "sr {sr}: {c:?}");
            }
        }
    }

    /// Criterion 3, magnitude half: a flat `H1` must come out flat, with no
    /// step at any crossover. Uses a *partially* coherent fixture so the
    /// coherence half below is not vacuous.
    #[test]
    fn splice_is_continuous_in_magnitude() {
        let l = ladder::layout(96_000).unwrap();
        let h = Complex::new(0.5, -0.25);
        let f = Fixture::new(&l, h, 0.6, 4);
        let edges = column_edges(&l, 20.0, 48_000.0, 48.0);
        let cols = assemble(&l, &f.view(), &edges);
        for c in &cols {
            assert!(
                (c.h1 - h).norm() < 1e-9,
                "H1 moved at {} Hz (stage {}, blend {}): {:?}",
                c.freq,
                c.stage,
                c.blend,
                c.h1
            );
        }
    }

    /// Criterion 3, coherence half. A flat reference is fully coherent, so
    /// γ² = 1 everywhere and a step *cannot* appear — the stimulus has to be
    /// partially coherent for this assertion to be able to fail.
    #[test]
    fn splice_is_continuous_in_coherence_under_a_partially_coherent_stimulus() {
        let l = ladder::layout(48_000).unwrap();
        let gamma2 = 0.55;
        let f = Fixture::new(&l, Complex::new(1.0, 0.0), gamma2, 4);
        let edges = column_edges(&l, 20.0, 24_000.0, 48.0);
        let cols = assemble(&l, &f.view(), &edges);
        assert!(cols.iter().any(|c| c.blend > 0.0), "no blend was exercised");
        for c in &cols {
            assert!(
                (c.coherence - gamma2).abs() < 1e-9,
                "coherence stepped at {} Hz (blend {}): {}",
                c.freq,
                c.blend,
                c.coherence
            );
        }
        // And the fixture is genuinely partially coherent, so a step was
        // possible in the first place.
        assert!(gamma2 < 0.99);
    }

    /// Unequal `N` across a crossover is exactly what a uniform wall-clock
    /// τ would produce, and it puts a fixed-frequency step in the trust
    /// indicator. The blend cannot repair that, so this pins that the ladder
    /// is fed matched variance rather than relying on the splice to hide it.
    #[test]
    fn a_coherence_step_appears_if_the_bands_are_not_variance_matched() {
        let l = ladder::layout(48_000).unwrap();
        let mut f = Fixture::new(&l, Complex::new(1.0, 0.0), 0.5, 4);
        // Stage 0 under-averaged: raise its uncorrelated floor, as a smaller
        // N_eff would.
        for v in f.syy[0].iter_mut() {
            *v *= 4.0;
        }
        let edges = column_edges(&l, 20.0, 24_000.0, 48.0);
        let cols = assemble(&l, &f.view(), &edges);
        let x = l.stages[1].f_top;
        let below = cols
            .iter()
            .find(|c| c.freq > x * 0.9 && c.freq < x)
            .unwrap();
        let above = cols
            .iter()
            .find(|c| c.freq > l.stages[1].blend_top * 1.1)
            .unwrap();
        assert!(
            (below.coherence - above.coherence).abs() > 0.1,
            "the mismatched case must be detectable, else the matched test is vacuous"
        );
    }

    /// The blend is a genuine crossfade: weights run 0 -> 1 and the two stages
    /// are actually mixed, not switched.
    #[test]
    fn crossover_blends_rather_than_switches() {
        let l = ladder::layout(48_000).unwrap();
        let mut f = Fixture::new(&l, Complex::new(1.0, 0.0), 0.9, 4);
        // Give stage 0 a different H1 so the mix is observable.
        for v in f.sxy[0].iter_mut() {
            *v = Complex::new(2.0, 0.0);
        }
        let edges = column_edges(&l, 20.0, 24_000.0, 96.0);
        let cols = assemble(&l, &f.view(), &edges);
        let mid: Vec<&Column> = cols
            .iter()
            .filter(|c| c.stage == 1 && c.blend > 0.05 && c.blend < 0.95)
            .collect();
        assert!(mid.len() >= 4, "expected a blend region, got {}", mid.len());
        for c in mid {
            assert!(
                c.h1.re > 1.0 && c.h1.re < 2.0,
                "column at {} Hz is switched, not blended: {:?}",
                c.freq,
                c.h1
            );
        }
    }

    /// Reported window and Δf must be monotone in frequency — a display that
    /// reported the dominant stage's window would zig-zag through a blend.
    #[test]
    fn reported_window_is_monotone_across_the_ladder() {
        let l = ladder::layout(96_000).unwrap();
        let f = Fixture::new(&l, Complex::new(1.0, 0.0), 0.7, 4);
        let edges = column_edges(&l, 20.0, 48_000.0, 48.0);
        let cols = assemble(&l, &f.view(), &edges);
        for w in cols.windows(2) {
            assert!(
                w[1].window_s <= w[0].window_s + 1e-12,
                "window rose with frequency at {} Hz: {} -> {}",
                w[1].freq,
                w[0].window_s,
                w[1].window_s
            );
            assert!(w[1].df >= w[0].df - 1e-12, "Δf fell with frequency");
        }
    }

    /// The display fills downward as rungs settle. Only the bottom rung takes
    /// the full 2.56 s; withholding everything until it does would hide a live
    /// top band for most of that.
    #[test]
    fn unsettled_rungs_drop_their_columns_instead_of_blanking_the_display() {
        let l = ladder::layout(96_000).unwrap();
        let f = Fixture::new(&l, Complex::new(0.5, 0.0), 0.8, 4);
        let edges = column_edges(&l, 20.0, 48_000.0, 48.0);

        let all = assemble(&l, &f.view(), &edges);
        // Top rung only: everything above the 0/1 crossover blend, nothing below.
        let top = assemble(&l, &f.view_settled(&[true, false, false]), &edges);
        // Top two rungs.
        let mid = assemble(&l, &f.view_settled(&[true, true, false]), &edges);

        assert!(!top.is_empty(), "a settled top rung must draw something");
        assert!(top.len() < mid.len(), "{} vs {}", top.len(), mid.len());
        assert!(mid.len() < all.len(), "{} vs {}", mid.len(), all.len());

        // Each partial set is a suffix of the full one: the display fills from
        // the top down, it does not gain and lose columns in the middle.
        let tail = |c: &[Column]| c.iter().map(|x| x.freq).collect::<Vec<_>>();
        let (fa, ft, fm) = (tail(&all), tail(&top), tail(&mid));
        assert_eq!(
            ft,
            fa[fa.len() - ft.len()..],
            "top-rung columns are not the top of the band"
        );
        assert_eq!(
            fm,
            fa[fa.len() - fm.len()..],
            "two-rung columns are not the top of the band"
        );

        // Nothing is drawn below the settled rungs' reach.
        let lowest_top = ft[0];
        assert!(
            lowest_top > l.stages[1].f_top,
            "top rung alone reached {lowest_top} Hz, below its own crossover"
        );
    }

    /// The constraint that makes the partial fill safe: every emitted column is
    /// backed by the same N, whichever rungs happen to be settled. An
    /// under-averaged column would put the coherence step back, at a frequency
    /// that moves as the ladder warms.
    #[test]
    fn every_emitted_column_carries_the_same_n_while_warming() {
        let l = ladder::layout(96_000).unwrap();
        let f = Fixture::new(&l, Complex::new(1.0, 0.0), 0.6, 4);
        let edges = column_edges(&l, 20.0, 48_000.0, 48.0);
        for settled in [
            [true, false, false],
            [true, true, false],
            [true, true, true],
        ] {
            let cols = assemble(&l, &f.view_settled(&settled), &edges);
            assert!(!cols.is_empty(), "{settled:?}");
            assert!(
                cols.iter().all(|c| c.n == 4),
                "{settled:?}: N varies across the emitted columns"
            );
            assert!(
                cols.iter().all(|c| (c.coherence - 0.6).abs() < 1e-9),
                "{settled:?}: coherence stepped while warming"
            );
        }
    }

    /// A crossover needs both its contributors. A column blending a settled
    /// rung with an unsettled one must be dropped, not drawn from the settled
    /// half alone — that would be a visible seam that heals itself.
    #[test]
    fn a_blend_column_needs_both_of_its_stages_settled() {
        let l = ladder::layout(96_000).unwrap();
        let f = Fixture::new(&l, Complex::new(1.0, 0.0), 0.9, 4);
        let edges = column_edges(&l, 20.0, 48_000.0, 48.0);
        let cols = assemble(&l, &f.view_settled(&[true, false, false]), &edges);

        // The 0/1 crossover blend region is [f_top(1), blend_top(1)].
        let (lo, hi) = (l.stages[1].f_top, l.stages[1].blend_top);
        assert!(
            !cols.iter().any(|c| c.freq >= lo && c.freq <= hi),
            "a half-settled crossover was drawn"
        );
        // ...and it appears once the second rung settles.
        let both = assemble(&l, &f.view_settled(&[true, true, false]), &edges);
        assert!(both.iter().any(|c| c.freq >= lo && c.freq <= hi));
    }

    /// No rung settled: nothing at all, rather than an empty-but-present set a
    /// caller might render as a flat line.
    #[test]
    fn no_settled_rung_yields_no_columns() {
        let l = ladder::layout(96_000).unwrap();
        let f = Fixture::new(&l, Complex::new(1.0, 0.0), 0.9, 4);
        let edges = column_edges(&l, 20.0, 48_000.0, 48.0);
        assert!(assemble(&l, &f.view_settled(&[false, false, false]), &edges).is_empty());
    }
}
