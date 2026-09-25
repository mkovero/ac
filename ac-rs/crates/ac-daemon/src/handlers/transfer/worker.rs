//! `transfer_stream`'s thread: the one part of this command that needs an
//! audio backend, a socket and a clock.
//!
//! Launch resolution is [`super::plan`]; what the loop decides per tick is
//! [`super::session`]. What is left here is the sequence that needs the
//! outside world — open the engine, drain capture, poll the drive, publish,
//! tear down — and the handles it needs to do it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::audio::make_engine;
use crate::handlers::snapshot::{SnapshotRingState, SpoolEntry};
use crate::handlers::{busy_guard, cfg_guard, send_pub, spawn_worker};
use crate::server::ServerState;
use crate::workers::{with_unpoisoned, DriveState, RelockRequest, StoppingEngine};

use super::plan::SessionPlan;
use super::session::{SessionState, TickEvents};
use super::window::Window;

/// Which output ports a `transfer_stream` worker must open and connect at
/// launch. Deliberately a pure fn, so the unit tests below
/// (`drivable_session_connects_output_with_drive_off`,
/// `drivable_session_connects_distinct_ref_output`,
/// `passive_session_opens_no_output_ports`) pin the exact port list
/// headlessly.
///
/// End to end, the fake backend sees one bit of this decision: an empty vs
/// non-empty port list (#204). With no port open, a driven fake captures no
/// generator signal — silence, or the fake's dither floor — and
/// `it_set_drive.rs::drive_on_lowers_the_captured_level_and_off_returns_it_within_one_frame`
/// fails. The fake does not see *which* ports are connected: a wrong port,
/// or `ref_out_port` dropped while `out_port` stays connected, still reads
/// as driven there and needs a live JACK/box run to observe.
///
/// Keyed on `drivable`, **not** `drive`: allocation and emission are
/// separate. A drivable session comes up connected but silent — emission
/// stays gated on `set_drive` / launch-time `drive`.
pub(super) fn drive_out_ports(drivable: bool, out_port: &str, ref_out_port: &str) -> Vec<String> {
    if !drivable {
        Vec::new()
    } else if ref_out_port != out_port {
        vec![out_port.to_string(), ref_out_port.to_string()]
    } else {
        vec![out_port.to_string()]
    }
}

type DriveSlot = Arc<Mutex<Option<Arc<DriveState>>>>;
type RelockSlot = Arc<Mutex<Option<Arc<RelockRequest>>>>;
type SnapshotRingSlot = Arc<Mutex<Option<Arc<Mutex<SnapshotRingState>>>>>;
type SnapshotSpool = Arc<Mutex<std::collections::HashMap<String, SpoolEntry>>>;

/// The `ServerState` slots one `transfer_stream` session publishes, owned
/// from the moment they are published until the guard drops (#432).
///
/// Built by [`TransferSessionGuard::publish`], which is also what publishes
/// `drive_state`, `relock_state` and the snapshot ring, so no line of code sits between a slot
/// becoming visible and something being responsible for clearing it. Moved
/// into the worker, it is dropped on every way out of the thread — normal
/// completion, an early `return` when the engine will not open or start, or
/// a panic unwinding the worker — and its `Drop` does the teardown that used
/// to sit only at the bottom of [`run_session`], in the same order.
///
/// Holds plain `Arc`s rather than `&ServerState` so a test can build one
/// without a daemon.
struct TransferSessionGuard {
    drive_state_slot: DriveSlot,
    relock_state_slot: RelockSlot,
    snapshot_ring_slot: SnapshotRingSlot,
    snapshot_spool: SnapshotSpool,
    drive_state: Arc<DriveState>,
    relock_state: Arc<RelockRequest>,
    snapshot_ring: Arc<Mutex<SnapshotRingState>>,
}

impl TransferSessionGuard {
    /// Publish `drive_state`, `relock_state` and the (pending) snapshot ring
    /// into their slots and take ownership of clearing them, along with the
    /// spool the worker fills.
    fn publish(
        drive_state_slot: DriveSlot,
        relock_state_slot: RelockSlot,
        snapshot_ring_slot: SnapshotRingSlot,
        snapshot_spool: SnapshotSpool,
        drive_state: Arc<DriveState>,
        relock_state: Arc<RelockRequest>,
        snapshot_ring: Arc<Mutex<SnapshotRingState>>,
    ) -> TransferSessionGuard {
        let guard = TransferSessionGuard {
            drive_state_slot,
            relock_state_slot,
            snapshot_ring_slot,
            snapshot_spool,
            drive_state,
            relock_state,
            snapshot_ring,
        };
        let drive = guard.drive_state.clone();
        with_unpoisoned(&guard.drive_state_slot, |slot| *slot = Some(drive));
        let relock = guard.relock_state.clone();
        with_unpoisoned(&guard.relock_state_slot, |slot| *slot = Some(relock));
        let ring = guard.snapshot_ring.clone();
        with_unpoisoned(&guard.snapshot_ring_slot, |slot| *slot = Some(ring));
        guard
    }
}

/// Empty `slot` only if it still holds `own`, so a finishing session can
/// never clear state a successor has since published.
fn clear_if_own<T>(slot: &Mutex<Option<Arc<T>>>, own: &Arc<T>) {
    with_unpoisoned(slot, |slot| {
        if slot.as_ref().is_some_and(|cur| Arc::ptr_eq(cur, own)) {
            *slot = None;
        }
    });
}

impl Drop for TransferSessionGuard {
    // Runs during unwinding when the worker panicked, so nothing here may
    // panic: every lock goes through `with_unpoisoned`, and `clear_spool`
    // accepts a poisoned lock.
    fn drop(&mut self) {
        clear_if_own(&self.drive_state_slot, &self.drive_state);
        clear_if_own(&self.relock_state_slot, &self.relock_state);

        // Snapshot ring/spool lifecycle ends with the session (deliverable
        // 3's retention policy — module doc, `handlers/snapshot.rs`). The
        // ring is published at handler time (#188), so a successor can have
        // published its own before this guard drops: clear only our own.
        clear_if_own(&self.snapshot_ring_slot, &self.snapshot_ring);
        crate::handlers::snapshot::clear_spool(&self.snapshot_spool);
        self.snapshot_spool.clear_poison();
    }
}

/// The handles a running session needs that are not data: the PUB socket,
/// the shared toggles, and the guard over the `ServerState` slots this
/// session owns for its lifetime.
///
/// Separate from [`SessionPlan`] because the split is not cosmetic —
/// `drive_state`, `relock_state` and the snapshot ring are constructed and
/// published *before*
/// the worker spawns, which is what closes the race described in
/// [`transfer_stream`]. A plan built and then handed over could not have
/// that property.
struct SessionIo {
    pub_tx: crossbeam_channel::Sender<Vec<u8>>,
    mic_corr_enabled: Arc<AtomicBool>,
    session: TransferSessionGuard,
}

pub fn transfer_stream(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "transfer_stream");
    cfg_guard!(state);

    let plan = match SessionPlan::resolve(state, cmd) {
        Ok(p) => p,
        Err(reply) => return reply,
    };

    // Publish the drive state BEFORE spawning the worker, so it exists
    // the instant this handler returns `{"ok":true}`. If it were created
    // inside the closure, there would be a window — the whole of
    // `eng.start`, tens to hundreds of ms of JACK port registration —
    // during which a client that got the ok reply and immediately
    // armed+fired would be told "no session running". #182's stimulus
    // client is exactly that caller. Constructing it here closes the race
    // structurally rather than narrowing it.
    //
    // `relock_state` is published alongside it for the same structural
    // reason: closing the window between the CTRL reply and the worker
    // actually existing, rather than narrowing it.
    //
    // The snapshot ring is published here too (#188), but *pending*: its
    // cap in samples needs the engine's sample rate, which only exists
    // after `eng.start`. The worker calls `start(sr)` on it once the
    // engine is up; until then `snapshot` refuses with the "starting"
    // error rather than claiming there is no session.
    //
    // The guard that publishes all three is the thing that clears them, on
    // every way the worker can end (#432).
    let snapshot_ring = Arc::new(Mutex::new(SnapshotRingState::pending(
        plan.snapshot_ring_s,
        plan.unique_chans.clone(),
        plan.pairs.clone(),
        plan.weighting.tag().to_string(),
        plan.integration_tag.clone(),
        plan.unique_cals.clone(),
    )));
    let session = TransferSessionGuard::publish(
        state.drive_state.clone(),
        state.relock_state.clone(),
        state.snapshot_ring.clone(),
        state.snapshot_spool.clone(),
        Arc::new(DriveState::new(plan.drive, plan.level_dbfs)),
        Arc::new(RelockRequest::new()),
        snapshot_ring,
    );

    let io = SessionIo {
        pub_tx: state.pub_tx.clone(),
        mic_corr_enabled: state.mic_correction_enabled.clone(),
        session,
    };

    // Built before the plan is moved into the worker; nothing in it
    // depends on the worker existing.
    let reply = plan.launch_reply();
    let worker = spawn_worker(state, "transfer_stream", move |stop| {
        run_session(plan, io, stop);
    });
    {
        let mut workers = state.workers.lock().unwrap();
        workers.insert("transfer_stream".to_string(), worker);
    }
    reply
}

/// Open the engine, register every capture port, apply the fake-backend
/// knobs, and flush what accrued before any of that happened.
///
/// `None` when the engine will not start; the error has already been
/// published, because by this point the REP reply is long sent and the PUB
/// channel is the only way to say so.
///
/// The engine is stopped when the returned handle drops, and on the `None`
/// paths once it has been built.
fn open_engine(
    plan: &SessionPlan,
    pub_tx: &crossbeam_channel::Sender<Vec<u8>>,
) -> Option<StoppingEngine> {
    // Passive mode (default): open no output ports at all so the daemon
    // doesn't need exclusive access to the playback side — the user is
    // driving the DUT externally. Any drivable session connects its
    // outputs here and stays silent until drive is asked for; without
    // this the generator plays onto an unconnected port.
    let out_ports: Vec<String> = drive_out_ports(plan.drivable, &plan.out_port, &plan.ref_out_port);

    let mut eng = match make_engine(plan.fake, plan.backend_required.as_deref()) {
        Ok(eng) => StoppingEngine::new(eng),
        Err(e) => {
            send_pub(
                pub_tx,
                "error",
                &json!({"cmd":"transfer_stream","message":e.to_string()}),
            );
            return None;
        }
    };
    let main_port = plan.unique_ports[0].clone();
    if let Err(e) = eng.start(&out_ports, Some(&main_port)) {
        send_pub(
            pub_tx,
            "error",
            &json!({"cmd":"transfer_stream","message":format!("{e}")}),
        );
        return None;
    }
    for p in &plan.unique_ports[1..] {
        if let Err(e) = eng.add_ref_input(p) {
            eprintln!("transfer_stream: warning — ref input {p}: {e}");
        }
    }
    if plan.fake {
        if let Some((gain, delay_samples)) = plan.fake_correlated_pair {
            eng.set_correlated_pair(gain, delay_samples);
        }
        // After `add_ref_input` above, so the ring count matches the
        // channels this session actually captures.
        if let Some(process_secs) = plan.fake_ring_process_secs {
            eng.enable_ring_mode(
                process_secs,
                plan.unique_ports.len().saturating_sub(1),
                plan.fake_ring_period,
            );
        }
    }
    Some(eng)
}

/// The session, from a started engine to a stopped one.
fn run_session(mut plan: SessionPlan, io: SessionIo, stop: Arc<AtomicBool>) {
    let SessionIo {
        ref pub_tx,
        ref mic_corr_enabled,
        session: guard,
    } = io;
    let drive_state = &guard.drive_state;
    let relock_state = &guard.relock_state;

    // Every `return` and panic from here on drops `guard` (and, once it
    // exists, `eng`), which is what clears this session's slots.
    let Some(mut eng) = open_engine(&plan, pub_tx) else {
        return;
    };
    let sr = eng.sample_rate();
    let statics = plan.frame_statics(sr, eng.backend_name());

    // Snapshot ring (handoff: snapshot-backend M1, deliverable 1):
    // raw pre-processing samples for every unique session channel,
    // capped at `snapshot_ring_s` seconds. The handler published it
    // pending (#188). Crash-safety: wipe any stale spool from a prior
    // session before the ring is started, so no snapshot this session
    // writes can be deleted by the wipe — a pending ring refuses every
    // `snapshot` (module doc, `handlers/snapshot.rs`).
    // A refused spool path ends the session here; dropping `guard` clears
    // the slots it published (#432).
    if let Err(r) =
        crate::handlers::snapshot::reset_spool_dir(&plan.snapshot_spool_dir, &guard.snapshot_spool)
    {
        eng.stop();
        send_pub(
            pub_tx,
            "error",
            &json!({"cmd":"transfer_stream","message":r.message()}),
        );
        return;
    }
    let snapshot_ring = guard.snapshot_ring.clone();
    snapshot_ring.lock().unwrap().start(sr);

    // Analysis window: the last `n_averages` Welch blocks, cut on the
    // **stream's own** `k·step` lattice rather than from the head of a
    // freely-sliding buffer (#208). `chunk_secs` is the capture tick,
    // which is no longer the same thing as the analysis rate — see
    // `drain_to_block_lattice` and `n_averages` on the wire.
    let window = Window::new(sr, 4);
    let chunk_secs = 0.05;

    // `drive_state` was constructed and published by the handler
    // before this worker was spawned (see the note there) — the
    // worker only holds and polls it. Seeded from the launch params:
    // the legacy scripted `drive: true` path behaves exactly as
    // before; `ac transfer` never sets that param and drives only
    // via `set_drive`.

    // What the engine is currently doing, so the poll below acts
    // only on transitions — an unchanged 250 ms resend must not
    // re-drive the engine four times a second.
    let mut engine_on = plan.drive;
    let mut engine_level = plan.level_dbfs;
    if plan.drive {
        eng.set_pink(ac_core::shared::generator::dbfs_to_amplitude(
            plan.level_dbfs,
        ));
    }
    // Warmup flush: discard whatever's buffered before the engine
    // started (real hardware) / prime the fake generator (fake
    // backend). Goes through `capture_multi`, never the
    // single-channel `capture_block`, for two independent reasons.
    //
    // 1. Issue #216 — the ring skew. `capture_block` clears the
    //    measurement ring only (`CaptureRings::capture_block` ->
    //    `clear_meas`). The reference rings were registered above and
    //    keep everything that accrued, so each one leaves warmup
    //    exactly `0.2 s · sr` samples ahead of meas. Nothing in the
    //    streaming loop re-syncs them: `capture_multi_contiguous`
    //    pops `min_occupied()` from every ring, which is invariant
    //    under a constant offset, and the per-tick
    //    `clear_meas_and_refs` that used to destroy the offset went
    //    away with #207. The skew is therefore permanent for the
    //    session — measured on the rig as a constant 19200 samples
    //    at 96 kHz across 929 ticks of two runs. It costs -200 ms on
    //    the reported `delay_ms`, drags coherence to ~0.64 (0.2 s of
    //    a 1 s Welch segment no longer shared, 0.8² = 0.64), and
    //    moves `magnitude_db` by 2.5 dB mean / 32.6 dB worst bin.
    //    `capture_multi` clears meas and every ref together and pops
    //    the same count from each, so all rings leave warmup at the
    //    same phase.
    // 2. The fake correlated-pair stimulus. `capture_block` only
    //    reads the meas-role port, which would advance
    //    `FakeEngine`'s meas-side position counter with no matching
    //    ref-side advance, desyncing the pair's `gain`/
    //    `delay_samples` relationship before the main loop even
    //    starts (found by the M1.5 I-B parity test failing with a
    //    corrupted downstream FLAC encode — traced to meas and ref
    //    silently reading unrelated windows of the same generator).
    //
    // Reason 2 was why this was already conditional; reason 1 is why
    // the condition was the bug. Backends with no ref registered fall
    // through to the same meas-only clear `capture_block` did.
    let _ = eng.capture_multi(0.2);

    // Everything the loop below maintains across ticks. The engine,
    // the socket, the stop flag and the clock stay out here; what goes
    // in is decidable from a `Vec` of samples, which is what makes the
    // per-tick decisions testable without a daemon.
    let mut session = SessionState::new(
        statics,
        window,
        std::mem::take(&mut plan.pair_ctx),
        plan.unique_ports.len(),
        chunk_secs,
        plan.integration_tau_s,
    );

    // Drain telemetry (#208 D1). Off unless `AC_DRAIN_TELEMETRY` is set:
    // this slice adds no behaviour, and a streaming session must log
    // nothing new by default.
    let mut drain_telemetry = crate::audio::drain_telemetry::DrainTelemetry::from_env(sr);

    // Last `relock` generation consumed (#226). Seeded from the
    // request state's own start value rather than 0 so a generation
    // already at some count from a prior worker's Arc (there isn't
    // one — this Arc is fresh per session) can never be mistaken for
    // a pending request; harmless either way, but this is the value
    // that makes "no request yet" mean the same thing here as it does
    // in `RelockRequest::new`.
    let mut last_relock_gen = relock_state.generation();

    while !stop.load(Ordering::Relaxed) {
        // Contiguous drain (#207). `capture_multi` would clear the ring
        // before waiting, discarding whatever accrued while the previous
        // tick was being processed — so the `rings` window assembled below
        // would be ~50 spliced fragments presented as continuous time.
        let bufs = match eng.capture_multi_contiguous(chunk_secs) {
            Ok(b) => b,
            Err(e) => {
                send_pub(
                    pub_tx,
                    "error",
                    &json!({"cmd":"transfer_stream","message":format!("{e}")}),
                );
                break;
            }
        };
        if stop.load(Ordering::Relaxed) {
            break;
        }

        // #254: fewer buffers than the session has capture channels is
        // unrecoverable, and it used to be handled by silence. The rings
        // beyond what the backend returned never reach `nperseg`, so the
        // warmup gate below `continue`s for the life of the session: the
        // reply was already `ok: true`, nothing publishes, nothing errors,
        // and the client cannot tell this from a slow start.
        //
        // Erroring here rather than at launch covers every backend,
        // including ones not written yet, and covers a backend that
        // changes its mind mid-session — the count is only knowable from
        // what capture actually returned.
        if bufs.len() < session.n_channels() {
            send_pub(
                pub_tx,
                "error",
                &json!({
                    "cmd": "transfer_stream",
                    "message": format!(
                        "capture returned {} channel buffer(s) for a session over {} \
                         capture channel(s) {:?}; no frames can be produced. Check the \
                         backend's multi-channel capture support, the `pairs` list, and \
                         the port names each channel resolved to: {:?}",
                        bufs.len(),
                        session.n_channels(),
                        plan.unique_chans,
                        plan.unique_ports,
                    ),
                }),
            );
            break;
        }

        // #208 D1: samples popped per tick against wall clock. Logged
        // before any of this tick's processing so the interval it reports
        // is the loop's own period, not a partial one. `bufs[0]` is the
        // measurement channel and every other channel is popped to the
        // same length by `capture_multi_contiguous`.
        if drain_telemetry.enabled() {
            let n = bufs.first().map(|b| b.len()).unwrap_or(0);
            if let Some(t) = drain_telemetry.tick(
                n,
                &eng.last_drain_occupancy(),
                eng.discarded_samples(),
                std::time::Instant::now(),
            ) {
                eprintln!("{}", t.raw);
                if let Some(summary) = t.summary {
                    eprintln!("{summary}");
                }
            }
        }

        // Dead-man + drive poll, once per capture tick. The timeout
        // is evaluated here rather than on message arrival because
        // silence is precisely the condition being detected.
        drive_state.expire_if_stale(crate::workers::DRIVE_DEADMAN_MS);
        let want_on = drive_state.on();
        let want_level = drive_state.level_dbfs();
        let prev_engine_on = engine_on;
        if want_on != engine_on || (want_on && want_level != engine_level) {
            if want_on {
                eng.set_pink(ac_core::shared::generator::dbfs_to_amplitude(want_level));
            } else {
                eng.set_silence();
            }
            engine_on = want_on;
            engine_level = want_level;
        }
        // `engine_on` is now this tick's observed state — the edge
        // (#226) is the false→true transition of that assignment, not
        // a re-derivation, so it is read straight off the before/after
        // pair rather than compared against commanded state.
        let drive_edge_on = !prev_engine_on && engine_on;

        // Manual re-lock (#226), read right after the drive poll so a
        // re-lock request and the tick's delay attempt never
        // interleave — `SessionState::tick` consumes it before
        // acquisition. The flush it triggers, and the drive-edge flush
        // beside it, both live in the session because both are
        // decisions about held locks and neither needs the engine.
        let relock_gen = relock_state.generation();
        let relock_requested = relock_gen != last_relock_gen;
        last_relock_gen = relock_gen;

        // Observed drive state (#228). Built from `engine_on`/`engine_level`
        // — what was actually applied to the engine on this tick, after the
        // dead-man above and after `set_drive`'s refusal check against the
        // fixed emission maximum (#459) — not from what a client last asked
        // for. A fault indicator fed by commanded state would show `NO
        // REFERENCE` while the daemon had already dead-manned the drive, or
        // show nothing while a stale level lingered; reporting belief
        // rather than observation is the defect class #228 exists to make
        // visible.
        //
        // `level_dbfs` is null while off, so there is no stale number to
        // misread, and carries the applied value while on — a value that,
        // since #459, is exactly the level the client asked for (a level
        // above the maximum never reaches `engine_level` at all; `set_drive`
        // refused it instead).
        //
        // `drivable` distinguishes "this session could drive and is not"
        // (the indicator's idle row) from "this session never drives" —
        // an external-DUT session, where silence from the daemon says
        // nothing about whether signal is present.
        let drive_msg = ac_core::wire::WireDrive {
            on: engine_on,
            level_dbfs: engine_on.then_some(engine_level),
            drivable: plan.drivable,
        };

        // Feed the snapshot ring the same raw, pre-processing `bufs`
        // the H1 sliding window derives from — same capture, second
        // (larger, longer-retention) consumer.
        snapshot_ring.lock().unwrap().push_tick(&bufs);

        let messages = session.tick(
            &bufs,
            TickEvents {
                engine_on,
                drive_edge_on,
                relock_requested,
                mc_enabled: mic_corr_enabled.load(Ordering::Relaxed),
            },
            &drive_msg,
            std::time::Instant::now(),
        );

        // Keep the snapshot ring's copy of the locks in sync — cheap
        // (small Vec), and simpler than pushing incrementally from
        // inside the acquisition loop. Written after the tick rather
        // than between acquisition and the fan-out, so a `snapshot`
        // arriving during a fan-out reads the previous tick's locks.
        // A lock only changes at acquisition, so the difference is
        // confined to the one tick a pair first locks on.
        //
        // The ladder provenance rides the same sync (#221): after the tick,
        // so a ladder built or flushed this tick is described as it now
        // stands, and a snapshot taken mid-tick reads the previous tick's.
        {
            let mut ring = snapshot_ring.lock().unwrap();
            ring.delay_samples = session.delay_samples();
            ring.mtw = session.mtw_provenance();
            // Both count the same `bufs`, so a ladder origin taken from one
            // converts into the other's coordinates. If they ever part, every
            // snapshot replays a different block set than the screen drew.
            debug_assert_eq!(
                ring.pushed_total(),
                session.consumed,
                "snapshot ring and session disagree about the stream length"
            );
        }

        for msg in messages {
            send_pub(pub_tx, "data", &msg);
        }
    }

    if engine_on {
        eng.set_silence();
    }

    // Capture-contiguity instrumentation (handoff-capture-contiguity D2).
    // `capture_multi` clears the ring before waiting, so every tick
    // discards whatever accrued while the previous tick was being
    // processed — meaning the window `rings` assembles above is spliced by
    // this many samples in total, not the continuous stretch of time the
    // FFT assumes. Reported once at teardown rather than per tick so it
    // cannot flood a long session, and deliberately kept off the wire (the
    // frame contract is unchanged).
    let discarded = eng.discarded_samples();
    if discarded > 0 {
        let spliced_s = discarded as f64 / sr as f64;
        eprintln!(
            "transfer_stream: capture discontinuity — {discarded} samples \
             ({spliced_s:.2} s) discarded by the pre-wait ring clear; the \
             analysis window is not contiguous"
        );
    }

    // Engine stopped, then this session's slots cleared, then `done` — the
    // order teardown has always had. Dropped explicitly here rather than at
    // scope end so `done` still follows the clear.
    drop(eng);

    // Known, bounded: a `set_drive` arriving between this worker's
    // last poll and the slot-clear below returns `{"ok":true}` for a
    // session that will not act on it — a few ms at teardown. Not
    // worth a lock redesign; the dead-man and the client's own
    // teardown both converge on silence regardless.
    drop(guard);

    send_pub(
        pub_tx,
        "done",
        &json!({"cmd":"transfer_stream","stopped":true}),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    // The regression: a drivable session must connect its output even though
    // launch-time `drive` is false. Against the old `!drive` gate this is
    // empty and the generator plays onto an unconnected port. This test pins
    // the port list; the no-ports half of the regression is also caught end
    // to end by `it_set_drive.rs`'s drive-level test under the fake (#204).
    #[test]
    fn drivable_session_connects_output_with_drive_off() {
        assert_eq!(
            drive_out_ports(true, "system:playback_1", "system:playback_1"),
            vec!["system:playback_1".to_string()]
        );
    }

    #[test]
    fn drivable_session_connects_distinct_ref_output() {
        assert_eq!(
            drive_out_ports(true, "system:playback_1", "system:playback_2"),
            vec![
                "system:playback_1".to_string(),
                "system:playback_2".to_string()
            ]
        );
    }

    // Passive sessions must still touch no playback port at all — the
    // external-DUT workflow depends on the daemon keeping its hands off.
    #[test]
    fn passive_session_opens_no_output_ports() {
        assert!(drive_out_ports(false, "system:playback_1", "system:playback_2").is_empty());
    }

    /// The four slots a `ServerState` would hand a session, all empty.
    struct Slots {
        drive: DriveSlot,
        relock: RelockSlot,
        ring: SnapshotRingSlot,
        spool: SnapshotSpool,
    }

    impl Slots {
        fn new() -> Slots {
            Slots {
                drive: Arc::new(Mutex::new(None)),
                relock: Arc::new(Mutex::new(None)),
                ring: Arc::new(Mutex::new(None)),
                spool: Arc::new(Mutex::new(std::collections::HashMap::new())),
            }
        }

        fn publish(&self) -> TransferSessionGuard {
            TransferSessionGuard::publish(
                self.drive.clone(),
                self.relock.clone(),
                self.ring.clone(),
                self.spool.clone(),
                Arc::new(DriveState::new(false, -40.0)),
                Arc::new(RelockRequest::new()),
                pending_ring(),
            )
        }

        fn assert_all_cleared(&self) {
            assert!(self.drive.lock().unwrap().is_none(), "drive slot left set");
            assert!(
                self.relock.lock().unwrap().is_none(),
                "relock slot left set"
            );
            assert!(
                self.ring.lock().unwrap().is_none(),
                "snapshot ring left set"
            );
            assert!(
                self.spool.lock().unwrap().is_empty(),
                "spool left populated"
            );
        }
    }

    /// A ring as the handler publishes it: pending, no sample rate yet.
    fn pending_ring() -> Arc<Mutex<SnapshotRingState>> {
        Arc::new(Mutex::new(SnapshotRingState::pending(
            crate::handlers::snapshot::RingSeconds::new(0.01).unwrap(),
            vec![0, 1],
            vec![(0, 1)],
            "Z".to_string(),
            "fast".to_string(),
            vec![None, None],
        )))
    }

    /// Start the ring and add a spool entry, as a running worker would.
    fn fill_worker_slots(guard: &TransferSessionGuard) {
        guard.snapshot_ring.lock().unwrap().start(48_000);
        guard.snapshot_spool.lock().unwrap().insert(
            "snap".to_string(),
            SpoolEntry {
                path: std::path::PathBuf::from("/nonexistent/ac-issue-432-snap"),
                bytes: 0,
                sha256: String::new(),
                duration_s: 0.0,
                channels: Vec::new(),
            },
        );
    }

    /// #188: the ring slot is set by `publish` itself, before any worker
    /// runs, and holds the pending ring — so `snapshot` in that window sees a
    /// session that is starting, never an empty slot.
    #[test]
    fn publish_makes_drive_and_relock_visible_before_any_worker_runs() {
        let slots = Slots::new();
        let guard = slots.publish();
        assert!(slots.drive.lock().unwrap().is_some());
        assert!(slots.relock.lock().unwrap().is_some());
        {
            let ring = slots.ring.lock().unwrap();
            let ring = ring.as_ref().expect("ring slot set before any worker");
            assert!(Arc::ptr_eq(ring, &guard.snapshot_ring));
            assert!(!ring.lock().unwrap().is_started());
        }
        drop(guard);
        slots.assert_all_cleared();
    }

    /// #432: a worker that panics must still leave every slot cleared —
    /// the guard's `Drop` runs during unwinding.
    #[test]
    fn guard_in_a_panicking_thread_clears_every_slot() {
        let slots = Slots::new();
        let guard = slots.publish();
        let handle = std::thread::spawn(move || {
            let guard = guard;
            fill_worker_slots(&guard);
            panic!("worker panic under test");
        });
        assert!(
            handle.join().is_err(),
            "the worker thread must have panicked"
        );
        slots.assert_all_cleared();
    }

    /// #432: a slot poisoned before the guard drops must not turn the drop
    /// into a panic-in-panic (which would abort the daemon), and must be
    /// left unpoisoned so a later CTRL handler's `lock().unwrap()` works.
    #[test]
    fn guard_drop_tolerates_and_clears_poisoned_slots() {
        let slots = Slots::new();
        let guard = slots.publish();
        fill_worker_slots(&guard);

        let (drive, relock, ring, spool) = (
            slots.drive.clone(),
            slots.relock.clone(),
            slots.ring.clone(),
            slots.spool.clone(),
        );
        let poisoner = std::thread::spawn(move || {
            let _d = drive.lock().unwrap();
            let _r = relock.lock().unwrap();
            let _g = ring.lock().unwrap();
            let _s = spool.lock().unwrap();
            panic!("poison every slot");
        });
        assert!(poisoner.join().is_err());
        assert!(slots.drive.is_poisoned() && slots.spool.is_poisoned());

        drop(guard);

        for poisoned in [
            slots.drive.is_poisoned(),
            slots.relock.is_poisoned(),
            slots.ring.is_poisoned(),
            slots.spool.is_poisoned(),
        ] {
            assert!(!poisoned, "guard drop must clear slot poison");
        }
        slots.assert_all_cleared();
    }

    /// #432: a finishing session must not clear drive/relock state a
    /// successor has since published; #188 extends that to the snapshot
    /// ring, now published at handler time too. Goes red if the
    /// `Arc::ptr_eq` check in `clear_if_own` is dropped, or if the ring is
    /// cleared unconditionally.
    #[test]
    fn guard_leaves_a_successors_drive_and_relock_state_alone() {
        let slots = Slots::new();
        let guard = slots.publish();
        let successor_drive = Arc::new(DriveState::new(false, -40.0));
        let successor_relock = Arc::new(RelockRequest::new());
        let successor_ring = pending_ring();
        *slots.drive.lock().unwrap() = Some(successor_drive.clone());
        *slots.relock.lock().unwrap() = Some(successor_relock.clone());
        *slots.ring.lock().unwrap() = Some(successor_ring.clone());

        drop(guard);

        let drive = slots.drive.lock().unwrap();
        let relock = slots.relock.lock().unwrap();
        let ring = slots.ring.lock().unwrap();
        assert!(drive
            .as_ref()
            .is_some_and(|d| Arc::ptr_eq(d, &successor_drive)));
        assert!(relock
            .as_ref()
            .is_some_and(|r| Arc::ptr_eq(r, &successor_relock)));
        assert!(ring
            .as_ref()
            .is_some_and(|r| Arc::ptr_eq(r, &successor_ring)));
    }
}
