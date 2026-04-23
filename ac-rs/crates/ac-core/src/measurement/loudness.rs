//! Tier 1 — ITU-R BS.1770-5 loudness measurement.
//!
//! Phase A (this commit) ships the K-weighting filter cascade (§2.1
//! pre-filter high-shelf + §2.2 RLB high-pass) and the 400 ms /
//! 100 ms-step mean-square gating-block accumulator (§2.3). Momentary /
//! short-term / integrated computation with two-pass gating, LRA, and
//! true-peak land in later phases — see `docs/loudness-bs1770-5.md`.
//!
//! The K-weighting coefficients are re-derived at runtime from the two
//! closed-form biquad designs BS.1770 uses — a custom Vh/Vb high-shelf
//! for the pre-filter (not a plain RBJ cookbook shelf) and an
//! un-normalized 1/-2/1 high-pass for the RLB stage. Their parameters are
//! chosen so that at fs = 48 kHz the coefficients reproduce Annex 1
//! Table 1 exactly; at other rates (44.1 / 88.2 / 96 / 192 kHz) the same
//! formulas give a consistent K curve without rate-specific lookup
//! tables. A unit test locks the 48 kHz derivation against Annex 1.
//! Reference implementations: libebur128, ffmpeg's `af_ebur128`.

use std::collections::VecDeque;
use std::f64::consts::PI;

use anyhow::{bail, Result};
use realfft::num_complex::Complex;

use crate::measurement::report::StandardsCitation;

/// LKFS formula offset, BS.1770-5 §2.5: `L = -0.691 + 10·log10(MS_K)`.
/// Compensates for K-weighting's ~+0.691 dB gain at the 1 kHz reference so
/// a 0 dBFS 1 kHz sine gives −3.01 LKFS.
pub const LKFS_OFFSET_DB: f64 = -0.691;

/// Pre-filter parameters. At fs = 48 kHz the custom Vh/Vb shelf below
/// reproduces Annex 1 Table 1 exactly.
const PRE_F0_HZ: f64 = 1_681.974_450_955_532;
const PRE_Q: f64 = 0.707_175_236_955_419_6;
const PRE_GAIN_DB: f64 = 3.999_843_853_973_347;

/// RLB high-pass parameters. Reproduces Annex 1 a1/a2 at 48 kHz; the
/// filter's numerator is fixed at 1/-2/1 per §2.2 (not unity-gain
/// normalized — the gain offset is absorbed by the LKFS formula).
const RLB_F0_HZ: f64 = 38.135_470_876_024_44;
const RLB_Q: f64 = 0.500_327_037_323_877_3;

/// Gating block timing, BS.1770-5 §2.3. 400 ms window, 75 % overlap
/// (→ 100 ms step).
const BLOCK_DURATION_S: f64 = 0.400;
const BLOCK_STEP_S: f64 = 0.100;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl Biquad {
    fn transfer(&self, z_inv: Complex<f64>) -> Complex<f64> {
        let z2 = z_inv * z_inv;
        let num = Complex::new(self.b0, 0.0) + z_inv * self.b1 + z2 * self.b2;
        let den = Complex::new(1.0, 0.0) + z_inv * self.a1 + z2 * self.a2;
        num / den
    }
}

/// Streaming K-weighting filter. Apply per channel; state is preserved
/// across `apply` calls so the caller can feed arbitrarily-sized blocks.
#[derive(Clone, Debug)]
pub struct KWeighting {
    sample_rate: u32,
    biquads: [Biquad; 2],
    state: [[f64; 2]; 2],
}

impl KWeighting {
    pub fn new(sample_rate: u32) -> Result<Self> {
        if sample_rate == 0 {
            bail!("sample_rate must be positive");
        }
        let fs = sample_rate as f64;
        let biquads = [
            bs1770_pre_filter(PRE_F0_HZ, PRE_Q, PRE_GAIN_DB, fs),
            bs1770_rlb_filter(RLB_F0_HZ, RLB_Q, fs),
        ];
        Ok(Self {
            sample_rate,
            biquads,
            state: [[0.0; 2]; 2],
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Run `samples` through the K-weighting cascade. Streaming: state is
    /// preserved across calls.
    pub fn apply(&mut self, samples: &[f32]) -> Vec<f32> {
        samples
            .iter()
            .map(|&x| self.process_sample(x as f64) as f32)
            .collect()
    }

    fn process_sample(&mut self, x_in: f64) -> f64 {
        let mut x = x_in;
        for (bq, s) in self.biquads.iter().zip(self.state.iter_mut()) {
            // Direct Form II Transposed.
            let y = bq.b0 * x + s[0];
            s[0] = bq.b1 * x - bq.a1 * y + s[1];
            s[1] = bq.b2 * x - bq.a2 * y;
            x = y;
        }
        x
    }

    pub fn reset(&mut self) {
        self.state = [[0.0; 2]; 2];
    }

    /// Magnitude of the full K cascade at `f_hz`, in dB.
    pub fn magnitude_db(&self, f_hz: f64) -> f64 {
        let omega = 2.0 * PI * f_hz / self.sample_rate as f64;
        let z_inv = Complex::from_polar(1.0, -omega);
        let h: Complex<f64> = self
            .biquads
            .iter()
            .fold(Complex::new(1.0, 0.0), |acc, bq| acc * bq.transfer(z_inv));
        20.0 * h.norm().log10()
    }
}

/// 400 ms / 100 ms-step gating-block accumulator. Feed K-weighted samples
/// via [`push`](Self::push); completed block mean-squares are returned in
/// emission order. Nothing is emitted until the ring has accumulated a
/// full 400 ms of audio.
pub struct GatingBlock {
    sample_rate: u32,
    block_len: usize,
    step_len: usize,
    ring: VecDeque<f64>,
    running_sum: f64,
    step_counter: usize,
}

impl GatingBlock {
    pub fn new(sample_rate: u32) -> Result<Self> {
        if sample_rate == 0 {
            bail!("sample_rate must be positive");
        }
        let fs = sample_rate as f64;
        let block_len = (fs * BLOCK_DURATION_S).round() as usize;
        let step_len = (fs * BLOCK_STEP_S).round() as usize;
        Ok(Self {
            sample_rate,
            block_len,
            step_len,
            ring: VecDeque::with_capacity(block_len + step_len),
            running_sum: 0.0,
            step_counter: 0,
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn block_len(&self) -> usize {
        self.block_len
    }

    pub fn step_len(&self) -> usize {
        self.step_len
    }

    /// Push samples (any length). Returns zero or more completed block
    /// mean-squares in the order they completed.
    pub fn push(&mut self, samples: &[f32]) -> Vec<f64> {
        let mut out = Vec::new();
        for &x in samples {
            let sq = (x as f64) * (x as f64);
            self.ring.push_back(sq);
            self.running_sum += sq;
            if self.ring.len() > self.block_len {
                let dropped = self.ring.pop_front().expect("ring non-empty");
                self.running_sum -= dropped;
            }
            self.step_counter += 1;
            if self.ring.len() >= self.block_len && self.step_counter >= self.step_len {
                // Clamp against subtractive-cancellation drift at bit-noise
                // level — mean-square is physically non-negative.
                let ms = (self.running_sum / self.block_len as f64).max(0.0);
                out.push(ms);
                self.step_counter = 0;
            }
        }
        out
    }

    pub fn reset(&mut self) {
        self.ring.clear();
        self.running_sum = 0.0;
        self.step_counter = 0;
    }
}

/// Convert a (K-weighted) mean-square to LKFS using the BS.1770-5 §2.5
/// offset. Mono / single-channel usage passes `ms` directly; multichannel
/// callers pre-compute the channel-weighted sum of mean-squares per §2.4
/// and pass that. `ms ≤ 0` maps to `-∞` (silence).
pub fn ms_to_lkfs(ms: f64) -> f64 {
    if ms <= 0.0 {
        f64::NEG_INFINITY
    } else {
        LKFS_OFFSET_DB + 10.0 * ms.log10()
    }
}

pub fn citation() -> StandardsCitation {
    StandardsCitation {
        standard: "ITU-R BS.1770-5".into(),
        clause: "§2.1 Pre-filter, §2.2 RLB weighting, §2.3 Gating block".into(),
        verified: false,
    }
}

// ---------------------------------------------------------------------------
// BS.1770 biquad designs (from libebur128 / ffmpeg ebur128).
// These are NOT plain RBJ cookbook formulas — the pre-filter uses a
// custom Vh/Vb shelf and the RLB high-pass uses a fixed 1/-2/1 numerator
// without unity-gain normalization. Chosen so that at fs = 48 kHz the
// coefficients reproduce Annex 1 Table 1 exactly.
// ---------------------------------------------------------------------------

fn bs1770_pre_filter(f0: f64, q: f64, gain_db: f64, fs: f64) -> Biquad {
    let k = (PI * f0 / fs).tan();
    let k2 = k * k;
    let vh = 10.0_f64.powf(gain_db / 20.0);
    // Empirical exponent — see libebur128 ebur128.c; makes the shelf
    // meet the Annex 1 coefficients without an explicit gain trim.
    let vb = vh.powf(0.499_666_774_155);
    let a0p = 1.0 + k / q + k2;

    Biquad {
        b0: (vh + vb * k / q + k2) / a0p,
        b1: 2.0 * (k2 - vh) / a0p,
        b2: (vh - vb * k / q + k2) / a0p,
        a1: 2.0 * (k2 - 1.0) / a0p,
        a2: (1.0 - k / q + k2) / a0p,
    }
}

fn bs1770_rlb_filter(f0: f64, q: f64, fs: f64) -> Biquad {
    let k = (PI * f0 / fs).tan();
    let k2 = k * k;
    let a0p = 1.0 + k / q + k2;

    Biquad {
        b0: 1.0,
        b1: -2.0,
        b2: 1.0,
        a1: 2.0 * (k2 - 1.0) / a0p,
        a2: (1.0 - k / q + k2) / a0p,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const FS: u32 = 48_000;

    /// Annex 1 Table 1 — published digital coefficients at 48 kHz. These
    /// are what commercial EBU R128 decoders lock against. Our RBJ
    /// derivation must reproduce them to ~1e-6 or tighter.
    const ANNEX1_PRE_B0: f64 = 1.535_124_859_586_97;
    const ANNEX1_PRE_B1: f64 = -2.691_696_189_406_38;
    const ANNEX1_PRE_B2: f64 = 1.198_392_810_852_85;
    const ANNEX1_PRE_A1: f64 = -1.690_659_293_182_41;
    const ANNEX1_PRE_A2: f64 = 0.732_480_774_215_85;

    const ANNEX1_RLB_B0: f64 = 1.0;
    const ANNEX1_RLB_B1: f64 = -2.0;
    const ANNEX1_RLB_B2: f64 = 1.0;
    const ANNEX1_RLB_A1: f64 = -1.990_047_454_833_98;
    const ANNEX1_RLB_A2: f64 = 0.990_072_250_366_21;

    #[test]
    fn coefficients_match_bs1770_annex1_at_48khz() {
        let k = KWeighting::new(FS).unwrap();
        let pre = k.biquads[0];
        let rlb = k.biquads[1];
        let tol = 1e-6;
        assert!((pre.b0 - ANNEX1_PRE_B0).abs() < tol, "pre b0 = {}", pre.b0);
        assert!((pre.b1 - ANNEX1_PRE_B1).abs() < tol, "pre b1 = {}", pre.b1);
        assert!((pre.b2 - ANNEX1_PRE_B2).abs() < tol, "pre b2 = {}", pre.b2);
        assert!((pre.a1 - ANNEX1_PRE_A1).abs() < tol, "pre a1 = {}", pre.a1);
        assert!((pre.a2 - ANNEX1_PRE_A2).abs() < tol, "pre a2 = {}", pre.a2);
        assert!((rlb.b0 - ANNEX1_RLB_B0).abs() < tol, "rlb b0 = {}", rlb.b0);
        assert!((rlb.b1 - ANNEX1_RLB_B1).abs() < tol, "rlb b1 = {}", rlb.b1);
        assert!((rlb.b2 - ANNEX1_RLB_B2).abs() < tol, "rlb b2 = {}", rlb.b2);
        assert!((rlb.a1 - ANNEX1_RLB_A1).abs() < tol, "rlb a1 = {}", rlb.a1);
        assert!((rlb.a2 - ANNEX1_RLB_A2).abs() < tol, "rlb a2 = {}", rlb.a2);
    }

    #[test]
    fn rejects_zero_sample_rate() {
        assert!(KWeighting::new(0).is_err());
        assert!(GatingBlock::new(0).is_err());
    }

    #[test]
    fn k_weighting_gain_at_1khz_matches_lkfs_offset() {
        // LKFS = -0.691 + 10·log10(MS). A 0 dBFS 1 kHz sine has
        // MS = 0.5 unfiltered; to map to -3.01 LKFS the filter must
        // contribute ~+0.691 dB at the reference frequency.
        let k = KWeighting::new(FS).unwrap();
        let db = k.magnitude_db(1000.0);
        assert!(
            (db - 0.691).abs() < 0.05,
            "K-weighting @ 1 kHz = {db:.3} dB, expected ~+0.691 dB"
        );
    }

    #[test]
    fn k_weighting_rolls_off_at_low_frequencies() {
        // The RLB high-pass places -3 dB near 38 Hz; at 20 Hz expect
        // clearly more attenuation than that.
        let k = KWeighting::new(FS).unwrap();
        let db_20 = k.magnitude_db(20.0);
        assert!(
            db_20 < -6.0,
            "K-weighting @ 20 Hz = {db_20:.2} dB, expected < −6 dB"
        );
    }

    #[test]
    fn dc_is_rejected_after_settling() {
        let mut k = KWeighting::new(FS).unwrap();
        let dc = vec![1.0_f32; (FS as usize) * 2];
        let y = k.apply(&dc);
        let tail: Vec<f64> = y.iter().rev().take(100).map(|&v| v as f64).collect();
        let rms = (tail.iter().map(|v| v * v).sum::<f64>() / tail.len() as f64).sqrt();
        assert!(rms < 1e-3, "DC tail RMS {rms:e} should be near zero");
    }

    #[test]
    fn reset_clears_state() {
        let mut k = KWeighting::new(FS).unwrap();
        let impulse = vec![1.0_f32; 1000];
        let _ = k.apply(&impulse);
        k.reset();
        let zeros = vec![0.0_f32; 1000];
        let y = k.apply(&zeros);
        for &v in &y {
            assert!(v.abs() < 1e-9);
        }
    }

    #[test]
    fn zero_input_emits_zero_blocks() {
        let mut b = GatingBlock::new(FS).unwrap();
        let zeros = vec![0.0_f32; FS as usize];
        let ms = b.push(&zeros);
        assert!(!ms.is_empty(), "expected ≥ 1 block from 1 s of input");
        for v in ms {
            assert!(v.abs() < 1e-12, "zero input → ms {v}");
        }
    }

    #[test]
    fn gating_block_primes_then_emits_every_100ms() {
        let mut b = GatingBlock::new(FS).unwrap();
        // 399 ms — not enough yet.
        let early = vec![0.1_f32; ((FS as f64) * 0.399) as usize];
        assert!(b.push(&early).is_empty(), "no block before 400 ms");
        // Cross the 400 ms boundary → one block.
        let across = vec![0.1_f32; ((FS as f64) * 0.002) as usize];
        let first = b.push(&across);
        assert_eq!(first.len(), 1);
        // One more step → second block.
        let step = vec![0.1_f32; ((FS as f64) * 0.100) as usize];
        let second = b.push(&step);
        assert_eq!(second.len(), 1);
    }

    #[test]
    fn zero_dbfs_sine_at_997hz_gives_minus_3_01_lkfs() {
        // BS.1770 reference frequency is 997 Hz (a prime near 1 kHz,
        // avoiding coincidence with DFT grids in downstream tests).
        let mut k = KWeighting::new(FS).unwrap();
        let mut b = GatingBlock::new(FS).unwrap();
        let w = 2.0 * PI * 997.0 / FS as f64;
        let x: Vec<f32> = (0..(FS * 2) as usize)
            .map(|i| (w * i as f64).sin() as f32)
            .collect();
        let y = k.apply(&x);
        let blocks = b.push(&y);
        assert!(!blocks.is_empty(), "no blocks from 2 s of audio");
        // Tail-average to avoid the first block that straddles filter
        // settling. 4 × 100 ms blocks = 400 ms of averaging.
        let n = 4;
        let lkfs: f64 = blocks
            .iter()
            .rev()
            .take(n)
            .map(|&ms| ms_to_lkfs(ms))
            .sum::<f64>()
            / n as f64;
        assert!(
            (lkfs - -3.01).abs() < 0.05,
            "0 dBFS 997 Hz sine → {lkfs:.3} LKFS, expected −3.01 ±0.05"
        );
    }

    #[test]
    fn ms_to_lkfs_silence_is_neg_infinity() {
        assert_eq!(ms_to_lkfs(0.0), f64::NEG_INFINITY);
        assert_eq!(ms_to_lkfs(-1e-20), f64::NEG_INFINITY);
    }

    #[test]
    fn ms_to_lkfs_roundtrip() {
        // LKFS(1.0) = -0.691 + 0 = -0.691
        assert!((ms_to_lkfs(1.0) - -0.691).abs() < 1e-12);
        // LKFS(0.5) = -0.691 - 3.0103 = -3.7013 — matches the value a
        // pre-filter (unity at HF, not 0.691 dB boost) would yield.
        assert!((ms_to_lkfs(0.5) - (-0.691 - 10.0 * 2.0_f64.log10())).abs() < 1e-12);
    }

    #[test]
    fn citation_shape() {
        let c = citation();
        assert!(
            c.standard.contains("ITU-R BS.1770-5"),
            "got standard = {}",
            c.standard
        );
        assert!(c.clause.contains("§2.1"));
        assert!(!c.verified);
    }

    #[test]
    fn k_weighting_coefficients_stable_at_other_sample_rates() {
        // The design path yields finite, non-NaN coefficients at every
        // sample rate we expect to see. Regression-catch against a future
        // refactor that accidentally divides by zero at some boundary.
        for &sr in &[44_100u32, 48_000, 88_200, 96_000, 192_000] {
            let k = KWeighting::new(sr).unwrap();
            for bq in k.biquads.iter() {
                for v in [bq.b0, bq.b1, bq.b2, bq.a1, bq.a2] {
                    assert!(v.is_finite(), "non-finite coeff at fs={sr}: {v}");
                }
            }
            // Gain at 1 kHz stays within the expected small neighbourhood
            // of +0.691 dB across rates (±0.05 dB).
            let db = k.magnitude_db(1000.0);
            assert!(
                (db - 0.691).abs() < 0.05,
                "K @ 1 kHz, fs={sr}: {db:.3} dB off spec"
            );
        }
    }
}
