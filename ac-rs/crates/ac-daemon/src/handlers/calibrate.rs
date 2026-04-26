//! Calibrate state machine: plays reference tone, prompts for output and
//! input Vrms readings, writes cal.json. Routes the worker's terminal frame
//! to `done` or `error` based on the save outcome.

use std::sync::atomic::Ordering;

use serde_json::{json, Value};

use ac_core::shared::calibration::Calibration;

use crate::audio::make_engine;
use crate::server::ServerState;

use super::{
    busy_guard, capture_rms, read_dmm_vrms, resolve_input, resolve_output, rms_to_dbfs,
    send_pub, spawn_worker, wait_cal_reply,
};

pub fn calibrate(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "calibrate");
    let cfg    = state.cfg.lock().unwrap().clone();
    let out_ch = cmd.get("output_channel")
        .and_then(Value::as_u64)
        .unwrap_or(cfg.output_channel as u64) as u32;
    let in_ch  = cmd.get("input_channel")
        .and_then(Value::as_u64)
        .unwrap_or(cfg.input_channel as u64) as u32;
    let ref_dbfs = cmd.get("ref_dbfs").and_then(Value::as_f64).unwrap_or(-10.0);

    let pub_tx       = state.pub_tx.clone();
    let fake         = state.fake_audio;
    let out_port     = resolve_output(&cfg, state);
    let cal_reply_tx = state.cal_reply_tx.clone();

    let worker = spawn_worker(state, "calibrate", move |stop| {
        let mut eng = make_engine(fake);
        if let Err(e) = eng.start(&[out_port.clone()], None) {
            send_pub(&pub_tx, "error", &json!({"cmd":"calibrate","message":format!("{e}")}));
            return;
        }
        let amp = ac_core::shared::generator::dbfs_to_amplitude(ref_dbfs);
        eng.set_tone(1000.0, amp);

        // Step 1 — output voltage
        let (tx1, rx1) = crossbeam_channel::bounded(1);
        *cal_reply_tx.lock().unwrap() = Some(tx1);
        let dmm_v1 = cfg.dmm_host.as_deref().and_then(|h| read_dmm_vrms(h, 3));
        send_pub(&pub_tx, "cal_prompt", &json!({
            "step":     1,
            "text":     "Measure output Vrms at DUT input. Enter reading or press Enter to skip.",
            "dmm_vrms": dmm_v1,
        }));
        let out_vrms = wait_cal_reply(&rx1, &stop, 120);
        *cal_reply_tx.lock().unwrap() = None;
        if stop.load(Ordering::Relaxed) {
            eng.set_silence(); eng.stop(); return;
        }

        // Step 2 — input voltage
        let (tx2, rx2) = crossbeam_channel::bounded(1);
        *cal_reply_tx.lock().unwrap() = Some(tx2);
        let dmm_v2 = cfg.dmm_host.as_deref().and_then(|h| read_dmm_vrms(h, 3));
        send_pub(&pub_tx, "cal_prompt", &json!({
            "step":     2,
            "text":     "Measure input Vrms at DUT output. Enter reading or press Enter to skip.",
            "dmm_vrms": dmm_v2,
        }));
        let in_vrms = wait_cal_reply(&rx2, &stop, 120);
        *cal_reply_tx.lock().unwrap() = None;

        eng.set_silence();
        eng.stop();

        // Load existing entry to preserve unrelated fields (notably the
        // SPL pistonphone reading set by `calibrate_spl`); only voltage
        // fields are overwritten here.
        let mut cal = Calibration::load_or_new(out_ch, in_ch, None);
        cal.ref_dbfs          = ref_dbfs;
        cal.vrms_at_0dbfs_out = out_vrms;
        cal.vrms_at_0dbfs_in  = in_vrms;
        let save_err = cal.save(None).err().map(|e| e.to_string());

        let key = cal.key();
        let mut cal_done_frame = json!({
            "key":               key,
            "vrms_at_0dbfs_out": out_vrms,
            "vrms_at_0dbfs_in":  in_vrms,
        });
        if let Some(ref e) = save_err { cal_done_frame["error"] = json!(e); }
        send_pub(&pub_tx, "cal_done", &cal_done_frame);

        // Route terminal frame on save outcome so the Python client, which
        // treats `done` vs `error` as the authoritative signal, sees failures.
        match save_err {
            Some(e) => send_pub(&pub_tx, "error", &json!({
                "cmd":     "calibrate",
                "message": format!("save failed: {e}"),
            })),
            None => send_pub(&pub_tx, "done", &json!({
                "cmd": "calibrate",
                "key": key,
            })),
        }
    });

    {
        let mut workers = state.workers.lock().unwrap();
        workers.insert("calibrate".to_string(), worker);
    }
    json!({"ok": true})
}

pub fn cal_reply(state: &ServerState, cmd: &Value) -> Value {
    let vrms = cmd.get("vrms").and_then(Value::as_f64); // None if JSON null or absent
    let tx = state.cal_reply_tx.lock().unwrap();
    if let Some(ref t) = *tx {
        let _ = t.send(vrms);
    }
    json!({"ok": true})
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
    let cfg    = state.cfg.lock().unwrap().clone();
    let out_ch = cmd.get("output_channel")
        .and_then(Value::as_u64)
        .unwrap_or(cfg.output_channel as u64) as u32;
    let in_ch  = cmd.get("input_channel")
        .and_then(Value::as_u64)
        .unwrap_or(cfg.input_channel as u64) as u32;
    let capture_s = cmd.get("capture_s").and_then(Value::as_f64).unwrap_or(1.0);

    let pub_tx       = state.pub_tx.clone();
    let fake         = state.fake_audio;
    let mut cfg_in   = cfg.clone();
    cfg_in.input_channel = in_ch;
    cfg_in.input_port    = None;
    let in_port      = resolve_input(&cfg_in, state);
    let cal_reply_tx = state.cal_reply_tx.clone();

    let worker = spawn_worker(state, "calibrate_spl", move |stop| {
        let mut eng = make_engine(fake);
        if let Err(e) = eng.start(&[], Some(&in_port)) {
            send_pub(&pub_tx, "error", &json!({"cmd":"calibrate_spl","message":format!("{e}")}));
            return;
        }
        // Prompt the user to seat the pistonphone, wait for the green
        // light. The reply value itself is unused — we just need a
        // synchronisation point so the capture sees the reference tone,
        // not silence or seating noise.
        let (tx, rx) = crossbeam_channel::bounded(1);
        *cal_reply_tx.lock().unwrap() = Some(tx);
        send_pub(&pub_tx, "cal_prompt", &json!({
            "step":     1,
            "text":     "Apply 94 dB SPL pistonphone reference, press Enter when ready (q to cancel).",
            "kind":     "spl",
        }));
        let _ = wait_cal_reply(&rx, &stop, 300);
        *cal_reply_tx.lock().unwrap() = None;
        if stop.load(Ordering::Relaxed) {
            eng.stop(); return;
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
        if let Some(ref e) = save_err { cal_done_frame["error"] = json!(e); }
        send_pub(&pub_tx, "cal_done", &cal_done_frame);

        match save_err {
            Some(e) => send_pub(&pub_tx, "error", &json!({
                "cmd":     "calibrate_spl",
                "message": format!("save failed: {e}"),
            })),
            None => send_pub(&pub_tx, "done", &json!({
                "cmd": "calibrate_spl",
                "key": key,
            })),
        }
    });

    {
        let mut workers = state.workers.lock().unwrap();
        workers.insert("calibrate_spl".to_string(), worker);
    }
    json!({"ok": true})
}
