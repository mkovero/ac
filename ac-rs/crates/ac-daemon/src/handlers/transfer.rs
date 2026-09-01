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
    apply_drive_ceiling, busy_guard, cfg_guard, read_dmm_vrms, ref_output_migration_warning,
    resolve_output, resolve_ref_output, send_pub, spawn_worker,
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

/// Scale a linear amplitude spectrum by a channel's voltage
/// calibration, if it has one. No-op for an uncalibrated channel.
///
/// A constant per-channel factor, so it commutes with column
/// aggregation (`sqrt(Σ(c·x)²) = c·sqrt(Σx²)`) — which is what lets it
/// be applied to the full-resolution spectrum before aggregation rather
/// than to the aggregated columns after.
fn apply_voltage_cal(amp: &mut [f64], cal: Option<&Calibration>) {
    if let Some(scale) = cal.and_then(|c| c.vrms_at_0dbfs_in) {
        for v in amp.iter_mut() {
            *v *= scale;
        }
    }
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

/// `relock` (#226) — discard every pair's held delay lock in the
/// **running** `transfer_stream` session, so the worker's next tick
/// retries acquisition from scratch. A held lock is a maintained
/// quantity, not a cached one: the operator asking is one of the two
/// events that invalidate it (the other is the drive coming on, handled
/// inside the worker loop itself).
///
/// Dispatched like `set_drive`: targets a live worker without spawning
/// one, so it has no `cmd_group` entry and never consults `check_busy`.
/// Session-wide, no `pair` selector — the flush is a session event and a
/// per-pair variant is scope this issue does not need.
pub fn relock(state: &ServerState, _cmd: &Value) -> Value {
    let slot = state.relock_state.lock().unwrap();
    match slot.as_ref() {
        Some(r) => {
            r.request();
            json!({"ok": true})
        }
        None => json!({"ok": false, "error": "no transfer_stream session running"}),
    }
}

/// A pair's held delay lock (#226). `driving` records whether the drive
/// was on at the tick this lock was accepted — the qualifier the drive
/// off→on edge reads to decide whether this lock is stale by
/// construction (taken against silence) or survives (taken while
/// driving, so a dead-man drop and resume must not disturb it). Carried
/// inside the `Option` rather than beside it so a pair that is currently
/// unlocked cannot hold a stale, meaningless flag: provenance exists only
/// when a lock does.
#[derive(Debug, Clone, Copy)]
struct Lock {
    samples: i64,
    driving: bool,
}

/// Everything about one pair that is fixed for the session: the channels
/// it names, where those channels sit in the capture buffers, and the
/// calibration each leg carries. Built once at launch, read-only after.
///
/// This replaces a `Vec` per field indexed by pair. That shape cost a
/// seven-deep `zip` at the per-pair fan-out — deep enough that
/// `delay_attempts` was read by index rather than joining it — and made
/// each vec an independent chance to index the wrong pair, with nothing
/// in the types saying they had to agree.
struct PairCtx {
    /// Position in the launch `pairs` list. Frames publish in this order,
    /// and it is the index into the per-tick ladder column vectors.
    pos: usize,
    meas_ch: u32,
    ref_ch: u32,
    /// Index of the measurement channel in the capture buffers / `rings`.
    mi: usize,
    /// Index of the reference channel in the capture buffers / `rings`.
    ri: usize,
    meas_cal: Option<Calibration>,
    ref_cal: Option<Calibration>,
    /// `meas_cal`'s mic-curve, lifted out because the mag/phase/re/im
    /// correction path takes it alone and must stay untouched
    /// (additive-only discipline). A ref-leg curve is refused at launch,
    /// so there is deliberately no `ref_curve` twin.
    meas_curve: Option<ac_core::shared::calibration::MicResponse>,
}

/// Everything about one pair that the worker loop maintains across ticks.
///
/// Plain data on purpose: the per-pair fan-out takes `&PairState`, so
/// every field here has to be `Sync`. The ladder (`MtwPair`) is
/// deliberately *not* a field — it owns an FFT planner, and it is
/// consumed into `mtw_columns` before the fan-out rather than read
/// inside it, so it stays a separate vec alongside.
struct PairState {
    /// Delay cache: ref↔meas propagation is constant during a streaming
    /// session (fixed hardware path), so we estimate once per pair on
    /// warmup and reuse the result. Skipping `estimate_delay` per tick
    /// (a 262 k-point FFT+IFFT at 2.5 s ring / 48 kHz) cuts the hot-loop
    /// work from ~17 ms → ~3 ms and takes the refresh rate from choppy
    /// ~8.5 Hz to the capture-interval-limited rate.
    ///
    /// That rate is ~16.6 Hz, not the ~10 Hz an older note claimed: the
    /// limit is `chunk_secs` (0.05 s) plus per-tick work, and
    /// `chunk_secs` was 0.2 when the ~10 Hz figure was written. Measured
    /// 2026-08-06 on `--fake-audio` at 48 kHz over 30 s, two pairs,
    /// median inter-frame gap 60.3 ms; the rig sees 17.5–18 Hz at 96 kHz.
    delay: Option<Lock>,
    /// A pair whose delay estimate was *refused* (no prominent
    /// correlation peak — #227) stays unlocked and is retried, because
    /// the cause is usually transient from the software's point of view:
    /// an unpatched reference leg or a muted source that the operator
    /// then fixes. Retry is rate-limited because each attempt is the same
    /// full-ring FFT+IFFT the cache above exists to avoid, and the inputs
    /// it reads only turn over on the ring's own timescale.
    next_attempt: Option<std::time::Instant>,
    /// Peak-to-median prominence from the most recent attempt, locked or
    /// refused. Published so a session that never locks still says how
    /// far short it fell — the estimator's one empirical constant is set
    /// from this distribution, and a bare "refused" would not measure it.
    prominence: Option<Value>,
    /// How many delay estimates this pair has completed, accepted or
    /// refused. Published as `delay_attempts` (#238).
    ///
    /// This is the only thing on the wire that separates "warming up"
    /// from "refusing": both publish `delay_locked: false`, and until an
    /// attempt has run there is no statement to make about the pair at
    /// all. The consumer that needs it is the fault indicator, which may
    /// not paint `LOST LOCK` on a session that has simply not been asked
    /// a question yet — see `ac-scene::fault`.
    ///
    /// A count, not a verdict. It says the estimator ran; it says nothing
    /// about how close the result came, which is the estimator's own
    /// business (`delay_evidence`, diagnostic-only).
    ///
    /// MONOTONE for the life of the session — never reset, including by
    /// #226's re-locking. Resetting it would make a pair that locked and
    /// then started refusing read as one that has not been asked yet, and
    /// the fault indicator answers "nothing to report" to that.
    attempts: u32,
    /// Per-pair `spl` time-integration state (F/S EMA, n_bands=1 —
    /// handoff: transfer-frame-v2 M0). `None` for a pair whose meas
    /// channel has no SPL calibration layer; `spl` stays `null` for that
    /// pair's whole session, matching `spl_offsets` in `monitor.rs`.
    /// Session-static per D10, so decided once at construction rather
    /// than re-checked per tick.
    spl_integ: Option<ac_core::visualize::time_integration::EmaIntegrator>,
    /// Timestamp of the last `spl` integration step, for its `dt`.
    spl_last: Option<std::time::Instant>,
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
/// Falsified by `pinned_window_tests` below, which runs the discarded
/// exact-trim drain side by side with this one on the same burst.
fn drain_to_block_lattice(ring: &mut Vec<f32>, target_total: usize, step: usize) {
    while ring.len() >= target_total + step {
        ring.drain(..step);
    }
}

impl PairState {
    fn new(spl_integ: Option<ac_core::visualize::time_integration::EmaIntegrator>) -> Self {
        Self {
            delay: None,
            next_attempt: None,
            prominence: None,
            attempts: 0,
            spl_integ,
            spl_last: None,
        }
    }

    /// Discard this pair's held lock and its `ladder`, and clear the
    /// retry timer so the next tick attempts acquisition immediately
    /// rather than waiting out `RELOCK_RETRY`. Leaves `attempts` and
    /// `prominence` untouched — the first must stay monotone (a reset
    /// would make a locked-then-refusing pair read as one never asked),
    /// and the second is last-attempt evidence that the next attempt
    /// overwrites on its own.
    ///
    /// Takes the ladder slot as an argument because `MtwPair` cannot live
    /// in `PairState` (see the type's note), but a flush that dropped the
    /// lock without the ladder would leave a ladder aligned to an offset
    /// no longer held.
    fn flush(&mut self, ladder: &mut Option<ac_core::visualize::mtw::MtwPair>) {
        self.delay = None;
        self.next_attempt = None;
        *ladder = None;
    }
}

/// Every `transfer_stream` launch parameter that comes from the request
/// alone, validated.
///
/// Deliberately a pure function of `cmd`: it reads no `ServerState` and
/// no config, so the whole of the request contract — defaults, ranges,
/// and the three rejections — is decidable without a daemon, a socket, or
/// an audio backend. That is why `level_dbfs` is left UNCLAMPED here; the
/// ceiling is `cfg.drive_max_dbfs`, and folding it in would drag the
/// config in and make the reachable-value tests need one.
#[derive(Debug)]
struct TransferParams {
    drive: bool,
    drivable: bool,
    /// As requested. Still to be clamped to `cfg.drive_max_dbfs` by the
    /// caller (#360) before it reaches `DriveState` or a loudspeaker.
    level_dbfs: f64,
    fake_correlated_pair: Option<(f64, usize)>,
    fake_ring_process_secs: Option<f64>,
    fake_ring_period: usize,
    mtw_ppo: f64,
    mtw_n_blocks: usize,
    pairs: Vec<(u32, u32)>,
    weighting: ac_core::visualize::weighting_curves::WeightingCurve,
    /// Normalised (lower-cased) tag, echoed on every frame as
    /// `spl_integration`.
    integration_tag: String,
    integration_tau_s: f64,
}

/// Parse and validate `transfer_stream`'s launch parameters. `Err` is a
/// message suitable for `{"ok": false, "error": ...}`.
///
/// Out-of-range numeric knobs (`mtw_ppo`, `mtw_n_blocks`) fall back to
/// their defaults rather than erroring — that is the pre-existing
/// contract, preserved here, and `filter` is what implements it.
fn parse_params(cmd: &Value) -> Result<TransferParams, String> {
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
    // only inside `if fake { ... }` in the worker — real backends never
    // see it.
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
    // `process_secs` defaults to the transfer worker's own measured compute
    // on the tick that matters — the one where the ring crosses a block
    // boundary and the analysis is recomputed: ~5 ms per pair in release,
    // against ~1 ms on the nine ticks in ten that reuse the held estimate.
    // Modelling the worst tick is the point: a splice is a function of the
    // longest gap, not the average one.
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

    let pairs = parse_transfer_pairs(cmd)?;

    // Per-meas-channel SPL session params (D10 — static for the session,
    // set once here, not live-toggleable). `weighting`'s wire contract
    // (handoff: transfer-frame-v2 M0) is a strict 3-way A/C/Z — no
    // "off" — so `WeightingCurve::from_tag`'s existing rejection of
    // anything else (including "off") is exactly the validation wanted.
    let weighting_tag = cmd.get("weighting").and_then(Value::as_str).unwrap_or("Z");
    let weighting =
        ac_core::visualize::weighting_curves::WeightingCurve::from_tag(weighting_tag)
            .ok_or_else(|| format!("weighting must be one of A, C, Z, got '{weighting_tag}'"))?;
    let integration_tag = cmd
        .get("integration")
        .and_then(Value::as_str)
        .unwrap_or("fast")
        .to_ascii_lowercase();
    let integration_tau_s = match integration_tag.as_str() {
        "fast" => ac_core::visualize::time_integration::TAU_FAST_S,
        "slow" => ac_core::visualize::time_integration::TAU_SLOW_S,
        _ => {
            return Err(format!(
                "integration must be 'fast' or 'slow', got '{integration_tag}'"
            ))
        }
    };

    Ok(TransferParams {
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
    })
}

/// Frame inputs that are fixed for the worker's whole life.
///
/// Split from [`TickInputs`] on exactly that axis: anything here is
/// derived from `sr` or from a launch parameter, so a test can build one
/// and reuse it, and a reviewer can see at a glance that nothing in it
/// can drift between ticks.
struct FrameStatics {
    sr: u32,
    /// Fixed log-column grid for `meas_spectrum`/`ref_spectrum` (D18).
    spec_f_min: f64,
    spec_f_max: f64,
    spec_n_columns: usize,
    weighting: ac_core::visualize::weighting_curves::WeightingCurve,
    integration_tag: String,
    mtw_ppo: f64,
    mtw_n_blocks: usize,
    /// Ladder description, shipped whole with every frame so a consumer
    /// can interpret a column's `stage` without knowing the layout rules,
    /// and so a saved frame stays interpretable if those rules change.
    mtw_stages: Value,
}

/// Frame inputs that change every capture tick.
///
/// The rings are deliberately absent: assembly reads the cached
/// [`PairAnalysis`] instead, which is what lets a frame ship on every tick
/// while the estimate behind it advances only when the ring does.
struct TickInputs<'a> {
    /// Raw pre-calibration peaks (§4.2) from THIS tick's blocks, indexed
    /// by `PairCtx::mi`/`ri`.
    tick_peaks_dbfs: &'a [Option<f64>],
    /// Global mic-correction toggle, sampled once per tick so every pair in
    /// a frame agrees about it.
    mc_enabled: bool,
    /// Observed drive state (#228), identical for every pair in the tick.
    drive_msg: &'a Value,
    /// Ladder columns and settled-rung flags, indexed by `PairCtx::pos`.
    /// Recomputed every tick — the ladder is a push pipeline.
    mtw_columns: &'a [Option<Vec<ac_core::visualize::mtw::splice::Column>>],
    mtw_settled: &'a [Vec<bool>],
    /// The held H1 estimate per pair, indexed by `PairCtx::pos`. `None`
    /// means no segment yet, which publishes a settling frame.
    analysis: &'a [Option<PairAnalysis>],
    /// Capture channels the session has rings for. A pair naming a channel
    /// outside that is dropped from the frame entirely rather than
    /// publishing a partial one (#254) — which is a different thing from
    /// having no estimate yet, and must not be reported as one.
    n_channels: usize,
}

/// Per-channel provenance tags (tier-framing labelled-tag rules, #97/#98
/// vocabulary): `"on"`/`"none"` for voltage and SPL — neither has a
/// daemon-side enable toggle, unlike mic-curve — and `"on"`/`"off"`/
/// `"none"` for `mic_curve` via `mic_correction_tag`.
///
/// The reference leg's `mic_curve` is structurally almost always
/// `"none"`: a ref-channel mic curve is refused at request time.
///
/// Shared by the settling frame and the analysis frame so the two cannot
/// disagree about a session constant.
fn cal_tags_value(
    meas_cal: Option<&Calibration>,
    ref_cal: Option<&Calibration>,
    meas_mic_tag: &str,
    mc_enabled: bool,
) -> Value {
    let ref_curve_loaded = ref_cal.is_some_and(|c| c.mic_response.is_some());
    json!({
        "meas": {
            "voltage": if meas_cal.and_then(|c| c.vrms_at_0dbfs_in).is_some() { "on" } else { "none" },
            "spl":     if meas_cal.and_then(Calibration::spl_offset_db).is_some() { "on" } else { "none" },
            "mic_curve": meas_mic_tag,
        },
        "ref": {
            "voltage": if ref_cal.and_then(|c| c.vrms_at_0dbfs_in).is_some() { "on" } else { "none" },
            "spl":     if ref_cal.and_then(Calibration::spl_offset_db).is_some() { "on" } else { "none" },
            "mic_curve": mic::mic_correction_tag(ref_curve_loaded, mc_enabled),
        },
    })
}

/// The frame a pair publishes before its ring holds a whole Welch segment.
///
/// Same key set as the analysis frame, with every H1-derived field empty
/// or null and `n_averages: 0` saying so. What it does carry is everything
/// that never depended on the analysis window: the observed drive state,
/// the raw capture peaks, the attempt count, and the calibration tags.
///
/// The alternative — the loop `continue`ing until the window fills —
/// suppressed those too, so for the first second of a session a client
/// could not tell a daemon that had not started from one whose drive had
/// already dead-manned, and `ac-scene::fault` had no frame to read. The
/// analysis window and time-to-first-frame are different quantities and
/// this is what stops one setting the other.
///
/// `spec_freqs` is empty here rather than carrying the session's fixed
/// grid, so the three spectrum arrays agree in length on this frame as
/// they do on every other.
#[allow(clippy::too_many_arguments)]
fn settling_frame(
    ctx: &PairCtx,
    st: &PairState,
    statics: &FrameStatics,
    meas_peak: Value,
    ref_peak: Value,
    mc_tag: &str,
    mc_enabled: bool,
    drive_msg: &Value,
) -> Value {
    json!({
        "type":            "transfer_stream",
        "cmd":             "transfer_stream",
        "mtw":             Value::Null,
        "freqs":           Vec::<f64>::new(),
        "magnitude_db":    Vec::<f64>::new(),
        "phase_deg":       Vec::<f64>::new(),
        "coherence":       Vec::<f64>::new(),
        "delay_samples":   0,
        "delay_ms":        0.0,
        "delay_locked":    false,
        "delay_attempts":  st.attempts,
        "delay_evidence":  st.prominence,
        "meas_peak_dbfs":  meas_peak,
        "ref_peak_dbfs":   ref_peak,
        "ref_channel":     ctx.ref_ch,
        "meas_channel":    ctx.meas_ch,
        "sr":              statics.sr,
        // Zero blocks: this frame carries no Welch estimate at all, which
        // is a different statement from the `1` a first-segment frame
        // makes. A consumer reading coherence's `1/N` bias needs the
        // difference, and so does anyone deciding whether an empty
        // magnitude array is a fault or a start.
        "n_averages":      0,
        // No estimate exists to number. `null` rather than 0, so the
        // first real estimate's `0` cannot be mistaken for a repeat of
        // something that was never sent.
        "analysis_seq":    Value::Null,
        "mic_correction":  mc_tag,
        "spec_freqs":      Vec::<f64>::new(),
        "meas_spectrum":   Vec::<f64>::new(),
        "ref_spectrum":    Vec::<f64>::new(),
        "spl":             Value::Null,
        "spl_weighting":   statics.weighting.tag(),
        "spl_integration": statics.integration_tag.as_str(),
        "cal_tags":        cal_tags_value(ctx.meas_cal.as_ref(), ctx.ref_cal.as_ref(), mc_tag, mc_enabled),
        "drive":           drive_msg.clone(),
    })
}

/// What an H1 estimate is a function of.
///
/// The Welch estimate reads whole segments from the ring's start, so it
/// cannot change while the ring start and the segment count both hold —
/// `drain_to_block_lattice` moves the start only in whole `step` units
/// (#208), and the samples appended past the last complete segment are
/// not read. Everything else the estimate depends on is here: the
/// alignment offset it was computed at, and the mic-correction toggle
/// that scales it.
///
/// Equal keys therefore mean an identical result, which is what makes the
/// cache a cache rather than a staleness policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AnalysisKey {
    /// Samples drained from the ring since session start — the ring
    /// start's absolute position in the stream, and so the absolute
    /// position of every segment boundary.
    dropped: usize,
    /// Complete Welch segments in the ring.
    n_blocks: usize,
    /// The alignment offset this estimate was computed at. A pair that
    /// locks mid-window must not keep publishing an unaligned estimate
    /// until the next boundary.
    delay: i64,
    /// The global mic-correction toggle, which a client can flip between
    /// ticks.
    mc_enabled: bool,
}

/// One pair's H1 estimate and everything derived from it, held between
/// block boundaries.
///
/// At 48 kHz the ring advances every 0.5 s while the loop ticks at 20 Hz,
/// so without this the same estimate is recomputed about ten times — a
/// 2.5 s Welch pass and a full-resolution IFFT per pair per tick, all of
/// it producing bytes identical to the previous tick's. #419 named the
/// waste and left it; this is the part of the loop that pays for it.
///
/// Arrays are stored already as `Value` because the only thing left to do
/// with them is serialize them.
struct PairAnalysis {
    key: AnalysisKey,
    /// Increments once per recomputation, published as `analysis_seq`.
    /// A consumer comparing it across frames can tell an estimate that is
    /// genuinely new from the same one shipped again, which is otherwise
    /// invisible: the arrays are byte-identical either way.
    seq: u64,
    n_blocks: usize,
    delay_samples: i64,
    delay_ms: f64,
    freqs: Value,
    magnitude_db: Value,
    phase_deg: Value,
    coherence: Value,
    spec_freqs: Value,
    meas_spectrum: Value,
    ref_spectrum: Value,
    /// Broadband weighted level before time integration. The EMA that
    /// consumes it still steps every tick with that tick's `dt`, so
    /// holding the raw value here changes no `spl` number: it was
    /// recomputed identically on every tick before.
    spl_raw: Option<f64>,
    /// Phase 4b sidecar payload, absent when the IFFT produced nothing.
    ir: Option<IrPayload>,
}

/// The `visualize/ir` sidecar's per-analysis content. The channel and
/// lock fields are added at assembly, from live state.
struct IrPayload {
    samples: Value,
    stride: usize,
    dt_ms: f64,
    t_origin_ms: f64,
}

/// Compute one pair's H1 estimate and everything derived from it.
///
/// `None` when the pair's channels are not present in the rings, which
/// drops the pair from the frame rather than publishing a partial one.
fn analyse_pair(
    ctx: &PairCtx,
    st: &PairState,
    statics: &FrameStatics,
    rings: &[Vec<f32>],
    key: AnalysisKey,
    seq: u64,
) -> Option<PairAnalysis> {
    let &FrameStatics {
        sr,
        spec_f_min,
        spec_f_max,
        spec_n_columns,
        weighting,
        ..
    } = statics;
    let (curve_opt, meas_cal_opt, ref_cal_opt) = (&ctx.meas_curve, &ctx.meas_cal, &ctx.ref_cal);
    let mc_enabled = key.mc_enabled;
    let meas = rings.get(ctx.mi)?.as_slice();
    let refb = rings.get(ctx.ri)?.as_slice();

    // An unlocked pair (still warming up, or refused by the prominence
    // gate — #227) is measured unaligned rather than aligned to a guess.
    // `delay_locked` on the frame is what keeps that distinguishable: a
    // refused pair and a genuine 0-sample digital loopback both report
    // `delay_ms` 0.0, and #216 established that the loopback case is
    // legitimately 0.0, so the number alone cannot carry the difference.
    let result = ac_core::visualize::transfer::h1_estimate_with_delay(refb, meas, sr, key.delay);

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
    // Mic-curve correction (#101) on the measurement leg only — the
    // reference leg was guarded at launch. H1's dB magnitude has the mic
    // over-read embedded; subtract the curve at each downsampled bin to
    // recover truth.
    if mc_enabled {
        if let Some(curve) = curve_opt.as_ref() {
            mic::apply_mic_curve_inplace_f64(curve, &freqs, &mut mag);
        }
    }

    // Calibrated per-channel spectra (D18, handoff: transfer-frame-v2 M0).
    // Full-resolution `result.meas_amp`/`result.ref_amp` — NOT the 2000-pt
    // indices above, the same full-res-then-aggregate split the IR sidecar
    // uses — so the mic-curve's per-freq correction is applied at native
    // Welch-bin resolution, then `spectrum_to_columns_wire` band-power-
    // aggregates to the fixed grid: same aggregator, same tests, as the
    // monitor `spectrum` frame.
    let mut mc_meas_amp = result.meas_amp.clone();
    if mc_enabled {
        if let Some(curve) = curve_opt.as_ref() {
            for (amp, &f) in mc_meas_amp.iter_mut().zip(result.freqs.iter()) {
                *amp *= mic::mic_curve_scale(curve, f);
            }
        }
    }
    // `spl` is computed from the mic-corrected (acoustic truth) spectrum,
    // before voltage-cal scaling below — SPL derives from the dBFS +
    // spl_offset_db model (Calibration::spl_offset_db), independent of the
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

    // Voltage cal (D3), applied here — post mic-curve, pre-aggregation.
    // Both legs go through the same helper: the meas and ref spectra are
    // divided against each other downstream, so a scale applied to one leg
    // under rules that have drifted from the other's is an error that
    // cancels out of every check but the answer.
    let meas_amp_wire = {
        let mut a = mc_meas_amp;
        apply_voltage_cal(&mut a, meas_cal_opt.as_ref());
        a
    };
    let ref_amp_wire = {
        let mut a = result.ref_amp.clone();
        apply_voltage_cal(&mut a, ref_cal_opt.as_ref());
        a
    };
    let (meas_spectrum, spec_freqs) = ac_core::visualize::aggregate::spectrum_to_columns_wire(
        &meas_amp_wire,
        sr as f64,
        spec_f_min,
        spec_f_max,
        spec_n_columns,
    );
    let (ref_spectrum, _) = ac_core::visualize::aggregate::spectrum_to_columns_wire(
        &ref_amp_wire,
        sr as f64,
        spec_f_min,
        spec_f_max,
        spec_n_columns,
    );

    // Phase 4b: IR sidecar from the full-resolution complex H — the IFFT
    // needs the raw nperseg/2+1 bins to recover h(t) correctly, so it
    // reads `result.re`/`result.im` and not the 2000-point display arrays.
    // Time-domain output is 1 s long at sr (matches the 1 Hz Welch
    // resolution); downsampled to ≤2000 samples for wire economy by
    // stride-picking. Mic-curve correction is intentionally NOT applied to
    // the IR: the `curve_opt` branch above corrects only the downsampled
    // `mag`, and the full-resolution H here is uncorrected. For the
    // visualization-only Tier 2 IR view this is acceptable; a Tier-1
    // calibrated IR goes through the sweep measurement, not
    // `transfer_stream`.
    let ir_full = ac_core::visualize::transfer::impulse_response_from_h(&result.re, &result.im);
    let ir = if ir_full.is_empty() {
        None
    } else {
        const IR_MAX_SAMPLES: usize = 2000;
        let stride = (ir_full.len() / IR_MAX_SAMPLES).max(1);
        let ir_ds: Vec<f32> = ir_full.iter().step_by(stride).copied().collect();
        // t_origin_ms = -mid_ms because `impulse_response_from_h` centres
        // the IR peak at the middle of the array (t=0 in the user's
        // mental model).
        let dt_ms = 1000.0 / sr as f64 * stride as f64;
        let t_origin_ms = -((ir_ds.len() / 2) as f64) * dt_ms;
        Some(IrPayload {
            samples: json!(ir_ds),
            stride,
            dt_ms,
            t_origin_ms,
        })
    };

    let _ = st;
    Some(PairAnalysis {
        key,
        seq,
        n_blocks: key.n_blocks,
        delay_samples: result.delay_samples,
        delay_ms: result.delay_ms,
        freqs: json!(freqs),
        magnitude_db: json!(mag),
        phase_deg: json!(phase),
        coherence: json!(coh),
        spec_freqs: json!(spec_freqs),
        meas_spectrum: json!(meas_spectrum),
        ref_spectrum: json!(ref_spectrum),
        spl_raw,
        ir,
    })
}

/// Build one pair's wire messages for this tick: the `transfer_stream`
/// frame, plus a Phase 4b `visualize/ir` sidecar when there is an
/// estimate to derive one from. Returns the pair's launch position
/// alongside them, and the **un-integrated** broadband SPL — integration
/// holds `&mut` per-pair state and so happens on the worker thread, after
/// the fan-out.
///
/// Everything expensive already happened in [`analyse_pair`]; what is
/// left is assembly from that plus this tick's live scalars, which is why
/// a frame can ship every tick while the estimate behind it advances at
/// the ring's own rate.
fn build_pair_messages(
    ctx: &PairCtx,
    st: &PairState,
    statics: &FrameStatics,
    tick: &TickInputs<'_>,
) -> Option<(usize, Vec<Value>, Option<f64>)> {
    let &PairCtx {
        pos,
        meas_ch,
        ref_ch,
        mi,
        ri,
        ..
    } = ctx;
    let &TickInputs {
        tick_peaks_dbfs,
        mc_enabled,
        drive_msg,
        mtw_columns,
        mtw_settled,
        analysis,
        n_channels,
    } = tick;
    if mi >= n_channels || ri >= n_channels {
        return None;
    }
    let &FrameStatics {
        sr,
        mtw_ppo,
        mtw_n_blocks,
        ..
    } = statics;
    // `-inf` (digital silence) travels as JSON null: serde_json cannot
    // serialise a non-finite float, so the conversion is explicit here
    // rather than left to the `json!` site.
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
    let mc_tag = mic::mic_correction_tag(ctx.meas_curve.is_some(), mc_enabled);

    let Some(Some(a)) = analysis.get(pos) else {
        // No estimate yet — the ring does not hold a whole Welch segment.
        // Publish anyway; see `settling_frame`.
        return Some((
            pos,
            vec![settling_frame(
                ctx, st, statics, meas_peak, ref_peak, mc_tag, mc_enabled, drive_msg,
            )],
            None,
        ));
    };

    let cal_tags = cal_tags_value(
        ctx.meas_cal.as_ref(),
        ctx.ref_cal.as_ref(),
        mc_tag,
        mc_enabled,
    );

    // Multi-time-window columns (additive; `null` until every rung holds a
    // full N blocks — 2.56 s at the bottom, the design's stated settling
    // time. Gating on the full N is what makes the reported N
    // unambiguous: every column is the mean of the same number of
    // blocks). Unlike the Welch arrays above, these are recomputed every
    // tick: the ladder is a push pipeline fed the fresh capture buffers,
    // so its columns really do move at the frame rate.
    //
    // Every column ships the Δf, window and N that produced it. That is
    // not decoration: neighbouring columns can come from windows 12x
    // apart, and coherence from uncorrelated inputs floats near 1/N, so
    // without those a screenshot of this display is not interpretable.
    // `bins` is criterion 1 made observable — it is never zero.
    //
    // dB is applied here, daemon-side, per the display-truth rule:
    // `ac-view` plots what it is given and does no `log10` of its own.
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
            // Which rungs have settled, shallowest first. Shipped so a
            // consumer can distinguish "still warming, more band coming"
            // from "this is all there is" — a short column list looks the
            // same either way, and the difference decides whether a blank
            // low end is a fault.
            "settled_stages": mtw_settled
                .get(pos)
                .cloned()
                .unwrap_or_default(),
            "stages":       &statics.mtw_stages,
        }),
    };

    let transfer_msg = json!({
        "type":            "transfer_stream",
        "cmd":             "transfer_stream",
        "mtw":             mtw_msg,
        "freqs":           a.freqs,
        "magnitude_db":    a.magnitude_db,
        "phase_deg":       a.phase_deg,
        "coherence":       a.coherence,
        "delay_samples":   a.delay_samples,
        "delay_ms":        a.delay_ms,
        "delay_locked":    st.delay.is_some(),
        "delay_attempts":  st.attempts,
        "delay_evidence":  st.prominence,
        "meas_peak_dbfs":  meas_peak,
        "ref_peak_dbfs":   ref_peak,
        "ref_channel":     ref_ch,
        "meas_channel":    meas_ch,
        "sr":              sr,
        // Welch blocks actually averaged into THIS frame (#208) — 1 while
        // the window fills, then `n_averages` for the rest of the session,
        // and 0 on a settling frame. Shipped because coherence carries a
        // `1/N` bias, so a coherence figure without N is not
        // interpretable: a consumer that saw N move silently could not
        // tell a settling display from a DUT that changed.
        "n_averages":      a.n_blocks,
        // Which estimate these arrays are. Increments when the analysis is
        // recomputed, which is once per Welch hop — slower than the frame
        // rate, so consecutive frames repeat the same arrays by design.
        // Without this the repetition is invisible and a stalled estimator
        // looks exactly like a stationary DUT.
        "analysis_seq":    a.seq,
        "mic_correction":  mc_tag,
        "spec_freqs":      a.spec_freqs,
        "meas_spectrum":   a.meas_spectrum,
        "ref_spectrum":    a.ref_spectrum,
        "spl":             Value::Null,
        "spl_weighting":   statics.weighting.tag(),
        "spl_integration": statics.integration_tag.as_str(),
        "cal_tags":        cal_tags,
        "drive":           drive_msg.clone(),
    });

    let mut out = vec![transfer_msg];
    if let Some(ir) = a.ir.as_ref() {
        out.push(json!({
            "type":          "visualize/ir",
            "cmd":           "transfer_stream",
            "samples":       ir.samples,
            "sr":            sr,
            "stride":        ir.stride,
            "dt_ms":         ir.dt_ms,
            "t_origin_ms":   ir.t_origin_ms,
            "ref_channel":   ref_ch,
            "meas_channel":  meas_ch,
            "delay_samples": a.delay_samples,
            "delay_ms":      a.delay_ms,
            "delay_locked":  st.delay.is_some(),
            "analysis_seq":  a.seq,
        }));
    }
    Some((pos, out, a.spl_raw))
}

/// Retry interval for a refused delay estimate — see
/// [`PairState::next_attempt`].
const RELOCK_RETRY: std::time::Duration = std::time::Duration::from_secs(1);

/// The analysis window's geometry, all of it derived from the sample rate.
///
/// `nperseg`/`step` mirror `h1_estimate`'s internal Welch settings; a
/// mismatch here would make `n_averages` on the wire a claim about a
/// segmentation the estimator does not perform.
#[derive(Debug, Clone, Copy)]
struct Window {
    /// Welch segment length. `sr` — 1 Hz bin width.
    nperseg: usize,
    /// Segment hop, `nperseg / 2` for 50% overlap. Also the quantum the
    /// ring is drained in (#208), which is what pins the block grid to
    /// the stream.
    step: usize,
    /// Segments averaged once the window is full — the steady-state
    /// `n_averages` the frame reports.
    n_averages: usize,
}

impl Window {
    fn new(sr: u32, n_averages: usize) -> Self {
        let nperseg = sr as usize;
        Self {
            nperseg,
            step: nperseg / 2,
            n_averages,
        }
    }

    /// Ring length holding exactly `n_averages` complete segments.
    /// Derived rather than stored so the three numbers cannot disagree.
    fn target_total(&self) -> usize {
        self.nperseg + self.step * (self.n_averages - 1)
    }
}

/// What this tick observed outside the analysis: the drive state the
/// worker actually applied to its engine, the two events that invalidate
/// a lock, and the global mic-correction toggle.
///
/// Sampled once per tick by the worker and handed in whole, so every pair
/// in a frame agrees about all four. The drive poll itself stays in the
/// worker because applying it needs the engine; what reaches the analysis
/// is the observation, never the command (#228).
#[derive(Debug, Clone, Copy)]
struct TickEvents {
    /// Drive state as applied to the engine on this tick, after the
    /// dead-man and after `set_drive`'s clamp. Recorded as a new lock's
    /// `driving` provenance.
    engine_on: bool,
    /// False→true transition of `engine_on` since the previous tick
    /// (#226): the signal a lock was taken against just changed.
    drive_edge_on: bool,
    /// A `relock` request arrived since the previous tick (#226).
    relock_requested: bool,
    /// `mic_correction_enabled`, sampled once so a frame cannot be built
    /// half-corrected.
    mc_enabled: bool,
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
struct SessionState {
    statics: FrameStatics,
    window: Window,
    /// Per-pair session constants, in launch order.
    ctx: Vec<PairCtx>,
    /// Per-pair maintained state, same order and length as `ctx`.
    pairs: Vec<PairState>,
    /// Sliding H1 window per unique capture channel, indexed by
    /// `PairCtx::mi`/`ri`.
    rings: Vec<Vec<f32>>,
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
    ladders: Vec<Option<ac_core::visualize::mtw::MtwPair>>,
    /// A layout the ladder cannot serve (an unsupported rate) degrades to
    /// "no ladder" rather than to a dead session, and is logged once.
    ladder_failed: bool,
    /// The held H1 estimate per pair, same order as `ctx`. Recomputed
    /// only when [`AnalysisKey`] changes — see [`PairAnalysis`].
    analysis: Vec<Option<PairAnalysis>>,
    /// Samples drained from the rings since session start. Half of the
    /// analysis key: it is the ring start's absolute position in the
    /// stream, so it changes exactly when the Welch segment boundaries do.
    dropped: usize,
    /// Next `analysis_seq`. Session-wide rather than per-pair so a
    /// consumer watching two pairs sees one ordering.
    next_seq: u64,
    /// Capture tick, used only as the `spl` integrator's `dt` on the
    /// first step, where there is no previous timestamp to subtract.
    chunk_secs: f64,
}

impl SessionState {
    fn new(
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
    fn n_channels(&self) -> usize {
        self.rings.len()
    }

    /// Each pair's held lock, in launch order, for the snapshot ring's
    /// provenance copy.
    fn delay_samples(&self) -> Vec<Option<i64>> {
        self.pairs
            .iter()
            .map(|st| st.delay.map(|l| l.samples))
            .collect()
    }

    /// Discard every pair's lock and ladder — a `relock` request (#226).
    fn flush_all(&mut self) {
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
    fn flush_locks_taken_against_silence(&mut self) {
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
    fn push_rings(&mut self, bufs: &[Vec<f32>]) {
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
    fn n_blocks(&self) -> usize {
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
    fn acquire_missing_locks(&mut self, ev: TickEvents, now: std::time::Instant) {
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
    fn advance_ladders(
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
    fn refresh_analysis(&mut self, n_blocks: usize, mc_enabled: bool) {
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
    fn tick(
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

    // ---- parse_params -------------------------------------------------
    //
    // These exist because `parse_params` reads no `ServerState` and no
    // config. Every one of them used to require a live daemon to reach.

    fn params(v: Value) -> Result<TransferParams, String> {
        parse_params(&v)
    }

    #[test]
    fn params_defaults_are_passive_and_z_weighted() {
        let p = params(json!({"pairs": [[0, 1]]})).unwrap();
        assert!(!p.drive, "default session must not drive");
        assert!(!p.drivable, "default session must open no output ports");
        assert_eq!(p.level_dbfs, -10.0);
        assert_eq!(p.weighting.tag(), "Z");
        assert_eq!(p.integration_tag, "fast");
        assert_eq!(
            p.integration_tau_s,
            ac_core::visualize::time_integration::TAU_FAST_S
        );
        assert!(p.fake_correlated_pair.is_none());
        assert!(p.fake_ring_process_secs.is_none());
    }

    // Legacy `drive: true` must still imply drivable, or the generator
    // plays onto a port that was never opened.
    #[test]
    fn params_drive_implies_drivable() {
        let p = params(json!({"pairs": [[0, 1]], "drive": true})).unwrap();
        assert!(p.drivable);
    }

    #[test]
    fn params_drivable_alone_does_not_drive() {
        let p = params(json!({"pairs": [[0, 1]], "drivable": true})).unwrap();
        assert!(p.drivable);
        assert!(!p.drive);
    }

    // #360: the ceiling is `cfg.drive_max_dbfs`, which this fn cannot see.
    // It must therefore hand back exactly what was asked for — a clamp
    // appearing here would be a second, config-blind ceiling.
    #[test]
    fn params_do_not_clamp_level() {
        let p = params(json!({"pairs": [[0, 1]], "level_dbfs": 0.0})).unwrap();
        assert_eq!(p.level_dbfs, 0.0);
    }

    // The wire contract is a strict 3-way A/C/Z. "off" is the specific
    // value worth pinning: it is accepted by other weighting knobs in this
    // daemon and must be refused here.
    #[test]
    fn params_reject_off_weighting() {
        assert!(params(json!({"pairs": [[0, 1]], "weighting": "off"})).is_err());
    }

    #[test]
    fn params_reject_unknown_weighting() {
        let e = params(json!({"pairs": [[0, 1]], "weighting": "B"})).unwrap_err();
        assert!(e.contains("A, C, Z"), "{e}");
    }

    #[test]
    fn params_accept_lowercase_weighting() {
        assert_eq!(
            params(json!({"pairs": [[0, 1]], "weighting": "a"}))
                .unwrap()
                .weighting
                .tag(),
            "A"
        );
    }

    #[test]
    fn params_integration_slow_and_case_insensitive() {
        let p = params(json!({"pairs": [[0, 1]], "integration": "SLOW"})).unwrap();
        assert_eq!(p.integration_tag, "slow", "tag is normalised for the wire");
        assert_eq!(
            p.integration_tau_s,
            ac_core::visualize::time_integration::TAU_SLOW_S
        );
    }

    #[test]
    fn params_reject_unknown_integration() {
        assert!(params(json!({"pairs": [[0, 1]], "integration": "medium"})).is_err());
    }

    // Out-of-range ladder knobs fall back to the default rather than
    // erroring. That is the pre-existing contract; this test is here so a
    // future tightening to a rejection is a visible decision rather than a
    // silent one.
    #[test]
    fn params_out_of_range_ladder_knobs_fall_back() {
        let p = params(json!({
            "pairs": [[0, 1]],
            "mtw_ppo": 10_000.0,
            "mtw_n_blocks": 0,
        }))
        .unwrap();
        assert_eq!(p.mtw_ppo, ac_core::visualize::mtw::ladder::P_REF);
        assert_eq!(
            p.mtw_n_blocks,
            ac_core::visualize::mtw::average::DEFAULT_N_BLOCKS
        );
    }

    #[test]
    fn params_in_range_ladder_knobs_are_taken() {
        let p = params(json!({"pairs": [[0, 1]], "mtw_ppo": 24.0, "mtw_n_blocks": 8})).unwrap();
        assert_eq!(p.mtw_ppo, 24.0);
        assert_eq!(p.mtw_n_blocks, 8);
    }

    // Presence of the key selects ring mode; its absence leaves the
    // on-demand generator in place. An empty object is still presence.
    #[test]
    fn params_fake_ring_presence_selects_mode() {
        let p = params(json!({"pairs": [[0, 1]], "fake_ring": {}})).unwrap();
        assert_eq!(p.fake_ring_process_secs, Some(0.005));
        assert_eq!(p.fake_ring_period, 1024);
    }

    #[test]
    fn params_pair_error_propagates() {
        assert!(params(json!({"pairs": []})).is_err());
    }

    // ---- build_pair_messages -----------------------------------------
    //
    // Same reason: the frame contract used to be reachable only through a
    // daemon, a socket and an audio backend.

    const TEST_SR: u32 = 8000;

    fn test_statics() -> FrameStatics {
        let spec_f_min = 20.0_f64;
        let spec_f_max = TEST_SR as f64 / 2.0;
        FrameStatics {
            sr: TEST_SR,
            spec_f_min,
            spec_f_max,
            spec_n_columns: ac_core::visualize::aggregate::transfer_spectrum_n_columns(
                spec_f_min, spec_f_max,
            ),
            weighting: ac_core::visualize::weighting_curves::WeightingCurve::Z,
            integration_tag: "fast".to_string(),
            mtw_ppo: ac_core::visualize::mtw::ladder::P_REF,
            mtw_n_blocks: ac_core::visualize::mtw::average::DEFAULT_N_BLOCKS,
            mtw_stages: Value::Null,
        }
    }

    fn test_ctx() -> PairCtx {
        PairCtx {
            pos: 0,
            meas_ch: 2,
            ref_ch: 5,
            mi: 0,
            ri: 1,
            meas_cal: None,
            ref_cal: None,
            meas_curve: None,
        }
    }

    /// One Welch segment of a correlated pair: ref is a tone, meas is the
    /// same tone at half amplitude. Enough for H1 to produce a frame.
    fn test_rings() -> Vec<Vec<f32>> {
        let n = TEST_SR as usize;
        let refb: Vec<f32> = (0..n)
            .map(|i| (i as f32 * 2.0 * std::f32::consts::PI * 100.0 / TEST_SR as f32).sin())
            .collect();
        let meas: Vec<f32> = refb.iter().map(|s| s * 0.5).collect();
        vec![meas, refb]
    }

    fn call(
        ctx: &PairCtx,
        st: &PairState,
        statics: &FrameStatics,
        rings: &[Vec<f32>],
        peaks: &[Option<f64>],
    ) -> Option<(usize, Vec<Value>, Option<f64>)> {
        let drive_msg = json!({"on": false, "level_dbfs": Value::Null, "drivable": false});
        let cols: Vec<Option<Vec<ac_core::visualize::mtw::splice::Column>>> = vec![None];
        let settled: Vec<Vec<bool>> = vec![Vec::new()];
        // Assembly reads a held estimate, so the estimate is computed
        // here — the same call the session makes when a ring crosses a
        // block boundary.
        let key = AnalysisKey {
            dropped: 0,
            n_blocks: 4,
            delay: st.delay.map(|l| l.samples).unwrap_or(0),
            mc_enabled: false,
        };
        let analysis = vec![analyse_pair(ctx, st, statics, rings, key, 0)];
        let tick = TickInputs {
            tick_peaks_dbfs: peaks,
            mc_enabled: false,
            drive_msg: &drive_msg,
            mtw_columns: &cols,
            mtw_settled: &settled,
            analysis: &analysis,
            n_channels: rings.len(),
        };
        build_pair_messages(ctx, st, statics, &tick)
    }

    #[test]
    fn frame_carries_channels_and_position() {
        let rings = test_rings();
        let (pos, batch, _) = call(
            &test_ctx(),
            &PairState::new(None),
            &test_statics(),
            &rings,
            &[Some(-6.0), Some(-3.0)],
        )
        .expect("full rings must produce a frame");
        assert_eq!(pos, 0);
        let f = &batch[0];
        assert_eq!(f["type"], "transfer_stream");
        assert_eq!(f["meas_channel"], 2);
        assert_eq!(f["ref_channel"], 5);
        assert_eq!(f["sr"], TEST_SR);
        assert_eq!(f["meas_peak_dbfs"], -6.0);
        assert_eq!(f["ref_peak_dbfs"], -3.0);
    }

    // `delay_locked` is the field that keeps a refused pair distinguishable
    // from a genuine 0-sample digital loopback (#216, #227) — both report
    // `delay_ms` 0.0, so the number alone cannot carry the difference.
    #[test]
    fn unlocked_pair_reports_delay_locked_false() {
        let rings = test_rings();
        let (_, batch, _) = call(
            &test_ctx(),
            &PairState::new(None),
            &test_statics(),
            &rings,
            &[None, None],
        )
        .unwrap();
        assert_eq!(batch[0]["delay_locked"], false);
    }

    #[test]
    fn locked_pair_reports_delay_locked_true() {
        let rings = test_rings();
        let mut st = PairState::new(None);
        st.delay = Some(Lock {
            samples: 0,
            driving: true,
        });
        let (_, batch, _) = call(&test_ctx(), &st, &test_statics(), &rings, &[None, None]).unwrap();
        assert_eq!(batch[0]["delay_locked"], true);
    }

    // The count that separates "warming up" from "refusing" for the fault
    // indicator (#238). It is read straight off `PairState`, so a frame
    // must echo whatever the estimator has recorded.
    #[test]
    fn frame_echoes_attempt_count() {
        let rings = test_rings();
        let mut st = PairState::new(None);
        st.attempts = 7;
        let (_, batch, _) = call(&test_ctx(), &st, &test_statics(), &rings, &[None, None]).unwrap();
        assert_eq!(batch[0]["delay_attempts"], 7);
    }

    // Digital silence is `-inf` dBFS, which serde_json cannot serialise.
    // It has to reach the wire as null, not as a substituted number.
    #[test]
    fn silent_channel_peak_is_null_not_a_number() {
        let rings = test_rings();
        let (_, batch, _) = call(
            &test_ctx(),
            &PairState::new(None),
            &test_statics(),
            &rings,
            &[None, Some(-3.0)],
        )
        .unwrap();
        assert!(batch[0]["meas_peak_dbfs"].is_null());
    }

    // An uncalibrated pair must say so on both legs, and publish no SPL —
    // not a zero, which would read as a real 0 dB SPL measurement.
    #[test]
    fn uncalibrated_pair_tags_none_and_publishes_no_spl() {
        let rings = test_rings();
        let (_, batch, spl_raw) = call(
            &test_ctx(),
            &PairState::new(None),
            &test_statics(),
            &rings,
            &[None, None],
        )
        .unwrap();
        assert!(spl_raw.is_none());
        assert!(batch[0]["spl"].is_null());
        for leg in ["meas", "ref"] {
            let t = &batch[0]["cal_tags"][leg];
            assert_eq!(t["voltage"], "none", "{leg}");
            assert_eq!(t["spl"], "none", "{leg}");
            assert_eq!(t["mic_curve"], "none", "{leg}");
        }
    }

    // The ladder is additive: a pair with no columns yet publishes a frame
    // with `mtw` null, never a frame withheld or a partial ladder.
    #[test]
    fn absent_ladder_publishes_null_not_a_withheld_frame() {
        let rings = test_rings();
        let (_, batch, _) = call(
            &test_ctx(),
            &PairState::new(None),
            &test_statics(),
            &rings,
            &[None, None],
        )
        .unwrap();
        assert!(batch[0]["mtw"].is_null());
    }

    // Phase 4b sidecar rides along with the frame, from the same H1 result.
    #[test]
    fn ir_sidecar_accompanies_the_frame() {
        let rings = test_rings();
        let (_, batch, _) = call(
            &test_ctx(),
            &PairState::new(None),
            &test_statics(),
            &rings,
            &[None, None],
        )
        .unwrap();
        assert_eq!(batch.len(), 2, "frame + IR sidecar");
        assert_eq!(batch[1]["type"], "visualize/ir");
        assert_eq!(batch[1]["meas_channel"], 2);
        assert!(batch[1]["samples"]
            .as_array()
            .is_some_and(|a| !a.is_empty()));
    }

    // A pair whose channels are not in this tick's rings is dropped, not
    // published half-built.
    #[test]
    fn missing_channel_buffer_drops_the_pair() {
        let rings = test_rings();
        let mut ctx = test_ctx();
        ctx.ri = 9;
        assert!(call(
            &ctx,
            &PairState::new(None),
            &test_statics(),
            &rings,
            &[None, None]
        )
        .is_none());
    }
}

/// The positive control #208 was closed without.
///
/// `work/planning/state-live-spectrum.md` records the gap in as many words:
/// the A/B used a 6 s level step, which is longer than the analysis window,
/// so its edge gives a monotone ramp on *both* builds and cannot excite the
/// symptom. These tests use a burst **shorter than one Welch block** and
/// score only the ticks where it sits entirely inside the analysis window —
/// there the total energy is constant, so a sound estimator must report a
/// flat line and every dB of spread is artifact.
///
/// Both drains run on the same stream in the same test, because "pinned, not
/// sliding" is only a claim if the rejected implementation is measured next
/// to it.
#[cfg(test)]
mod pinned_window_tests {
    use super::{drain_to_block_lattice, Window};

    const SR: u32 = 8_000;
    const BURST_START_S: f64 = 1.0;
    const BURST_LEN_S: f64 = 0.25; // shorter than the 1 s block — the whole point

    /// The production geometry, from the production type — not a second
    /// copy of the arithmetic. A window this test derived for itself could
    /// stay green while the session's own moved out from under it.
    fn params() -> Window {
        Window::new(SR, 4)
    }

    /// Deterministic broadband burst in silence. No rng dependency: a
    /// fixed-seed LCG keeps this test reproducible across toolchains.
    fn burst_stream(total_s: f64) -> Vec<f32> {
        let n = (total_s * SR as f64) as usize;
        let mut x = vec![0.0f32; n];
        let a = (BURST_START_S * SR as f64) as usize;
        let b = ((BURST_START_S + BURST_LEN_S) * SR as f64) as usize;
        let mut s: u32 = 0x1234_5678;
        for v in x[a..b].iter_mut() {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            *v = ((s >> 8) as f32 / 8_388_608.0 - 1.0) * 0.1;
        }
        x
    }

    /// The drain this replaced: trim to an exact length every tick, which
    /// slides the block grid across the audio.
    fn drain_exact(ring: &mut Vec<f32>, target_total: usize, _step: usize) {
        if ring.len() > target_total {
            let d = ring.len() - target_total;
            ring.drain(..d);
        }
    }

    /// Broadband level of one published frame, in dB. Uses the real
    /// estimator, not a reimplementation of it.
    fn level_db(ring: &[f32]) -> f64 {
        let r = ac_core::visualize::transfer::h1_estimate_with_delay(ring, ring, SR, 0);
        let p: f64 = r.meas_amp.iter().map(|a| a * a).sum();
        10.0 * p.max(1e-30).log10()
    }

    /// Run the worker's ring loop with `drain`, returning `(t_s, level_db)`
    /// for every tick that would have published a frame.
    fn run(drain: fn(&mut Vec<f32>, usize, usize)) -> Vec<(f64, f64)> {
        let w = params();
        let (nperseg, step, n_averages, target_total) =
            (w.nperseg, w.step, w.n_averages, w.target_total());
        let chunk = (0.05 * SR as f64) as usize;
        let x = burst_stream(6.0);
        let mut ring: Vec<f32> = Vec::new();
        let mut out = Vec::new();
        let mut t = 0usize;
        while t + chunk <= x.len() {
            ring.extend_from_slice(&x[t..t + chunk]);
            drain(&mut ring, target_total, step);
            t += chunk;
            // The production warmup gate, verbatim: one segment.
            if ring.len() < nperseg {
                continue;
            }
            // Score only frames at full N. The artifact under test is
            // re-weighting at a *constant* block count; a frame from a
            // still-filling window legitimately reports a different level, and
            // mixing those in would let a real defect hide behind honest
            // settling. Mirrors the frame's own `n_averages`.
            if (ring.len() - nperseg) / step + 1 != n_averages {
                continue;
            }
            out.push((t as f64 / SR as f64, level_db(&ring)));
        }
        out
    }

    /// dB spread over the ticks where the burst is wholly inside the window.
    fn spread_while_fully_inside(series: &[(f64, f64)]) -> (f64, usize) {
        let win_s = params().target_total() as f64 / SR as f64;
        // Window covers [t - win_s, t]; burst is inside once t has passed its
        // end and until t - win_s passes its start. Trim a tick off each edge
        // so quantisation of the chunk grid is not scored.
        let lo = BURST_START_S + BURST_LEN_S + 0.05;
        let hi = BURST_START_S + win_s - 0.05;
        let v: Vec<f64> = series
            .iter()
            .filter(|(t, _)| *t >= lo && *t <= hi)
            .map(|(_, d)| *d)
            .collect();
        if v.len() < 3 {
            return (f64::NAN, v.len());
        }
        let max = v.iter().cloned().fold(f64::MIN, f64::max);
        let min = v.iter().cloned().fold(f64::MAX, f64::min);
        (max - min, v.len())
    }

    /// The fix: a burst held entirely inside the window reports one level.
    #[test]
    fn pinned_grid_holds_a_stationary_burst_at_a_constant_level() {
        let (spread, n) = spread_while_fully_inside(&run(drain_to_block_lattice));
        assert!(
            n >= 10,
            "only {n} scored frames — the control is not running"
        );
        assert!(
            spread < 0.05,
            "pinned grid still moved a constant burst by {spread:.2} dB over {n} frames"
        );
    }

    /// The control. If this ever stops failing, the burst has become
    /// incapable of exciting the defect and the test above proves nothing.
    #[test]
    fn exact_trim_drain_reweights_the_same_burst() {
        let (spread, n) = spread_while_fully_inside(&run(drain_exact));
        assert!(
            n >= 10,
            "only {n} scored frames — the control is not running"
        );
        assert!(
            spread > 3.0,
            "the discarded exact-trim drain moved the burst by only {spread:.2} dB \
             over {n} frames; this stimulus can no longer excite #208, so the \
             pinned-grid test above is not evidence of anything"
        );
    }

    /// The drain's own contract: length lands in `[target_total,
    /// target_total + step)`, which is what fits exactly `n_averages` blocks.
    #[test]
    fn drain_keeps_the_ring_on_the_block_lattice() {
        let w = params();
        let (nperseg, step, n_avg, target_total) =
            (w.nperseg, w.step, w.n_averages, w.target_total());
        let mut ring: Vec<f32> = Vec::new();
        for _ in 0..400 {
            ring.extend(std::iter::repeat_n(0.0f32, 137)); // coprime with step
            drain_to_block_lattice(&mut ring, target_total, step);
            if ring.len() >= target_total {
                assert!(
                    ring.len() < target_total + step,
                    "ring grew to {} beyond the window",
                    ring.len()
                );
                let blocks = (ring.len() - nperseg) / step + 1;
                assert_eq!(blocks, n_avg, "ring of {} fits {blocks} blocks", ring.len());
            }
        }
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
