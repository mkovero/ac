//! The per-tick state machine.
//!
//! [`SessionState`] holds everything a streaming session maintains across
//! ticks and nothing else: no engine, no socket, no stop flag, no
//! `Instant::now()`. [`SessionState::tick`] takes this tick's capture
//! buffers, what the worker observed, and the current time, and returns the
//! messages to publish — so every decision it makes is decidable from a
//! `Vec` of samples.

use rayon::prelude::*;
use serde_json::{json, Value};

use ac_core::shared::calibration::Calibration;

use super::analysis::{analyse_pair, AnalysisKey, PairAnalysis};
use super::frame::{build_pair_messages, raw_peak_dbfs, FrameStatics, TickInputs};
use super::pair::{Lock, PairCtx, PairState};
use super::window::{drain_to_block_lattice, Window};

/// Retry interval for a refused delay estimate — see
/// [`PairState::next_attempt`].
pub(super) const RELOCK_RETRY: std::time::Duration = std::time::Duration::from_secs(1);

/// What this tick observed outside the analysis: the drive state the
/// worker actually applied to its engine, the two events that invalidate
/// a lock, and the global mic-correction toggle.
///
/// Sampled once per tick by the worker and handed in whole, so every pair
/// in a frame agrees about all four. The drive poll itself stays in the
/// worker because applying it needs the engine; what reaches the analysis
/// is the observation, never the command (#228).
#[derive(Debug, Clone, Copy)]
pub(super) struct TickEvents {
    /// Drive state as applied to the engine on this tick, after the
    /// dead-man and after `set_drive`'s clamp. Recorded as a new lock's
    /// `driving` provenance.
    pub(super) engine_on: bool,
    /// False→true transition of `engine_on` since the previous tick
    /// (#226): the signal a lock was taken against just changed.
    pub(super) drive_edge_on: bool,
    /// A `relock` request arrived since the previous tick (#226).
    pub(super) relock_requested: bool,
    /// `mic_correction_enabled`, sampled once so a frame cannot be built
    /// half-corrected.
    pub(super) mc_enabled: bool,
}

/// Everything the streaming session maintains across ticks — and nothing
/// else. No engine, no socket, no `Instant::now()`, no stop flag.
///
/// That exclusion is the point. Before this type the whole per-tick
/// decision set — the warmup gate, the delay retry timer, the two lock
/// flushes, the ladder's construction and the `spl` integrator's `dt` —
/// lived in a 400-line closure body reachable only by standing up a
/// daemon, a ZMQ socket and an audio backend. A defect in any of it could
/// be demonstrated only through a live integration test, which is why the
/// #208 drain had to be re-implemented inside its own test module to be
/// scored at all.
///
/// [`SessionState::tick`] takes this tick's capture buffers, what the
/// worker observed, and the current time, and returns the messages to
/// publish. Everything it decides is therefore decidable from a `Vec` of
/// samples.
pub(super) struct SessionState {
    pub(super) statics: FrameStatics,
    pub(super) window: Window,
    /// Per-pair session constants, in launch order.
    pub(super) ctx: Vec<PairCtx>,
    /// Per-pair maintained state, same order and length as `ctx`.
    pub(super) pairs: Vec<PairState>,
    /// Sliding H1 window per unique capture channel, indexed by
    /// `PairCtx::mi`/`ri`.
    pub(super) rings: Vec<Vec<f32>>,
    /// Multi-time-window ladder per pair, same order as `ctx`. Not a
    /// `PairState` field — see that type's note.
    ///
    /// Purely **additive**: it runs alongside the full-rate Welch
    /// estimator and replaces nothing. That is not caution, it is
    /// required — `spl` derives from the same `gyy` the Welch path
    /// produces and has to stay bit-identical, and `meas_spectrum` /
    /// `ref_spectrum` are calibrated absolute levels, which `Gxy/Gxx`'s
    /// cancellation of `|Hdec|²` does not cover (see `visualize::mtw`'s
    /// fence).
    ///
    /// Fed the fresh per-tick `bufs`, never the `rings` sliding window:
    /// the ladder is a push pipeline, and pushing a re-segmented sliding
    /// buffer into it would reproduce #208's re-analysis one level down.
    pub(super) ladders: Vec<Option<ac_core::visualize::mtw::MtwPair>>,
    /// A layout the ladder cannot serve (an unsupported rate) degrades to
    /// "no ladder" rather than to a dead session, and is logged once.
    pub(super) ladder_failed: bool,
    /// The held H1 estimate per pair, same order as `ctx`. Recomputed
    /// only when [`AnalysisKey`] changes — see [`PairAnalysis`].
    pub(super) analysis: Vec<Option<PairAnalysis>>,
    /// Samples drained from the rings since session start. Half of the
    /// analysis key: it is the ring start's absolute position in the
    /// stream, so it changes exactly when the Welch segment boundaries do.
    pub(super) dropped: usize,
    /// Next `analysis_seq`. Session-wide rather than per-pair so a
    /// consumer watching two pairs sees one ordering.
    pub(super) next_seq: u64,
    /// Capture tick, used only as the `spl` integrator's `dt` on the
    /// first step, where there is no previous timestamp to subtract.
    pub(super) chunk_secs: f64,
}

impl SessionState {
    pub(super) fn new(
        statics: FrameStatics,
        window: Window,
        ctx: Vec<PairCtx>,
        n_channels: usize,
        chunk_secs: f64,
        integration_tau_s: f64,
    ) -> Self {
        // The `spl` integrator is the only per-pair field decided at
        // construction rather than by the loop: a meas channel with no SPL
        // calibration layer publishes `spl: null` for the whole session
        // (session-static per D10, matching `spl_offsets` in `monitor.rs`).
        let pairs: Vec<PairState> = ctx
            .iter()
            .map(|c| {
                PairState::new(
                    c.meas_cal
                        .as_ref()
                        .and_then(Calibration::spl_offset_db)
                        .map(|_| {
                            ac_core::visualize::time_integration::EmaIntegrator::new(
                                integration_tau_s,
                                1,
                            )
                        }),
                )
            })
            .collect();
        let n_pairs = ctx.len();
        let ladders = (0..n_pairs).map(|_| None).collect();
        let rings = (0..n_channels)
            .map(|_| Vec::with_capacity(window.target_total() + window.step))
            .collect();
        Self {
            statics,
            window,
            ctx,
            pairs,
            rings,
            ladders,
            ladder_failed: false,
            analysis: (0..n_pairs).map(|_| None).collect(),
            dropped: 0,
            next_seq: 0,
            chunk_secs,
        }
    }

    /// How many capture channels this session assembled rings for. The
    /// worker compares it against what capture actually returned (#254).
    pub(super) fn n_channels(&self) -> usize {
        self.rings.len()
    }

    /// Each pair's held lock, in launch order, for the snapshot ring's
    /// provenance copy.
    pub(super) fn delay_samples(&self) -> Vec<Option<i64>> {
        self.pairs
            .iter()
            .map(|st| st.delay.map(|l| l.samples))
            .collect()
    }

    /// Discard every pair's lock and ladder — a `relock` request (#226).
    pub(super) fn flush_all(&mut self) {
        for (st, ladder) in self.pairs.iter_mut().zip(self.ladders.iter_mut()) {
            st.flush(ladder);
        }
    }

    /// The drive off→on edge (#226). A lock is stale by construction —
    /// not by drift, not by a threshold — the instant a drive that was off
    /// starts driving, because the signal producing it just changed.
    ///
    /// The qualifier: only a lock acquired *while the drive was off* is
    /// discarded, so a dead-man drop and resume of a lock taken while
    /// driving survives untouched — nothing about that lock's premise
    /// changed. A pair that is currently unlocked gets its retry timer
    /// cleared instead, so acquisition is attempted this tick rather than
    /// up to `RELOCK_RETRY` later.
    pub(super) fn flush_locks_taken_against_silence(&mut self) {
        for (st, ladder) in self.pairs.iter_mut().zip(self.ladders.iter_mut()) {
            match st.delay {
                Some(Lock { driving: false, .. }) => st.flush(ladder),
                Some(Lock { driving: true, .. }) => {
                    // Acquired while driving — the dead-man/resume thrash
                    // case. Survives untouched.
                }
                None => st.next_attempt = None,
            }
        }
    }

    /// Append this tick's capture to every ring and trim each back to the
    /// analysis window on the block lattice (#208).
    /// Append this tick's capture to every ring, trim each back to the
    /// analysis window on the block lattice (#208), and advance the
    /// dropped-sample counter by what the trim removed.
    ///
    /// Every ring is popped to the same length by
    /// `capture_multi_contiguous`, so one counter describes them all; the
    /// first ring's drain is measured and the rest follow it.
    pub(super) fn push_rings(&mut self, bufs: &[Vec<f32>]) {
        let (target_total, step) = (self.window.target_total(), self.window.step);
        let before = self.rings.first().map(Vec::len).unwrap_or(0);
        let appended = bufs.first().map(Vec::len).unwrap_or(0);
        for (r, buf) in self.rings.iter_mut().zip(bufs.iter()) {
            r.extend_from_slice(buf);
            drain_to_block_lattice(r, target_total, step);
        }
        let after = self.rings.first().map(Vec::len).unwrap_or(0);
        self.dropped += (before + appended).saturating_sub(after);
    }

    /// Welch segments `welch_all` will actually average over this tick's
    /// rings — the same `while pos + nperseg <= len` walk it does,
    /// evaluated here so the frame can state it. Rises 1 → `n_averages`
    /// while the window fills, then is pinned there by the drain.
    /// Zero means no ring holds a whole segment yet, which is a state the
    /// frame reports rather than a state that suppresses it — see
    /// [`settling_frame`]. Saturating rather than wrapping: this is the
    /// arithmetic that used to underflow the instant anything ran before
    /// the warmup gate.
    pub(super) fn n_blocks(&self) -> usize {
        let Window { nperseg, step, .. } = self.window;
        self.rings
            .iter()
            .map(|r| match r.len().checked_sub(nperseg) {
                Some(extra) => extra / step + 1,
                None => 0,
            })
            .min()
            .unwrap_or(0)
    }

    /// Estimate any pair's missing delay, rate-limited. Runs at most once
    /// per pair per `RELOCK_RETRY` while unlocked, and not at all once
    /// locked: ref↔meas propagation is constant during a session (fixed
    /// hardware path), and each attempt is a full-ring FFT+IFFT.
    pub(super) fn acquire_missing_locks(&mut self, ev: TickEvents, now: std::time::Instant) {
        let sr = self.statics.sr;
        for (ctx, st) in self.ctx.iter().zip(self.pairs.iter_mut()) {
            if st.delay.is_some() {
                continue;
            }
            if st.next_attempt.is_some_and(|t| now < t) {
                continue;
            }
            let (Some(meas), Some(refb)) = (self.rings.get(ctx.mi), self.rings.get(ctx.ri)) else {
                continue;
            };
            let est = ac_core::visualize::transfer::estimate_delay_detailed(
                refb.as_slice(),
                meas.as_slice(),
                sr,
            );
            // `driving` is this tick's observed engine state — the
            // provenance a future drive edge (#226) reads to decide
            // whether this lock is stale by construction.
            st.delay = est.lag.map(|samples| Lock {
                samples,
                driving: ev.engine_on,
            });
            // Counted here rather than at the top of the loop: this is the
            // branch where an estimate actually ran, so the count means
            // "the estimator has answered", not "the loop reached the
            // retry site".
            st.attempts = st.attempts.saturating_add(1);
            // Full lock evidence, not just the ratio: the competing peaks
            // are what make DIRECT_PEAK_FRACTION settleable offline, and
            // they cannot be reconstructed from a finished session.
            st.prominence = Some(json!({
                "prominence":   est.prominence,
                "peak_lag":     est.peak_lag,
                "peak_value":   est.peak_value,
                // The strongest peak the estimator is not allowed to
                // select. Published so ring skew (#216) and stimulus-onset
                // ripples stay diagnosable from a capture rather than
                // needing another rig session.
                "noncausal_peak_lag":   est.noncausal_peak_lag,
                "noncausal_peak_value": est.noncausal_peak_value,
                "median_value": est.median_value,
                // Uncontaminated noise floor for the offline
                // re-thresholding experiment; see
                // DelayEstimate::negative_lag_median.
                "negative_lag_median": est.negative_lag_median,
                "candidates":   est.candidates.iter()
                    .map(|c| json!({"lag": c.lag, "value": c.value}))
                    .collect::<Vec<_>>(),
            }));
            if est.lag.is_none() {
                st.next_attempt = Some(now + RELOCK_RETRY);
            }
        }
    }

    /// Build each pair's ladder once its alignment offset is known — the
    /// offset is applied at full rate, before decimation, so it has to
    /// exist before the first sample enters — then push this tick's fresh
    /// buffers through and read the columns back.
    pub(super) fn advance_ladders(
        &mut self,
        bufs: &[Vec<f32>],
    ) -> (
        Vec<Option<Vec<ac_core::visualize::mtw::splice::Column>>>,
        Vec<Vec<bool>>,
    ) {
        let FrameStatics {
            sr,
            spec_f_min,
            spec_f_max,
            mtw_ppo,
            mtw_n_blocks,
            ..
        } = self.statics;
        for (slot, st) in self.ladders.iter_mut().zip(self.pairs.iter()) {
            if slot.is_some() || self.ladder_failed {
                continue;
            }
            let Some(delay) = st.delay.map(|l| l.samples) else {
                continue;
            };
            match ac_core::visualize::mtw::MtwPair::new(sr, delay, mtw_n_blocks) {
                Ok(p) => *slot = Some(p),
                Err(e) => {
                    eprintln!("transfer_stream: MTW ladder unavailable at {sr} Hz: {e}");
                    self.ladder_failed = true;
                }
            }
        }
        // Sequential rather than folded into the per-pair rayon fan-out:
        // the ladders are `&mut` and the fan-out borrows `rings`
        // immutably, and one 4096-point FFT pair per stage per tick is not
        // what makes this loop expensive.
        let columns = self
            .ladders
            .iter_mut()
            .zip(self.ctx.iter())
            .map(|(slot, ctx)| {
                let p = slot.as_mut()?;
                let meas = bufs.get(ctx.mi)?;
                let refb = bufs.get(ctx.ri)?;
                p.push(meas, refb);
                p.columns(spec_f_min, spec_f_max, mtw_ppo)
            })
            .collect();
        // Sampled after the push, so it describes the frame being built.
        let settled = self
            .ladders
            .iter()
            .map(|slot| {
                slot.as_ref()
                    .map(|p| p.settled_stages())
                    .unwrap_or_default()
            })
            .collect();
        (columns, settled)
    }

    /// Recompute the held H1 estimate for every pair whose
    /// [`AnalysisKey`] has changed, and leave the rest alone.
    ///
    /// The key changes when the ring start moves (a whole `step`, so every
    /// 0.5 s at 48 kHz), when the segment count changes (only while the
    /// window fills), when a pair's lock changes, or when the
    /// mic-correction toggle flips. Between those the estimate is
    /// bit-identical to the one already held, which at a 20 Hz tick means
    /// this used to run a 2.5 s Welch pass and a full-resolution IFFT per
    /// pair about ten times per distinct answer. #419 named the waste and
    /// left it.
    ///
    /// The pairs that do need recomputing are fanned out; a tick where
    /// none do costs one key comparison per pair.
    pub(super) fn refresh_analysis(&mut self, n_blocks: usize, mc_enabled: bool) {
        let statics = &self.statics;
        let rings = &self.rings;
        let dropped = self.dropped;
        let stale: Vec<(usize, AnalysisKey)> = self
            .ctx
            .iter()
            .zip(self.pairs.iter())
            .zip(self.analysis.iter())
            .filter_map(|((ctx, st), held)| {
                let key = AnalysisKey {
                    dropped,
                    n_blocks,
                    delay: st.delay.map(|l| l.samples).unwrap_or(0),
                    mc_enabled,
                };
                match held {
                    Some(a) if a.key == key => None,
                    _ => Some((ctx.pos, key)),
                }
            })
            .collect();
        if stale.is_empty() {
            return;
        }
        // Sequence numbers are handed out before the fan-out so they do
        // not depend on completion order.
        let seq0 = self.next_seq;
        self.next_seq += stale.len() as u64;
        let ctx = &self.ctx;
        let pairs = &self.pairs;
        let fresh: Vec<(usize, Option<PairAnalysis>)> = stale
            .par_iter()
            .enumerate()
            .map(|(i, &(pos, key))| {
                (
                    pos,
                    analyse_pair(&ctx[pos], &pairs[pos], statics, rings, key, seq0 + i as u64),
                )
            })
            .collect();
        for (pos, a) in fresh {
            // A pair whose channels are missing from the rings keeps
            // whatever it held rather than gaining a half-built estimate;
            // `build_pair_messages` publishes a settling frame for a pair
            // that has never had one.
            if a.is_some() {
                self.analysis[pos] = a;
            }
        }
    }

    /// One capture tick, from raw buffers to the messages to publish.
    ///
    /// `now` is a parameter rather than an `Instant::now()` call so the
    /// delay retry timer and the `spl` integrator's `dt` are both driven
    /// by the caller's clock.
    pub(super) fn tick(
        &mut self,
        bufs: &[Vec<f32>],
        ev: TickEvents,
        drive_msg: &Value,
        now: std::time::Instant,
    ) -> Vec<Value> {
        // Consumed before this tick's own estimate, so a re-lock request
        // and the tick's delay attempt never interleave.
        if ev.relock_requested {
            self.flush_all();
        }
        if ev.drive_edge_on {
            self.flush_locks_taken_against_silence();
        }

        // Raw capture peaks (§4.2), per unique-port index, from THIS
        // tick's blocks — before any calibration, weighting, or
        // aggregation. Deliberately not derived from `rings` (a
        // multi-segment window, not the frame's blocks) and not from
        // `TransferResult`'s `meas_amp`/`ref_amp` (window-normalised and
        // calibration-adjacent). The meters exist to judge gain staging,
        // and a calibrated or band-aggregated value hides clipping — which
        // is the one thing they must never do.
        let tick_peaks_dbfs: Vec<Option<f64>> = bufs.iter().map(|b| raw_peak_dbfs(b)).collect();

        self.push_rings(bufs);

        // Analysis readiness, which is NOT the same question as whether to
        // publish. `n_blocks == 0` means no ring holds a whole Welch
        // segment, so there is no H1 and no delay estimate to be had; the
        // frame says so and ships regardless.
        //
        // The two gates were one `continue` until now, and that made the
        // analysis window set time-to-first-frame. It is why the window
        // cannot simply be widened: waiting for the full `target_total`
        // pushes the first frame from 1.0 s to 2.5 s, past the 1.5 s drive
        // dead-man, so a client that sets the drive and waits for a lock
        // without sending keepalives has the drive expire *before* the
        // first delay attempt, takes its lock against silence, and loses
        // it on the next drive edge. `it_relock`'s two survives-a-resume
        // tests are that sequence.
        //
        // Separating them does not by itself widen anything — `n_averages`
        // still rises 1 → 4 and the frame still states it (#419) — but the
        // dead-man now bounds only the delay estimate's own gate, which is
        // one segment because a cross-correlation needs one, not because
        // the Welch average does.
        let n_blocks = self.n_blocks();
        if n_blocks > 0 {
            self.acquire_missing_locks(ev, now);
            self.refresh_analysis(n_blocks, ev.mc_enabled);
        }
        let (mtw_columns, mtw_settled) = self.advance_ladders(bufs);

        // Assembly, not analysis: the expensive work happened above and
        // only when the ring moved. What is left is building JSON from the
        // held estimate plus this tick's live scalars, fanned out across
        // the rayon pool so multi-pair sessions (e.g. 4 mic positions
        // against one reference) scale with core count. Published back in
        // original pair order.
        let tick = TickInputs {
            tick_peaks_dbfs: &tick_peaks_dbfs,
            mc_enabled: ev.mc_enabled,
            drive_msg,
            mtw_columns: &mtw_columns,
            mtw_settled: &mtw_settled,
            analysis: &self.analysis,
            n_channels: self.rings.len(),
        };
        let statics = &self.statics;
        let built: Vec<(usize, Vec<Value>, Option<f64>)> = self
            .ctx
            .par_iter()
            .zip(self.pairs.par_iter())
            .filter_map(|(ctx, st)| build_pair_messages(ctx, st, statics, &tick))
            .collect();

        let mut out = Vec::with_capacity(built.len() * 2);
        for (pos, mut batch, spl_raw) in built {
            // Sequential, indexed by `PairCtx::pos` — the EMA integrator
            // is `&mut` per pair and cannot be advanced inside the
            // parallel closure above. `pos` is the pair's position in the
            // launch list, carried through `filter_map`, never the
            // post-filter Vec position.
            let st = &mut self.pairs[pos];
            if let (Some(raw), Some(integ)) = (spl_raw, st.spl_integ.as_mut()) {
                let dt = st
                    .spl_last
                    .map(|t| now.duration_since(t).as_secs_f64())
                    .unwrap_or(self.chunk_secs)
                    .max(1e-6);
                st.spl_last = Some(now);
                let integrated = integ.update(&[raw], dt)[0];
                if let Some(first) = batch.first_mut() {
                    first["spl"] = json!(integrated);
                }
            }
            out.extend(batch);
        }
        out
    }
}

#[cfg(test)]
mod session_tests;
