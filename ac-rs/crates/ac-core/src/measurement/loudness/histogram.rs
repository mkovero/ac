//! Bounded history of channel-weighted mean-squares, bucketed by LKFS.
//!
//! Backs the gated statistics in [`super::state`]: see the type doc for
//! what a per-value history cost and what the bins approximate.

use super::{lkfs_to_ms, lu_ratio, ms_to_lkfs, ABSOLUTE_GATE_LKFS};

/// Loudness-histogram layout. Bin 0 opens exactly at the absolute gate,
/// so a value the gate would discard never enters the histogram at all.
/// 0.1 LU bins over a 100 LU span reach +30 LKFS, well past any level a
/// converter can deliver, and match the bin width libebur128 uses for
/// the same job -- an order of magnitude finer than the EBU Tech 3341 /
/// 3342 compliance tolerances the gated statistics are judged against.
const HIST_MIN_LKFS: f64 = ABSOLUTE_GATE_LKFS;
pub(super) const HIST_BIN_LU: f64 = 0.1;
const HIST_BINS: usize = 1_000;

#[derive(Clone, Copy, Default)]
struct HistBin {
    count: u64,
    sum_ms: f64,
}

/// Bounded history of channel-weighted mean-squares, bucketed by LKFS.
///
/// The gated statistics ask the history three questions: how many values
/// cleared the absolute gate, what their mean is, and where the order
/// statistics of the relative gate's survivors fall. None of those needs
/// the individual values back. Keeping them in a `Vec` made
/// `integrated`, `loudness_range` and `gated_duration_s` each cost O(n)
/// against an `n` that grows ten entries per second for as long as the
/// session runs, and the monitor pays that on every emit tick on every
/// channel -- quadratic in session length. A fixed bin array answers all
/// three in bounded time and bounded memory.
///
/// Each bin carries an exact `sum_ms` rather than a bin-centre stand-in,
/// so a gated mean is exact but for the values sitting in the single bin
/// the relative gate cuts through. Percentiles resolve to the bin width.
pub(super) struct LoudnessHistogram {
    bins: Vec<HistBin>,
    /// Values admitted (i.e. at or above the absolute gate).
    count: u64,
    /// Exact sum of the admitted values.
    sum_ms: f64,
}

impl LoudnessHistogram {
    pub(super) fn new() -> Self {
        Self {
            bins: vec![HistBin::default(); HIST_BINS],
            count: 0,
            sum_ms: 0.0,
        }
    }

    /// Bin for a mean-square, or `None` when it sits below the absolute
    /// gate. The admission test is written in the mean-square domain
    /// against the same `lkfs_to_ms(ABSOLUTE_GATE_LKFS)` the exact
    /// two-pass gate uses, so a value landing exactly on the gate falls
    /// the same way in both.
    pub(super) fn bin_index(ms: f64) -> Option<usize> {
        if ms < lkfs_to_ms(ABSOLUTE_GATE_LKFS) {
            return None;
        }
        Some(Self::bin_of_lkfs(ms_to_lkfs(ms)))
    }

    /// Bin containing `lkfs`, clamped to the array at both ends.
    pub(super) fn bin_of_lkfs(lkfs: f64) -> usize {
        let offset = (lkfs - HIST_MIN_LKFS).max(0.0);
        ((offset / HIST_BIN_LU) as usize).min(HIST_BINS - 1)
    }

    /// Centre level of bin `i`, used when reporting a percentile.
    pub(super) fn bin_centre_lkfs(i: usize) -> f64 {
        HIST_MIN_LKFS + (i as f64 + 0.5) * HIST_BIN_LU
    }

    pub(super) fn push(&mut self, ms: f64) {
        let Some(i) = Self::bin_index(ms) else {
            return;
        };
        self.bins[i].count += 1;
        self.bins[i].sum_ms += ms;
        self.count += 1;
        self.sum_ms += ms;
    }

    /// Count of values that cleared the absolute gate.
    pub(super) fn count(&self) -> u64 {
        self.count
    }

    /// First bin at or above the relative gate, which sits
    /// `rel_delta_lu` below the mean of everything admitted. `None` only
    /// when nothing has been admitted.
    pub(super) fn relative_gate_bin(&self, rel_delta_lu: f64) -> Option<usize> {
        if self.count == 0 {
            return None;
        }
        let ungated_mean_ms = self.sum_ms / self.count as f64;
        Some(Self::bin_of_lkfs(ms_to_lkfs(
            ungated_mean_ms * lu_ratio(rel_delta_lu),
        )))
    }

    /// Count and exact mean-square sum of the relative gate's survivors.
    pub(super) fn gated_totals(&self, rel_delta_lu: f64) -> Option<(u64, f64)> {
        let start = self.relative_gate_bin(rel_delta_lu)?;
        let mut n = 0u64;
        let mut sum = 0.0;
        for bin in &self.bins[start..] {
            n += bin.count;
            sum += bin.sum_ms;
        }
        if n == 0 {
            None
        } else {
            Some((n, sum))
        }
    }

    /// Number of values at or above the relative gate.
    pub(super) fn gated_count(&self, rel_delta_lu: f64) -> u64 {
        self.gated_totals(rel_delta_lu).map_or(0, |(n, _)| n)
    }

    /// Mean mean-square of the relative gate's survivors -- the second
    /// pass of BS.1770-5 §2.4.
    pub(super) fn gated_mean_ms(&self, rel_delta_lu: f64) -> Option<f64> {
        self.gated_totals(rel_delta_lu)
            .map(|(n, sum)| sum / n as f64)
    }

    /// Level at fractional rank `p` among the relative gate's survivors,
    /// in LKFS. Rank convention matches the exact reference in
    /// `state`: position `p * (n - 1)` in ascending order, here resolved
    /// to the containing bin's centre rather than interpolated between
    /// neighbours.
    pub(super) fn gated_percentile_lkfs(&self, rel_delta_lu: f64, p: f64) -> Option<f64> {
        let start = self.relative_gate_bin(rel_delta_lu)?;
        let (n, _) = self.gated_totals(rel_delta_lu)?;
        let rank = (p.clamp(0.0, 1.0) * (n - 1) as f64).floor() as u64;
        let mut seen = 0u64;
        for (i, bin) in self.bins.iter().enumerate().skip(start) {
            seen += bin.count;
            if seen > rank {
                return Some(Self::bin_centre_lkfs(i));
            }
        }
        None
    }

    pub(super) fn reset(&mut self) {
        self.bins.fill(HistBin::default());
        self.count = 0;
        self.sum_ms = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::super::RELATIVE_GATE_DELTA_LU;
    use super::*;

    #[test]
    fn histogram_drops_below_gate_and_clamps_above_range() {
        let mut h = LoudnessHistogram::new();
        let abs_gate_ms = lkfs_to_ms(ABSOLUTE_GATE_LKFS);
        // Below the absolute gate: never admitted.
        h.push(abs_gate_ms * 0.5);
        h.push(0.0);
        assert_eq!(h.count(), 0);
        assert!(h.gated_mean_ms(RELATIVE_GATE_DELTA_LU).is_none());
        // Exactly on the gate: admitted, and into bin 0.
        h.push(abs_gate_ms);
        assert_eq!(h.count(), 1);
        assert_eq!(LoudnessHistogram::bin_index(abs_gate_ms), Some(0));
        // Absurdly loud: clamped into the top bin rather than lost.
        let huge = lkfs_to_ms(500.0);
        assert_eq!(LoudnessHistogram::bin_index(huge), Some(HIST_BINS - 1));
        h.push(huge);
        assert_eq!(h.count(), 2);
    }

    #[test]
    fn histogram_reset_clears_bins_not_just_totals() {
        let mut h = LoudnessHistogram::new();
        h.push(1.0);
        h.reset();
        assert_eq!(h.count(), 0);
        assert!(h.gated_mean_ms(RELATIVE_GATE_DELTA_LU).is_none());
        // A cleared total over uncleared bins would still report the old
        // value here, because the relative gate would find the stale bin.
        assert_eq!(h.gated_count(RELATIVE_GATE_DELTA_LU), 0);
    }
}
