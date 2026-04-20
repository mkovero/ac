//! `sweep_level` / `sweep_frequency` — drive output over a stepped range
//! and publish per-point analysis frames.

use std::sync::atomic::Ordering;

use serde_json::{json, Value};

use crate::audio::make_engine;
use crate::server::ServerState;

use super::super::{busy_guard, resolve_output, send_pub, spawn_worker};

pub fn sweep_level(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "sweep_level");
    let freq_hz    = match cmd.get("freq_hz").and_then(Value::as_f64) {
        Some(v) => v,
        None    => return json!({"ok": false, "error": "missing freq_hz"}),
    };
    let start_dbfs = cmd.get("start_dbfs").and_then(Value::as_f64).unwrap_or(-20.0);
    let stop_dbfs  = cmd.get("stop_dbfs") .and_then(Value::as_f64).unwrap_or(0.0);
    let duration   = cmd.get("duration")  .and_then(Value::as_f64).unwrap_or(1.0);
    let cfg        = state.cfg.lock().unwrap().clone();
    let out_port   = resolve_output(&cfg, state);
    let out_port_reply = out_port.clone();

    let pub_tx = state.pub_tx.clone();
    let fake   = state.fake_audio;

    let worker = spawn_worker(state, "sweep_level", move |stop| {
        let mut eng = make_engine(fake);
        if let Err(e) = eng.start(&[out_port], None) {
            send_pub(&pub_tx, "error", &json!({"cmd":"sweep_level","message":format!("{e}")}));
            return;
        }
        let start_amp = ac_core::generator::dbfs_to_amplitude(start_dbfs);
        eng.set_tone(freq_hz, start_amp);
        let t0 = std::time::Instant::now();
        while !stop.load(Ordering::Relaxed) {
            let elapsed = t0.elapsed().as_secs_f64();
            if elapsed >= duration { break; }
            let t = elapsed / duration;
            let db = start_dbfs + (stop_dbfs - start_dbfs) * t;
            eng.set_tone(freq_hz, ac_core::generator::dbfs_to_amplitude(db));
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
    json!({"ok": true, "out_port": out_port_reply})
}

pub fn sweep_frequency(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "sweep_frequency");
    let start_hz   = cmd.get("start_hz")  .and_then(Value::as_f64).unwrap_or(20.0);
    let stop_hz    = cmd.get("stop_hz")   .and_then(Value::as_f64).unwrap_or(20_000.0);
    let level_dbfs = cmd.get("level_dbfs").and_then(Value::as_f64).unwrap_or(-10.0);
    let duration   = cmd.get("duration")  .and_then(Value::as_f64).unwrap_or(1.0);
    let cfg        = state.cfg.lock().unwrap().clone();
    let out_port   = resolve_output(&cfg, state);
    let out_port_reply = out_port.clone();
    let amplitude  = ac_core::generator::dbfs_to_amplitude(level_dbfs);

    let pub_tx = state.pub_tx.clone();
    let fake   = state.fake_audio;

    let worker = spawn_worker(state, "sweep_frequency", move |stop| {
        let mut eng = make_engine(fake);
        if let Err(e) = eng.start(&[out_port], None) {
            send_pub(&pub_tx, "error", &json!({"cmd":"sweep_frequency","message":format!("{e}")}));
            return;
        }
        eng.set_tone(start_hz, amplitude);
        let t0 = std::time::Instant::now();
        while !stop.load(Ordering::Relaxed) {
            let elapsed = t0.elapsed().as_secs_f64();
            if elapsed >= duration { break; }
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
    json!({"ok": true, "out_port": out_port_reply})
}
