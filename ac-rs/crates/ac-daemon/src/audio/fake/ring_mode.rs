//! Opt-in ring-backed capture mode for [`FakeEngine`](super::FakeEngine).
//!
//! Routes fake capture through the same [`CaptureRings`] drain the JACK
//! backend uses, so `clear()`-before-wait has the same observable
//! consequence here that it has on real hardware. See [`FakeRings`].

use ringbuf::traits::Producer;
use ringbuf::HeapProd;

use crate::audio::rings::CaptureRings;

/// Ring capacity for the opt-in ring-backed mode. Generous relative to any
/// single capture request so an overrun is a genuine backlog signal rather
/// than an artifact of a tight buffer.
pub(super) const FAKE_RING_CAPACITY: usize = 4 * 192_000;

/// Which shared drain sequence a ring-mode capture should run. The variants
/// differ only in whether they clear before waiting and how many channels
/// they return — the point of routing all four through one enum is that the
/// ordering itself is never restated here.
pub(super) enum RingDrain {
    Block,
    /// The non-clearing arm.
    Available,
    Stereo,
    Multi,
    /// The #207 fix: no pre-wait clear, drains everything available.
    MultiContiguous,
}

/// Push a whole block into a ring producer. A short push means the ring
/// overran and the newest samples were dropped — bounded memory, same as the
/// JACK producer.
pub(super) fn push_block(prod: &mut HeapProd<f32>, block: &[f32]) {
    prod.push_slice(block);
}

/// Opt-in ring-backed capture state (`fake_ring` request param).
///
/// The default `FakeEngine` synthesises samples on demand from a
/// phase-continuous cursor: there is no ring, and nothing is ever cleared or
/// discarded, so the default backend is *structurally incapable* of
/// reproducing a splice, drop, or backlog defect. This mode routes fake
/// capture through the same [`CaptureRings`] drain the JACK backend uses, so
/// `clear()`-before-wait has the same observable consequence here that it has
/// on real hardware.
///
/// The clock is synthetic, not wall-clock: each capture call advances a
/// virtual sample cursor by exactly the samples it needs and synthesises them
/// into the ring. Deterministic, and it removes the `thread::sleep` that
/// makes the on-demand path run in real time.
pub(super) struct FakeRings {
    pub(super) rings: CaptureRings,
    pub(super) meas_prod: HeapProd<f32>,
    pub(super) ref_prods: Vec<HeapProd<f32>>,
    /// Seconds of audio the ring accrues between one tick's pop and the next
    /// tick's `clear()` — i.e. the consumer's processing time, which is what
    /// the pre-wait `clear()` actually throws away.
    ///
    /// **This is the independent variable of the splice experiment, not a
    /// simulated delay.** Gap length sets the spacing of the spectral
    /// replicas a spliced window produces, so a test can sweep this and check
    /// the measured spacing against the `sr/L` the splice hypothesis predicts
    /// (`handoff-capture-contiguity.md`, acceptance criterion 5). A zero-gap
    /// run is the contiguous control.
    pub(super) process_secs: f64,
    /// Absolute sample position for tone synthesis. `Stimulus::Tones`
    /// restarts its phase at `t = 0` on every `make_samples_for` call, which
    /// is fine for the on-demand path (one call per capture) but would make
    /// *every* ring-mode window spliced regardless of the `clear()`, hiding
    /// the very effect under test. Ring mode therefore generates from this
    /// running cursor. `Noise` and `CorrelatedPair` already carry their own
    /// continuous state and ignore it.
    pub(super) tone_pos: u64,
    /// Producer granularity in samples — the backend's period/quantum.
    ///
    /// **The dominant term, established on hardware.** A JACK process callback
    /// pushes one whole period at a time, so at `clear()` time the ring holds a
    /// whole number of periods, never a partial one. The discarded gap is
    /// therefore always `k · period` samples, and the phase discontinuity a
    /// splice imposes is `frac(f · period / sr)` cycles — **exactly zero** for
    /// any tone that is an integer multiple of `sr / period`.
    ///
    /// Measured on the RME Babyface Pro, `sr = 96 000`, quantum 1024
    /// (`sr/period = 93.75 Hz`): 12 000, 15 000 and 18 000 Hz — every one an
    /// exact multiple of 93.75 — gave a single clean peak, while 15 100 Hz
    /// (161.07 cycles per period) splattered into a 20 Hz comb. A
    /// sample-accurate producer predicts splatter at *all four* and so is
    /// simply wrong about which frequencies expose the defect. Modelling this
    /// is what makes the fake predict hardware rather than merely resemble it.
    pub(super) period: usize,
    /// Sub-period time accrued but not yet materialised as a whole period.
    ///
    /// Carrying this is what turns a 5 ms gap at 96 kHz (480 samples, less
    /// than one 1024-sample period) into "one whole period discarded on ~32%
    /// of ticks and nothing on the rest" — the 0-or-1024 pattern the hardware
    /// shows — rather than a constant 480.
    pub(super) residue: usize,
}
