//! `sweep_level` / `sweep_frequency` — drive output over a stepped range
//! and publish per-point analysis frames. Output-only generators, reached
//! from the CLI via `ac generate level` / `ac generate frequency` (#282;
//! `ac sweep ir`, which captures and analyses, moved to `plot_ir` in
//! `plot.rs`).

use std::sync::atomic::Ordering;

use serde_json::{json, Value};

use crate::audio::make_engine;
use crate::server::ServerState;

use super::super::{apply_drive_ceiling, busy_guard, resolve_output, send_pub, spawn_worker};

pub fn sweep_level(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "sweep_level");
    let freq_hz = match cmd.get("freq_hz").and_then(Value::as_f64) {
        Some(v) => v,
        None => return json!({"ok": false, "error": "missing freq_hz"}),
    };
    // Raw request, unclamped: this is the shape of the ramp, not the level
    // that reaches the engine. Each computed point on the ramp is clamped
    // individually below (#360) — a sweep whose top end exceeds the
    // ceiling flattens there rather than running unclamped or being
    // refused outright, mirroring `set_drive`'s "clamp is normal
    // operation" discipline.
    let start_dbfs = cmd
        .get("start_dbfs")
        .and_then(Value::as_f64)
        .unwrap_or(-20.0);
    let stop_dbfs = cmd.get("stop_dbfs").and_then(Value::as_f64).unwrap_or(0.0);
    let duration = cmd.get("duration").and_then(Value::as_f64).unwrap_or(1.0);
    let cfg = state.cfg.lock().unwrap().clone();
    let ceiling = cfg.drive_max_dbfs;
    let out_port = match resolve_output(&cfg, state) {
        Ok(p) => p,
        Err(e) => return json!({"ok": false, "error": e}),
    };
    let out_port_reply = out_port.clone();
    // Applied endpoints echoed on the sync reply — monotone under a `min`
    // clamp, so this is exactly the range the ramp actually covers.
    let start_dbfs_applied = apply_drive_ceiling(ceiling, start_dbfs);
    let stop_dbfs_applied = apply_drive_ceiling(ceiling, stop_dbfs);

    let pub_tx = state.pub_tx.clone();
    let fake = state.fake_audio;

    let worker = spawn_worker(state, "sweep_level", move |stop| {
        let mut eng = make_engine(fake);
        if let Err(e) = eng.start(&[out_port], None) {
            send_pub(
                &pub_tx,
                "error",
                &json!({"cmd":"sweep_level","message":format!("{e}")}),
            );
            return;
        }
        let start_amp =
            ac_core::shared::generator::dbfs_to_amplitude(apply_drive_ceiling(ceiling, start_dbfs));
        eng.set_tone(freq_hz, start_amp);
        let t0 = std::time::Instant::now();
        while !stop.load(Ordering::Relaxed) {
            let elapsed = t0.elapsed().as_secs_f64();
            if elapsed >= duration {
                break;
            }
            let t = elapsed / duration;
            let db = apply_drive_ceiling(ceiling, start_dbfs + (stop_dbfs - start_dbfs) * t);
            eng.set_tone(freq_hz, ac_core::shared::generator::dbfs_to_amplitude(db));
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        eng.set_silence();
        eng.stop();
        send_pub(&pub_tx, "done", &json!({"cmd":"sweep_level"}));
    });

    {
        let mut workers = state.workers.lock().unwrap();
        workers.insert("sweep_level".to_string(), worker);
    }
    json!({
        "ok": true,
        "out_port": out_port_reply,
        "start_dbfs": start_dbfs_applied,
        "stop_dbfs": stop_dbfs_applied,
    })
}

pub fn sweep_frequency(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "sweep_frequency");
    let start_hz = cmd.get("start_hz").and_then(Value::as_f64).unwrap_or(20.0);
    let stop_hz = cmd
        .get("stop_hz")
        .and_then(Value::as_f64)
        .unwrap_or(20_000.0);
    let level_dbfs = cmd
        .get("level_dbfs")
        .and_then(Value::as_f64)
        .unwrap_or(-10.0);
    let duration = cmd.get("duration").and_then(Value::as_f64).unwrap_or(1.0);
    let cfg = state.cfg.lock().unwrap().clone();
    // #360: `sweep_frequency` puts a stimulus on a physical output.
    let level_dbfs = apply_drive_ceiling(cfg.drive_max_dbfs, level_dbfs);
    let out_port = match resolve_output(&cfg, state) {
        Ok(p) => p,
        Err(e) => return json!({"ok": false, "error": e}),
    };
    let out_port_reply = out_port.clone();
    let amplitude = ac_core::shared::generator::dbfs_to_amplitude(level_dbfs);

    let pub_tx = state.pub_tx.clone();
    let fake = state.fake_audio;

    let worker = spawn_worker(state, "sweep_frequency", move |stop| {
        let mut eng = make_engine(fake);
        if let Err(e) = eng.start(&[out_port], None) {
            send_pub(
                &pub_tx,
                "error",
                &json!({"cmd":"sweep_frequency","message":format!("{e}")}),
            );
            return;
        }
        eng.set_tone(start_hz, amplitude);
        let t0 = std::time::Instant::now();
        while !stop.load(Ordering::Relaxed) {
            let elapsed = t0.elapsed().as_secs_f64();
            if elapsed >= duration {
                break;
            }
            let t = elapsed / duration;
            let freq = start_hz * (stop_hz / start_hz).powf(t);
            eng.set_tone(freq, amplitude);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        eng.set_silence();
        eng.stop();
        send_pub(&pub_tx, "done", &json!({"cmd":"sweep_frequency"}));
    });

    {
        let mut workers = state.workers.lock().unwrap();
        workers.insert("sweep_frequency".to_string(), worker);
    }
    json!({"ok": true, "out_port": out_port_reply, "level_dbfs": level_dbfs})
}
