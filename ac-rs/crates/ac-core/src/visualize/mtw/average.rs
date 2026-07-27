//! Per-stage averaging of the **cross-spectra**: a plain mean of the last
//! `N` completed analysis blocks.
//!
//! # Upstream of the division, always
//!
//! The average runs on `Sxx`, `Syy` and `Sxy` — never on `|H1|`, never on a dB
//! quantity. Averaging after the division biases magnitude (the mean of a
//! ratio is not the ratio of means) and destroys coherence's meaning entirely:
//! `γ² = |Sxy|²/(Sxx·Syy)` is a statement about a *set* of blocks, and an
//! averaged `γ²` is not a coherence of anything. Same class as the
//! `aggregate.rs` double-conversion incident. Structurally enforced here:
//! [`BlockAverage`] holds no state derived from `H1`, and the only way to get
//! an `H1` out of it is [`BlockAverage::derive`], which divides at read time.
//!
//! # A plain average, and N uniform across stages
//!
//! Not an exponential one. Exponential averaging settles in roughly seven
//! blocks against four for the same statistical quality, and it buys nothing
//! once block boundaries are fixed — the fixed boundaries are what stop a
//! transient being re-analysed at a shifting weighting (#208), not the shape
//! of the averaging window.
//!
//! `N` is uniform across all three stages. The alternative — a uniform
//! wall-clock time constant — gives *different* effective average counts per
//! stage, because the stages complete blocks at very different cadences: 47,
//! 5.9 and 1.5 at 96 kHz. Since `E[γ̂²] ≈ γ² + (1−γ²)/N`, that is a coherence
//! bias of 0.02 / 0.17 / 0.68 and so a ~0.5 step at a fixed frequency, which
//! reads as a property of the DUT rather than of the analyser. Uniform `N = 4`
//! puts the block contribution to the bias at 0.25 everywhere.
//!
//! That is the *block* contribution only. A display column sums several FFT
//! bins, which averages further, and the bin count per column changes at a
//! crossover — so the total effective depth is not uniform and a residual
//! coherence step remains. Measured and accepted rather than modelled; see
//! `design-mtw-ladder.md`. Nothing here derives a depth figure from `N`.
//!
//! # What N costs
//!
//! Settling is `W + hop·(N−1)` per stage. At 96 kHz with `N = 4`: 0.11 s at
//! the top, 0.85 s in the middle, **2.56 s** at the bottom. Today's bottom is
//! 2.5 s, so low frequency is unchanged and the top improves roughly
//! twelvefold. Lowering `N` would speed the bottom up (2.05 s at 3, 1.54 s at
//! 2) but `N` is uniform, so it raises the coherence floor across the *whole*
//! display — 0.33 and 0.50 respectively, and at 0.50 a coherence reading has
//! stopped meaning anything.

use std::collections::VecDeque;

use realfft::num_complex::Complex;

/// Blocks averaged per stage.
pub const DEFAULT_N_BLOCKS: usize = 4;

// A "variance-equivalent block count" using Welch's ρ = 1/6 overlap
// correction used to live here, and the coherence floor was documented as
// `1/3.2 = 0.312` rather than `1/N`. **That was wrong and is removed.**
//
// ρ = 1/6 corrects the *variance of a power-spectrum estimate* for 50%
// overlapping Hann segments. The magnitude-squared coherence bias on
// uncorrelated inputs is a different functional, and the correction does not
// transfer to it. Measured on this pipeline — single-bin, non-blend columns,
// 30 runs per point — the floor tracks the nominal `N`:
//
//   N       1/N      1/N_var(rho=1/6)   measured floor   implied N_eff
//   2     0.5000            0.5833           0.5053            1.98
//   4     0.2500            0.3125           0.2548            3.92
//   8     0.1250            0.1615           0.1309            7.64
//  16     0.0625            0.0820           0.0681           14.68
//
// Overlap costs under 10%, growing slowly with N — nothing like the 20% the
// correction claimed, and in the same direction as no correction at all. The
// "corrected" figure was further from the truth than the uncorrected one, so
// shipping it was worse than shipping nothing.
//
// Do not reintroduce a correction here without measuring it against this
// table. The per-column bin count is a *separate* and larger factor on the
// effective depth; see `splice` and the note in `design-mtw-ladder.md`.

/// Averaged cross-spectra: `(Sxx, Syy, Sxy)`, one entry per bin.
pub type CrossSpectra = (Vec<f64>, Vec<f64>, Vec<Complex<f64>>);

/// The last `N` blocks' cross-spectra for one stage.
pub struct BlockAverage {
    n_blocks: usize,
    bins: usize,
    sxx: VecDeque<Vec<f64>>,
    syy: VecDeque<Vec<f64>>,
    sxy: VecDeque<Vec<Complex<f64>>>,
    /// Blocks analysed since the session began, not the number retained.
    total: u64,
}

/// What a stage contributes to the splice: scale-free, stage-independent, and
/// therefore blendable across a crossover without any decimator deconvolution.
pub struct StageEstimate {
    pub h1: Vec<Complex<f64>>,
    pub coherence: Vec<f64>,
    pub n: usize,
}

impl BlockAverage {
    pub fn new(bins: usize, n_blocks: usize) -> Self {
        let n_blocks = n_blocks.max(1);
        Self {
            n_blocks,
            bins,
            sxx: VecDeque::with_capacity(n_blocks),
            syy: VecDeque::with_capacity(n_blocks),
            sxy: VecDeque::with_capacity(n_blocks),
            total: 0,
        }
    }

    pub fn n_blocks(&self) -> usize {
        self.n_blocks
    }

    /// Blocks analysed since the session began. Each block of audio
    /// contributes to exactly one of these — see
    /// [`super::MtwPair::push`] for the fixed-boundary segmentation that
    /// makes that true.
    pub fn total_blocks(&self) -> u64 {
        self.total
    }

    /// Blocks currently held, capped at `n_blocks`.
    pub fn held(&self) -> usize {
        self.sxx.len()
    }

    /// True once the stage holds a full `N` blocks — i.e. once it has settled.
    pub fn settled(&self) -> bool {
        self.sxx.len() >= self.n_blocks
    }

    /// Fold one completed block's raw cross-spectra in, evicting the oldest.
    pub fn push_block(&mut self, sxx: Vec<f64>, syy: Vec<f64>, sxy: Vec<Complex<f64>>) {
        assert_eq!(sxx.len(), self.bins, "bin count changed mid-session");
        assert_eq!(syy.len(), self.bins, "bin count changed mid-session");
        assert_eq!(sxy.len(), self.bins, "bin count changed mid-session");
        if self.sxx.len() == self.n_blocks {
            self.sxx.pop_front();
            self.syy.pop_front();
            self.sxy.pop_front();
        }
        self.sxx.push_back(sxx);
        self.syy.push_back(syy);
        self.sxy.push_back(sxy);
        self.total += 1;
    }

    /// Mean of the retained blocks, per bin.
    ///
    /// Summed fresh rather than carried as a running total with the evicted
    /// block subtracted: the sum is four terms, and a running total would
    /// accumulate cancellation error over a session with no upside.
    pub fn mean(&self) -> Option<CrossSpectra> {
        let k = self.sxx.len();
        if k == 0 {
            return None;
        }
        let inv = 1.0 / k as f64;
        let mut sxx = vec![0.0; self.bins];
        let mut syy = vec![0.0; self.bins];
        let mut sxy = vec![Complex::new(0.0, 0.0); self.bins];
        for b in 0..k {
            for i in 0..self.bins {
                sxx[i] += self.sxx[b][i];
                syy[i] += self.syy[b][i];
                sxy[i] += self.sxy[b][i];
            }
        }
        for i in 0..self.bins {
            sxx[i] *= inv;
            syy[i] *= inv;
            sxy[i] *= inv;
        }
        Some((sxx, syy, sxy))
    }

    /// Divide — the one place a ratio is taken.
    pub fn derive(&self) -> Option<StageEstimate> {
        let (sxx, syy, sxy) = self.mean()?;
        let mut h1 = Vec::with_capacity(self.bins);
        let mut coherence = Vec::with_capacity(self.bins);
        for i in 0..self.bins {
            h1.push(if sxx[i] > 0.0 {
                sxy[i] / sxx[i]
            } else {
                Complex::new(0.0, 0.0)
            });
            let denom = sxx[i] * syy[i];
            coherence.push(if denom > 0.0 {
                (sxy[i].norm_sqr() / denom).clamp(0.0, 1.0)
            } else {
                0.0
            });
        }
        Some(StageEstimate {
            h1,
            coherence,
            n: self.sxx.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn z(n: usize) -> Vec<Complex<f64>> {
        vec![Complex::new(0.0, 0.0); n]
    }

    /// The window is exactly `N` blocks long: block `N+1` evicts block 1, and
    /// what falls out stops affecting the answer entirely. An exponential
    /// average would leave a decaying tail instead.
    #[test]
    fn only_the_last_n_blocks_contribute() {
        let mut a = BlockAverage::new(1, 4);
        for v in [100.0, 1.0, 1.0, 1.0] {
            a.push_block(vec![v], vec![1.0], z(1));
        }
        let (sxx, _, _) = a.mean().unwrap();
        assert!((sxx[0] - 25.75).abs() < 1e-12, "{}", sxx[0]);

        // One more block pushes the outlier out completely — not partially.
        a.push_block(vec![1.0], vec![1.0], z(1));
        let (sxx, _, _) = a.mean().unwrap();
        assert_eq!(sxx[0], 1.0, "the evicted block must leave no tail");
        assert_eq!(a.held(), 4);
        assert_eq!(a.total_blocks(), 5);
    }

    #[test]
    fn settles_after_exactly_n_blocks() {
        let mut a = BlockAverage::new(2, 4);
        for i in 0..3 {
            assert!(!a.settled(), "settled early at {i} blocks");
            a.push_block(vec![1.0; 2], vec![1.0; 2], z(2));
        }
        a.push_block(vec![1.0; 2], vec![1.0; 2], z(2));
        assert!(a.settled());
    }

    /// Criterion 4, structurally: the accumulator holds cross-spectra only, so
    /// the ratio is taken once, at the end. Averaging the per-block ratios
    /// instead gives a different (biased) answer, and this pins which one is
    /// implemented.
    #[test]
    fn averaging_happens_before_the_division_not_after() {
        let mut a = BlockAverage::new(1, 2);
        a.push_block(vec![1.0], vec![1.0], vec![Complex::new(1.0, 0.0)]);
        a.push_block(vec![9.0], vec![9.0], vec![Complex::new(1.0, 0.0)]);
        let got = a.derive().unwrap();

        let ratio_of_means = Complex::new(1.0, 0.0) / 5.0; // Sxy_mean / Sxx_mean
        let mean_of_ratios = Complex::new(0.5 * (1.0 + 1.0 / 9.0), 0.0);
        assert!((got.h1[0] - ratio_of_means).norm() < 1e-12);
        assert!(
            (got.h1[0] - mean_of_ratios).norm() > 1e-3,
            "the two conventions must be distinguishable or this test is vacuous"
        );
    }

    /// Criterion 5: coherence from uncorrelated inputs floats at `1/N` — the
    /// reason `N` ships in the frame. Measured, not asserted from the formula.
    ///
    /// Blocks here are independent, so the figure is exactly `1/N`. The live
    /// path overlaps its Hann blocks by 50%, which was once assumed to lift
    /// the floor to 0.31 at N = 4; measurement puts it at 0.255, i.e. still
    /// `1/N` within ~2%. See the note above the accumulator.
    #[test]
    fn uncorrelated_coherence_floats_at_one_over_n() {
        let mut state = 0xdead_beef_0bad_f00du64;
        let mut rand = move || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((state >> 40) as f64 / (1u64 << 24) as f64) * 2.0 - 1.0
        };
        for n in [2usize, 4, 16] {
            // Average many independent realisations of the N-block estimate.
            let mut acc = 0.0;
            let trials = 4_000;
            for _ in 0..trials {
                let mut a = BlockAverage::new(1, n);
                for _ in 0..n {
                    let x = Complex::new(rand(), rand());
                    let y = Complex::new(rand(), rand());
                    a.push_block(vec![x.norm_sqr()], vec![y.norm_sqr()], vec![x.conj() * y]);
                }
                acc += a.derive().unwrap().coherence[0];
            }
            let mean = acc / f64::from(trials);
            let want = 1.0 / n as f64;
            assert!(
                (mean - want).abs() < 0.25 * want,
                "N {n}: coherence floor {mean}, want ~{want}"
            );
        }
    }

    /// A fully coherent pair must still read ~1 — the bias floor must not
    /// become a ceiling.
    #[test]
    fn coherent_pair_reads_unity() {
        let mut a = BlockAverage::new(1, 4);
        for k in 1..10 {
            let x = Complex::new(f64::from(k), 0.5 * f64::from(k));
            let y = x * Complex::new(0.5, 0.25);
            a.push_block(vec![x.norm_sqr()], vec![y.norm_sqr()], vec![x.conj() * y]);
        }
        let got = a.derive().unwrap();
        assert!(
            (got.coherence[0] - 1.0).abs() < 1e-9,
            "{}",
            got.coherence[0]
        );
        assert!((got.h1[0] - Complex::new(0.5, 0.25)).norm() < 1e-9);
    }
}
