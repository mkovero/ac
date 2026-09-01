//! `transfer_stream` launch and its capture loop: the one part of this
//! command that needs an audio backend, a socket and a thread.
//!
//! Resolution and validation happen here because they need `ServerState` and
//! the config; everything the loop then decides per tick lives in
//! [`super::session`].

use std::sync::atomic::Ordering;

use serde_json::{json, Value};

use ac_core::shared::calibration::Calibration;

use crate::audio::make_engine;
use crate::handlers::{
    apply_drive_ceiling, busy_guard, cfg_guard, ref_output_migration_warning, resolve_output,
    resolve_ref_output, send_pub, spawn_worker,
};
use crate::server::ServerState;

use super::frame::FrameStatics;
use super::pair::PairCtx;
use super::request::{parse_params, TransferParams};
use super::session::{SessionState, TickEvents};
use super::window::Window;

/// Which output ports a `transfer_stream` worker must open and connect at
/// launch. Deliberately a pure fn: this decision is invisible to the fake
/// backend (which synthesizes the generator's signal into the capture
/// buffer regardless of routing), so peak-based tests cannot see it and a
/// live JACK run is the only other observer. This seam makes it falsifiable
/// headlessly.
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

pub fn transfer_stream(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "transfer_stream");
    cfg_guard!(state);

    let TransferParams {
        drive,
        drivable,
        level_dbfs,
        fake_correlated_pair,
        fake_ring_process_secs,
        fake_ring_period,
        mtw_ppo,
        mtw_n_blocks,
        pairs,
        weighting,
        integration_tag,
        integration_tau_s,
    } = match parse_params(cmd) {
        Ok(p) => p,
        Err(e) => return json!({"ok": false, "error": e}),
    };

    let cfg = state.cfg.lock().unwrap().clone();
    // #360: a second, independent unclamped path from the same field
    // `set_drive` already clamps — this seeds `DriveState` directly when
    // `drive: true`, and is never touched by `set_drive`'s own clamp
    // unless the client calls it again later. Clamped here, before the
    // `DriveState::new` construction below, so the stored state never
    // holds an unclamped value regardless of whether `set_drive` is ever
    // called in this session.
    let level_dbfs = apply_drive_ceiling(cfg.drive_max_dbfs, level_dbfs);
    let capture_ports = crate::handlers::cached_capture_ports(state);

    // Resolve each unique capture channel to a port name once. `unique_ports`
    // drives the JACK port-registration order; each pair indexes into it.
    let mut unique_chans: Vec<u32> = Vec::new();
    for &(m, r) in &pairs {
        for c in [m, r] {
            if !unique_chans.contains(&c) {
                unique_chans.push(c);
            }
        }
    }
    let mut unique_ports = Vec::with_capacity(unique_chans.len());
    for &ch in &unique_chans {
        match capture_ports.get(ch as usize) {
            Some(p) => unique_ports.push(p.clone()),
            None => {
                return json!({"ok": false,
                "error": format!("channel {ch} out of range (n_capture={})", capture_ports.len())})
            }
        }
    }

    let out_port = match resolve_output(&cfg, state) {
        Ok(p) => p,
        Err(e) => return json!({"ok": false, "error": e}),
    };
    let ref_out_port = match resolve_ref_output(&cfg, state) {
        Ok(p) => p,
        Err(e) => return json!({"ok": false, "error": e}),
    };

    // Sync routing-capability check so CPAL-only environments get an
    // immediate REP error instead of a silent worker exit that the UI never
    // sees (the async `send_pub("error", …)` path below is still needed for
    // per-capture failures once the worker is live).
    {
        let probe_eng = make_engine(state.fake_audio);
        if !probe_eng.supports_routing() {
            return json!({
                "ok": false,
                "error": format!("{} backend does not support port routing", probe_eng.backend_name()),
            });
        }
    }

    let out_ch = cfg.output_channel;

    // One calibration load per unique capture channel, and every
    // per-pair view below is taken from it. `Calibration::load` re-reads
    // and re-parses the whole calibration file on every call, and this
    // handler used to ask the same question of the same file four times
    // over — the ref-curve refusal, the meas mic-curve, the meas and ref
    // full calibrations, and the snapshot ring's per-channel copy.
    //
    // Not only cheaper: it also makes those views consistent by
    // construction. Separate loads read the file at separate instants,
    // so a calibration rewritten mid-launch could give the refusal check
    // a channel with no curve and the correction path one with a curve —
    // the exact split this check exists to prevent.
    let unique_cals: Vec<Option<Calibration>> = unique_chans
        .iter()
        .map(|&ch| Calibration::load(out_ch, ch, None).ok().flatten())
        .collect();
    // Every channel named in `pairs` is in `unique_chans` by
    // construction, so this never misses: `None` means "no calibration
    // stored for this channel", never "channel not found".
    let cal_for = |ch: u32| -> Option<&Calibration> {
        unique_chans
            .iter()
            .position(|&c| c == ch)
            .and_then(|i| unique_cals[i].as_ref())
    };

    // Calibration discipline (#101): a mic-curve on the **reference**
    // channel is a misconfiguration — H1 = Y/X is a ratio, applying a
    // mic correction to the reference leg cancels against the meas-leg
    // correction (or worse, biases it the wrong way when only one leg
    // has a curve). Refuse the request synchronously with a clear
    // message so the user knows to clear the curve or swap channels.
    for &(_, ref_ch) in &pairs {
        if cal_for(ref_ch).is_some_and(|c| c.mic_response.is_some()) {
            return json!({
                "ok": false,
                "error": format!(
                    "transfer ref channel {ref_ch} has a mic-curve loaded — \
                     transfer is a ratio, applying a curve to the reference leg \
                     is wrong. Run `ac calibrate mic-curve clear input {ref_ch}` \
                     or swap meas/ref."
                ),
            });
        }
    }

    let pub_tx = state.pub_tx.clone();
    let fake = state.fake_audio;
    let mic_corr_enabled = state.mic_correction_enabled.clone();

    let out_port_r = out_port.clone();
    let meas_port_r = unique_ports.first().cloned().unwrap_or_default();
    let ref_port_r = unique_ports
        .get(1)
        .cloned()
        .unwrap_or_else(|| meas_port_r.clone());
    let pairs_r = pairs.clone();

    // Session-static per-pair context: buffer indices (precomputed so the
    // worker loop doesn't re-scan per tick) plus the full per-leg
    // calibration that feeds `meas_spectrum`/`ref_spectrum`/`spl`/
    // `cal_tags` (handoff: transfer-frame-v2 M0).
    //
    // `meas_curve` stays a field of its own rather than being read back
    // out of `meas_cal` at each use: the mag/phase/re/im correction path
    // takes the curve alone and is left untouched (additive-only
    // discipline). Both come from the same `unique_cals` load, so they
    // cannot disagree about a channel.
    let pair_ctx: Vec<PairCtx> = pairs
        .iter()
        .enumerate()
        .map(|(pos, &(meas, r))| PairCtx {
            pos,
            meas_ch: meas,
            ref_ch: r,
            mi: unique_chans.iter().position(|&c| c == meas).unwrap(),
            ri: unique_chans.iter().position(|&c| c == r).unwrap(),
            meas_cal: cal_for(meas).cloned(),
            ref_cal: cal_for(r).cloned(),
            meas_curve: cal_for(meas).and_then(|c| c.mic_response.clone()),
        })
        .collect();

    // Snapshot ring (handoff: snapshot-backend M1) takes the
    // per-unique-channel calibration directly — not per-pair, so a
    // channel used in >1 pair gets one entry — for `.acsnap`
    // provenance; the ring itself is built inside the worker once `sr`
    // is known.
    let snapshot_ring_slot = state.snapshot_ring.clone();
    let drive_state_slot = state.drive_state.clone();
    let relock_state_slot = state.relock_state.clone();
    let snapshot_spool = state.snapshot_spool.clone();
    let snapshot_spool_dir = ac_core::config::snapshot_spool_dir(&cfg);
    let snapshot_ring_s = cfg.snapshot_ring_s;
    let snapshot_weighting_tag = weighting.tag().to_string();
    let snapshot_integration_tag = integration_tag.clone();
    let unique_chans_for_ring = unique_chans.clone();
    let pairs_for_ring = pairs.clone();

    // Publish the drive state BEFORE spawning the worker, so it exists
    // the instant this handler returns `{"ok":true}`. If it were created
    // inside the closure (as `snapshot_ring` still is), there would be a
    // window — the whole of `eng.start`, tens to hundreds of ms of JACK
    // port registration — during which a client that got the ok reply and
    // immediately armed+fired would be told "no session running". #182's
    // stimulus client is exactly that caller. Constructing it here closes
    // the race structurally rather than narrowing it. (The snapshot_ring
    // twin is tracked separately.)
    let drive_state = std::sync::Arc::new(crate::workers::DriveState::new(drive, level_dbfs));
    *drive_state_slot.lock().unwrap() = Some(drive_state.clone());
    let drive_state_for_worker = drive_state;

    // Published before spawn for the same structural reason as
    // `drive_state` above: closing the window between the CTRL reply and
    // the worker actually existing, rather than narrowing it.
    let relock_state = std::sync::Arc::new(crate::workers::RelockRequest::new());
    *relock_state_slot.lock().unwrap() = Some(relock_state.clone());
    let relock_state_for_worker = relock_state;

    let worker = spawn_worker(state, "transfer_stream", move |stop| {
        let amplitude = ac_core::shared::generator::dbfs_to_amplitude(level_dbfs);
        let drive_state = drive_state_for_worker;
        let relock_state = relock_state_for_worker;

        // Passive mode (default): open no output ports at all so the daemon
        // doesn't need exclusive access to the playback side — the user is
        // driving the DUT externally. Any drivable session connects its
        // outputs here and stays silent until drive is asked for; without
        // this the generator plays onto an unconnected port.
        let out_ports: Vec<String> = drive_out_ports(drivable, &out_port, &ref_out_port);

        let mut eng = make_engine(fake);
        let main_port = unique_ports[0].clone();
        if let Err(e) = eng.start(&out_ports, Some(&main_port)) {
            send_pub(
                &pub_tx,
                "error",
                &json!({"cmd":"transfer_stream","message":format!("{e}")}),
            );
            return;
        }
        for p in &unique_ports[1..] {
            if let Err(e) = eng.add_ref_input(p) {
                eprintln!("transfer_stream: warning — ref input {p}: {e}");
            }
        }
        if fake {
            if let Some((gain, delay_samples)) = fake_correlated_pair {
                eng.set_correlated_pair(gain, delay_samples);
            }
            // After `add_ref_input` above, so the ring count matches the
            // channels this session actually captures.
            if let Some(process_secs) = fake_ring_process_secs {
                eng.enable_ring_mode(
                    process_secs,
                    unique_ports.len().saturating_sub(1),
                    fake_ring_period,
                );
            }
        }

        let sr = eng.sample_rate();
        // Fixed log-column grid for `meas_spectrum`/`ref_spectrum` (D18) —
        // computed once since it's constant for the worker's lifetime
        // (sr never changes mid-session); `spectrum_to_columns_wire`'s
        // returned freqs are then identical on every tick by construction
        // (AC #1).
        let spec_f_min = 20.0_f64;
        let spec_f_max = sr as f64 / 2.0;
        let spec_n_columns =
            ac_core::visualize::aggregate::transfer_spectrum_n_columns(spec_f_min, spec_f_max);

        // Ladder description, session-static (it derives from `sr`, which does
        // not change mid-session). Shipped with every frame so a consumer can
        // interpret the per-column `stage` index without knowing the layout
        // rules, and so a saved frame stays interpretable if those rules
        // change later.
        let mtw_stages: Value = match ac_core::visualize::mtw::ladder::layout(sr) {
            Ok(l) => Value::Array(
                l.stages
                    .iter()
                    .map(|s| {
                        json!({
                            // `W + hop·(N−1)` — how long this rung takes to
                            // fill its average. Shipped so a viewer can say
                            // how stale a band is without deriving it from
                            // the frame rate.
                            "settling_s": ac_core::visualize::mtw::settling_seconds(s, mtw_n_blocks),
                            "decim":     s.decim,
                            "rate":      s.rate,
                            "df":        s.df,
                            "window_s":  s.window_s,
                            "hop_s":     s.hop_s,
                            "f_valid":   s.f_valid,
                            "f_top":     s.f_top,
                            "blend_top": s.blend_top,
                        })
                    })
                    .collect(),
            ),
            Err(_) => Value::Null,
        };

        // Every frame input that is fixed for this worker's life, gathered
        // once. `mtw_stages` is moved in rather than cloned per frame.
        let statics = FrameStatics {
            sr,
            spec_f_min,
            spec_f_max,
            spec_n_columns,
            weighting,
            integration_tag,
            mtw_ppo,
            mtw_n_blocks,
            mtw_stages,
        };

        // Snapshot ring (handoff: snapshot-backend M1, deliverable 1):
        // raw pre-processing samples for every unique session channel,
        // capped at `snapshot_ring_s` seconds. Crash-safety: wipe any
        // stale spool from a prior session before publishing this one's
        // ring handle (module doc, `handlers/snapshot.rs`).
        crate::handlers::snapshot::reset_spool_dir(&snapshot_spool_dir, &snapshot_spool);
        let snapshot_cap_samples = (snapshot_ring_s * sr as f64).round() as usize;
        let snapshot_ring = std::sync::Arc::new(std::sync::Mutex::new(
            crate::handlers::snapshot::SnapshotRingState::new(
                sr,
                unique_chans_for_ring.clone(),
                snapshot_cap_samples,
                pairs_for_ring.clone(),
                snapshot_weighting_tag.clone(),
                snapshot_integration_tag.clone(),
                unique_cals.clone(),
            ),
        ));
        *snapshot_ring_slot.lock().unwrap() = Some(snapshot_ring.clone());

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
        let mut engine_on = drive;
        let mut engine_level = level_dbfs;
        if drive {
            eng.set_pink(amplitude);
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
            pair_ctx,
            unique_ports.len(),
            chunk_secs,
            integration_tau_s,
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
                        &pub_tx,
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
                    &pub_tx,
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
                            unique_chans,
                            unique_ports,
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
            // dead-man above and after `set_drive`'s clamp to `drive_max_dbfs`
            // — not from what a client last asked for. A fault indicator fed by
            // commanded state would show `NO REFERENCE` while the daemon had
            // already dead-manned the drive, or show nothing while the drive
            // was live at a clamped level; reporting belief rather than
            // observation is the defect class #228 exists to make visible.
            //
            // `level_dbfs` is null while off, so there is no stale number to
            // misread, and carries the applied (clamped) value while on: drive
            // on but clamped to something inaudible is a real measurement with
            // a bad SNR, which is a different fault from either on or off.
            //
            // `drivable` distinguishes "this session could drive and is not"
            // (the indicator's idle row) from "this session never drives" —
            // an external-DUT session, where silence from the daemon says
            // nothing about whether signal is present.
            let drive_msg = json!({
                "on":         engine_on,
                "level_dbfs": if engine_on { json!(engine_level) } else { Value::Null },
                "drivable":   drivable,
            });

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
            snapshot_ring.lock().unwrap().delay_samples = session.delay_samples();

            for msg in messages {
                send_pub(&pub_tx, "data", &msg);
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

        eng.stop();
        *drive_state_slot.lock().unwrap() = None;
        *relock_state_slot.lock().unwrap() = None;

        // Known, bounded: a `set_drive` arriving between this worker's
        // last poll and the slot-clear below returns `{"ok":true}` for a
        // session that will not act on it — a few ms at teardown. Not
        // worth a lock redesign; the dead-man and the client's own
        // teardown both converge on silence regardless.
        //
        // Snapshot ring/spool lifecycle ends with the session (deliverable
        // 3's retention policy — module doc, `handlers/snapshot.rs`).
        *snapshot_ring_slot.lock().unwrap() = None;
        crate::handlers::snapshot::clear_spool(&snapshot_spool);

        send_pub(
            &pub_tx,
            "done",
            &json!({"cmd":"transfer_stream","stopped":true}),
        );
    });

    {
        let mut workers = state.workers.lock().unwrap();
        workers.insert("transfer_stream".to_string(), worker);
    }
    let mut reply = json!({
        "ok":           true,
        "out_port":     out_port_r,
        "meas_port":    meas_port_r,
        "ref_port":     ref_port_r,
        "pairs":        pairs_r,
        // Legacy fields — filled with the first pair so old clients keep working.
        "meas_channel": pairs_r.first().map(|p| p.0).unwrap_or(0),
        "ref_channel":  pairs_r.first().map(|p| p.1).unwrap_or(0),
    });
    // #225 migration notice — see `ref_output_migration_warning`. Repeated on
    // every launch reply rather than once, so a client that connects to an
    // already-running daemon still sees it.
    if let Some(w) = ref_output_migration_warning(&cfg) {
        reply["warnings"] = json!([w]);
    }
    reply
}

#[cfg(test)]
mod tests {
    use super::*;

    // The regression: a drivable session must connect its output even though
    // launch-time `drive` is false. Against the old `!drive` gate this is
    // empty, the generator plays onto an unconnected port, and the interface
    // is silent — invisible to every peak-based test.
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
}
