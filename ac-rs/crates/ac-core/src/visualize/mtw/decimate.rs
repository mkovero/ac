//! Polyphase FIR decimation for the ladder, and the pair type that makes the
//! two channels structurally incapable of drifting apart.
//!
//! # Why a pair type
//!
//! `H1 = Gxy/Gxx` is transparent to a decimator: writing `Xdec = Hdec·X`,
//!
//! ```text
//! Gxy_dec = |Hdec|²·Gxy      Gxx_dec = |Hdec|²·Gxx      H1 = Gxy/Gxx
//! ```
//!
//! so the decimator's phase cancels in the conjugate product and its magnitude
//! cancels in the ratio — including its group delay, and including a
//! non-linear-phase design. That cancellation is the whole reason alignment is
//! one offset per pair rather than one per band.
//!
//! It holds only if **both channels traverse the same filter with the same
//! phase**. Two decimator instances with independent sample counters can drift
//! by up to `M − 1` samples — 20 ms at `M = 64` on a 3 kHz stage — and nothing
//! in the output says so; it looks exactly like the HF coherence loss the
//! ladder exists to prevent. So the guarantee is a type: [`PairDecimator`]
//! owns one coefficient set, both delay lines and a *single* phase counter,
//! and its only input method takes both channels at once and rejects unequal
//! lengths. There is no API through which one channel can advance without the
//! other.
//!
//! # Independent chains, not a cascade
//!
//! Each stage decimates from the aligned full-rate pair rather than from the
//! stage above it. A cascade would be cheaper and would inherit lockstep for
//! free, but **it does not exist at 44.1 kHz**: the factors there are 1/4/15,
//! and 15 is not a multiple of 4. Independent chains cost ~7 MAC per input
//! sample per channel — noise against the 4096-point FFTs — and remove a whole
//! class of "do the stages stay in phase with each other" questions.

use std::f64::consts::PI;

use super::ladder::Stage;

/// Stopband attenuation for the ladder's decimators.
///
/// The magnitude floor in the H1 estimator is 1e-6 (−120 dB), so aliasing
/// above a shallower stopband would land inside the displayed range.
pub const STOPBAND_DB: f64 = 90.0;

/// Modified Bessel function of the first kind, order zero. Series form; the
/// arguments here are small (`beta ≤ 9` for 90 dB) so it converges in a few
/// terms.
fn bessel_i0(x: f64) -> f64 {
    let half = x / 2.0;
    let mut term = 1.0;
    let mut sum = 1.0;
    for k in 1..64 {
        let r = half / f64::from(k);
        term *= r * r;
        sum += term;
        if term < 1e-18 * sum {
            break;
        }
    }
    sum
}

/// Magnitude response in dB at `f` Hz for an FIR running at `sr`.
fn response_db(taps: &[f64], f: f64, sr: f64) -> f64 {
    let (mut re, mut im) = (0.0, 0.0);
    for (n, &t) in taps.iter().enumerate() {
        let w = -2.0 * PI * f * n as f64 / sr;
        re += t * w.cos();
        im += t * w.sin();
    }
    20.0 * re.hypot(im).max(1e-30).log10()
}

/// Worst (least attenuated) response anywhere from `f_stop` to Nyquist.
fn worst_stopband_db(taps: &[f64], f_stop: f64, sr: f64) -> f64 {
    let nyq = sr / 2.0;
    // Fine enough to catch the first sidelobe, which is where a Kaiser design
    // is worst and where the length estimate falls short.
    let steps = 512;
    let mut worst = f64::NEG_INFINITY;
    for i in 0..=steps {
        let f = f_stop + (nyq - f_stop) * i as f64 / steps as f64;
        worst = worst.max(response_db(taps, f, sr));
    }
    worst
}

/// Build one Kaiser-windowed lowpass of exactly `n` taps.
fn kaiser_taps(n: usize, f_pass: f64, f_stop: f64, sr: f64, beta: f64) -> Vec<f64> {
    // Cut at the transition-band centre, normalised to cycles/sample.
    let fc = 0.5 * (f_pass + f_stop) / sr;
    let mid = (n - 1) as f64 / 2.0;
    let i0_beta = bessel_i0(beta);
    let mut h: Vec<f64> = (0..n)
        .map(|i| {
            let x = i as f64 - mid;
            let sinc = if x.abs() < 1e-12 {
                2.0 * fc
            } else {
                (2.0 * PI * fc * x).sin() / (PI * x)
            };
            let r = x / mid;
            let w = bessel_i0(beta * (1.0 - r * r).max(0.0).sqrt()) / i0_beta;
            sinc * w
        })
        .collect();
    let dc: f64 = h.iter().sum();
    if dc.abs() > 0.0 {
        for v in h.iter_mut() {
            *v /= dc;
        }
    }
    h
}

/// Kaiser-windowed linear-phase FIR lowpass, normalised to unity DC gain, that
/// **actually meets** `atten_db` everywhere above `f_stop`.
///
/// Odd length (type I) so the group delay is an exact integer number of
/// samples — which matters not for H1, where it cancels, but for anyone
/// reading the decimated stream directly.
///
/// Kaiser's length estimate `N ≈ (A − 7.95) / (2.285·Δω)` is an approximation
/// and lands a few dB short at the band edge: at 90 dB it delivers about 85,
/// and the shortfall varies with the transition ratio, so no fixed fudge
/// factor is right at every rate. The length is therefore **measured and
/// grown** rather than trusted. That turns "stopband ≥ 90 dB" from a claim
/// about a formula into a property of the coefficients actually returned,
/// which is what the aliasing argument needs — the H1 magnitude floor is
/// −120 dB, so anything folding in above the stopband lands inside the
/// displayed range.
///
/// Design-time only: a handful of response evaluations, once per stage per
/// session.
pub fn kaiser_lowpass(f_pass: f64, f_stop: f64, sr: f64, atten_db: f64) -> Vec<f64> {
    assert!(
        f_stop > f_pass && f_pass > 0.0 && sr > 0.0,
        "kaiser_lowpass: need 0 < f_pass < f_stop, got {f_pass}..{f_stop} at {sr} Hz"
    );
    let dw = 2.0 * PI * (f_stop - f_pass) / sr;
    let a = atten_db;
    let beta = if a > 50.0 {
        0.1102 * (a - 8.7)
    } else if a >= 21.0 {
        0.5842 * (a - 21.0).powf(0.4) + 0.078_86 * (a - 21.0)
    } else {
        0.0
    };
    let n_est = ((a - 7.95) / (2.285 * dw)).ceil().max(3.0) as usize;
    let mut n = if n_est.is_multiple_of(2) {
        n_est + 1
    } else {
        n_est
    };
    // Generous ceiling: the estimate is short by tens of percent, not by
    // multiples. Hitting this means the transition band is degenerate, which
    // the ladder's passband guard already excludes.
    let n_max = (n_est * 4).max(n + 64);
    loop {
        let h = kaiser_taps(n, f_pass, f_stop, sr, beta);
        if worst_stopband_db(&h, f_stop, sr) <= -atten_db || n >= n_max {
            return h;
        }
        n += 2;
    }
}

/// A two-channel decimator with one coefficient set and one phase counter.
///
/// See the module docs for why the pairing is a type rather than a convention.
pub struct PairDecimator {
    taps: Vec<f64>,
    decim: usize,
    /// Samples consumed since the last emitted output. **One counter for both
    /// channels** — this field is the invariant.
    phase: usize,
    line_meas: Vec<f64>,
    line_ref: Vec<f64>,
    /// Write position, shared by both delay lines for the same reason.
    head: usize,
}

impl PairDecimator {
    /// Build from explicit taps.
    pub fn new(taps: Vec<f64>, decim: usize) -> Self {
        assert!(!taps.is_empty(), "PairDecimator needs at least one tap");
        assert!(decim >= 1, "decimation factor must be >= 1");
        let n = taps.len();
        Self {
            taps,
            decim,
            phase: 0,
            line_meas: vec![0.0; n],
            line_ref: vec![0.0; n],
            head: 0,
        }
    }

    /// Pass-through for stage 0, which is always full rate.
    pub fn identity() -> Self {
        Self::new(vec![1.0], 1)
    }

    /// The decimator a ladder stage needs.
    ///
    /// Passband reaches [`Stage::blend_top`] — the highest frequency the stage
    /// is ever read at, not its nominal `f_top`, because the crossover blend
    /// draws on it a third of an octave further up. Stopband starts at
    /// `rate − blend_top`: content above that folds into the served band,
    /// while content between the two folds only into the part of the decimated
    /// band this stage never reads (the stage above serves it), which is
    /// harmless. Specifying the stopband from what is *read* rather than from
    /// Nyquist is what keeps these filters short.
    pub fn for_stage(sr: f64, stage: &Stage) -> Self {
        if stage.decim == 1 {
            return Self::identity();
        }
        let taps = kaiser_lowpass(
            stage.blend_top,
            stage.rate - stage.blend_top,
            sr,
            STOPBAND_DB,
        );
        Self::new(taps, stage.decim)
    }

    pub fn decim(&self) -> usize {
        self.decim
    }

    pub fn taps(&self) -> &[f64] {
        &self.taps
    }

    /// Samples of filter transient before the output is fully settled.
    pub fn transient_samples(&self) -> usize {
        self.taps.len()
    }

    /// Push one equal-length block of each channel, appending decimated output.
    ///
    /// # Panics
    ///
    /// If the two blocks differ in length. That is a programming error, not a
    /// runtime condition: a caller that can hand these two different sample
    /// counts has already lost the phase-lock guarantee, and continuing would
    /// corrupt H1's phase silently rather than loudly.
    pub fn push(
        &mut self,
        meas: &[f32],
        reference: &[f32],
        out_meas: &mut Vec<f64>,
        out_ref: &mut Vec<f64>,
    ) {
        assert_eq!(
            meas.len(),
            reference.len(),
            "PairDecimator: meas and ref blocks must be the same length \
             ({} vs {}) — unequal advance is exactly the drift this type exists \
             to prevent",
            meas.len(),
            reference.len()
        );
        if self.decim == 1 && self.taps.len() == 1 && self.taps[0] == 1.0 {
            out_meas.extend(meas.iter().map(|&v| f64::from(v)));
            out_ref.extend(reference.iter().map(|&v| f64::from(v)));
            return;
        }
        let n = self.taps.len();
        for (&m, &r) in meas.iter().zip(reference.iter()) {
            self.line_meas[self.head] = f64::from(m);
            self.line_ref[self.head] = f64::from(r);
            self.head = if self.head + 1 == n { 0 } else { self.head + 1 };
            self.phase += 1;
            if self.phase < self.decim {
                continue;
            }
            self.phase = 0;
            // Walk backwards from the newest sample so `taps[0]` multiplies it.
            let mut idx = if self.head == 0 { n - 1 } else { self.head - 1 };
            let mut acc_m = 0.0;
            let mut acc_r = 0.0;
            for &t in self.taps.iter() {
                acc_m += t * self.line_meas[idx];
                acc_r += t * self.line_ref[idx];
                idx = if idx == 0 { n - 1 } else { idx - 1 };
            }
            out_meas.push(acc_m);
            out_ref.push(acc_r);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::ladder;
    use super::*;

    #[test]
    fn kaiser_lowpass_meets_its_passband_and_stopband() {
        let sr = 48_000.0;
        let taps = kaiser_lowpass(1_000.0, 5_000.0, sr, STOPBAND_DB);
        assert_eq!(taps.len() % 2, 1, "type I linear phase needs odd length");
        assert!((response_db(&taps, 0.0, sr)).abs() < 1e-9, "unity DC gain");
        for f in [10.0, 200.0, 700.0, 1_000.0] {
            assert!(response_db(&taps, f, sr).abs() < 0.1, "passband at {f} Hz");
        }
        for f in [5_000.0, 8_000.0, 14_000.0, 23_900.0] {
            let d = response_db(&taps, f, sr);
            assert!(d < -STOPBAND_DB, "stopband at {f} Hz is only {d} dB");
        }
    }

    /// Symmetric taps — linear phase, so the group delay is common-mode and
    /// cancels in H1 rather than tilting it.
    #[test]
    fn taps_are_symmetric() {
        let taps = kaiser_lowpass(1_000.0, 5_000.0, 48_000.0, STOPBAND_DB);
        let n = taps.len();
        for i in 0..n / 2 {
            assert!((taps[i] - taps[n - 1 - i]).abs() < 1e-15, "tap {i}");
        }
    }

    /// Every stage's own filter must protect its own served band, at every
    /// supported rate. Checks the actual fold-in frequencies rather than a
    /// nominal cutoff.
    #[test]
    fn stage_filters_protect_the_band_each_stage_actually_serves() {
        for sr in [44_100u32, 48_000, 96_000, 192_000] {
            let srf = f64::from(sr);
            let l = ladder::layout(sr).unwrap();
            for (i, s) in l.stages.iter().enumerate().skip(1) {
                let d = PairDecimator::for_stage(srf, s);
                // Passband: flat over everything this stage is read at.
                for frac in [0.01, 0.25, 0.5, 0.9, 1.0] {
                    let f = s.blend_top * frac;
                    let db = response_db(d.taps(), f, srf);
                    assert!(db.abs() < 0.5, "sr {sr} stage {i}: {f} Hz at {db} dB");
                }
                // Stopband: everything that folds into the served band.
                let mut f = s.rate - s.blend_top;
                while f < srf / 2.0 {
                    let db = response_db(d.taps(), f, srf);
                    assert!(
                        db < -STOPBAND_DB,
                        "sr {sr} stage {i}: alias source {f} Hz only {db} dB down"
                    );
                    f *= 1.05;
                }
            }
        }
    }

    /// The invariant the type exists for: one shared phase counter means the
    /// two channels emit the same number of samples from the same input
    /// positions, for **any** block partitioning of the same stream.
    #[test]
    fn output_is_independent_of_how_the_input_is_chunked() {
        let taps = kaiser_lowpass(200.0, 2_800.0, 48_000.0, STOPBAND_DB);
        let src: Vec<f32> = (0..20_000)
            .map(|i| ((i as f32) * 0.001).sin() + 0.3 * ((i as f32) * 0.031).sin())
            .collect();
        let other: Vec<f32> = src.iter().map(|v| v * 0.5).collect();

        let mut whole = (Vec::new(), Vec::new());
        PairDecimator::new(taps.clone(), 16).push(&src, &other, &mut whole.0, &mut whole.1);

        for chunk in [1usize, 7, 1024, 4096] {
            let mut d = PairDecimator::new(taps.clone(), 16);
            let mut got = (Vec::new(), Vec::new());
            for c in src.chunks(chunk).zip(other.chunks(chunk)) {
                d.push(c.0, c.1, &mut got.0, &mut got.1);
            }
            assert_eq!(got.0.len(), whole.0.len(), "chunk {chunk}");
            for (a, b) in got.0.iter().zip(whole.0.iter()) {
                assert!((a - b).abs() < 1e-12, "chunk {chunk}");
            }
            for (a, b) in got.1.iter().zip(whole.1.iter()) {
                assert!((a - b).abs() < 1e-12, "chunk {chunk} ref leg");
            }
        }
    }

    /// Both channels come out the same length from the same input positions —
    /// stated directly, since "up to M−1 apart" is the failure being excluded.
    #[test]
    fn both_channels_advance_together() {
        let mut d = PairDecimator::new(vec![1.0; 33], 15);
        let a: Vec<f32> = (0..4_001).map(|i| i as f32).collect();
        let b: Vec<f32> = a.iter().map(|v| -v).collect();
        let (mut om, mut or_) = (Vec::new(), Vec::new());
        for (ca, cb) in a.chunks(97).zip(b.chunks(97)) {
            d.push(ca, cb, &mut om, &mut or_);
            assert_eq!(om.len(), or_.len(), "channels diverged mid-stream");
        }
        assert_eq!(om.len(), 4_001 / 15);
        // Identical inputs up to sign must give identical outputs up to sign:
        // any phase difference would break this immediately.
        for (m, r) in om.iter().zip(or_.iter()) {
            assert!((m + r).abs() < 1e-9);
        }
    }

    #[test]
    #[should_panic(expected = "same length")]
    fn unequal_blocks_are_refused() {
        let mut d = PairDecimator::identity();
        let (mut a, mut b) = (Vec::new(), Vec::new());
        d.push(&[0.0, 1.0], &[0.0], &mut a, &mut b);
    }

    /// Stage 0 is a pass-through, exactly — no filtering, no delay, no
    /// resampling. The full-rate band must not be touched by ladder machinery.
    #[test]
    fn stage_zero_is_a_bit_exact_passthrough() {
        let l = ladder::layout(48_000).unwrap();
        let mut d = PairDecimator::for_stage(48_000.0, &l.stages[0]);
        let a: Vec<f32> = (0..1_000).map(|i| (i as f32 * 0.37).sin()).collect();
        let (mut om, mut or_) = (Vec::new(), Vec::new());
        d.push(&a, &a, &mut om, &mut or_);
        assert_eq!(om.len(), a.len());
        for (i, v) in om.iter().enumerate() {
            assert_eq!(*v, f64::from(a[i]));
        }
    }

    /// Cost check behind the "independent chains are affordable" decision:
    /// taps/M per input sample, summed over the ladder.
    #[test]
    fn per_sample_cost_stays_small_at_every_rate() {
        for sr in [44_100u32, 48_000, 96_000, 192_000] {
            let srf = f64::from(sr);
            let l = ladder::layout(sr).unwrap();
            let mac: f64 = l
                .stages
                .iter()
                .skip(1)
                .map(|s| {
                    let d = PairDecimator::for_stage(srf, s);
                    d.taps().len() as f64 / s.decim as f64
                })
                .sum();
            assert!(
                mac < 25.0,
                "sr {sr}: {mac} MAC per input sample per channel"
            );
        }
    }
}
