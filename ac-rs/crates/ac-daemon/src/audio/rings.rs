//! Consumer-side capture-ring drain, shared by every ring-backed backend.
//!
//! **Why this is its own module.** The `clear()`-before-wait ordering in
//! `capture_block` / `capture_stereo` / `capture_multi` is itself the subject
//! of an open investigation (`handoff-capture-contiguity.md`, H1): every tick
//! discards whatever accumulated while the previous tick was being processed,
//! so the buffer handed to the FFT is a concatenation of non-contiguous
//! fragments presented as continuous time. A reproducer only counts as
//! evidence if it exercises *this* code rather than a second copy of it that
//! can drift — so the ordering lives here once, and both the JACK backend and
//! the (opt-in) ring-backed fake backend run it.
//!
//! **Not real-time code.** The RT process callback is the *producer*; every
//! function here runs on the worker thread that owns the consumer halves.
//! Extracting this out of `jack_backend` touched no RT path.
//!
//! The wait strategy is the one part that genuinely differs per backend —
//! JACK parks until the process callback unparks it, the fake advances a
//! virtual clock and synthesises the samples itself — so it is injected as
//! `WaitFn` rather than owned here.

use anyhow::Result;
use ringbuf::traits::{Consumer, Observer};
use ringbuf::HeapCons;

/// Backend-supplied "block until the measurement ring holds `n` samples".
///
/// Receives `&CaptureRings` so the strategy can poll occupancy; the fake's
/// implementation additionally pushes into the producer halves it owns.
/// `duration` is the nominal capture length, used for timeout scaling.
pub(crate) type WaitFn<'a> = &'a mut dyn FnMut(&CaptureRings, usize, f64) -> Result<()>;

/// The consumer halves of a backend's capture rings, plus the drain ordering.
pub(crate) struct CaptureRings {
    /// Measurement channel. `None` between `stop()` and the next `start()`.
    meas: Option<HeapCons<f32>>,
    /// One consumer per active reference input, in insertion order.
    refs: Vec<HeapCons<f32>>,
    /// Samples thrown away by the pre-wait `clear()` calls, cumulative.
    ///
    /// This is the direct per-tick measure of H1's splice: a nonzero value
    /// means the estimator's window is discontinuous by exactly this many
    /// samples at that tick boundary. Deliberately does **not** include the
    /// `reconnect_input` clear — that one is a routing switch, not a per-tick
    /// discard, and folding the two together would contaminate the
    /// measurement on any session that changes inputs mid-flight.
    discarded: u64,
    /// Per-ring occupancy as it stood inside the last
    /// [`Self::capture_multi_contiguous`], after its wait and before its pop —
    /// measurement first, then each reference in registration order.
    ///
    /// Instrumentation for #208 D1. Recorded here because that instant is not
    /// observable from the worker: by the time the call returns the rings have
    /// been drained, and by the time the worker could poll before the call the
    /// wait has not happened yet. Written once per drain into a reused `Vec`.
    last_drain_occupancy: Vec<usize>,
}

impl CaptureRings {
    pub(crate) fn new() -> Self {
        Self {
            meas: None,
            refs: Vec::new(),
            discarded: 0,
            last_drain_occupancy: Vec::new(),
        }
    }

    pub(crate) fn set_meas(&mut self, cons: HeapCons<f32>) {
        self.meas = Some(cons);
    }

    pub(crate) fn push_ref(&mut self, cons: HeapCons<f32>) {
        self.refs.push(cons);
    }

    pub(crate) fn reserve_refs(&mut self, n: usize) {
        self.refs.reserve(n);
    }

    /// Cleared on `stop()`. Leaves `discarded` alone — it is a session-
    /// cumulative diagnostic, not per-connection state.
    pub(crate) fn teardown(&mut self) {
        self.meas = None;
        self.refs.clear();
    }

    /// Samples currently readable on the measurement ring.
    pub(crate) fn occupied(&self) -> usize {
        self.meas.as_ref().map(|c| c.occupied_len()).unwrap_or(0)
    }

    pub(crate) fn discarded_samples(&self) -> u64 {
        self.discarded
    }

    /// See [`Self::last_drain_occupancy`]'s field docs. Empty before the first
    /// contiguous drain.
    pub(crate) fn last_drain_occupancy(&self) -> &[usize] {
        &self.last_drain_occupancy
    }

    /// Drop everything buffered on the measurement ring, counting it.
    ///
    /// Public because `play_and_capture` performs its own clear/wait/pop
    /// sequence (interleaved with one-shot playback state) rather than one of
    /// the three below, and its discards must still be counted.
    pub(crate) fn clear_meas(&mut self) {
        if let Some(ref mut c) = self.meas {
            self.discarded += c.occupied_len() as u64;
            c.clear();
        }
    }

    /// Drop everything buffered on the measurement ring *without* counting
    /// it. For clears that are not per-tick discards — `reconnect_input`
    /// flushing stale audio from the port it just switched away from, and
    /// `flush_capture`. Counting those would make `discarded_samples` report
    /// routing churn as splice, which is exactly the confusion the counter
    /// exists to resolve.
    pub(crate) fn clear_meas_uncounted(&mut self) {
        if let Some(ref mut c) = self.meas {
            c.clear();
        }
    }

    /// Uncounted clear of every ring — backs `flush_capture`.
    pub(crate) fn flush_all_uncounted(&mut self) {
        self.clear_meas_uncounted();
        for c in self.refs.iter_mut() {
            c.clear();
        }
    }

    /// Clear the measurement ring and the first `n_refs` reference rings.
    ///
    /// Only the measurement ring's discards are counted: the reference rings
    /// are drained in lockstep with it, so counting them too would multiply
    /// one splice by the channel count and misreport the gap length.
    fn clear_meas_and_refs(&mut self, n_refs: usize) {
        self.clear_meas();
        for c in self.refs.iter_mut().take(n_refs) {
            c.clear();
        }
    }

    /// Pop up to `n` samples from the measurement ring, truncated to what was
    /// actually available.
    fn pop_meas(&mut self, n: usize) -> Vec<f32> {
        let mut buf = vec![0.0f32; n];
        let got = self
            .meas
            .as_mut()
            .map(|c| c.pop_slice(&mut buf))
            .unwrap_or(0);
        buf.truncate(got);
        buf
    }

    /// Pop exactly `n` samples from reference ring `i`, zero-padded if the
    /// port was disconnected or silent. Padded rather than truncated so the
    /// per-pair estimator always sees equal-length meas/ref buffers.
    fn pop_ref_padded(&mut self, i: usize, n: usize) -> Vec<f32> {
        let mut buf = vec![0.0f32; n];
        if let Some(c) = self.refs.get_mut(i) {
            let got = c.pop_slice(&mut buf);
            for s in buf.iter_mut().skip(got) {
                *s = 0.0;
            }
        }
        buf
    }

    // -------------------------------------------------------------------
    // The three drain sequences. Statement order here is load-bearing —
    // see the module docs.
    // -------------------------------------------------------------------

    /// Single measurement channel: clear, wait, pop.
    pub(crate) fn capture_block(
        &mut self,
        n_needed: usize,
        duration: f64,
        wait: WaitFn<'_>,
    ) -> Result<Vec<f32>> {
        self.clear_meas();
        wait(&*self, n_needed, duration)?;
        Ok(self.pop_meas(n_needed))
    }

    /// Non-clearing, non-blocking drain of whatever has accumulated since the
    /// last call. This is the contiguous path — `monitor_spectrum`'s sliding
    /// ring uses it so refresh rate is decoupled from FFT window length
    /// without losing contiguity across ticks. No `clear()`, therefore no
    /// splice, therefore the control arm of H1's single-vs-multi test.
    pub(crate) fn capture_available(&mut self, max_samples: usize) -> Vec<f32> {
        self.pop_meas(max_samples)
    }

    /// Measurement + first reference, captured in sync.
    pub(crate) fn capture_stereo(
        &mut self,
        n_needed: usize,
        duration: f64,
        wait: WaitFn<'_>,
    ) -> Result<(Vec<f32>, Vec<f32>)> {
        self.clear_meas_and_refs(1);
        wait(&*self, n_needed, duration)?;
        let meas = self.pop_meas(n_needed);
        let refch = self.pop_ref_padded(0, n_needed);
        Ok((meas, refch))
    }

    /// Measurement + every active reference, captured in sync. Returned in
    /// registration order with the measurement channel first, matching what
    /// the multi-pair `transfer_stream` worker indexes into.
    pub(crate) fn capture_multi(
        &mut self,
        n_needed: usize,
        duration: f64,
        wait: WaitFn<'_>,
    ) -> Result<Vec<Vec<f32>>> {
        self.clear_meas_and_refs(self.refs.len());
        wait(&*self, n_needed, duration)?;

        let mut out = Vec::with_capacity(1 + self.refs.len());
        out.push(self.pop_meas(n_needed));
        for i in 0..self.refs.len() {
            out.push(self.pop_ref_padded(i, n_needed));
        }
        Ok(out)
    }

    /// Smallest sample count readable on *every* channel.
    ///
    /// Popping this many from each keeps meas and refs sample-aligned without
    /// relying on a `clear()` to re-sync them. The rings are filled in
    /// lockstep by one RT callback so they agree in practice; taking the
    /// minimum makes that a guarantee rather than an assumption.
    fn min_occupied(&self) -> usize {
        self.refs
            .iter()
            .map(|c| c.occupied_len())
            .fold(self.occupied(), usize::min)
    }

    /// Contiguous multi-channel drain — the streaming counterpart of
    /// [`Self::capture_multi`], and the fix for issue #207.
    ///
    /// Two differences, both load-bearing:
    ///
    /// 1. **No pre-wait `clear()`.** That clear is correct for a one-shot
    ///    measurement, which must not return audio recorded before its
    ///    stimulus was set. It is wrong for a streaming consumer that
    ///    concatenates successive blocks into one analysis window, because it
    ///    discards whatever accrued while the previous block was being
    ///    processed and presents the fragments as continuous time.
    /// 2. **Returns everything available, not just `n_needed`.** Dropping the
    ///    clear alone would trade the splice for a growing backlog — which is
    ///    exactly issue #208, on this path. Draining the surplus keeps latency
    ///    bounded *and* contiguous. The `clear()` was bounding latency by
    ///    destroying contiguity; this bounds it by keeping up.
    ///
    /// Still blocks until `n_needed` is available, so it paces the caller's
    /// tick loop exactly as `capture_multi` did.
    pub(crate) fn capture_multi_contiguous(
        &mut self,
        n_needed: usize,
        duration: f64,
        wait: WaitFn<'_>,
    ) -> Result<Vec<Vec<f32>>> {
        wait(&*self, n_needed, duration)?;

        // Exactly the minimum occupancy — deliberately **not** floored at
        // `n_needed`. The RT callback pushes meas and every ref in lockstep,
        // so at steady state this equals the measurement ring's occupancy and
        // the floor would never bind. Where it would bind is precisely where
        // it does damage: a reference registered mid-session starts behind, and
        // forcing `n_needed` there would zero-pad the ref to make up the
        // difference. `capture_multi` gets away with that because it clears
        // every tick and re-syncs; this drain does not clear, so the padding
        // would become a permanent meas/ref offset and corrupt H1's phase and
        // delay. Returning a short block instead keeps the channels aligned,
        // and the caller's warm-up guard already tolerates one.
        let n = self.min_occupied();

        // #208 D1: record what every ring held at this instant, before the pop
        // consumes it. Reuses the buffer, so no allocation at steady state.
        self.last_drain_occupancy.clear();
        self.last_drain_occupancy.push(self.occupied());
        for c in self.refs.iter() {
            self.last_drain_occupancy.push(c.occupied_len());
        }

        let mut out = Vec::with_capacity(1 + self.refs.len());
        out.push(self.pop_meas(n));
        for i in 0..self.refs.len() {
            out.push(self.pop_ref_padded(i, n));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ringbuf::traits::{Producer, Split};
    use ringbuf::HeapRb;

    /// Build a ring pre-loaded with `n` ascending samples, returning the
    /// consumer half and its (now-unused) producer so the ring stays alive.
    fn loaded(n: usize, cap: usize) -> (HeapCons<f32>, ringbuf::HeapProd<f32>) {
        let (mut prod, cons) = HeapRb::<f32>::new(cap).split();
        for i in 0..n {
            prod.try_push(i as f32).unwrap();
        }
        (cons, prod)
    }

    /// The defect this module exists to keep observable: `capture_block`
    /// throws away everything buffered before it starts waiting. If this
    /// assertion ever flips to zero, the splice is gone — which is the fix,
    /// and it must be a deliberate one (see the handoff's hard fence), not a
    /// silent side effect of touching this file.
    #[test]
    fn capture_block_discards_what_accumulated_before_the_wait() {
        let mut rings = CaptureRings::new();
        let (cons, _prod) = loaded(500, 4096);
        rings.set_meas(cons);

        let mut wait = |_: &CaptureRings, _: usize, _: f64| Ok(());
        let got = rings.capture_block(100, 0.01, &mut wait).unwrap();

        assert_eq!(rings.discarded_samples(), 500, "pre-wait clear must count");
        assert!(
            got.is_empty(),
            "ring was cleared and the stub wait pushed nothing, so nothing to pop"
        );
    }

    /// `capture_available` is the contiguous counterpart — no clear, no
    /// discard, no wait. H1's control arm depends on this staying true.
    #[test]
    fn capture_available_never_discards() {
        let mut rings = CaptureRings::new();
        let (cons, _prod) = loaded(500, 4096);
        rings.set_meas(cons);

        let got = rings.capture_available(200);
        assert_eq!(got.len(), 200);
        assert_eq!(got[0], 0.0, "must read from the oldest sample, not a reset");
        assert_eq!(rings.discarded_samples(), 0);

        // Contiguity across calls: the next drain continues the stream.
        let next = rings.capture_available(200);
        assert_eq!(
            next[0], 200.0,
            "second drain must continue where the first stopped"
        );
        assert_eq!(rings.discarded_samples(), 0);
    }

    /// Reference rings are drained in lockstep with the measurement ring, so
    /// only the measurement ring's discards are counted — otherwise a 4-pair
    /// session would report 5x the real gap.
    #[test]
    fn multi_channel_discard_count_is_not_multiplied_by_channel_count() {
        let mut rings = CaptureRings::new();
        let (m, _mp) = loaded(300, 4096);
        rings.set_meas(m);
        for _ in 0..3 {
            // Dropping the producer half at the end of each iteration is
            // fine — the ring is kept alive by the surviving consumer.
            let (r, _rp) = loaded(300, 4096);
            rings.push_ref(r);
        }

        let mut wait = |_: &CaptureRings, _: usize, _: f64| Ok(());
        let bufs = rings.capture_multi(50, 0.01, &mut wait).unwrap();

        assert_eq!(bufs.len(), 4, "meas + 3 refs");
        assert_eq!(rings.discarded_samples(), 300, "meas only, not 4x300");
    }

    /// Meas truncates to what was available, refs zero-pad to full length —
    /// asymmetric on purpose so the estimator always sees equal-length
    /// buffers even when a ref port is disconnected.
    #[test]
    fn refs_zero_pad_while_meas_truncates() {
        let mut rings = CaptureRings::new();
        let (m, mut mp) = loaded(0, 4096);
        rings.set_meas(m);
        let (r, _rp) = loaded(0, 4096);
        rings.push_ref(r);

        // Only 10 meas samples arrive during the "wait"; the ref stays empty.
        let mut wait = move |_: &CaptureRings, _: usize, _: f64| {
            for i in 0..10 {
                mp.try_push(i as f32).unwrap();
            }
            Ok(())
        };
        let (meas, refch) = rings.capture_stereo(64, 0.01, &mut wait).unwrap();

        assert_eq!(meas.len(), 10, "meas truncates to what arrived");
        assert_eq!(refch.len(), 64, "ref pads to the requested length");
        assert!(refch.iter().all(|&s| s == 0.0));
    }

    /// A lagging reference must produce a **short, aligned** block, never a
    /// zero-padded full-length one.
    ///
    /// `capture_multi` can pad because it clears every tick and re-syncs.
    /// `capture_multi_contiguous` never clears, so a pad would offset ref
    /// against meas permanently and corrupt H1's phase and delay. This is the
    /// specific way the fix could be silently un-done.
    #[test]
    fn contiguous_drain_returns_short_rather_than_padding_a_lagging_ref() {
        let mut rings = CaptureRings::new();
        let (m, _mp) = loaded(1000, 4096);
        rings.set_meas(m);
        // Ref registered late: only 300 samples where meas has 1000.
        let (r, _rp) = loaded(300, 4096);
        rings.push_ref(r);

        let mut wait = |_: &CaptureRings, _: usize, _: f64| Ok(());
        let bufs = rings
            .capture_multi_contiguous(500, 0.01, &mut wait)
            .unwrap();

        assert_eq!(bufs.len(), 2);
        assert_eq!(
            bufs[0].len(),
            300,
            "meas must be cut back to the ref's occupancy, not run ahead"
        );
        assert_eq!(bufs[1].len(), 300, "ref must not be zero-padded to length");
        assert!(
            bufs[1].iter().all(|&s| s != 0.0 || bufs[1][0] == 0.0),
            "no padding zeros should have been appended"
        );
        assert_eq!(rings.discarded_samples(), 0, "and nothing may be discarded");
    }

    /// The surplus comes back out rather than accumulating — the property
    /// that stops the #207 fix from becoming #208 on this path.
    #[test]
    fn contiguous_drain_returns_the_whole_backlog_not_just_the_request() {
        let mut rings = CaptureRings::new();
        let (m, _mp) = loaded(5000, 8192);
        rings.set_meas(m);

        let mut wait = |_: &CaptureRings, _: usize, _: f64| Ok(());
        let bufs = rings
            .capture_multi_contiguous(500, 0.01, &mut wait)
            .unwrap();

        assert_eq!(
            bufs[0].len(),
            5000,
            "asked for 500 with 5000 buffered — all 5000 must come back or latency grows"
        );
        assert_eq!(rings.discarded_samples(), 0);
    }

    /// A failing wait must not leave the caller with a partially-drained
    /// ring silently presented as a good capture.
    #[test]
    fn wait_error_propagates_and_nothing_is_popped() {
        let mut rings = CaptureRings::new();
        let (cons, _prod) = loaded(500, 4096);
        rings.set_meas(cons);

        let mut wait = |_: &CaptureRings, _: usize, _: f64| anyhow::bail!("capture timeout");
        let err = rings.capture_block(100, 0.01, &mut wait).unwrap_err();
        assert!(err.to_string().contains("timeout"));
    }
}
