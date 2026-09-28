//! The arrival IR (#706): a short, fast impulse response for watching an
//! arrival and its early reflections move, beside the 1 s Welch IR.
//!
//! The Welch IR ([`super::transfer::h1_estimate_with_delay`]) takes 1 s
//! segments and a 0.5 s hop over four averages: it changes every 0.5 s and
//! holds 2.5 s of audio, which the operator found too sluggish to follow an
//! adjustment. This one takes [`SEGMENT_S`] segments at 75 % overlap and
//! averages [`BLOCKS`] of them: a new answer every 62.5 ms, from the last
//! 0.5625 s. The price is length — ±125 ms around the held delay — and
//! 4 Hz spectral resolution; late reverberation stays with the 1 s IR.
//!
//! No new estimator: the blocks are the ladder's ([`super::mtw`]'s
//! `analyse_block`, `BlockAverage`), `H₁ = Sxy/Sxx` as in the Welch path, and
//! the IR is [`super::transfer::impulse_response_from_h`]. Alignment is by
//! the held delay, applied to the samples ([`super::mtw::align`]) rather
//! than as a phase rotation: a short segment only sees the arrival if the
//! two legs are shifted into correspondence first.

use realfft::num_complex::Complex;
use realfft::RealFftPlanner;

use super::mtw::average::BlockAverage;

/// Segment length, seconds: 4 Hz bins, an IR ±125 ms long.
pub const SEGMENT_S: f64 = 0.25;
/// Segment length over hop: 75 % overlap.
pub const HOP_DIVISOR: usize = 4;
/// Blocks averaged. With 75 % overlap, six blocks cover 0.5625 s.
pub const BLOCKS: usize = 6;

/// The arrival IR's running state for one pair, fed the pair's
/// delay-aligned full-rate stream.
pub struct LiveIr {
    nperseg: usize,
    hop: usize,
    window: Vec<f64>,
    planner: RealFftPlanner<f64>,
    buf_meas: Vec<f64>,
    buf_ref: Vec<f64>,
    avg: BlockAverage,
    /// Blocks analysed since this was built; a new IR exists when it moves.
    blocks: u64,
    /// Aligned samples still to discard: after a gap, the aligner's queue
    /// pairs up to `|delay|` samples from before it with samples after it
    /// (Codex recheck of #706).
    skip: usize,
}

impl LiveIr {
    pub fn new(sr: u32) -> Self {
        let nperseg = segment_len(sr);
        Self {
            nperseg,
            hop: nperseg / HOP_DIVISOR,
            window: super::mtw::hann(nperseg),
            planner: RealFftPlanner::new(),
            buf_meas: Vec::with_capacity(nperseg * 2),
            buf_ref: Vec::with_capacity(nperseg * 2),
            avg: BlockAverage::new(nperseg / 2 + 1, BLOCKS),
            blocks: 0,
            skip: 0,
        }
    }

    /// As [`Self::new`], discarding the first `skip` aligned samples pushed.
    pub fn after_gap(sr: u32, skip: usize) -> Self {
        Self {
            skip,
            ..Self::new(sr)
        }
    }

    /// Push delay-aligned samples (`meas[n]` paired with `ref[n − D]`),
    /// equal lengths. Blocks sit on a fixed grid from the first sample
    /// pushed, as the ladder's do.
    pub fn push(&mut self, meas: &[f32], reference: &[f32]) {
        let drop = self.skip.min(meas.len());
        self.skip -= drop;
        let (meas, reference) = (&meas[drop..], &reference[drop..]);
        self.buf_meas.extend(meas.iter().map(|&v| f64::from(v)));
        self.buf_ref.extend(reference.iter().map(|&v| f64::from(v)));
        let mut pos = 0;
        while pos + self.nperseg <= self.buf_meas.len() {
            super::mtw::analyse_block(
                &mut self.planner,
                &self.window,
                &self.buf_meas[pos..pos + self.nperseg],
                &self.buf_ref[pos..pos + self.nperseg],
                &mut self.avg,
            );
            self.blocks += 1;
            pos += self.hop;
        }
        if pos > 0 {
            self.buf_meas.drain(..pos);
            self.buf_ref.drain(..pos);
        }
    }

    /// Blocks analysed so far. The IR changes exactly when this does.
    pub fn blocks(&self) -> u64 {
        self.blocks
    }

    /// The IR, `fftshift`-centred like the Welch one, once all [`BLOCKS`]
    /// are in — never from a partial average, whose noise would read as
    /// reflections.
    pub fn impulse_response(&self) -> Option<Vec<f32>> {
        if !self.avg.settled() {
            return None;
        }
        let (sxx, _, sxy) = self.avg.mean()?;
        let (re, im): (Vec<f64>, Vec<f64>) = sxx
            .iter()
            .zip(&sxy)
            .map(|(&x, &xy)| {
                // The Welch path's H₁, with its floor.
                let h: Complex<f64> = xy / x.max(1e-30);
                (h.re, h.im)
            })
            .unzip();
        let ir = super::transfer::impulse_response_from_h(&re, &im);
        (!ir.is_empty()).then_some(ir)
    }
}

/// Segment length in samples at `sr`: [`SEGMENT_S`], even.
pub fn segment_len(sr: u32) -> usize {
    ((f64::from(sr) * SEGMENT_S).round() as usize) & !1
}

/// Seconds between answers at `sr`.
pub fn hop_s(sr: u32) -> f64 {
    (segment_len(sr) / HOP_DIVISOR) as f64 / f64::from(sr)
}

/// Seconds of audio one answer is made from.
pub fn span_s(sr: u32) -> f64 {
    let n = segment_len(sr) as f64;
    (n + (n / HOP_DIVISOR as f64) * (BLOCKS - 1) as f64) / f64::from(sr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::visualize::mtw::MtwPair;

    const SR: u32 = 48_000;

    fn noise(n: usize, seed: u64) -> Vec<f32> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s = s
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (((s >> 40) as f64 / (1u64 << 24) as f64) * 2.0 - 1.0) as f32 * 0.5
            })
            .collect()
    }

    /// The DUT: `gain` and `delay` samples late.
    fn dut(reference: &[f32], delay: usize, gain: f32) -> Vec<f32> {
        (0..reference.len())
            .map(|i| {
                if i >= delay {
                    reference[i - delay] * gain
                } else {
                    0.0
                }
            })
            .collect()
    }

    fn peak(ir: &[f32]) -> (usize, f32) {
        ir.iter()
            .enumerate()
            .map(|(i, v)| (i, v.abs()))
            .fold((0, 0.0), |a, b| if b.1 > a.1 { b } else { a })
    }

    /// Fed through the ladder's aligner at the held delay, the arrival sits
    /// at the IR's centre (the residual, 0) at the DUT's gain — even for a
    /// delay close to the segment length. The rejected alternative, the
    /// Welch path's phase rotation applied to unshifted 250 ms segments, is
    /// computed here: with 0.2 s of delay only a fifth of each segment
    /// pair overlaps, and the arrival collapses.
    #[test]
    fn the_arrival_sits_at_the_residual_where_phase_rotation_loses_it() {
        let delay = 9_600usize; // 0.2 s
        let reference = noise(SR as usize * 3, 7);
        let meas = dut(&reference, delay, 0.5);

        let mut p = MtwPair::new(SR, delay as i64, 4).unwrap().with_live_ir(SR);
        for (m, r) in meas.chunks(2_400).zip(reference.chunks(2_400)) {
            p.push(m, r);
        }
        let ir = p.live_ir().unwrap().impulse_response().expect("settled");
        let (at, height) = peak(&ir);
        assert_eq!(at, ir.len() / 2, "arrival not at the residual");
        assert!((height - 0.5).abs() < 0.05, "arrival height {height}");

        // Rejected: unshifted segments, delay removed by rotating Sxy.
        let n = segment_len(SR);
        let mut rot = LiveIr::new(SR);
        let (m64, r64): (Vec<f32>, Vec<f32>) = (meas.clone(), reference.clone());
        rot.push(&m64, &r64);
        let (sxx, _, sxy) = rot.avg.mean().unwrap();
        let (re, im): (Vec<f64>, Vec<f64>) = sxx
            .iter()
            .zip(&sxy)
            .enumerate()
            .map(|(k, (&x, &xy))| {
                let ph = 2.0 * std::f64::consts::PI * k as f64 * delay as f64 / n as f64;
                let h = xy * Complex::new(ph.cos(), ph.sin()) / x.max(1e-30);
                (h.re, h.im)
            })
            .unzip();
        let rotated = crate::visualize::transfer::impulse_response_from_h(&re, &im);
        let (_, rotated_height) = peak(&rotated);
        assert!(
            rotated_height < 0.25,
            "phase rotation kept {rotated_height} of 0.5 — the alignment buys nothing"
        );
    }

    /// Nothing before all six blocks; after that a new answer every hop, and
    /// only then.
    #[test]
    fn it_answers_once_settled_and_then_every_hop() {
        let mut ir = LiveIr::new(SR);
        let n = segment_len(SR);
        let hop = n / HOP_DIVISOR;
        let reference = noise(n + hop * BLOCKS, 3);
        // Independent noise on the measurement, so each answer differs.
        let meas: Vec<f32> = dut(&reference, 0, 1.0)
            .iter()
            .zip(noise(reference.len(), 11))
            .map(|(m, n)| m + 0.1 * n)
            .collect();
        let settle = n + hop * (BLOCKS - 1);
        ir.push(&meas[..settle - 1], &reference[..settle - 1]);
        assert!(
            ir.impulse_response().is_none(),
            "answered from a partial average"
        );
        ir.push(&meas[settle - 1..settle], &reference[settle - 1..settle]);
        assert_eq!(ir.blocks(), BLOCKS as u64);
        let first = ir.impulse_response().expect("settled");
        ir.push(
            &meas[settle..settle + hop - 1],
            &reference[settle..settle + hop - 1],
        );
        assert_eq!(ir.blocks(), BLOCKS as u64, "a block before a whole hop");
        ir.push(
            &meas[settle + hop - 1..settle + hop],
            &reference[settle + hop - 1..settle + hop],
        );
        assert_eq!(ir.blocks(), BLOCKS as u64 + 1);
        assert!(
            ir.impulse_response().unwrap() != first,
            "a new block left the IR unchanged"
        );
        assert!((hop_s(SR) - 0.0625).abs() < 1e-12);
        assert!((span_s(SR) - 0.5625).abs() < 1e-12);
    }

    /// Codex recheck of #706: restarted after a gap, the arrival IR discards
    /// the aligner's backlog — the first `skip` samples — before its first
    /// segment, so no block pairs audio from both sides of the gap.
    #[test]
    fn after_a_gap_the_aligner_backlog_is_discarded() {
        let n = segment_len(SR);
        let skip = 480;
        let x = noise(n + skip, 5);
        let mut ir = LiveIr::after_gap(SR, skip);
        ir.push(&x[..n + skip - 1], &x[..n + skip - 1]);
        assert_eq!(ir.blocks(), 0, "a block took backlog samples");
        ir.push(&x[n + skip - 1..], &x[n + skip - 1..]);
        assert_eq!(ir.blocks(), 1);
    }
}
