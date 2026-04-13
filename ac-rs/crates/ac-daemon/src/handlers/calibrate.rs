//! Calibrate state machine: plays reference tone, prompts for output and
//! input Vrms readings, writes cal.json. Routes the worker's terminal frame
//! to `done` or `error` based on the save outcome.

use std::sync::atomic::Ordering;

use serde_json::{json, Value};

use ac_core::calibration::Calibration;

use crate::audio::make_engine;
use crate::server::ServerState;

use super::{busy_guard, read_dmm_vrms, resolve_output, send_pub, spawn_worker, wait_cal_reply};

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
        let amp = ac_core::generator::dbfs_to_amplitude(ref_dbfs);
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

        let mut cal = Calibration::new(out_ch, in_ch);
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
