//! The analysis window: its geometry, and the drain that keeps it cut on
//! the stream's own block lattice rather than on the ring's head (#208).

/// The analysis window's geometry, all of it derived from the sample rate.
///
/// `nperseg`/`step` mirror `h1_estimate`'s internal Welch settings; a
/// mismatch here would make `n_averages` on the wire a claim about a
/// segmentation the estimator does not perform.
#[derive(Debug, Clone, Copy)]
pub(super) struct Window {
    /// Welch segment length. `sr` — 1 Hz bin width.
    pub(super) nperseg: usize,
    /// Segment hop, `nperseg / 2` for 50% overlap. Also the quantum the
    /// ring is drained in (#208), which is what pins the block grid to
    /// the stream.
    pub(super) step: usize,
    /// Segments averaged once the window is full — the steady-state
    /// `n_averages` the frame reports.
    pub(super) n_averages: usize,
}

impl Window {
    pub(super) fn new(sr: u32, n_averages: usize) -> Self {
        let nperseg = sr as usize;
        Self {
            nperseg,
            step: nperseg / 2,
            n_averages,
        }
    }

    /// Ring length holding exactly `n_averages` complete segments.
    /// Derived rather than stored so the three numbers cannot disagree.
    pub(super) fn target_total(&self) -> usize {
        self.nperseg + self.step * (self.n_averages - 1)
    }
}

/// Trim a capture ring to the analysis window, dropping **whole `step`
/// units only** (#208).
///
/// The ring start then only ever sits on the stream's own `k·step`
/// lattice, so the blocks `welch_all` cuts at ring offsets `0, step,
/// 2·step, …` land on fixed absolute sample positions. Every event is
/// analysed once, at one weight, for the life of the session.
///
/// Trimming to an exact `target_total` every tick instead — what this did
/// before — advances the ring start by one capture chunk per tick and drags
/// the whole block grid across the audio. A fixed event then drifts from the
/// ring's edge, where only ONE block covers it, to the ring's middle, where
/// TWO do, and back out. That is a ~6 dB swing in how much the event
/// contributes, with the Hann shape on top of it. `n_averages = 1` hides the
/// whole thing (one block, no grid, nothing to slide), which is why it
/// presented as "averaging is broken".
///
/// Leaves up to `step - 1` samples of tail unconsumed. That is the point: a
/// block is analysed when it is complete and not before. Length stays inside
/// `[target_total, target_total + step)`, which fits exactly `n_averages`
/// blocks at both ends of that range.
///
/// Falsified by [`pinned_window_tests`], which runs the discarded
/// exact-trim drain side by side with this one on the same burst.
pub(super) fn drain_to_block_lattice(ring: &mut Vec<f32>, target_total: usize, step: usize) {
    while ring.len() >= target_total + step {
        ring.drain(..step);
    }
}

#[cfg(test)]
mod pinned_window_tests;
