//! Calibrate state machine: plays reference tone, prompts for output and
//! input Vrms readings, writes cal.json. Routes the worker's terminal frame
//! to `done` or `error` based on the save outcome.
//!
//! One file per command, mirroring the calibration layers in
//! `ac_core::shared::calibration`: this module owns the voltage prompts
//! (`calibrate` / `cal_reply`), [`tau`] owns the interface-latency sweep
//! the same run piggybacks on, [`spl`] the pistonphone reference, and
//! [`mic_curve`] the frequency-response curve.

use std::sync::atomic::Ordering;

use serde_json::{json, Value};

use ac_core::config::Config;
use ac_core::shared::calibration::Calibration;

use crate::audio::make_engine;
use crate::server::ServerState;

use super::{
    apply_drive_ceiling, busy_guard, capture_rms, cfg_guard, read_dmm_vrms, resolve_input,
    resolve_output_by_channel, rms_to_dbfs, send_pub, spawn_worker, wait_cal_reply, CalReply,
};

mod mic_curve;
mod spl;
mod tau;

pub use mic_curve::{calibrate_mic_curve, set_mic_correction_enabled};
pub use spl::calibrate_spl;

use tau::{measure_tau_twice, tau_result, TAU_METHOD};

/// Channel pair a calibration command addresses: explicit fields win, the
/// session config supplies the rest. Spelled once because all four
/// handlers key the same `cal.json` entry, and a pair that disagreed
/// between two of them would write one channel's reading under another's
/// key.
fn channels_from(cmd: &Value, cfg: &Config) -> (u32, u32) {
    let out_ch = cmd
        .get("output_channel")
        .and_then(Value::as_u64)
        .unwrap_or(cfg.output_channel as u64) as u32;
    let in_ch = cmd
        .get("input_channel")
        .and_then(Value::as_u64)
        .unwrap_or(cfg.input_channel as u64) as u32;
    (out_ch, in_ch)
}

/// Resolve the capture port for `in_ch`, ignoring any sticky
/// `cfg.input_port`. A calibration run keys its entry by channel number,
/// so the port it actually captures on has to be derived from that same
/// number (#358) rather than from whatever the last command left behind.
fn resolve_input_by_channel(
    cfg: &Config,
    state: &ServerState,
    in_ch: u32,
) -> Result<String, String> {
    let mut cfg_in = cfg.clone();
    cfg_in.input_channel = in_ch;
    cfg_in.input_port = None;
    resolve_input(&cfg_in, state)
}

/// Emit the terminal frames every calibration worker ends with: the
/// `cal_done` payload, then `done` or `error` depending on the save.
///
/// The split matters to clients: the Python client treats `done` vs
/// `error` as the authoritative signal and would otherwise report a failed
/// save as a successful calibration.
fn finish_cal(
    pub_tx: &crossbeam_channel::Sender<Vec<u8>>,
    cmd: &'static str,
    key: &str,
    mut cal_done_frame: Value,
    save_err: Option<String>,
) {
    if let Some(ref e) = save_err {
        cal_done_frame["error"] = json!(e);
    }
    send_pub(pub_tx, "cal_done", &cal_done_frame);
    match save_err {
        Some(e) => send_pub(
            pub_tx,
            "error",
            &json!({
                "cmd":     cmd,
                "message": format!("save failed: {e}"),
            }),
        ),
        None => send_pub(
            pub_tx,
            "done",
            &json!({
                "cmd": cmd,
                "key": key,
            }),
        ),
    }
}

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

pub fn calibrate(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "calibrate");
    cfg_guard!(state);
    let cfg = state.cfg.lock().unwrap().clone();
    let (out_ch, in_ch) = channels_from(cmd, &cfg);
    // #360: an omitted `ref_dbfs` becomes "whatever this session's ceiling
    // is", not a second hardcoded number (-10.0) that has to be kept in
    // sync with `drive_max_dbfs`'s own default by convention. An
    // explicitly-passed value is then clamped the same way — defense in
    // depth, and the single binding every downstream quantity (amp,
    // ref_amp, out_scale, in_scale, cal.ref_dbfs) derives from, so the
    // calibration stays internally consistent (measured-and-scaled-at-the-
    // same-level).
    let ref_dbfs = cmd
        .get("ref_dbfs")
        .and_then(Value::as_f64)
        .unwrap_or(cfg.drive_max_dbfs);
    let ref_dbfs = apply_drive_ceiling(cfg.drive_max_dbfs, ref_dbfs);

    let pub_tx = state.pub_tx.clone();
    let fake = state.fake_audio;
    // #358: `out_ch` above already keys the saved calibration entry — it
    // must also decide which port the tone actually leaves on, or the key
    // and the port can name different channels (rig repro: key `out1_in3`,
    // tone on the sticky-resolved `cfg.output_channel` port, capture on a
    // dead loopback return). `resolve_output_by_channel` is the same
    // explicit-wins-over-sticky rule `generate`'s multi-channel form
    // already uses (`resolve_channels` in `audio/generate.rs`): an
    // explicit channel that differs from `cfg.output_channel` is resolved
    // by index, bypassing `cfg.output_port`; a channel that matches falls
    // through to `resolve_output`, so an unrelated sticky port is left
    // alone when the caller didn't ask to override anything.
    let out_port = match resolve_output_by_channel(&cfg, state, out_ch) {
        Ok(p) => p,
        Err(e) => return json!({"ok": false, "error": e}),
    };
    let in_port = match resolve_input_by_channel(&cfg, state, in_ch) {
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

        // Fallback conditions for the `cal_done` wire frame when τ isn't
        // measured this run (low SNR, or a lifecycle error before any
        // conditions were captured) — ZMQ.md requires `tau_sample_rate` /
        // `tau_period_size` present regardless of `tau_state`.
        let fallback_sample_rate = eng.sample_rate();
        let fallback_period_size = eng.period_size();

        eng.set_silence();
        eng.stop();

        // τ (interface latency, #281/#347) — not prompt-driven, so it does
        // not add a third interactive step. Always attempted regardless of
        // the loopback state step 2 established (#368: τ used to be gated
        // on that captured-level proxy; it is now gated on its own
        // deconvolved peak's SNR instead, inside `measure_tau` itself) and
        // regardless of whether either voltage prompt was answered or
        // skipped — the cheap-refresh path (#279: both prompts skipped)
        // still refreshes τ. #347: a single reading is not
        // a measurement of τ on this stack, so this now runs two
        // independent client lifecycles (`measure_tau_twice`), decoupled
        // from the voltage-cal `eng` above (already stopped) — see that
        // function's doc for why the lifecycle boundary matters.
        let ref_amp = ac_core::shared::generator::dbfs_to_amplitude(ref_dbfs);
        let tau_outcome =
            tau_result(|| measure_tau_twice(fake, cfg.device, &out_port, &in_port, ref_amp));

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
        if let Some(entry) = tau_outcome.stored_entry(TAU_METHOD) {
            cal.tau_history.push(entry);
        }
        let save_err = cal.save(None).err().map(|e| e.to_string());

        let key = cal.key();
        let (tau_sample_rate, tau_period_size) = match tau_outcome.conditions() {
            Some(c) => (c.sample_rate, c.period_size),
            None => (fallback_sample_rate, fallback_period_size),
        };
        // Values reported are what is now stored, not what this run
        // measured — with `*_state` naming which of the three that is.
        let mut cal_done_frame = json!({
            "key":                  key,
            "vrms_at_0dbfs_out":    cal.vrms_at_0dbfs_out,
            "vrms_at_0dbfs_in":     cal.vrms_at_0dbfs_in,
            "out_state":            out_state,
            "in_state":             in_state,
            "tau_sample_rate":      tau_sample_rate,
            "tau_period_size":      tau_period_size,
            // #370: the port names actually resolved server-side for this
            // run, not the client's copy of the request — the reporter's
            // repro was ten identical readings because the ports actually
            // in use never surfaced anywhere.
            "input_port":           in_port,
            "output_port":          out_port,
        });
        tau_outcome.write_frame(&mut cal_done_frame);
        finish_cal(&pub_tx, "calibrate", &key, cal_done_frame, save_err);
    });

    {
        let mut workers = state.workers.lock().unwrap();
        workers.insert("calibrate".to_string(), worker);
    }
    json!({"ok": true, "ref_dbfs": ref_dbfs})
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
