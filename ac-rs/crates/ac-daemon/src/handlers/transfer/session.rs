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

/// Per-tick session behaviour, driven directly rather than through a
/// daemon.
///
/// Everything here was previously reachable only from a live ZMQ session:
/// the warmup gate, the block count the frame reports, the two lock
/// flushes and the refusal retry timer all lived inside the worker
/// closure. `it_relock` covers three of them end to end and is the right
/// test for the protocol, but it cannot advance the clock, so the retry
/// interval below had no test at all — a `RELOCK_RETRY` of zero, or of an
/// hour, would both have stayed green.
#[cfg(test)]
mod session_tests {
    use super::*;

    const SR: u32 = 48_000;
    const CHUNK: usize = (SR as usize) / 20; // 0.05 s, the capture tick

    /// Deterministic broadband noise. Fixed-seed LCG rather than an rng
    /// dependency, so a failure reproduces across toolchains.
    fn noise(n: usize, seed: u32) -> Vec<f32> {
        let mut s = seed | 1;
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (s >> 8) as f32 / (1 << 23) as f32 - 1.0
            })
            .collect()
    }

    fn statics() -> FrameStatics {
        FrameStatics {
            sr: SR,
            spec_f_min: 20.0,
            spec_f_max: SR as f64 / 2.0,
            spec_n_columns: ac_core::visualize::aggregate::transfer_spectrum_n_columns(
                20.0,
                SR as f64 / 2.0,
            ),
            weighting: ac_core::visualize::weighting_curves::WeightingCurve::from_tag("Z").unwrap(),
            integration_tag: "fast".to_string(),
            mtw_ppo: ac_core::visualize::mtw::ladder::P_REF,
            mtw_n_blocks: ac_core::visualize::mtw::average::DEFAULT_N_BLOCKS,
            mtw_stages: Value::Null,
        }
    }

    /// One pair, channel 0 measurement against channel 1 reference, no
    /// calibration of any kind — `spl` and the cal tags are not what these
    /// tests are about.
    fn session() -> SessionState {
        SessionState::new(
            statics(),
            Window::new(SR, 4),
            vec![PairCtx {
                pos: 0,
                meas_ch: 0,
                ref_ch: 1,
                mi: 0,
                ri: 1,
                meas_cal: None,
                ref_cal: None,
                meas_curve: None,
            }],
            2,
            0.05,
            ac_core::visualize::time_integration::TAU_FAST_S,
        )
    }

    fn events(engine_on: bool) -> TickEvents {
        TickEvents {
            engine_on,
            drive_edge_on: false,
            relock_requested: false,
            mc_enabled: false,
        }
    }

    fn drive_msg(on: bool) -> Value {
        json!({"on": on, "level_dbfs": if on { json!(-20.0) } else { Value::Null }, "drivable": true})
    }

    /// Feed `n` ticks of a correlated pair — measurement is the reference
    /// delayed by `delay` samples, which is what the estimator is meant to
    /// find — and return every frame published.
    fn run_correlated(
        s: &mut SessionState,
        n: usize,
        delay: usize,
        ev: TickEvents,
        t0: std::time::Instant,
    ) -> Vec<Value> {
        let x = noise(CHUNK * (n + 2) + delay, 0x5eed);
        let mut out = Vec::new();
        for k in 0..n {
            let r0 = delay + k * CHUNK;
            let refb = x[r0..r0 + CHUNK].to_vec();
            let meas = x[r0 - delay..r0 - delay + CHUNK].to_vec();
            let now = t0 + std::time::Duration::from_millis(50 * k as u64);
            out.extend(
                s.tick(&[meas, refb], ev, &drive_msg(ev.engine_on), now)
                    .into_iter()
                    .filter(|m| m["type"] == json!("transfer_stream")),
            );
        }
        out
    }

    /// Uncorrelated legs: the estimator has nothing to lock to and must
    /// refuse rather than pick the tallest noise peak (#227).
    fn run_uncorrelated(
        s: &mut SessionState,
        ticks: &[std::time::Instant],
        ev: TickEvents,
    ) -> Vec<Value> {
        let mut out = Vec::new();
        for (k, &now) in ticks.iter().enumerate() {
            let meas = noise(CHUNK, 0x1000 + k as u32);
            let refb = noise(CHUNK, 0x9000 + k as u32);
            out.extend(
                s.tick(&[meas, refb], ev, &drive_msg(ev.engine_on), now)
                    .into_iter()
                    .filter(|m| m["type"] == json!("transfer_stream")),
            );
        }
        out
    }

    /// Publication does not wait on the analysis window. Every tick from
    /// the first produces a frame; the ones before a ring holds a whole
    /// Welch segment say `n_averages: 0` and carry empty analysis arrays,
    /// and everything that never depended on the window — the observed
    /// drive state, the capture peaks — is there from the start.
    ///
    /// Before this split the loop `continue`d, so for the first second a
    /// client could not tell a daemon that had not started from one whose
    /// drive had already dead-manned.
    #[test]
    fn a_frame_ships_from_the_first_tick_and_states_that_it_carries_no_analysis() {
        let mut s = session();
        let t0 = std::time::Instant::now();
        // One segment is `sr` samples = 20 ticks. The 20th completes it.
        let settling = run_correlated(&mut s, 19, 480, events(true), t0);
        assert_eq!(
            settling.len(),
            19,
            "a tick before the segment published nothing"
        );
        for f in &settling {
            assert_eq!(
                f["n_averages"],
                json!(0),
                "settling frame claimed a Welch block"
            );
            for key in [
                "freqs",
                "magnitude_db",
                "phase_deg",
                "coherence",
                "meas_spectrum",
            ] {
                assert_eq!(
                    f[key].as_array().map(Vec::len),
                    Some(0),
                    "{key} was not empty on a settling frame"
                );
            }
            assert_eq!(f["delay_locked"], json!(false));
            assert_eq!(
                f["drive"]["on"],
                json!(true),
                "drive state withheld while settling"
            );
        }
        // Peaks are measured from the tick's own blocks, so they are real
        // numbers on the very first frame — the thing the old gate hid.
        assert!(
            settling[0]["meas_peak_dbfs"].as_f64().is_some(),
            "capture peaks withheld while settling"
        );

        let analysing = run_correlated(&mut s, 1, 480, events(true), t0);
        let f = analysing
            .last()
            .expect("no frame on the tick that completed the segment");
        assert_eq!(f["n_averages"], json!(1));
        assert!(!f["freqs"].as_array().unwrap().is_empty());
    }

    /// The analysis advances on the ring, not on the loop.
    ///
    /// At 48 kHz the ring's start moves one `step` — 0.5 s — while the
    /// loop ticks 20 times, so nine frames in ten repeat the previous
    /// estimate exactly. That was true before this cache existed too; the
    /// difference is that the repetition was produced by recomputing a
    /// 2.5 s Welch pass and a full-resolution IFFT to arrive at the same
    /// bytes, and that it was invisible on the wire.
    #[test]
    fn the_analysis_advances_once_per_welch_hop_not_once_per_tick() {
        let mut s = session();
        let t0 = std::time::Instant::now();
        // Settle first: while the window fills, `n_blocks` changes and
        // every tick legitimately re-analyses.
        run_correlated(&mut s, 60, 480, events(false), t0);
        let frames = run_correlated(&mut s, 60, 480, events(false), t0);

        let seqs: Vec<u64> = frames
            .iter()
            .map(|f| f["analysis_seq"].as_u64().unwrap())
            .collect();
        assert!(
            seqs.windows(2).all(|w| w[1] >= w[0]),
            "analysis_seq went backwards: {seqs:?}"
        );
        let recomputes = seqs.windows(2).filter(|w| w[1] != w[0]).count();
        // 60 ticks of 0.05 s = 3.0 s; the hop is 0.5 s.
        assert_eq!(
            recomputes, 6,
            "expected one recomputation per 0.5 s hop over 3.0 s, got {recomputes}: {seqs:?}"
        );

        // And the repetition is real: same seq means the same numbers.
        for w in frames.windows(2) {
            let same_seq = w[0]["analysis_seq"] == w[1]["analysis_seq"];
            let same_mag = w[0]["magnitude_db"] == w[1]["magnitude_db"];
            assert_eq!(
                same_seq, same_mag,
                "analysis_seq and the arrays disagree about whether the estimate changed"
            );
        }
    }

    /// The cache must never be stale: what a frame carries has to equal
    /// what analysing the ring right now would produce.
    ///
    /// Checked mid-hop, where a stale cache is possible at all — on a
    /// boundary tick the two are trivially equal.
    #[test]
    fn a_held_estimate_equals_one_computed_from_the_ring_as_it_stands() {
        let mut s = session();
        let t0 = std::time::Instant::now();
        run_correlated(&mut s, 60, 480, events(false), t0);
        // Three more ticks: 0.15 s into a 0.5 s hop.
        let frames = run_correlated(&mut s, 3, 480, events(false), t0);
        let held = frames.last().unwrap();

        let key = AnalysisKey {
            dropped: s.dropped,
            n_blocks: s.n_blocks(),
            delay: s.pairs[0].delay.map(|l| l.samples).unwrap_or(0),
            mc_enabled: false,
        };
        let fresh = analyse_pair(&s.ctx[0], &s.pairs[0], &s.statics, &s.rings, key, 0)
            .expect("rings hold both channels");
        assert_eq!(
            held["magnitude_db"], fresh.magnitude_db,
            "the frame's magnitude is not what the ring says now"
        );
        assert_eq!(held["coherence"], fresh.coherence);
        assert_eq!(held["meas_spectrum"], fresh.meas_spectrum);
    }

    /// A lock arriving mid-hop must invalidate the estimate. The held one
    /// was computed unaligned, and publishing it until the next boundary
    /// would show an alignment the frame simultaneously claims to have.
    #[test]
    fn a_changed_lock_re_analyses_before_the_next_hop() {
        let mut s = session();
        let t0 = std::time::Instant::now();
        run_correlated(&mut s, 60, 480, events(false), t0);
        let before = run_correlated(&mut s, 1, 480, events(false), t0);
        let before = before.last().unwrap().clone();

        // Move the lock without moving the ring — the drive edge and
        // `relock` both do this in the middle of a hop.
        s.pairs[0].delay = Some(Lock {
            samples: 1200,
            driving: false,
        });
        let after = run_correlated(&mut s, 1, 480, events(false), t0);
        let after = after.last().unwrap();

        assert_ne!(
            before["analysis_seq"], after["analysis_seq"],
            "a changed lock did not re-analyse"
        );
        assert_eq!(after["delay_samples"], json!(1200));
        assert_ne!(
            before["magnitude_db"], after["magnitude_db"],
            "re-analysis at a different alignment produced the same H1"
        );
    }

    /// A settling frame and an analysis frame must be the same shape. They
    /// are built by two different functions, so nothing but this stops one
    /// gaining a field the other lacks — and a consumer meeting the
    /// difference reads it as a daemon that dropped a field mid-session.
    #[test]
    fn the_settling_frame_has_the_same_keys_as_an_analysis_frame() {
        fn keys(v: &Value) -> Vec<String> {
            let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
            k.sort();
            k
        }
        let mut s = session();
        let t0 = std::time::Instant::now();
        let settling = run_correlated(&mut s, 1, 480, events(false), t0);
        let analysing = run_correlated(&mut s, 20, 480, events(false), t0);
        assert_eq!(
            keys(&settling[0]),
            keys(analysing.last().unwrap()),
            "settling and analysis frames disagree about the frame's shape"
        );
    }

    /// `n_averages` is the frame's statement about its own coherence bias.
    /// It rises from 0 (no segment yet) through the window filling and then
    /// stops, because `drain_to_block_lattice` pins the ring inside one
    /// `step` of the target (#208).
    #[test]
    fn n_averages_climbs_to_the_window_depth_and_then_holds() {
        let mut s = session();
        let t0 = std::time::Instant::now();
        let frames = run_correlated(&mut s, 140, 480, events(false), t0);
        let seen: Vec<u64> = frames
            .iter()
            .map(|f| f["n_averages"].as_u64().unwrap())
            .collect();
        assert_eq!(seen.first(), Some(&0), "first frame claimed a Welch block");
        assert_eq!(
            seen.iter().find(|&&n| n > 0),
            Some(&1),
            "the first analysis frame did not report exactly one block"
        );
        assert_eq!(
            seen.last(),
            Some(&4),
            "settled frames do not report the window depth"
        );
        assert!(
            seen.windows(2).all(|w| w[1] >= w[0]),
            "n_averages went backwards: {seen:?}"
        );
        assert!(
            seen.iter().all(|&n| n <= 4),
            "n_averages exceeded the window depth: {seen:?}"
        );
    }

    /// A refused estimate must not be retried on the very next tick: each
    /// attempt is the same full-ring FFT+IFFT the delay cache exists to
    /// avoid, and its inputs only turn over on the ring's own timescale.
    ///
    /// The clock is a parameter, so this asserts the interval itself. A
    /// live session could only assert it by sleeping, which is why
    /// `RELOCK_RETRY` had no test before: any value at all was green.
    #[test]
    fn a_refused_delay_waits_out_the_retry_interval_before_trying_again() {
        let mut s = session();
        let t0 = std::time::Instant::now();
        // Fill the ring, then hold the clock still: every tick after the
        // first attempt is inside the retry window.
        let warm: Vec<std::time::Instant> = (0..20)
            .map(|k| t0 + std::time::Duration::from_millis(50 * k))
            .collect();
        let frames = run_uncorrelated(&mut s, &warm, events(true));
        let first = frames.last().expect("a frame once the segment is in");
        assert_eq!(
            first["delay_locked"],
            json!(false),
            "uncorrelated legs must not lock"
        );
        assert_eq!(
            first["delay_attempts"],
            json!(1),
            "expected exactly one attempt"
        );

        // Well inside RELOCK_RETRY: no second attempt.
        let held: Vec<std::time::Instant> = (0..5)
            .map(|k| t0 + std::time::Duration::from_millis(1000 + 50 * k))
            .collect();
        let frames = run_uncorrelated(&mut s, &held, events(true));
        assert_eq!(
            frames.last().unwrap()["delay_attempts"],
            json!(1),
            "retried before the interval elapsed"
        );

        // Past it: exactly one more.
        let after = vec![t0 + RELOCK_RETRY + std::time::Duration::from_millis(1500)];
        let frames = run_uncorrelated(&mut s, &after, events(true));
        assert_eq!(
            frames.last().unwrap()["delay_attempts"],
            json!(2),
            "did not retry after the interval elapsed"
        );
    }

    /// `relock` (#226) discards the held lock, and the attempt counter
    /// stays monotone across it — a pair that locked and then started
    /// refusing must not read as one never asked (`ac-scene::fault`).
    #[test]
    fn a_relock_request_drops_the_lock_and_leaves_the_attempt_count_monotone() {
        let mut s = session();
        let t0 = std::time::Instant::now();
        let frames = run_correlated(&mut s, 25, 480, events(true), t0);
        let locked = frames.last().unwrap();
        assert_eq!(
            locked["delay_locked"],
            json!(true),
            "correlated pair failed to lock"
        );
        assert_eq!(locked["delay_samples"], json!(480));
        let attempts_before = locked["delay_attempts"].as_u64().unwrap();

        let ev = TickEvents {
            relock_requested: true,
            ..events(true)
        };
        // The flush lands before this tick's own acquisition, so the pair
        // re-locks within the same tick — what changes is the attempt
        // count, which must have gone up rather than reset.
        let after = run_correlated(&mut s, 1, 480, ev, t0 + std::time::Duration::from_secs(5));
        let f = after.last().unwrap();
        assert!(
            f["delay_attempts"].as_u64().unwrap() > attempts_before,
            "relock did not cause a new attempt"
        );
    }

    /// The drive off→on edge discards a lock taken against silence and
    /// keeps one taken while driving (#226). `it_relock` covers both over
    /// ZMQ; here they are two assertions on the same held state.
    #[test]
    fn the_drive_edge_discards_a_lock_taken_against_silence_and_keeps_one_taken_driving() {
        let t0 = std::time::Instant::now();

        let mut silent = session();
        let frames = run_correlated(&mut silent, 25, 480, events(false), t0);
        assert_eq!(frames.last().unwrap()["delay_locked"], json!(true));
        assert!(matches!(
            silent.pairs[0].delay,
            Some(Lock { driving: false, .. })
        ));
        silent.flush_locks_taken_against_silence();
        assert!(
            silent.pairs[0].delay.is_none(),
            "a lock taken against silence survived the drive edge"
        );
        assert!(
            silent.ladders[0].is_none(),
            "the ladder outlived the lock it was aligned to"
        );

        let mut driving = session();
        let frames = run_correlated(&mut driving, 25, 480, events(true), t0);
        assert_eq!(frames.last().unwrap()["delay_locked"], json!(true));
        let held = driving.pairs[0].delay;
        assert!(matches!(held, Some(Lock { driving: true, .. })));
        driving.flush_locks_taken_against_silence();
        assert_eq!(
            driving.pairs[0].delay.map(|l| l.samples),
            held.map(|l| l.samples),
            "a lock taken while driving was discarded by a later drive edge"
        );
    }
}
