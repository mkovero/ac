//! Opt-in ring-backed capture mode for [`FakeEngine`](super::FakeEngine).
//!
//! Routes fake capture through the same [`CaptureRings`] drain the JACK
//! backend uses, so `clear()`-before-wait has the same observable
//! consequence here that it has on real hardware. See [`FakeRings`].

use anyhow::Result;
use ringbuf::traits::{Producer, Split};
use ringbuf::{HeapProd, HeapRb};

use crate::audio::rings::CaptureRings;

use super::stimulus::Synth;

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
fn push_block(prod: &mut HeapProd<f32>, block: &[f32]) {
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
    rings: CaptureRings,
    meas_prod: HeapProd<f32>,
    ref_prods: Vec<HeapProd<f32>>,
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
    process_secs: f64,
    /// Absolute sample position for tone synthesis. `Stimulus::Tones`
    /// restarts its phase at `t = 0` on every `make_samples_for` call, which
    /// is fine for the on-demand path (one call per capture) but would make
    /// *every* ring-mode window spliced regardless of the `clear()`, hiding
    /// the very effect under test. Ring mode therefore generates from this
    /// running cursor. `Noise` and `CorrelatedPair` already carry their own
    /// continuous state and ignore it.
    tone_pos: u64,
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
    period: usize,
    /// Sub-period time accrued but not yet materialised as a whole period.
    ///
    /// Carrying this is what turns a 5 ms gap at 96 kHz (480 samples, less
    /// than one 1024-sample period) into "one whole period discarded on ~32%
    /// of ticks and nothing on the rest" — the 0-or-1024 pattern the hardware
    /// shows — rather than a constant 480.
    residue: usize,
}

/// Synthesise one block per channel and push each into its ring, measurement
/// channel first then the refs, in the order `capture_multi` returns them.
///
/// Free-standing rather than a `FakeRings` method because the drain below
/// calls it from a closure that runs while `rings` is already mutably
/// borrowed; taking the producers directly keeps the two borrows disjoint.
fn push_all(
    meas_prod: &mut HeapProd<f32>,
    ref_prods: &mut [HeapProd<f32>],
    synth: &mut Synth<'_>,
    ports: &[Option<String>],
    duration: f64,
    tone_start: u64,
) {
    let mut ports = ports.iter();
    if let Some(port) = ports.next() {
        let block = synth.block(port.as_deref(), duration, tone_start);
        push_block(meas_prod, &block);
    }
    for (prod, port) in ref_prods.iter_mut().zip(ports) {
        let block = synth.block(port.as_deref(), duration, tone_start);
        push_block(prod, &block);
    }
}

impl FakeRings {
    /// Allocate the rings for `n_refs` reference channels plus the
    /// measurement channel, with `process_secs` of per-tick consumer
    /// processing time and a `period`-sample producer granularity.
    ///
    /// Reference rings are allocated up front rather than on
    /// `add_ref_input`, because the fake has no RT handler to hand producers
    /// to and the transfer worker registers its refs before the first tick.
    pub(super) fn new(process_secs: f64, n_refs: usize, period: usize) -> Self {
        let (meas_prod, meas_cons) = HeapRb::<f32>::new(FAKE_RING_CAPACITY).split();
        let mut rings = CaptureRings::new();
        rings.set_meas(meas_cons);

        let mut ref_prods = Vec::with_capacity(n_refs);
        rings.reserve_refs(n_refs);
        for _ in 0..n_refs {
            let (p, c) = HeapRb::<f32>::new(FAKE_RING_CAPACITY).split();
            ref_prods.push(p);
            rings.push_ref(c);
        }

        Self {
            rings,
            meas_prod,
            ref_prods,
            process_secs: process_secs.max(0.0),
            tone_pos: 0,
            period: period.max(1),
            residue: 0,
        }
    }

    pub(super) fn n_refs(&self) -> usize {
        self.ref_prods.len()
    }

    pub(super) fn discarded_samples(&self) -> u64 {
        self.rings.discarded_samples()
    }

    pub(super) fn last_drain_occupancy(&self) -> Vec<usize> {
        self.rings.last_drain_occupancy().to_vec()
    }

    /// Push `periods` whole periods of synthesised audio into the rings.
    ///
    /// The producer only ever moves in whole periods — that is the property
    /// that decides which stimulus frequencies expose the splice at all (see
    /// [`FakeRings::period`]).
    fn push_periods(&mut self, synth: &mut Synth<'_>, ports: &[Option<String>], periods: usize) {
        if periods == 0 {
            return;
        }
        let n = periods * self.period;
        let duration = n as f64 / synth.sample_rate as f64;
        let tone_start = self.tone_pos;
        self.tone_pos += n as u64;
        push_all(
            &mut self.meas_prod,
            &mut self.ref_prods,
            synth,
            ports,
            duration,
            tone_start,
        );
    }

    /// Model the consumer's processing time: the ring keeps filling while the
    /// caller works on the block it just popped. Whatever accrues here is
    /// exactly what the *next* tick's `clear()` discards — the splice.
    ///
    /// Accrued time is banked in `residue` and materialises only as whole
    /// periods, so a gap shorter than one period does not produce a small
    /// discard every tick — it produces a *whole period* discarded on some
    /// ticks and nothing on the others, which is what the hardware does.
    fn charge_processing(&mut self, synth: &mut Synth<'_>, ports: &[Option<String>]) {
        let gap = (synth.sample_rate as f64 * self.process_secs) as usize;
        let banked = self.residue + gap;
        let periods = banked / self.period;
        self.residue = banked % self.period;
        self.push_periods(synth, ports, periods);
    }

    /// One ring-mode capture tick.
    ///
    /// Timeline, matching what a real consumer does against a live ring:
    ///
    /// 1. the requested drain runs — `clear()` (for every variant except
    ///    `Available`) throws away whatever accrued during step 3 of the
    ///    *previous* tick, then the wait synthesises this tick's `n` samples
    ///    and they are popped;
    /// 2. the caller gets its block;
    /// 3. the caller processes it, during which `process_secs` more audio
    ///    accrues — charged here so the next tick's `clear()` has something
    ///    real to discard.
    ///
    /// Step 3 is what makes this reproduce anything. A virtual clock that
    /// advanced only inside the wait would leave the ring empty at every
    /// `clear()`, discarding nothing, and the mode would model no defect at
    /// all.
    pub(super) fn drain(
        &mut self,
        synth: &mut Synth<'_>,
        ports: &[Option<String>],
        n: usize,
        duration: f64,
        kind: RingDrain,
    ) -> Result<Vec<Vec<f32>>> {
        // The clearing drains empty the ring first, so plan the fill from what
        // will actually be there when the wait runs. Whole periods only — a
        // producer that could deliver a partial period would model the wrong
        // machine (see `FakeRings::period`).
        let occupied_at_wait = match kind {
            RingDrain::Available => self.rings.occupied(),
            _ => 0,
        };
        let shortfall = n.saturating_sub(occupied_at_wait);
        let fill = shortfall.div_ceil(self.period) * self.period;
        let wait_duration = fill as f64 / synth.sample_rate as f64;
        let tone_start = self.tone_pos;
        self.tone_pos += fill as u64;

        let out = {
            // Destructured so `rings` and the producers are disjoint borrows:
            // the drain owns the ordering, the waiter owns the fill.
            let FakeRings {
                rings,
                meas_prod,
                ref_prods,
                ..
            } = self;
            let mut wait = |_: &CaptureRings, _n: usize, _d: f64| {
                push_all(
                    meas_prod,
                    ref_prods,
                    synth,
                    ports,
                    wait_duration,
                    tone_start,
                );
                Ok(())
            };
            match kind {
                RingDrain::Block => rings.capture_block(n, duration, &mut wait).map(|b| vec![b]),
                RingDrain::Available => {
                    wait(&*rings, n, duration)?;
                    Ok(vec![rings.capture_available(n)])
                }
                RingDrain::Stereo => rings
                    .capture_stereo(n, duration, &mut wait)
                    .map(|(m, r)| vec![m, r]),
                RingDrain::Multi => rings.capture_multi(n, duration, &mut wait),
                RingDrain::MultiContiguous => {
                    rings.capture_multi_contiguous(n, duration, &mut wait)
                }
            }?
        };

        self.charge_processing(synth, ports);
        Ok(out)
    }
}
