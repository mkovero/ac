//! Reference alignment: one signed integer offset per pair, applied at full
//! rate before any decimation.
//!
//! Per `design-mtw-alignment.md` (option A). The alternative — converting the
//! delay into each band's own decimated rate — rounds by up to `M_b/2` input
//! samples, and because the ladder puts band top edges proportional to `1/M_b`
//! that rounding is a scale-invariant ~12° of phase at every band's top edge,
//! appearing as a phase step of up to ~24° at each crossover. Aligning once at
//! full rate has exactly zero rounding error: the offset is already an integer
//! there.
//!
//! Decimation latency does not need aligning per band. It is common-mode and
//! cancels in `H1 = Gxy/Gxx` provided both channels traverse identical chains,
//! which is what [`super::decimate::PairDecimator`] guarantees.
//!
//! # The offset is signed
//!
//! `estimate_delay(ref, meas)` returns `D` with `meas[n] ≈ ref[n − D]`, and on
//! today's hardware `D` is **negative** — about −19200 at 96 kHz while #216's
//! ring skew is live. A design that assumes a non-negative offset breaks on the
//! rig, so both directions are first-class here and both are tested.
//!
//! # Retention
//!
//! This aligner is a streaming FIFO, so it holds `|offset|` samples plus one
//! block, not a whole analysis window: the ladder never re-segments retained
//! audio (that re-analysis is #208's ripple), so each input sample enters each
//! stage exactly once. The larger `W_deepest + |offset| + tick + transient`
//! figure in `design-mtw-alignment.md` sizes the history the *snapshot* path
//! re-reads, which is a different consumer.

use std::collections::VecDeque;

/// Streaming aligner for one measurement/reference pair.
pub struct PairAligner {
    offset: i64,
    meas: VecDeque<f32>,
    reference: VecDeque<f32>,
    /// Leading samples still to be dropped from each leg before the two are
    /// in correspondence. Exactly one of these is non-zero.
    skip_meas: usize,
    skip_ref: usize,
}

impl PairAligner {
    /// `offset` is `D` from `estimate_delay(ref, meas)`: the aligner emits
    /// pairs `(meas[n], ref[n − D])`.
    pub fn new(offset: i64) -> Self {
        // Pairing meas[n] with ref[n−D] is the same as pairing meas[m+D] with
        // ref[m]. For D > 0 the first D meas samples have no partner; for
        // D < 0 the first |D| ref samples have none.
        Self {
            offset,
            meas: VecDeque::new(),
            reference: VecDeque::new(),
            skip_meas: offset.max(0) as usize,
            skip_ref: (-offset).max(0) as usize,
        }
    }

    pub fn offset(&self) -> i64 {
        self.offset
    }

    /// Samples currently held on either leg — the aligner's retention.
    pub fn buffered(&self) -> usize {
        self.meas.len().max(self.reference.len())
    }

    /// Push one block of each channel and append every pair that has become
    /// complete.
    ///
    /// The two input blocks need not be the same length: the aligner is the
    /// component that absorbs the offset, so a leg running ahead is its normal
    /// state, not an error. What it emits is always equal-length and in
    /// correspondence, which is what [`super::decimate::PairDecimator`]
    /// requires.
    pub fn push(
        &mut self,
        meas: &[f32],
        reference: &[f32],
        out_meas: &mut Vec<f32>,
        out_ref: &mut Vec<f32>,
    ) {
        self.meas.extend(meas.iter().copied());
        self.reference.extend(reference.iter().copied());

        let drop_m = self.skip_meas.min(self.meas.len());
        self.meas.drain(..drop_m);
        self.skip_meas -= drop_m;
        let drop_r = self.skip_ref.min(self.reference.len());
        self.reference.drain(..drop_r);
        self.skip_ref -= drop_r;

        if self.skip_meas > 0 || self.skip_ref > 0 {
            return;
        }
        let n = self.meas.len().min(self.reference.len());
        out_meas.extend(self.meas.drain(..n));
        out_ref.extend(self.reference.drain(..n));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A DUT that only delays: `meas[k] = ref[k − D]`, both legs cut from one
    /// source whose every sample is distinct. Alignment is then exactly
    /// recoverable, and the assertion needs no arithmetic — correctly aligned
    /// legs are *equal*, sample for sample. A one-sample error shows up as a
    /// value mismatch rather than as a statistic.
    fn round_trip(delay: i64) {
        let pad = delay.unsigned_abs() as usize;
        // Long enough that the skipped prefix is a small part of the run.
        let n = 5_000usize + 2 * pad;
        let source = |i: i64| -> f32 {
            if i < 0 {
                0.0
            } else {
                i as f32
            }
        };
        let reference: Vec<f32> = (0..n).map(|k| source(k as i64)).collect();
        let meas: Vec<f32> = (0..n).map(|k| source(k as i64 - delay)).collect();

        let mut a = PairAligner::new(delay);
        let (mut om, mut or_) = (Vec::new(), Vec::new());
        for (cm, cr) in meas.chunks(311).zip(reference.chunks(311)) {
            a.push(cm, cr, &mut om, &mut or_);
        }

        assert_eq!(om.len(), or_.len(), "emitted legs must be equal length");
        assert_eq!(om.len(), n - pad, "delay {delay}: wrong number of pairs");
        for i in 0..om.len() {
            assert_eq!(
                om[i], or_[i],
                "delay {delay}: pair {i} misaligned ({} vs {})",
                om[i], or_[i]
            );
        }
        // And the alignment is not vacuous — the legs genuinely differed.
        assert_ne!(meas[pad + 10], reference[pad + 10]);
    }

    #[test]
    fn aligns_a_positive_offset() {
        round_trip(1_200);
    }

    /// The sign that today's hardware actually produces.
    #[test]
    fn aligns_a_negative_offset() {
        round_trip(-19_200);
    }

    #[test]
    fn zero_offset_is_a_passthrough() {
        let mut a = PairAligner::new(0);
        let (mut om, mut or_) = (Vec::new(), Vec::new());
        let m: Vec<f32> = (0..100).map(|i| i as f32).collect();
        a.push(&m, &m, &mut om, &mut or_);
        assert_eq!(om, m);
        assert_eq!(or_, m);
        assert_eq!(a.buffered(), 0);
    }

    /// Emitted blocks are always equal-length, whatever the input blocks do —
    /// the precondition `PairDecimator` asserts on.
    #[test]
    fn emitted_legs_are_always_equal_length() {
        let mut a = PairAligner::new(-500);
        let (mut om, mut or_) = (Vec::new(), Vec::new());
        for i in 0..50 {
            let m = vec![i as f32; 37];
            let r = vec![i as f32; 91];
            a.push(&m, &r, &mut om, &mut or_);
            assert_eq!(om.len(), or_.len(), "diverged at block {i}");
        }
    }

    /// Retention is the offset, not a window: the aligner holds `|offset|`
    /// plus at most one block, so the ladder's memory does not scale with the
    /// deepest stage's window.
    #[test]
    fn retention_is_bounded_by_the_offset() {
        let offset = 19_200i64;
        let mut a = PairAligner::new(offset);
        let (mut om, mut or_) = (Vec::new(), Vec::new());
        let block = vec![0.0f32; 4_800];
        for _ in 0..40 {
            a.push(&block, &block, &mut om, &mut or_);
            assert!(
                a.buffered() <= offset as usize + block.len(),
                "buffered {} exceeds |offset| + block",
                a.buffered()
            );
        }
    }
}
