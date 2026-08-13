//! Calibrate state machine: plays reference tone, prompts for output and
//! input Vrms readings, writes cal.json. Routes the worker's terminal frame
//! to `done` or `error` based on the save outcome.

use std::sync::atomic::Ordering;

use serde_json::{json, Value};

use ac_core::measurement::sweep::{
    deconvolve_full, extract_irs, inverse_sweep, log_sweep, SweepParams,
};
use ac_core::shared::calibration::{Calibration, MicResponse, TauConditions, TauEntry};

use crate::audio::{make_engine, AudioEngine};
use crate::server::ServerState;

use super::{
    busy_guard, capture_rms, read_dmm_vrms, resolve_input, resolve_output, rms_to_dbfs, send_pub,
    spawn_worker, wait_cal_reply, CalReply,
};

/// Apply one prompt's reply to one stored voltage field and report which
/// of the three states the field ends up in.
///
/// `"measured"` — a reading was supplied and scaled into the field.
/// `"unchanged"` — nothing was supplied; the previously stored value stands.
/// `"absent"` — the field holds no value, either because it never did or
/// because the reply asked for it to be cleared.
///
/// Skipping must never write. That is the whole of #279: the old code
/// assigned `reading.map(..)` unconditionally, so a skipped prompt wrote
/// `None` over a good calibration and reported it as "not measured".
fn apply_cal_reading(field: &mut Option<f64>, reply: CalReply, scale: f64) -> &'static str {
    match reply {
        CalReply::Value(v) => {
            *field = Some(v * scale);
            "measured"
        }
        CalReply::Clear => {
            *field = None;
            "absent"
        }
        CalReply::Skip => {
            if field.is_some() {
                "unchanged"
            } else {
                "absent"
            }
        }
    }
}

/// Method tag stored on every [`TauEntry`] this handler produces.
const TAU_METHOD: &str = "farina_short_ess";

/// Short-ESS parameters for the τ measurement (#281). Deliberately much
/// shorter than a `plot_ir` sweep — this only has to locate the linear-IR
/// peak, not resolve harmonics or a decay tail. `TAU_WINDOW_LEN` / 2 sets
/// the largest round-trip delay this can report (≈85 ms at 48 kHz), and
/// `TAU_TAIL_S` is sized to keep that whole window inside the capture.
const TAU_F1_HZ: f64 = 100.0;
const TAU_DURATION_S: f64 = 0.2;
const TAU_TAIL_S: f64 = 0.15;
const TAU_WINDOW_LEN: usize = 8192;

/// Play a short ESS, deconvolve it, and return the interface round-trip
/// delay in seconds (peak of the linear IR, converted from samples).
///
/// Reuses the Farina machinery from `ac_core::measurement::sweep` exactly
/// as `plot_ir` does — see `handlers/audio/plot.rs` for the longer-form
/// version of the same technique.
fn measure_tau(eng: &mut dyn AudioEngine, amp: f64) -> anyhow::Result<f64> {
    let sr = eng.sample_rate();
    let f2_hz = (sr as f64 * 0.45).min(20_000.0);
    let params = SweepParams {
        f1_hz: TAU_F1_HZ,
        f2_hz,
        duration_s: TAU_DURATION_S,
        sample_rate: sr,
    };
    let sweep = log_sweep(&params)?;
    let amp = amp as f32;
    let scaled: Vec<f32> = sweep.iter().map(|&s| s * amp).collect();
    let captured = eng.play_and_capture(&scaled, TAU_TAIL_S)?;
    let inv = inverse_sweep(&params)?;
    let full = deconvolve_full(&captured, &inv);
    let irs = extract_irs(&full, &params, 1, TAU_WINDOW_LEN)?;
    let (peak_idx, _) = irs
        .linear
        .iter()
        .enumerate()
        .map(|(i, v)| (i, *v))
        .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
        .ok_or_else(|| anyhow::anyhow!("empty IR from τ sweep"))?;
    let offset_samples = peak_idx as i64 - (TAU_WINDOW_LEN / 2) as i64;
    Ok(offset_samples as f64 / sr as f64)
}

/// Turn the loopback flag established at step 2 into the `tau_state` /
/// `tau_s` / `tau_error` triple `calibrate` reports — the exact decision
/// #281 QA flagged as untestable because it was inlined in the worker
/// closure, reachable only through a full daemon spawn. `measure` is only
/// called when `is_loopback`, matching the worker's original behaviour of
/// never running the τ sweep on a run with no loopback detected.
fn tau_result(
    is_loopback: bool,
    measure: impl FnOnce() -> anyhow::Result<f64>,
) -> (&'static str, Option<f64>, Option<String>) {
    if !is_loopback {
        return ("not_measured_no_loopback", None, None);
    }
    match measure() {
        Ok(t) => ("measured", Some(t), None),
        Err(e) => (
            "error",
            None,
            Some(format!("\u{3c4} measurement failed: {e}")),
        ),
    }
}

pub fn calibrate(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "calibrate");
    let cfg = state.cfg.lock().unwrap().clone();
    let out_ch = cmd
        .get("output_channel")
        .and_then(Value::as_u64)
        .unwrap_or(cfg.output_channel as u64) as u32;
    let in_ch = cmd
        .get("input_channel")
        .and_then(Value::as_u64)
        .unwrap_or(cfg.input_channel as u64) as u32;
    let ref_dbfs = cmd.get("ref_dbfs").and_then(Value::as_f64).unwrap_or(-10.0);

    let pub_tx = state.pub_tx.clone();
    let fake = state.fake_audio;
    let out_port = match resolve_output(&cfg, state) {
        Ok(p) => p,
        Err(e) => return json!({"ok": false, "error": e}),
    };
    let mut cfg_in = cfg.clone();
    cfg_in.input_channel = in_ch;
    cfg_in.input_port = None;
    let in_port = match resolve_input(&cfg_in, state) {
        Ok(p) => p,
        Err(e) => return json!({"ok": false, "error": e}),
    };
    let cal_reply_tx = state.cal_reply_tx.clone();

    let worker = spawn_worker(state, "calibrate", move |stop| {
        let mut eng = make_engine(fake);
        // Open both directions: output for the reference tone, input so
        // step 2 can measure the actual ADC dBFS instead of assuming
        // unity-gain loopback when scaling the user's DMM reading.
        if let Err(e) = eng.start(std::slice::from_ref(&out_port), Some(&in_port)) {
            send_pub(
                &pub_tx,
                "error",
                &json!({"cmd":"calibrate","message":format!("{e}")}),
            );
            return;
        }
        let amp = ac_core::shared::generator::dbfs_to_amplitude(ref_dbfs);
        eng.set_tone(1000.0, amp);

        // Step 1 — output voltage. User's DMM reading is the analog
        // Vrms at the DAC output while the tone is playing at `ref_dbfs`,
        // so the projected 0 dBFS Vrms is `reading / amp(ref_dbfs)`.
        let (tx1, rx1) = crossbeam_channel::bounded(1);
        *cal_reply_tx.lock().unwrap() = Some(tx1);
        let dmm_v1 = cfg.dmm_host.as_deref().and_then(|h| read_dmm_vrms(h, 3));
        send_pub(
            &pub_tx,
            "cal_prompt",
            &json!({
                "step":      1,
                "text":      format!(
                    "Output cal — measure DAC output Vrms with DMM (1 kHz @ {ref_dbfs:.1} dBFS). \
                     Enter reading, or press Enter to keep the stored value."),
                "dmm_vrms":  dmm_v1,
                "ref_dbfs":  ref_dbfs,
            }),
        );
        let out_reply = wait_cal_reply(&rx1, &stop, 120);
        *cal_reply_tx.lock().unwrap() = None;
        if stop.load(Ordering::Relaxed) {
            eng.set_silence();
            eng.stop();
            return;
        }

        // Step 2 — input voltage. We need the *captured* input dBFS to
        // convert the user's DMM reading into `vrms_at_0dbfs_in`, so
        // capture briefly before prompting (after a short settle so the
        // fresh tone fills the ring). If the input is silent / unwired,
        // fall back to assuming loopback (`captured_dbfs = ref_dbfs`).
        eng.flush_capture();
        std::thread::sleep(std::time::Duration::from_millis(150));
        let captured_rms = capture_rms(&mut *eng, 0.3);
        let captured_dbfs = rms_to_dbfs(captured_rms);
        let in_dbfs_for_scale = if captured_dbfs > -80.0 {
            captured_dbfs
        } else {
            ref_dbfs
        };

        // Loopback heuristic: a unity-gain DAC→ADC loop puts captured
        // RMS-dBFS at `ref_dbfs - 3.01` (the sine peak/RMS factor). When
        // it lines up within ±2 dB, the user's output reading IS the
        // input reading — pre-fill it so they just hit Enter and the
        // prompt stops feeling redundant.
        let loopback_dbfs = ref_dbfs - 20.0 * 2f64.sqrt().log10();
        let is_loopback = (captured_dbfs - loopback_dbfs).abs() <= 2.0;
        let dmm_v2_real = cfg.dmm_host.as_deref().and_then(|h| read_dmm_vrms(h, 3));
        let dmm_v2 = dmm_v2_real.or(if is_loopback { out_reply.value() } else { None });

        let prompt_text = if is_loopback {
            format!(
                "Input cal — loopback detected ({captured_dbfs:.1} dBFS captured). \
                 Press Enter to reuse the output reading, or override (q to cancel)."
            )
        } else {
            format!(
                "Input cal — measure ADC input Vrms with DMM (captured {captured_dbfs:.1} dBFS). \
                 Enter reading, or press Enter to keep the stored value."
            )
        };
        let (tx2, rx2) = crossbeam_channel::bounded(1);
        *cal_reply_tx.lock().unwrap() = Some(tx2);
        send_pub(
            &pub_tx,
            "cal_prompt",
            &json!({
                "step":          2,
                "text":          prompt_text,
                "dmm_vrms":      dmm_v2,
                "captured_dbfs": captured_dbfs,
                "loopback":      is_loopback,
            }),
        );
        let in_reply = wait_cal_reply(&rx2, &stop, 120);
        *cal_reply_tx.lock().unwrap() = None;
        // Symmetric with the step-1 check above: a cancel here must also
        // abort the run rather than fall through to save. Before this fix
        // a `q` at the second prompt still committed step 1's reading and
        // `ref_dbfs`, while the CLI told the operator "Calibration
        // cancelled." (#294 QA correctness issue 1).
        if stop.load(Ordering::Relaxed) {
            eng.set_silence();
            eng.stop();
            return;
        }

        // τ (interface latency, #281) — not prompt-driven, so it piggybacks
        // on the loopback state established above rather than adding a
        // third interactive step. Runs here, before `eng.stop()`, because
        // `period_size()` needs a live client. Measured whenever a loopback
        // was detected this run, regardless of whether either voltage
        // prompt was answered or skipped — the cheap-refresh path (#279:
        // both prompts skipped) still refreshes τ.
        let tau_conditions = TauConditions {
            device: cfg.device,
            backend: eng.backend_name().to_string(),
            sample_rate: eng.sample_rate(),
            period_size: eng.period_size(),
            output_port: out_port.clone(),
            input_port: in_port.clone(),
        };
        let (tau_state, tau_s, tau_err) = tau_result(is_loopback, || {
            let amp = ac_core::shared::generator::dbfs_to_amplitude(ref_dbfs);
            measure_tau(&mut *eng, amp)
        });

        eng.set_silence();
        eng.stop();

        // Convert from "Vrms at the played/captured dBFS" → "Vrms at 0 dBFS".
        let out_scale = 1.0 / ac_core::shared::generator::dbfs_to_amplitude(ref_dbfs);
        let in_scale = 1.0 / ac_core::shared::generator::dbfs_to_amplitude(in_dbfs_for_scale);

        // Load existing entry to preserve unrelated fields (notably the
        // SPL pistonphone reading set by `calibrate_spl`), and to preserve
        // a voltage field whose prompt was skipped — a re-check of one leg
        // must not cost the other.
        let mut cal = Calibration::load_or_new(out_ch, in_ch, None);
        let out_state = apply_cal_reading(&mut cal.vrms_at_0dbfs_out, out_reply, out_scale);
        let in_state = apply_cal_reading(&mut cal.vrms_at_0dbfs_in, in_reply, in_scale);
        // `ref_dbfs` records the level the stored readings were taken at,
        // so it only moves when a reading did. A run that skipped both
        // prompts measured nothing and must leave the entry as it found it.
        if out_state == "measured" || in_state == "measured" {
            cal.ref_dbfs = ref_dbfs;
        }
        if tau_state == "measured" {
            cal.tau_history.push(TauEntry {
                conditions: tau_conditions.clone(),
                tau_s: tau_s.expect("tau_s is Some when tau_state is \"measured\""),
                measured_at: ac_core::shared::time::now_utc_iso8601(),
                method: TAU_METHOD.to_string(),
            });
        }
        let save_err = cal.save(None).err().map(|e| e.to_string());

        let key = cal.key();
        // Values reported are what is now stored, not what this run
        // measured — with `*_state` naming which of the three that is.
        let mut cal_done_frame = json!({
            "key":               key,
            "vrms_at_0dbfs_out": cal.vrms_at_0dbfs_out,
            "vrms_at_0dbfs_in":  cal.vrms_at_0dbfs_in,
            "out_state":         out_state,
            "in_state":          in_state,
            "tau_state":         tau_state,
            "tau_s":             tau_s,
            "tau_sample_rate":   tau_conditions.sample_rate,
            "tau_period_size":   tau_conditions.period_size,
        });
        if let Some(ref e) = tau_err {
            cal_done_frame["tau_error"] = json!(e);
        }
        if let Some(ref e) = save_err {
            cal_done_frame["error"] = json!(e);
        }
        send_pub(&pub_tx, "cal_done", &cal_done_frame);

        // Route terminal frame on save outcome so the Python client, which
        // treats `done` vs `error` as the authoritative signal, sees failures.
        match save_err {
            Some(e) => send_pub(
                &pub_tx,
                "error",
                &json!({
                    "cmd":     "calibrate",
                    "message": format!("save failed: {e}"),
                }),
            ),
            None => send_pub(
                &pub_tx,
                "done",
                &json!({
                    "cmd": "calibrate",
                    "key": key,
                }),
            ),
        }
    });

    {
        let mut workers = state.workers.lock().unwrap();
        workers.insert("calibrate".to_string(), worker);
    }
    json!({"ok": true})
}

pub fn cal_reply(state: &ServerState, cmd: &Value) -> Value {
    // `clear: true` is the only way to erase a stored value, and it has to
    // be asked for by name — a missing/null `vrms` means "I did not measure
    // this", which is not the same request (#279).
    let reply = if cmd.get("clear").and_then(Value::as_bool).unwrap_or(false) {
        CalReply::Clear
    } else {
        match cmd.get("vrms").and_then(Value::as_f64) {
            Some(v) => CalReply::Value(v),
            None => CalReply::Skip, // JSON null or absent
        }
    };
    let tx = state.cal_reply_tx.lock().unwrap();
    if let Some(ref t) = *tx {
        let _ = t.send(reply);
    }
    json!({"ok": true})
}

/// `calibrate_mic_curve` — attach (or clear) a mic frequency-response
/// correction curve on a channel.
///
/// CLI parses the .frd / .txt file and uploads the validated arrays via
/// this cmd, so the daemon never has to read user-supplied paths and
/// works the same way for local and remote daemons. Two operations:
/// `op = "set"` with `freqs_hz` + `gain_db` arrays, or `op = "clear"`
/// to drop a stored curve. Voltage and SPL fields on the same entry
/// stay untouched (`load_or_new` round-trip).
pub fn calibrate_mic_curve(state: &ServerState, cmd: &Value) -> Value {
    let cfg = state.cfg.lock().unwrap().clone();
    let out_ch = cmd
        .get("output_channel")
        .and_then(Value::as_u64)
        .unwrap_or(cfg.output_channel as u64) as u32;
    let in_ch = cmd
        .get("input_channel")
        .and_then(Value::as_u64)
        .unwrap_or(cfg.input_channel as u64) as u32;
    let op = cmd.get("op").and_then(Value::as_str).unwrap_or("set");

    let mut cal = Calibration::load_or_new(out_ch, in_ch, None);
    match op {
        "clear" => {
            cal.mic_response = None;
        }
        "set" => {
            let freqs: Option<Vec<f32>> = cmd.get("freqs_hz").and_then(Value::as_array).map(|a| {
                a.iter()
                    .filter_map(|v| v.as_f64().map(|v| v as f32))
                    .collect()
            });
            let gains: Option<Vec<f32>> = cmd.get("gain_db").and_then(Value::as_array).map(|a| {
                a.iter()
                    .filter_map(|v| v.as_f64().map(|v| v as f32))
                    .collect()
            });
            let (freqs_hz, gain_db) = match (freqs, gains) {
                (Some(f), Some(g)) if !f.is_empty() && f.len() == g.len() => (f, g),
                _ => {
                    return json!({"ok": false,
                    "error": "calibrate_mic_curve set: missing/mismatched freqs_hz/gain_db"})
                }
            };
            // Re-validate length bounds + monotonicity here too — the CLI
            // already parsed and validated, but a hostile or buggy client
            // shouldn't be able to write a malformed curve to disk.
            if freqs_hz.len() < MicResponse::MIN_POINTS {
                return json!({"ok": false,
                    "error": format!("curve too sparse: {} < {}", freqs_hz.len(), MicResponse::MIN_POINTS)});
            }
            if freqs_hz.len() > MicResponse::MAX_POINTS {
                return json!({"ok": false,
                    "error": format!("curve too dense: {} > {}", freqs_hz.len(), MicResponse::MAX_POINTS)});
            }
            for w in freqs_hz.windows(2) {
                if w[1] <= w[0] || !w[0].is_finite() || !w[1].is_finite() {
                    return json!({"ok": false,
                        "error": "freqs_hz must be strictly increasing and finite"});
                }
            }
            for &g in &gain_db {
                if !g.is_finite() {
                    return json!({"ok": false,
                        "error": "gain_db values must be finite"});
                }
            }
            let source_path = cmd
                .get("source_path")
                .and_then(Value::as_str)
                .map(str::to_string);
            cal.mic_response = Some(MicResponse {
                freqs_hz,
                gain_db,
                source_path,
                imported_at: ac_core::shared::time::now_utc_iso8601(),
            });
        }
        other => {
            return json!({"ok": false,
                "error": format!("calibrate_mic_curve: unknown op {other:?}, expected set/clear")});
        }
    }
    if let Err(e) = cal.save(None) {
        return json!({"ok": false, "error": format!("save failed: {e}")});
    }
    json!({
        "ok":      true,
        "key":     cal.key(),
        "loaded":  cal.mic_response.as_ref().map(|r| r.freqs_hz.len()).unwrap_or(0),
    })
}

/// `set_mic_correction_enabled` — toggle daemon-side mic-curve application.
/// When disabled, monitor frames carry the raw uncorrected magnitudes (a
/// loaded curve does not vanish — just stops being applied). Used by the
/// UI's `Shift+M` keybinding for diagnostics. The flag is process-wide;
/// per-channel curves are still respected, the toggle just gates them.
pub fn set_mic_correction_enabled(state: &ServerState, cmd: &Value) -> Value {
    let enabled = cmd.get("enabled").and_then(Value::as_bool).unwrap_or(true);
    state
        .mic_correction_enabled
        .store(enabled, std::sync::atomic::Ordering::Relaxed);
    json!({"ok": true, "enabled": enabled})
}

/// `calibrate_spl` — pistonphone-reference SPL calibration.
///
/// Captures ~1 s of audio on the input channel, computes its RMS in dBFS,
/// and stores that value as `mic_sensitivity_dbfs_at_94db_spl` so all
/// future dBFS readings on this channel can convert to dB SPL via
/// `dbspl = dbfs - mic_sens_dbfs + 94.0`. Voltage-cal fields on the same
/// entry are preserved.
///
/// Wire flow mirrors `calibrate`:
///   1. emit `cal_prompt` asking the user to apply the pistonphone,
///   2. wait for `cal_reply` (any value — only the act of replying is
///      meaningful; the user has had time to seat the calibrator),
///   3. capture, compute, save,
///   4. emit `cal_done` with the captured dBFS, then `done` / `error`.
pub fn calibrate_spl(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "calibrate_spl");
    let cfg = state.cfg.lock().unwrap().clone();
    let out_ch = cmd
        .get("output_channel")
        .and_then(Value::as_u64)
        .unwrap_or(cfg.output_channel as u64) as u32;
    let in_ch = cmd
        .get("input_channel")
        .and_then(Value::as_u64)
        .unwrap_or(cfg.input_channel as u64) as u32;
    let capture_s = cmd.get("capture_s").and_then(Value::as_f64).unwrap_or(1.0);

    let pub_tx = state.pub_tx.clone();
    let fake = state.fake_audio;
    let mut cfg_in = cfg.clone();
    cfg_in.input_channel = in_ch;
    cfg_in.input_port = None;
    let in_port = match resolve_input(&cfg_in, state) {
        Ok(p) => p,
        Err(e) => return json!({"ok": false, "error": e}),
    };
    let cal_reply_tx = state.cal_reply_tx.clone();

    let worker = spawn_worker(state, "calibrate_spl", move |stop| {
        let mut eng = make_engine(fake);
        if let Err(e) = eng.start(&[], Some(&in_port)) {
            send_pub(
                &pub_tx,
                "error",
                &json!({"cmd":"calibrate_spl","message":format!("{e}")}),
            );
            return;
        }
        // Prompt the user to seat the pistonphone, wait for the green
        // light. The reply value itself is unused — we just need a
        // synchronisation point so the capture sees the reference tone,
        // not silence or seating noise.
        let (tx, rx) = crossbeam_channel::bounded(1);
        *cal_reply_tx.lock().unwrap() = Some(tx);
        send_pub(
            &pub_tx,
            "cal_prompt",
            &json!({
                "step":     1,
                "text":     "Apply 94 dB SPL pistonphone reference, press Enter when ready (q to cancel).",
                "kind":     "spl",
            }),
        );
        let _ = wait_cal_reply(&rx, &stop, 300);
        *cal_reply_tx.lock().unwrap() = None;
        if stop.load(Ordering::Relaxed) {
            eng.stop();
            return;
        }

        // Brief settling period, then a clean capture.
        eng.flush_capture();
        std::thread::sleep(std::time::Duration::from_millis(100));
        let rms = capture_rms(&mut *eng, capture_s);
        let dbfs = rms_to_dbfs(rms);
        eng.stop();

        let mut cal = Calibration::load_or_new(out_ch, in_ch, None);
        cal.mic_sensitivity_dbfs_at_94db_spl = Some(dbfs);
        let save_err = cal.save(None).err().map(|e| e.to_string());

        let key = cal.key();
        let mut cal_done_frame = json!({
            "key":                              key,
            "mic_sensitivity_dbfs_at_94db_spl": dbfs,
            "kind":                             "spl",
        });
        if let Some(ref e) = save_err {
            cal_done_frame["error"] = json!(e);
        }
        send_pub(&pub_tx, "cal_done", &cal_done_frame);

        match save_err {
            Some(e) => send_pub(
                &pub_tx,
                "error",
                &json!({
                    "cmd":     "calibrate_spl",
                    "message": format!("save failed: {e}"),
                }),
            ),
            None => send_pub(
                &pub_tx,
                "done",
                &json!({
                    "cmd": "calibrate_spl",
                    "key": key,
                }),
            ),
        }
    });

    {
        let mut workers = state.workers.lock().unwrap();
        workers.insert("calibrate_spl".to_string(), worker);
    }
    json!({"ok": true})
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #281 QA correctness issue 3: the no-loopback path is hard to drive
    /// end-to-end under `--fake-audio` (the fake backend's step-2 capture
    /// always reads as loopback-shaped), so pin the decision down directly
    /// instead. `measure` must not run at all when there's no loopback.
    #[test]
    fn tau_result_no_loopback_short_circuits_without_measuring() {
        let mut called = false;
        let (state, tau_s, err) = tau_result(false, || {
            called = true;
            Ok(0.001)
        });
        assert_eq!(state, "not_measured_no_loopback");
        assert_eq!(tau_s, None);
        assert_eq!(err, None);
        assert!(
            !called,
            "measure must not run when no loopback was detected"
        );
    }

    #[test]
    fn tau_result_loopback_ok_reports_measured() {
        let (state, tau_s, err) = tau_result(true, || Ok(0.000_667));
        assert_eq!(state, "measured");
        assert_eq!(tau_s, Some(0.000_667));
        assert_eq!(err, None);
    }

    #[test]
    fn tau_result_loopback_err_reports_error_state_and_message() {
        let (state, tau_s, err) = tau_result(true, || Err(anyhow::anyhow!("timeout")));
        assert_eq!(state, "error");
        assert_eq!(tau_s, None);
        let msg = err.expect("error message present on failure");
        assert!(
            msg.contains("timeout"),
            "error message should name the failure: {msg}"
        );
    }
}
