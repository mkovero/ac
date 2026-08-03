//! Reference-plane commands: H1 transfer estimation and channel probe.
//! Both depend on engine port routing and the configured reference channel.

use std::sync::atomic::Ordering;

use rayon::prelude::*;
use serde_json::{json, Value};

use ac_core::shared::calibration::Calibration;

use crate::audio::make_engine;
use crate::handlers::mic;
use crate::server::ServerState;

use super::{
    busy_guard, read_dmm_vrms, ref_output_migration_warning, resolve_output, resolve_ref_output,
    send_pub, spawn_worker,
};

/// Parse the `pairs` and legacy `meas_channel`/`ref_channel` shapes of
/// `transfer_stream` into a canonical pair list. Returns an Err message
/// suitable for `{"ok": false, "error": ...}` on malformed input.
fn parse_transfer_pairs(cmd: &Value) -> Result<Vec<(u32, u32)>, String> {
    if let Some(arr) = cmd.get("pairs").and_then(Value::as_array) {
        if arr.is_empty() {
            return Err("pairs is empty".into());
        }
        let mut out = Vec::with_capacity(arr.len());
        for (i, p) in arr.iter().enumerate() {
            let tuple = p
                .as_array()
                .ok_or_else(|| format!("pairs[{i}] must be [meas, ref]"))?;
            if tuple.len() != 2 {
                return Err(format!(
                    "pairs[{i}] must have exactly 2 elements, got {}",
                    tuple.len()
                ));
            }
            let m = tuple[0]
                .as_u64()
                .ok_or_else(|| format!("pairs[{i}][0] must be unsigned int"))?;
            let r = tuple[1]
                .as_u64()
                .ok_or_else(|| format!("pairs[{i}][1] must be unsigned int"))?;
            out.push((m as u32, r as u32));
        }
        // De-dup identical pairs — harmless but wasteful to publish twice.
        out.sort_unstable();
        out.dedup();
        return Ok(out);
    }
    // Legacy single-pair form.
    let m = cmd
        .get("meas_channel")
        .and_then(Value::as_u64)
        .ok_or_else(|| "meas_channel required (or use pairs=[[m,r], ...])".to_string())?;
    let r = cmd
        .get("ref_channel")
        .and_then(Value::as_u64)
        .ok_or_else(|| "ref_channel required (or use pairs=[[m,r], ...])".to_string())?;
    Ok(vec![(m as u32, r as u32)])
}

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
fn drive_out_ports(drivable: bool, out_port: &str, ref_out_port: &str) -> Vec<String> {
    if !drivable {
        Vec::new()
    } else if ref_out_port != out_port {
        vec![out_port.to_string(), ref_out_port.to_string()]
    } else {
        vec![out_port.to_string()]
    }
}

/// `set_drive` (§4.3) — start, stop, or re-level the stimulus of a
/// running `transfer_stream` session.
///
/// Dispatched like `snapshot`: a CTRL command that targets a live worker
/// without spawning one, so it has no `cmd_group` entry and never
/// consults `check_busy`. That is not an exception carved out for it —
/// routing it through the busy guard would make it contend with the very
/// `Group::Transfer` worker it targets, and since this is also the
/// command that STOPS the drive, the contention would land on the
/// panic-stop path.
///
/// `level_dbfs` is required on every request, including `on: false`:
/// every message doubles as the keepalive, so every message is a full
/// state assertion rather than a delta against state the server would
/// otherwise have to remember.
/// `20·log10(max|sample|)` over one capture block, or `None` for
/// digital silence (which would be `-inf`, unrepresentable in JSON).
///
/// Takes raw capture samples. There is no calibrated variant of this on
/// purpose: a voltage-calibrated frame must not move the input meters.
fn raw_peak_dbfs(block: &[f32]) -> Option<f64> {
    let peak = block.iter().fold(0.0f32, |acc, &s| acc.max(s.abs()));
    if peak <= 0.0 {
        return None;
    }
    Some(20.0 * (peak as f64).log10())
}

pub fn set_drive(state: &ServerState, cmd: &Value) -> Value {
    let drive = {
        let slot = state.drive_state.lock().unwrap();
        match slot.as_ref() {
            Some(d) => d.clone(),
            None => return json!({"ok": false, "error": "no transfer_stream session running"}),
        }
    };

    let on = match cmd.get("on").and_then(Value::as_bool) {
        Some(v) => v,
        None => return json!({"ok": false, "error": "'on' required (bool)"}),
    };
    // A missing or non-finite level is a client bug. Coercing it would
    // hide that, and this is the one command where a silently
    // substituted number reaches a loudspeaker.
    let level = match cmd.get("level_dbfs").and_then(Value::as_f64) {
        Some(v) if v.is_finite() => v,
        _ => return json!({"ok": false, "error": "'level_dbfs' required (finite number)"}),
    };

    let ceiling = state.cfg.lock().unwrap().drive_max_dbfs;
    // Clamping is normal operation, not an error: a stimulus command
    // that fails instead of applying a safe level is a worse field
    // failure than one that quietly applies the ceiling. The echo below
    // is always the APPLIED value, so the client can see what happened.
    let applied = level.min(ceiling);
    drive.set(on, applied);

    json!({"ok": true, "on": on, "level_dbfs": applied})
}

pub fn transfer_stream(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "transfer_stream");

    // `drive` controls whether the daemon plays pink noise on the output
    // while capturing. Default `false` — the UI wants a purely passive H1
    // estimate against whatever the user is already driving into the inputs.
    // Set `true` to restore the old self-driving behavior (with `level_dbfs`
    // controlling amplitude).
    let drive = cmd.get("drive").and_then(Value::as_bool).unwrap_or(false);
    // `drivable` opens and connects the output ports at launch while staying
    // silent — the shape `ac transfer` needs, where drive arrives later via
    // `set_drive`. Legacy `drive=true` implies drivable. Neither means fully
    // passive: no output ports at all (external-DUT workflow).
    let drivable = drive
        || cmd
            .get("drivable")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    let level_dbfs = cmd
        .get("level_dbfs")
        .and_then(Value::as_f64)
        .unwrap_or(-10.0);

    // Fake-audio-only stimulus knob (handoff: parity-completion M1.5),
    // same pattern as `monitor_spectrum`'s `fake_tones`/`fake_noise_dbfs`
    // (`handlers/audio/monitor.rs`): read unconditionally here, applied
    // only inside `if fake { ... }` below — real backends never see it.
    let fake_correlated_pair: Option<(f64, usize)> =
        cmd.get("fake_correlated_pair").and_then(|v| {
            let gain = v.get("gain").and_then(Value::as_f64)?;
            let delay_samples = v.get("delay_samples").and_then(Value::as_u64)?;
            Some((gain, delay_samples as usize))
        });

    // Fake-audio-only capture knob (handoff-capture-contiguity D1): route
    // fake capture through a real ring so `clear()`-before-wait has the same
    // observable consequence it has on JACK. Presence of the key selects the
    // mode; absence leaves the on-demand generator in place, unchanged.
    //
    // `process_secs` defaults to the transfer worker's own measured per-tick
    // compute (~5 ms with the delay cached — see the hot-loop note further
    // down), so an unparameterised run models this worker rather than an
    // arbitrary gap.
    let fake_ring_process_secs: Option<f64> = cmd.get("fake_ring").map(|v| {
        v.get("process_secs")
            .and_then(Value::as_f64)
            .unwrap_or(0.005)
    });
    // Producer granularity. A real backend hands the ring one whole period at
    // a time, and that quantisation — not the gap length — is what decides
    // which stimulus frequencies expose the splice at all: a tone at an exact
    // multiple of `sr/period` survives a discarded period with zero phase
    // error. 1024 matches the verified rig (RME Babyface Pro at 96 kHz).
    let fake_ring_period: usize = cmd
        .get("fake_ring")
        .and_then(|v| v.get("period"))
        .and_then(Value::as_u64)
        .unwrap_or(1024) as usize;

    // Multi-time-window ladder parameters. Display density is a **parameter**,
    // not a constant — where it exceeds what a stage can resolve the column
    // grid widens rather than interpolating, so asking for more here buys
    // resolution only where the ladder can back it. It does not move the
    // ladder's crossovers, which are anchored to `mtw::ladder::P_REF`.
    let mtw_ppo: f64 = cmd
        .get("mtw_ppo")
        .and_then(Value::as_f64)
        .filter(|v| *v > 0.0 && *v <= 384.0)
        .unwrap_or(ac_core::visualize::mtw::ladder::P_REF);
    // Blocks averaged per stage, held *uniform across stages* so the coherence
    // bias `1/N` is the same either side of every crossover. A uniform
    // wall-clock time constant would instead give ~47 averages at stage 0
    // against ~1.5 at stage 2, putting a fixed-frequency step in the trust
    // indicator that reads as a property of the DUT.
    //
    // Lowering it speeds the bottom stage up but raises the coherence floor
    // across the whole display, since N is uniform: 0.33 at N = 3, 0.50 at
    // N = 2, and at 0.50 a coherence reading has stopped meaning anything.
    let mtw_n_blocks: usize = cmd
        .get("mtw_n_blocks")
        .and_then(Value::as_u64)
        .filter(|v| *v >= 1 && *v <= 64)
        .map(|v| v as usize)
        .unwrap_or(ac_core::visualize::mtw::average::DEFAULT_N_BLOCKS);

    let pairs = match parse_transfer_pairs(cmd) {
        Ok(p) => p,
        Err(e) => return json!({"ok": false, "error": e}),
    };

    // Per-meas-channel SPL session params (D10 — static for the session,
    // set once here, not live-toggleable). `weighting`'s wire contract
    // (handoff: transfer-frame-v2 M0) is a strict 3-way A/C/Z — no
    // "off" — so `WeightingCurve::from_tag`'s existing rejection of
    // anything else (including "off") is exactly the validation wanted.
    let weighting_tag = cmd.get("weighting").and_then(Value::as_str).unwrap_or("Z");
    let weighting = match ac_core::visualize::weighting_curves::WeightingCurve::from_tag(
        weighting_tag,
    ) {
        Some(w) => w,
        None => {
            return json!({"ok": false, "error": format!("weighting must be one of A, C, Z, got '{weighting_tag}'")})
        }
    };
    let integration_tag = cmd
        .get("integration")
        .and_then(Value::as_str)
        .unwrap_or("fast")
        .to_ascii_lowercase();
    let integration_tau_s = match integration_tag.as_str() {
        "fast" => ac_core::visualize::time_integration::TAU_FAST_S,
        "slow" => ac_core::visualize::time_integration::TAU_SLOW_S,
        _ => {
            return json!({"ok": false, "error": format!("integration must be 'fast' or 'slow', got '{integration_tag}'")})
        }
    };

    let cfg = state.cfg.lock().unwrap().clone();
    let capture_ports = super::cached_capture_ports(state);

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

    // Per-pair buffer indices into the `Vec<Vec<f32>>` that `capture_multi`
    // returns. Precomputed so the worker loop doesn't re-scan per iteration.
    let pair_idx: Vec<(usize, usize)> = pairs
        .iter()
        .map(|&(m, r)| {
            let mi = unique_chans.iter().position(|&c| c == m).unwrap();
            let ri = unique_chans.iter().position(|&c| c == r).unwrap();
            (mi, ri)
        })
        .collect();

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

    // Calibration discipline (#101): a mic-curve on the **reference**
    // channel is a misconfiguration — H1 = Y/X is a ratio, applying a
    // mic correction to the reference leg cancels against the meas-leg
    // correction (or worse, biases it the wrong way when only one leg
    // has a curve). Refuse the request synchronously with a clear
    // message so the user knows to clear the curve or swap channels.
    let out_ch = cfg.output_channel;
    for &(_, ref_ch) in &pairs {
        if let Ok(Some(cal)) = Calibration::load(out_ch, ref_ch, None) {
            if cal.mic_response.is_some() {
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

    // Per-pair measurement-channel mic-curves. Loaded once at worker
    // start; ref-channel curves were already refused above.
    let pair_meas_curves: Vec<Option<ac_core::shared::calibration::MicResponse>> = pairs
        .iter()
        .map(|&(meas, _)| {
            Calibration::load(out_ch, meas, None)
                .ok()
                .flatten()
                .and_then(|c| c.mic_response.clone())
        })
        .collect();

    // Full per-pair calibration (voltage / SPL / mic-curve) for
    // `meas_spectrum`/`ref_spectrum`/`spl`/`cal_tags` (handoff:
    // transfer-frame-v2 M0). Kept side by side with `pair_meas_curves`
    // above (mic_response only, feeds the existing mag/phase/re/im
    // correction) rather than merged, so that path stays untouched
    // (additive-only discipline).
    let pair_meas_cals: Vec<Option<Calibration>> = pairs
        .iter()
        .map(|&(meas, _)| Calibration::load(out_ch, meas, None).ok().flatten())
        .collect();
    let pair_ref_cals: Vec<Option<Calibration>> = pairs
        .iter()
        .map(|&(_, r)| Calibration::load(out_ch, r, None).ok().flatten())
        .collect();

    // Snapshot ring (handoff: snapshot-backend M1). Per-unique-channel
    // calibration (not per-pair — a channel used in >1 pair gets one
    // entry here) for `.acsnap` provenance; the ring itself is built
    // inside the worker once `sr` is known.
    let unique_cals: Vec<Option<Calibration>> = unique_chans
        .iter()
        .map(|&ch| Calibration::load(out_ch, ch, None).ok().flatten())
        .collect();
    let snapshot_ring_slot = state.snapshot_ring.clone();
    let drive_state_slot = state.drive_state.clone();
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

    let worker = spawn_worker(state, "transfer_stream", move |stop| {
        let amplitude = ac_core::shared::generator::dbfs_to_amplitude(level_dbfs);
        let drive_state = drive_state_for_worker;

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

        // Sliding window: keep the last `target_total` samples per unique
        // channel and recompute H1 every `chunk_secs`. nperseg/step mirror
        // h1_estimate's internal Welch settings. chunk_secs matches the
        // monitor tick default (50 ms → 20 Hz refresh) — now that per-tick
        // compute is ~5 ms (delay cached), the loop is capture-bound, not
        // compute-bound, and the view matches the FFT/CWT monitor cadence.
        let nperseg = sr as usize; // 1 Hz bin width
        let step = nperseg / 2; // 50% overlap
        let n_averages = 4;
        let target_total = nperseg + step * (n_averages - 1);
        let chunk_secs = 0.05;
        let mut rings: Vec<Vec<f32>> = (0..unique_ports.len())
            .map(|_| Vec::with_capacity(target_total + step))
            .collect();

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

        // Delay cache: ref↔meas propagation is constant during a streaming
        // session (fixed hardware path), so we estimate once per pair on
        // warmup and reuse the result. Skipping `estimate_delay` per tick
        // (a 262 k-point FFT+IFFT at 2.5 s ring / 48 kHz) cuts the hot-loop
        // work from ~17 ms → ~3 ms and takes the refresh rate from choppy
        // ~8.5 Hz to the capture-interval-limited ~10 Hz.
        let mut pair_delays: Vec<Option<i64>> = vec![None; pairs.len()];

        // A pair whose delay estimate was *refused* (no prominent correlation
        // peak — #227) stays unlocked and is retried, because the cause is
        // usually transient from the software's point of view: an unpatched
        // reference leg or a muted source that the operator then fixes. Retry
        // is rate-limited because each attempt is the same full-ring
        // FFT+IFFT the cache above exists to avoid, and the inputs it reads
        // only turn over on the ring's own timescale.
        const RELOCK_RETRY: std::time::Duration = std::time::Duration::from_secs(1);
        let mut next_delay_attempt: Vec<Option<std::time::Instant>> = vec![None; pairs.len()];

        // Peak-to-median prominence from the most recent attempt, locked or
        // refused. Published so a session that never locks still says how far
        // short it fell — the estimator's one empirical constant is set from
        // this distribution, and a bare "refused" would not measure it.
        let mut pair_prominence: Vec<Option<serde_json::Value>> = vec![None; pairs.len()];

        // Per-pair `spl` time-integration state (F/S EMA, n_bands=1 —
        // handoff: transfer-frame-v2 M0). `None` for a pair whose meas
        // channel has no SPL calibration layer; `spl` stays `null` for
        // that pair's whole session, matching `spl_offsets` in
        // `monitor.rs`. Session-static per D10, so built once here
        // rather than re-checked per tick.
        let mut spl_integrators: Vec<Option<ac_core::visualize::time_integration::EmaIntegrator>> =
            pair_meas_cals
                .iter()
                .map(|c| {
                    c.as_ref().and_then(Calibration::spl_offset_db).map(|_| {
                        ac_core::visualize::time_integration::EmaIntegrator::new(
                            integration_tau_s,
                            1,
                        )
                    })
                })
                .collect();
        let mut spl_last_ts: Vec<Option<std::time::Instant>> = vec![None; pairs.len()];

        // Drain telemetry (#208 D1). Off unless `AC_DRAIN_TELEMETRY` is set:
        // this slice adds no behaviour, and a streaming session must log
        // nothing new by default.
        let mut drain_telemetry = crate::audio::drain_telemetry::DrainTelemetry::from_env(sr);

        // Multi-time-window ladder, one per pair.
        //
        // Purely **additive**: it runs alongside the full-rate Welch estimator
        // above and replaces nothing. That is not caution, it is required —
        // `spl` is derived from the same `gyy` the Welch path produces and has
        // to stay bit-identical, and `meas_spectrum` / `ref_spectrum` are
        // calibrated absolute levels, which `Gxy/Gxx`'s cancellation of
        // `|Hdec|²` does not cover (see `visualize::mtw`'s fence).
        //
        // Fed the fresh per-tick `bufs`, never the `rings` sliding window: the
        // ladder is a push pipeline, and pushing a re-segmented sliding buffer
        // into it would reproduce #208's re-analysis one level down.
        let mut mtw: Vec<Option<ac_core::visualize::mtw::MtwPair>> =
            (0..pairs.len()).map(|_| None).collect();
        let mut mtw_failed = false;

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
            if want_on != engine_on || (want_on && want_level != engine_level) {
                if want_on {
                    eng.set_pink(ac_core::shared::generator::dbfs_to_amplitude(want_level));
                } else {
                    eng.set_silence();
                }
                engine_on = want_on;
                engine_level = want_level;
            }

            // Raw capture peaks (§4.2), per unique-port index, from
            // THIS tick's blocks — before any calibration, weighting, or
            // aggregation. Deliberately not derived from `rings` (a
            // multi-segment window, not the frame's blocks) and not from
            // `TransferResult`'s `meas_amp`/`ref_amp` (window-normalised
            // and calibration-adjacent). The meters exist to judge gain
            // staging, and a calibrated or band-aggregated value hides
            // clipping — which is the one thing they must never do.
            let tick_peaks_dbfs: Vec<Option<f64>> = bufs.iter().map(|b| raw_peak_dbfs(b)).collect();

            // Feed the snapshot ring the same raw, pre-processing `bufs`
            // the H1 sliding window below derives from — same capture,
            // second (larger, longer-retention) consumer.
            snapshot_ring.lock().unwrap().push_tick(&bufs);

            for (i, buf) in bufs.iter().enumerate() {
                if i >= rings.len() {
                    break;
                }
                let r = &mut rings[i];
                r.extend_from_slice(buf);
                if r.len() > target_total {
                    let drop = r.len() - target_total;
                    r.drain(..drop);
                }
            }

            // Warm-up: wait for the rings to fill to at least one Welch
            // segment so the first H1 has meaningful coherence.
            if rings.iter().any(|r| r.len() < nperseg) {
                continue;
            }

            // Estimate any missing per-pair delays once on first entry (after
            // the rings are full). Subsequent ticks reuse the cached value.
            for i in 0..pairs.len() {
                if pair_delays[i].is_some() {
                    continue;
                }
                let now = std::time::Instant::now();
                if next_delay_attempt[i].is_some_and(|t| now < t) {
                    continue;
                }
                let (mi, ri) = pair_idx[i];
                if let (Some(meas), Some(refb)) = (rings.get(mi), rings.get(ri)) {
                    let est = ac_core::visualize::transfer::estimate_delay_detailed(
                        refb.as_slice(),
                        meas.as_slice(),
                        sr,
                    );
                    pair_delays[i] = est.lag;
                    // Full lock evidence, not just the ratio: the competing
                    // peaks are what make DIRECT_PEAK_FRACTION settleable
                    // offline, and they cannot be reconstructed from a
                    // finished session.
                    pair_prominence[i] = Some(json!({
                        "prominence":   est.prominence,
                        "peak_lag":     est.peak_lag,
                        "peak_value":   est.peak_value,
                        "median_value": est.median_value,
                        "candidates":   est.candidates.iter()
                            .map(|c| json!({"lag": c.lag, "value": c.value}))
                            .collect::<Vec<_>>(),
                    }));
                    if est.lag.is_none() {
                        next_delay_attempt[i] = Some(now + RELOCK_RETRY);
                    }
                }
            }
            // Keep the ring's copy in sync — cheap (small Vec), and
            // simpler than trying to push incrementally from inside the
            // loop above.
            snapshot_ring.lock().unwrap().delay_samples = pair_delays.clone();

            // Build each pair's ladder once its alignment offset is known —
            // the offset is applied at full rate, before decimation, so it has
            // to exist before the first sample enters.
            for (i, slot) in mtw.iter_mut().enumerate() {
                if slot.is_some() || mtw_failed {
                    continue;
                }
                let Some(delay) = pair_delays[i] else {
                    continue;
                };
                match ac_core::visualize::mtw::MtwPair::new(sr, delay, mtw_n_blocks) {
                    Ok(p) => *slot = Some(p),
                    Err(e) => {
                        // The ladder is additive, so a layout it cannot serve
                        // (an unsupported rate) must degrade to "no ladder",
                        // not to a dead session. Logged once.
                        eprintln!("transfer_stream: MTW ladder unavailable at {sr} Hz: {e}");
                        mtw_failed = true;
                    }
                }
            }

            // Push this tick into every ladder and assemble its columns.
            // Sequential rather than folded into the per-pair rayon fan-out
            // below: the ladders are `&mut` and the fan-out borrows `rings`
            // immutably, and one 4096-point FFT pair per stage per tick is not
            // what makes this loop expensive.
            let mtw_columns: Vec<Option<Vec<ac_core::visualize::mtw::splice::Column>>> = mtw
                .iter_mut()
                .enumerate()
                .map(|(i, slot)| {
                    let p = slot.as_mut()?;
                    let (mi, ri) = pair_idx[i];
                    let meas = bufs.get(mi)?;
                    let refb = bufs.get(ri)?;
                    p.push(meas, refb);
                    p.columns(spec_f_min, spec_f_max, mtw_ppo)
                })
                .collect();
            // Sampled after the push, so it describes the frame being built.
            let mtw_settled: Vec<Vec<bool>> = mtw
                .iter()
                .map(|slot| {
                    slot.as_ref()
                        .map(|p| p.settled_stages())
                        .unwrap_or_default()
                })
                .collect();

            // Pairs are independent H1 estimates — fan out across the rayon
            // pool so multi-pair sessions (e.g. 4 mic positions against one
            // reference) scale linearly with core count. The rings are
            // read-only inside the per-pair closure; JSON is built on the
            // worker thread and published back in original pair order.
            let mc_enabled = mic_corr_enabled.load(Ordering::Relaxed);
            // Each pair can contribute multiple wire messages (the
            // transfer_stream frame + a Phase 4b visualize/ir sidecar
            // computed from the same H₁ result). flat_map flattens to
            // one Vec<Value> per filter_map; overall Vec<Vec<Value>>
            // is then chained into the per-message send loop below.
            let messages: Vec<(usize, Vec<Value>, Option<f64>)> = pairs
                .par_iter()
                .enumerate()
                .zip(pair_idx.par_iter())
                .zip(pair_delays.par_iter())
                .zip(pair_prominence.par_iter())
                .zip(pair_meas_curves.par_iter())
                .zip(pair_meas_cals.par_iter())
                .zip(pair_ref_cals.par_iter())
                .filter_map(
                    |(((((((pos, &(meas_ch, ref_ch)), &(mi, ri)), &delay_opt), prom_opt), curve_opt), meas_cal_opt), ref_cal_opt)| {
                        let meas = rings.get(mi)?.as_slice();
                        let refb = rings.get(ri)?.as_slice();
                        // `-inf` (digital silence) travels as JSON null:
                        // serde_json cannot serialise a non-finite float,
                        // so the conversion is explicit here rather than
                        // left to the `json!` site.
                        let meas_peak: Value = tick_peaks_dbfs
                            .get(mi)
                            .copied()
                            .flatten()
                            .map(Value::from)
                            .unwrap_or(Value::Null);
                        let ref_peak: Value = tick_peaks_dbfs
                            .get(ri)
                            .copied()
                            .flatten()
                            .map(Value::from)
                            .unwrap_or(Value::Null);
                        // An unlocked pair (still warming up, or refused by the
                        // prominence gate — #227) is measured unaligned rather
                        // than aligned to a guess. `delay_locked` below is what
                        // keeps that distinguishable on the wire: a refused pair
                        // and a genuine 0-sample digital loopback both report
                        // `delay_ms` 0.0, and #216 established that the loopback
                        // case is legitimately 0.0, so the number alone cannot
                        // carry the difference.
                        let delay = delay_opt.unwrap_or(0);
                        let result = ac_core::visualize::transfer::h1_estimate_with_delay(
                            refb, meas, sr, delay,
                        );

                        let n_pts = result.freqs.len();
                        let indices: Vec<usize> = if n_pts > 2000 {
                            let mut idx: Vec<usize> = (0..2000)
                                .map(|i| (i as f64 * (n_pts - 1) as f64 / 1999.0).round() as usize)
                                .collect();
                            idx.dedup();
                            idx
                        } else {
                            (0..n_pts).collect()
                        };

                        let freqs = indices.iter().map(|&i| result.freqs[i]).collect::<Vec<_>>();
                        let mut mag = indices
                            .iter()
                            .map(|&i| result.magnitude_db[i])
                            .collect::<Vec<_>>();
                        let phase = indices
                            .iter()
                            .map(|&i| result.phase_deg[i])
                            .collect::<Vec<_>>();
                        let coh = indices
                            .iter()
                            .map(|&i| result.coherence[i])
                            .collect::<Vec<_>>();
                        // unified.md Phase 3: complex H downsampled in
                        // lockstep so Tier 2 views (Nyquist, IR-via-IFFT,
                        // group-delay-from-complex) get H(ω) directly
                        // without re-deriving from mag/phase. Same `indices`
                        // → guaranteed consistent with mag/phase/coherence.
                        let mut re = indices.iter().map(|&i| result.re[i]).collect::<Vec<_>>();
                        let mut im = indices.iter().map(|&i| result.im[i]).collect::<Vec<_>>();

                        // Mic-curve correction (#101) on the measurement leg
                        // only — the reference leg was guarded above. H1's
                        // dB magnitude has the mic over-read embedded; subtract
                        // the curve at each downsampled bin to recover truth.
                        // Same correction applied multiplicatively to (re, im)
                        // — the curve is magnitude-only (no phase change), so
                        // scaling both re and im by 10^(-curve_db/20)
                        // preserves arg(H) while shrinking |H| consistently.
                        if mc_enabled {
                            if let Some(curve) = curve_opt.as_ref() {
                                mic::apply_mic_curve_inplace_f64(curve, &freqs, &mut mag);
                                for ((r, i), &f) in
                                    re.iter_mut().zip(im.iter_mut()).zip(freqs.iter())
                                {
                                    let corr_db = curve.correction_at(f as f32) as f64;
                                    let scale = 10.0_f64.powf(-corr_db / 20.0);
                                    *r *= scale;
                                    *i *= scale;
                                }
                            }
                        }
                        let mc_tag = mic::mic_correction_tag(curve_opt.is_some(), mc_enabled);

                        // Calibrated per-channel spectra (D18, handoff:
                        // transfer-frame-v2 M0). Full-resolution
                        // `result.meas_amp`/`result.ref_amp` (NOT the
                        // 2000-pt indices above — same full-res-then-
                        // aggregate split the IR sidecar below already
                        // uses) so the mic-curve's per-freq correction is
                        // applied at native Welch-bin resolution, then
                        // `spectrum_to_columns_wire` band-power-aggregates
                        // to the fixed grid — same aggregator, same tests,
                        // the monitor `spectrum` frame already uses.
                        let mut mc_meas_amp = result.meas_amp.clone();
                        if mc_enabled {
                            if let Some(curve) = curve_opt.as_ref() {
                                for (amp, &f) in
                                    mc_meas_amp.iter_mut().zip(result.freqs.iter())
                                {
                                    let corr_db = curve.correction_at(f as f32) as f64;
                                    *amp *= 10.0_f64.powf(-corr_db / 20.0);
                                }
                            }
                        }
                        // `spl` is computed from the mic-corrected (acoustic
                        // truth) spectrum, before voltage-cal scaling below
                        // — SPL derives from the dBFS + spl_offset_db model
                        // (Calibration::spl_offset_db), independent of the
                        // electrical Vrms voltage-cal layer.
                        let spl_raw: Option<f64> = meas_cal_opt
                            .as_ref()
                            .and_then(Calibration::spl_offset_db)
                            .map(|offset| {
                                ac_core::visualize::spl_level::weighted_broadband_dbfs(
                                    &mc_meas_amp,
                                    &result.freqs,
                                    weighting,
                                ) + offset
                            });

                        // Voltage cal (D3): a constant per-channel scale
                        // factor, so it commutes with column aggregation
                        // (`sqrt(Σ(c·x)²) = c·sqrt(Σx²)`) — applied here,
                        // post mic-curve, pre-aggregation.
                        let meas_amp_wire = {
                            let mut a = mc_meas_amp;
                            if let Some(scale) =
                                meas_cal_opt.as_ref().and_then(|c| c.vrms_at_0dbfs_in)
                            {
                                for v in a.iter_mut() {
                                    *v *= scale;
                                }
                            }
                            a
                        };
                        let ref_amp_wire = {
                            let mut a = result.ref_amp.clone();
                            if let Some(scale) =
                                ref_cal_opt.as_ref().and_then(|c| c.vrms_at_0dbfs_in)
                            {
                                for v in a.iter_mut() {
                                    *v *= scale;
                                }
                            }
                            a
                        };
                        let (meas_spectrum, spec_freqs) =
                            ac_core::visualize::aggregate::spectrum_to_columns_wire(
                                &meas_amp_wire,
                                sr as f64,
                                spec_f_min,
                                spec_f_max,
                                spec_n_columns,
                            );
                        let (ref_spectrum, _) =
                            ac_core::visualize::aggregate::spectrum_to_columns_wire(
                                &ref_amp_wire,
                                sr as f64,
                                spec_f_min,
                                spec_f_max,
                                spec_n_columns,
                            );

                        // Per-channel provenance tags (tier-framing
                        // labelled-tag rules, #97/#98 vocabulary):
                        // "on"/"none" for voltage and SPL (no daemon-side
                        // enable toggle for either, unlike mic-curve);
                        // "on"/"off"/"none" for mic_curve via the existing
                        // `mic_correction_tag`. Ref leg's mic_curve is
                        // structurally always "none" — a ref-channel mic
                        // curve is refused at request time, above.
                        let ref_curve_loaded = ref_cal_opt
                            .as_ref()
                            .is_some_and(|c| c.mic_response.is_some());
                        let cal_tags = json!({
                            "meas": {
                                "voltage": if meas_cal_opt.as_ref().and_then(|c| c.vrms_at_0dbfs_in).is_some() { "on" } else { "none" },
                                "spl":     if meas_cal_opt.as_ref().and_then(Calibration::spl_offset_db).is_some() { "on" } else { "none" },
                                "mic_curve": mc_tag,
                            },
                            "ref": {
                                "voltage": if ref_cal_opt.as_ref().and_then(|c| c.vrms_at_0dbfs_in).is_some() { "on" } else { "none" },
                                "spl":     if ref_cal_opt.as_ref().and_then(Calibration::spl_offset_db).is_some() { "on" } else { "none" },
                                "mic_curve": mic::mic_correction_tag(ref_curve_loaded, mc_enabled),
                            },
                        });

                        // Multi-time-window columns (additive; `null` until
                        // every rung holds a full N blocks — 2.56 s at the
                        // bottom, which is the design's stated settling time
                        // and matches what the full-rate estimator takes
                        // today. Gating on the full N is what makes the
                        // reported N unambiguous: every column is the mean of
                        // the same number of blocks).
                        //
                        // Every column ships the Δf, window and N that
                        // produced it. That is not decoration: neighbouring
                        // columns can come from windows 12x apart, and
                        // coherence from uncorrelated inputs floats near 1/N,
                        // so without those a screenshot of this display is not
                        // interpretable. `bins` is criterion 1 made
                        // observable — it is never zero.
                        //
                        // dB is applied here, daemon-side, per the display-
                        // truth rule: `ac-view` plots what it is given and
                        // does no `log10` of its own.
                        let mtw_msg = match mtw_columns.get(pos).and_then(|c| c.as_ref()) {
                            None => Value::Null,
                            Some(cols) => json!({
                                "freqs":        cols.iter().map(|c| c.freq).collect::<Vec<_>>(),
                                "f_lo":         cols.iter().map(|c| c.lo).collect::<Vec<_>>(),
                                "f_hi":         cols.iter().map(|c| c.hi).collect::<Vec<_>>(),
                                "magnitude_db": cols.iter()
                                    .map(|c| 20.0 * c.h1.norm().max(1e-6).log10())
                                    .collect::<Vec<_>>(),
                                "phase_deg":    cols.iter()
                                    .map(|c| c.h1.arg().to_degrees())
                                    .collect::<Vec<_>>(),
                                "coherence":    cols.iter().map(|c| c.coherence).collect::<Vec<_>>(),
                                "df":           cols.iter().map(|c| c.df).collect::<Vec<_>>(),
                                "window_s":     cols.iter().map(|c| c.window_s).collect::<Vec<_>>(),
                                "n":            cols.iter().map(|c| c.n).collect::<Vec<_>>(),
                                "stage":        cols.iter().map(|c| c.stage).collect::<Vec<_>>(),
                                "blend":        cols.iter().map(|c| c.blend).collect::<Vec<_>>(),
                                "bins":         cols.iter().map(|c| c.bins).collect::<Vec<_>>(),
                                "ppo":          mtw_ppo,
                                "n_blocks":     mtw_n_blocks,
                                // Which rungs have settled, shallowest first.
                                // Shipped so a consumer can distinguish "still
                                // warming, more band coming" from "this is all
                                // there is" — a short column list looks the
                                // same either way, and the difference decides
                                // whether a blank low end is a fault.
                                "settled_stages": mtw_settled
                                    .get(pos)
                                    .cloned()
                                    .unwrap_or_default(),
                                "stages":       mtw_stages,
                            }),
                        };

                        let transfer_msg = json!({
                            "type":            "transfer_stream",
                            "cmd":             "transfer_stream",
                            "mtw":             mtw_msg,
                            "freqs":           freqs,
                            "magnitude_db":    mag,
                            "phase_deg":       phase,
                            "coherence":       coh,
                            "re":              re,
                            "im":              im,
                            "delay_samples":   result.delay_samples,
                            "delay_ms":        result.delay_ms,
                            "delay_locked":    delay_opt.is_some(),
                            "delay_evidence":  prom_opt,
                            "meas_peak_dbfs":  meas_peak,
                            "ref_peak_dbfs":   ref_peak,
                            "ref_channel":     ref_ch,
                            "meas_channel":    meas_ch,
                            "sr":              sr,
                            "mic_correction":  mc_tag,
                            "spec_freqs":      spec_freqs,
                            "meas_spectrum":   meas_spectrum,
                            "ref_spectrum":    ref_spectrum,
                            "spl":             Value::Null,
                            "spl_weighting":   weighting.tag(),
                            "spl_integration": integration_tag.as_str(),
                            "cal_tags":        cal_tags,
                        });
                        // Phase 4b: IR sidecar from full-resolution
                        // complex H (NOT the downsampled re/im above —
                        // the IFFT needs the raw nperseg/2+1 bins to
                        // recover h(t) correctly). Time-domain output is
                        // 1 s long at sr (matches the 1 Hz Welch
                        // resolution); downsample to ≤2000 samples for
                        // wire economy by stride-picking. Mic-curve
                        // correction is intentionally NOT applied to the
                        // IR — the downsampled re/im version already
                        // would have it, but the full-resolution H here
                        // does not (mic-curve correction in the curve_opt
                        // branch above only touches the downsampled mag /
                        // re / im). For the visualization-only Tier 2 IR
                        // view this is acceptable; if a Tier-1 calibrated
                        // IR is wanted, that path goes through the sweep
                        // measurement, not transfer_stream.
                        let h_full_re = &result.re;
                        let h_full_im = &result.im;
                        let ir_full = ac_core::visualize::transfer::impulse_response_from_h(
                            h_full_re, h_full_im,
                        );
                        let ir_msg = if ir_full.is_empty() {
                            None
                        } else {
                            const IR_MAX_SAMPLES: usize = 2000;
                            let stride = (ir_full.len() / IR_MAX_SAMPLES).max(1);
                            let ir_ds: Vec<f32> = ir_full.iter().step_by(stride).copied().collect();
                            // t_origin_ms = -mid_ms because
                            // impulse_response_from_h centres the IR
                            // peak at the middle of the array (t=0 in
                            // the user's mental model).
                            let dt_ms = 1000.0 / sr as f64 * stride as f64;
                            let t_origin_ms = -((ir_ds.len() / 2) as f64) * dt_ms;
                            Some(json!({
                                "type":         "visualize/ir",
                                "cmd":          "transfer_stream",
                                "samples":      ir_ds,
                                "sr":           sr,
                                "stride":       stride,
                                "dt_ms":        dt_ms,
                                "t_origin_ms":  t_origin_ms,
                                "ref_channel":  ref_ch,
                                "meas_channel": meas_ch,
                                "delay_samples": result.delay_samples,
                                "delay_ms":     result.delay_ms,
                                "delay_locked": delay_opt.is_some(),
                            }))
                        };
                        let mut out = vec![transfer_msg];
                        if let Some(m) = ir_msg {
                            out.push(m);
                        }
                        Some((pos, out, spl_raw))
                    },
                )
                .collect();
            for (pos, mut batch, spl_raw) in messages {
                // Sequential, indexed by original pair position — the
                // EMA integrator holds mutable per-pair state that can't
                // safely live inside the parallel closure above (#pos is
                // preserved through `filter_map` via `enumerate()` at the
                // top of the chain, not the post-filter Vec position).
                if let (Some(raw), Some(integ)) = (spl_raw, spl_integrators[pos].as_mut()) {
                    let now = std::time::Instant::now();
                    let dt = spl_last_ts[pos]
                        .map(|t| now.duration_since(t).as_secs_f64())
                        .unwrap_or(chunk_secs)
                        .max(1e-6);
                    spl_last_ts[pos] = Some(now);
                    let integrated = integ.update(&[raw], dt)[0];
                    if let Some(first) = batch.first_mut() {
                        first["spl"] = json!(integrated);
                    }
                }
                for msg in batch {
                    send_pub(&pub_tx, "data", &msg);
                }
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

pub fn probe(state: &ServerState, _cmd: &Value) -> Value {
    busy_guard!(state, "probe");

    let fake = state.fake_audio;
    let pub_tx = state.pub_tx.clone();
    let cfg = state.cfg.lock().unwrap().clone();
    let dmm_host = cfg.dmm_host.clone();

    let (playback, capture) = (
        super::cached_playback_ports(state),
        super::cached_capture_ports(state),
    );
    let n_play = playback.len();
    let n_cap = capture.len();

    let worker = spawn_worker(state, "probe", move |stop| {
        let threshold_rms: f64 = 0.010 / (2.0f64.sqrt()); // 10 mVrms ≈ this linear RMS

        let freq = 1000.0;
        let amplitude = ac_core::shared::generator::dbfs_to_amplitude(-10.0);

        let mut eng = make_engine(fake);
        if !eng.supports_routing() {
            send_pub(
                &pub_tx,
                "error",
                &json!({
                    "cmd":     "probe",
                    "message": format!("{} backend does not support port routing", eng.backend_name()),
                }),
            );
            return;
        }
        if playback.is_empty() {
            send_pub(
                &pub_tx,
                "error",
                &json!({"cmd":"probe","message":"no playback ports"}),
            );
            return;
        }

        if let Err(e) = eng.start(&[playback[0].clone()], None) {
            send_pub(
                &pub_tx,
                "error",
                &json!({"cmd":"probe","message":format!("{e}")}),
            );
            return;
        }
        eng.set_tone(freq, amplitude);
        eng.disconnect_output(&playback[0]);

        // Phase 1: DMM output scan
        let mut analog_channels: Vec<usize> = Vec::new();
        if let Some(ref host) = dmm_host {
            send_pub(
                &pub_tx,
                "data",
                &json!({
                    "cmd": "probe", "phase": "output_start", "n_ports": n_play
                }),
            );
            for (i, port) in playback.iter().enumerate() {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                eng.connect_output(port).ok();
                std::thread::sleep(std::time::Duration::from_millis(400));
                let vrms = read_dmm_vrms(host, 3);
                eng.disconnect_output(port);
                let is_analog = vrms.map(|v| v > threshold_rms).unwrap_or(false);
                if is_analog {
                    analog_channels.push(i);
                }
                send_pub(
                    &pub_tx,
                    "data",
                    &json!({
                        "cmd": "probe", "phase": "output",
                        "channel": i, "port": port,
                        "vrms": vrms, "analog": is_analog,
                    }),
                );
            }
        } else {
            send_pub(
                &pub_tx,
                "data",
                &json!({
                    "cmd": "probe", "phase": "output_skip",
                    "message": "no DMM configured — skipping output scan",
                }),
            );
            analog_channels = (0..n_play).collect();
        }

        // Phase 2: Loopback detection
        if !stop.load(Ordering::Relaxed) {
            send_pub(
                &pub_tx,
                "data",
                &json!({
                    "cmd": "probe", "phase": "loopback_start",
                    "n_outputs": analog_channels.len(), "n_inputs": n_cap,
                }),
            );
        }

        if let Some(cap0) = capture.first() {
            eng.reconnect_input(cap0).ok();
        }

        for &out_idx in &analog_channels {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            eng.connect_output(&playback[out_idx]).ok();
            std::thread::sleep(std::time::Duration::from_millis(150));

            for (j, cap_port) in capture.iter().enumerate() {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                eng.reconnect_input(cap_port).ok();
                eng.flush_capture();
                std::thread::sleep(std::time::Duration::from_millis(50));
                let level_dbfs = match eng.capture_block(0.05) {
                    Ok(data) => {
                        let rms = (data.iter().map(|&x| (x as f64).powi(2)).sum::<f64>()
                            / data.len().max(1) as f64)
                            .sqrt();
                        20.0 * rms.max(1e-12).log10()
                    }
                    Err(_) => -120.0,
                };
                if level_dbfs > -30.0 {
                    send_pub(
                        &pub_tx,
                        "data",
                        &json!({
                            "cmd": "probe", "phase": "loopback",
                            "out_ch": out_idx, "out_port": &playback[out_idx],
                            "in_ch": j, "in_port": cap_port,
                            "level_dbfs": (level_dbfs * 10.0).round() / 10.0,
                        }),
                    );
                }
            }
            eng.disconnect_output(&playback[out_idx]);
        }

        eng.set_silence();
        eng.stop();
        send_pub(
            &pub_tx,
            "done",
            &json!({
                "cmd": "probe",
                "analog_channels": analog_channels,
            }),
        );
    });

    {
        let mut workers = state.workers.lock().unwrap();
        workers.insert("probe".to_string(), worker);
    }
    json!({ "ok": true, "n_playback": n_play, "n_capture": n_cap })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pairs_multi() {
        let cmd = json!({ "pairs": [[0, 3], [1, 3], [2, 3]] });
        assert_eq!(
            parse_transfer_pairs(&cmd).unwrap(),
            vec![(0, 3), (1, 3), (2, 3)]
        );
    }

    #[test]
    fn parse_pairs_dedups() {
        let cmd = json!({ "pairs": [[0, 3], [1, 3], [0, 3]] });
        assert_eq!(parse_transfer_pairs(&cmd).unwrap(), vec![(0, 3), (1, 3)]);
    }

    #[test]
    fn parse_pairs_legacy_single() {
        let cmd = json!({ "meas_channel": 0, "ref_channel": 3 });
        assert_eq!(parse_transfer_pairs(&cmd).unwrap(), vec![(0, 3)]);
    }

    #[test]
    fn parse_pairs_empty_errors() {
        let cmd = json!({ "pairs": [] });
        assert!(parse_transfer_pairs(&cmd).is_err());
    }

    #[test]
    fn parse_pairs_malformed_element_errors() {
        let cmd = json!({ "pairs": [[0, 3], [1]] });
        assert!(parse_transfer_pairs(&cmd).is_err());
    }

    #[test]
    fn parse_pairs_missing_fields_errors() {
        let cmd = json!({});
        assert!(parse_transfer_pairs(&cmd).is_err());
    }

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
