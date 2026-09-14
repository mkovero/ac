//! `calibrate_spl` — pistonphone-reference SPL calibration.

use std::sync::atomic::Ordering;

use serde_json::{json, Value};

use ac_core::shared::calibration::Calibration;

use crate::server::ServerState;

use super::super::{
    busy_guard, capture_rms, cfg_guard, make_engine_for_state, rms_to_dbfs, send_pub, spawn_worker,
    wait_cal_reply,
};
use super::{channels_from, finish_cal, resolve_input_by_channel};

pub fn calibrate_spl(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "calibrate_spl");
    cfg_guard!(state);
    let cfg = state.cfg.lock().unwrap().clone();
    let (out_ch, in_ch) = channels_from(cmd, &cfg);
    let capture_s = cmd.get("capture_s").and_then(Value::as_f64).unwrap_or(1.0);

    let pub_tx = state.pub_tx.clone();
    let mut eng = match make_engine_for_state(state) {
        Ok(eng) => eng,
        Err(e) => return json!({"ok": false, "error": e}),
    };
    let backend = eng.backend_name();
    let in_port = match resolve_input_by_channel(&cfg, state, in_ch) {
        Ok(p) => p,
        Err(e) => return json!({"ok": false, "error": e}),
    };
    let cal_reply_tx = state.cal_reply_tx.clone();

    let worker = spawn_worker(state, "calibrate_spl", move |stop| {
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
        let cal_done_frame = json!({
            "key":                              key,
            "mic_sensitivity_dbfs_at_94db_spl": dbfs,
            "kind":                             "spl",
            "backend":                          backend,
        });
        finish_cal(&pub_tx, "calibrate_spl", &key, cal_done_frame, save_err);
    });

    {
        let mut workers = state.workers.lock().unwrap();
        workers.insert("calibrate_spl".to_string(), worker);
    }
    json!({"ok": true, "backend": backend})
}
