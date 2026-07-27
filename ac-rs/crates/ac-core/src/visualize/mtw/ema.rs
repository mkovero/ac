//! Per-band exponential averaging of the **cross-spectra**.
//!
//! # Upstream of the division, always
//!
//! The EMA runs on `Sxx`, `Syy` and `Sxy` — never on `|H1|`, never on a dB
//! quantity. Smoothing after the division biases magnitude (the mean of a
//! ratio is not the ratio of means) and destroys coherence's meaning entirely:
//! `γ² = |Sxy|²/(Sxx·Syy)` is a statement about a *set* of segments, and an
//! averaged `γ²` is not a coherence of anything. Same class as the
//! `aggregate.rs` double-conversion incident. Structurally enforced here:
//! [`BandEma`] holds no state derived from `H1`, and the only way to get an
//! `H1` out of it is [`BandEma::derive`], which divides at read time.
//!
//! # Uniform N_eff, per-band time constant
//!
//! Bands see frames at very different cadences — 21 ms at stage 0 against
//! 683 ms at stage 2 on a 96 kHz ladder. A *uniform wall-clock* time constant
//! therefore gives wildly different effective average counts (≈47 against
//! ≈1.5 at τ = 1 s), and since `E[γ̂²] ≈ γ² + (1−γ²)/N_eff`, that is a
//! coherence step of ~0.5 sitting at a fixed frequency on every crossover —
//! reading as a property of the DUT. It is not tunable away: four independent
//! 1.365 s windows need 5.46 s of audio and cannot be had from a 1 s τ.
//!
//! So the *averaging* is what is held uniform, not the clock: every band uses
//! the same `N_target`, which for a fixed overlap makes [`alpha_for_n_eff`]
//! the same `α` in every band and lets the wall-clock time constant fall out
//! of each band's own cadence. This is what "variance matched" in the
//! crossover requires, and it also serves the display better than a uniform τ
//! would — HF settles in ~51 ms rather than a second.
//!
//! The price is real and is at the bottom: stage 2 reaches 95% in ~3τ ≈ 4.9 s
//! against the 2.5 s a 4-segment 1 s Welch takes today. That is what 0.73 Hz
//! resolution costs, and it should be visible in the view rather than
//! discovered.

use realfft::num_complex::Complex;

/// Lag-1 correlation between adjacent Welch segments for a Hann window at 50%
/// overlap. Moves with [`super::ladder::HOP`]; changing one without the other
/// makes the reported `N_eff` wrong rather than making the estimate wrong.
pub const HANN_50_RHO: f64 = 1.0 / 6.0;

/// Default effective number of averages per band. Matches the `n_averages = 4`
/// the full-rate Welch path uses today.
pub const DEFAULT_N_TARGET: f64 = 4.0;

/// Steady-state effective number of independent averages for a first-order
/// EMA over segments with lag-1 correlation `rho`.
///
/// `N_eff = (2 − α) / (α · (1 + 2ρ(1 − α)))`. Derived from
/// `Var[S] = α σ² (1 + 2ρ(1−α)) / (2 − α)` for `S = α Σ (1−α)^k x_{n−k}`.
pub fn n_eff_steady(alpha: f64, rho: f64) -> f64 {
    (2.0 - alpha) / (alpha * (1.0 + 2.0 * rho * (1.0 - alpha)))
}

/// The EMA coefficient giving `n_target` effective averages at overlap
/// correlation `rho` — the inverse of [`n_eff_steady`].
///
/// Solves `2ρN α² − (N + 2ρN + 1) α + 2 = 0` for the root in `(0, 1]`.
pub fn alpha_for_n_eff(n_target: f64, rho: f64) -> f64 {
    let n = n_target.max(1.0);
    if rho <= 0.0 {
        // Degenerate (no overlap correlation): the quadratic collapses.
        return (2.0 / (n + 1.0)).clamp(f64::MIN_POSITIVE, 1.0);
    }
    let a = 2.0 * rho * n;
    let b = n + 2.0 * rho * n + 1.0;
    let disc = (b * b - 8.0 * a).max(0.0);
    // Smaller root: the larger one exceeds 1 and is not an EMA coefficient.
    ((b - disc.sqrt()) / (2.0 * a)).clamp(f64::MIN_POSITIVE, 1.0)
}

/// Averaged cross-spectra for one band.
pub struct BandEma {
    alpha: f64,
    rho: f64,
    sxx: Vec<f64>,
    syy: Vec<f64>,
    sxy: Vec<Complex<f64>>,
    frames: u64,
    /// Sum of squared weights, and the sum of adjacent weight products.
    /// Tracked incrementally so `n_eff` is exact during warmup instead of
    /// reporting a steady-state figure the estimate has not yet earned.
    w2: f64,
    w_adj: f64,
    w_last: f64,
}

/// What a band contributes to the splice: scale-free, band-independent, and
/// therefore blendable across a crossover without any decimator deconvolution.
pub struct BandEstimate {
    pub h1: Vec<Complex<f64>>,
    pub coherence: Vec<f64>,
    pub n_eff: f64,
}

impl BandEma {
    pub fn new(n_bins: usize, n_target: f64, rho: f64) -> Self {
        Self {
            alpha: alpha_for_n_eff(n_target, rho),
            rho,
            sxx: vec![0.0; n_bins],
            syy: vec![0.0; n_bins],
            sxy: vec![Complex::new(0.0, 0.0); n_bins],
            frames: 0,
            w2: 0.0,
            w_adj: 0.0,
            w_last: 0.0,
        }
    }

    pub fn alpha(&self) -> f64 {
        self.alpha
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Effective number of independent averages accumulated so far.
    ///
    /// Exact for a finite run, converging to [`n_eff_steady`]. Reported
    /// alongside coherence because `E[γ̂²]` from uncorrelated signals floats
    /// near `1/N_eff` — a coherence reading cannot be judged without it.
    pub fn n_eff(&self) -> f64 {
        if self.frames == 0 {
            return 0.0;
        }
        1.0 / (self.w2 + 2.0 * self.rho * self.w_adj)
    }

    /// Read-only views of the accumulators, for the splice.
    ///
    /// Read-only on purpose: nothing outside this module may write an
    /// accumulator, and in particular nothing may store a derived quantity
    /// back into one. That is the structural half of "the EMA is upstream of
    /// the division".
    pub fn sxx(&self) -> &[f64] {
        &self.sxx
    }

    pub fn syy(&self) -> &[f64] {
        &self.syy
    }

    pub fn sxy(&self) -> &[Complex<f64>] {
        &self.sxy
    }

    /// Fold one segment's raw cross-spectra in.
    pub fn update(&mut self, sxx: &[f64], syy: &[f64], sxy: &[Complex<f64>]) {
        assert_eq!(sxx.len(), self.sxx.len(), "bin count changed mid-session");
        assert_eq!(syy.len(), self.syy.len(), "bin count changed mid-session");
        assert_eq!(sxy.len(), self.sxy.len(), "bin count changed mid-session");

        if self.frames == 0 {
            self.sxx.copy_from_slice(sxx);
            self.syy.copy_from_slice(syy);
            self.sxy.copy_from_slice(sxy);
            self.w2 = 1.0;
            self.w_adj = 0.0;
            self.w_last = 1.0;
        } else {
            let a = self.alpha;
            let b = 1.0 - a;
            for i in 0..self.sxx.len() {
                self.sxx[i] = b * self.sxx[i] + a * sxx[i];
                self.syy[i] = b * self.syy[i] + a * syy[i];
                self.sxy[i] = self.sxy[i] * b + sxy[i] * a;
            }
            self.w_adj = b * b * self.w_adj + b * self.w_last * a;
            self.w2 = b * b * self.w2 + a * a;
            self.w_last = a;
        }
        self.frames += 1;
    }

    /// Divide — the one place a ratio is taken.
    pub fn derive(&self) -> Option<BandEstimate> {
        if self.frames == 0 {
            return None;
        }
        let n = self.sxx.len();
        let mut h1 = Vec::with_capacity(n);
        let mut coherence = Vec::with_capacity(n);
        for i in 0..n {
            let sxx = self.sxx[i];
            h1.push(if sxx > 0.0 {
                self.sxy[i] / sxx
            } else {
                Complex::new(0.0, 0.0)
            });
            let denom = sxx * self.syy[i];
            coherence.push(if denom > 0.0 {
                (self.sxy[i].norm_sqr() / denom).clamp(0.0, 1.0)
            } else {
                0.0
            });
        }
        Some(BandEstimate {
            h1,
            coherence,
            n_eff: self.n_eff(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The design's headline number, re-derived rather than asserted from a
    /// comment: 4 effective averages at Hann/50% overlap is α = 0.3401, and it
    /// is the *same* α in every band because only the cadence differs.
    #[test]
    fn alpha_for_four_averages_matches_the_analytic_value() {
        let a = alpha_for_n_eff(4.0, HANN_50_RHO);
        assert!((a - 0.340_14).abs() < 1e-4, "{a}");
        assert!((n_eff_steady(a, HANN_50_RHO) - 4.0).abs() < 1e-9);
    }

    #[test]
    fn alpha_and_n_eff_are_inverses_over_the_useful_range() {
        for n in [1.0, 2.0, 4.0, 8.0, 32.0, 128.0] {
            for rho in [0.0, HANN_50_RHO, 0.3] {
                let a = alpha_for_n_eff(n, rho);
                assert!(a > 0.0 && a <= 1.0, "n {n} rho {rho}: alpha {a}");
                assert!(
                    (n_eff_steady(a, rho) - n).abs() < 1e-6,
                    "n {n} rho {rho}: round trip gave {}",
                    n_eff_steady(a, rho)
                );
            }
        }
    }

    /// Criterion 5: the reported `N_eff` matches the analytic value, and is
    /// honest during warmup rather than claiming the steady-state figure from
    /// the first frame.
    #[test]
    fn n_eff_is_exact_during_warmup_and_converges() {
        let mut e = BandEma::new(4, 4.0, HANN_50_RHO);
        let z = vec![Complex::new(0.0, 0.0); 4];
        assert_eq!(e.n_eff(), 0.0, "nothing averaged yet");
        e.update(&[1.0; 4], &[1.0; 4], &z);
        assert!(
            (e.n_eff() - 1.0).abs() < 1e-12,
            "one frame is one average, not four: {}",
            e.n_eff()
        );
        for _ in 0..2_000 {
            e.update(&[1.0; 4], &[1.0; 4], &z);
        }
        let want = n_eff_steady(e.alpha(), HANN_50_RHO);
        assert!((e.n_eff() - want).abs() < 1e-6, "{} vs {want}", e.n_eff());
    }

    /// The reported `N_eff` must be the *variance* it earns, not a frame
    /// count. Measured directly: average white noise and compare the observed
    /// variance reduction against `1/N_eff`.
    #[test]
    fn reported_n_eff_predicts_the_observed_variance_reduction() {
        let n_target = 8.0;
        let alpha = alpha_for_n_eff(n_target, 0.0);
        let mut state = 0x1234_5678_9abc_def0u64;
        let mut rand = move || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((state >> 40) as f64 / (1u64 << 24) as f64) * 2.0 - 1.0
        };

        let mut samples = Vec::new();
        for _ in 0..4_000 {
            // Independent segments (rho = 0), so the analytic figure is the
            // plain EMA one and the test is not circular.
            let mut e = BandEma::new(1, n_target, 0.0);
            for _ in 0..400 {
                let v = rand();
                e.update(&[v * v], &[1.0], &[Complex::new(0.0, 0.0)]);
            }
            samples.push(e.sxx[0]);
        }
        let mean: f64 = samples.iter().sum::<f64>() / samples.len() as f64;
        let var: f64 =
            samples.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / samples.len() as f64;

        // Variance of one raw x² sample of a uniform(-1,1) variate.
        let mut raw = Vec::new();
        for _ in 0..200_000 {
            let v = rand();
            raw.push(v * v);
        }
        let rmean: f64 = raw.iter().sum::<f64>() / raw.len() as f64;
        let rvar: f64 = raw.iter().map(|v| (v - rmean).powi(2)).sum::<f64>() / raw.len() as f64;

        let observed_n = rvar / var;
        assert!(
            (observed_n / n_target - 1.0).abs() < 0.15,
            "N_eff claims {n_target}, variance implies {observed_n}"
        );
        // `1.0 / alpha` would be the naive answer; make sure the test can tell.
        assert!(
            (1.0 / alpha - n_target).abs() > 1.0,
            "test cannot discriminate"
        );
    }

    /// Criterion 4, structurally: the accumulator holds cross-spectra only.
    /// Averaging `|H1|` instead would give a different (biased) answer, and
    /// this pins which one is implemented.
    #[test]
    fn averaging_happens_before_the_division_not_after() {
        // Two segments with the same Sxy but very different Sxx. Averaging the
        // ratio and taking the ratio of averages disagree; the estimator must
        // do the latter.
        let mut e = BandEma::new(1, 1.0, 0.0);
        e.update(&[1.0], &[1.0], &[Complex::new(1.0, 0.0)]);
        // alpha = 1 at n_target = 1 would just replace; use two averages.
        let mut e = BandEma::new(1, 2.0, 0.0);
        e.update(&[1.0], &[1.0], &[Complex::new(1.0, 0.0)]);
        e.update(&[9.0], &[9.0], &[Complex::new(1.0, 0.0)]);
        let got = e.derive().unwrap();

        let a = e.alpha();
        let sxx = (1.0 - a) * 1.0 + a * 9.0;
        let sxy = Complex::new(1.0, 0.0);
        let want_ratio_of_means = sxy / sxx;
        let mean_of_ratios = Complex::new((1.0 - a) * 1.0 + a * (1.0 / 9.0), 0.0);

        assert!((got.h1[0] - want_ratio_of_means).norm() < 1e-12);
        assert!(
            (got.h1[0] - mean_of_ratios).norm() > 1e-3,
            "the two conventions must be distinguishable for this test to mean anything"
        );
    }

    /// Coherence from an uncorrelated pair floats near `1/N_eff` — the reason
    /// `N_eff` ships alongside it. This also pins that coherence is derived at
    /// read time and never itself smoothed.
    #[test]
    fn uncorrelated_coherence_floats_near_one_over_n_eff() {
        let mut state = 0xdead_beef_0bad_f00du64;
        let mut rand = move || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((state >> 40) as f64 / (1u64 << 24) as f64) * 2.0 - 1.0
        };
        for n_target in [4.0, 16.0] {
            let mut e = BandEma::new(1, n_target, 0.0);
            for _ in 0..3_000 {
                let (x, y) = (Complex::new(rand(), rand()), Complex::new(rand(), rand()));
                e.update(&[x.norm_sqr()], &[y.norm_sqr()], &[x.conj() * y]);
            }
            let coh = e.derive().unwrap().coherence[0];
            let expect = 1.0 / n_target;
            assert!(
                coh < 4.0 * expect && coh > 0.05 * expect,
                "N_target {n_target}: coherence {coh} is nowhere near 1/N_eff = {expect}"
            );
        }
    }

    /// A fully coherent pair must still read ~1 — the bias floor must not
    /// become a ceiling.
    #[test]
    fn coherent_pair_reads_unity() {
        let mut e = BandEma::new(1, 4.0, HANN_50_RHO);
        for k in 1..500 {
            let x = Complex::new(k as f64, 0.5 * k as f64);
            let y = x * Complex::new(0.5, 0.25);
            e.update(&[x.norm_sqr()], &[y.norm_sqr()], &[x.conj() * y]);
        }
        let got = e.derive().unwrap();
        assert!(
            (got.coherence[0] - 1.0).abs() < 1e-9,
            "{}",
            got.coherence[0]
        );
        assert!((got.h1[0] - Complex::new(0.5, 0.25)).norm() < 1e-9);
    }
}
