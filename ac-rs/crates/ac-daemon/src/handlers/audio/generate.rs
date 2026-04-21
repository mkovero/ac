//! `generate` / `generate_pink` — continuous tone / pink-noise output.

use std::sync::atomic::Ordering;

use serde_json::{json, Value};

use crate::audio::make_engine;
use crate::server::ServerState;

use super::super::{busy_guard, resolve_output, send_pub, spawn_worker};

pub fn generate(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "generate");
    let freq_hz    = cmd.get("freq_hz")   .and_then(Value::as_f64).unwrap_or(1000.0);
    let level_dbfs = cmd.get("level_dbfs").and_then(Value::as_f64).unwrap_or(-10.0);
    let cfg        = state.cfg.lock().unwrap().clone();

    let out_port = vec![resolve_output(&cfg, state)];

    let pub_tx   = state.pub_tx.clone();
    let fake     = state.fake_audio;

    let worker = spawn_worker(state, "generate", move |stop| {
        let mut eng = make_engine(fake);
        if let Err(e) = eng.start(&out_port, None) {
            send_pub(&pub_tx, "error", &json!({"cmd":"generate","message":format!("{e}")}));
            return;
        }
        let amp = ac_core::shared::generator::dbfs_to_amplitude(level_dbfs);
        eng.set_tone(freq_hz, amp);
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        eng.set_silence();
        eng.stop();
        send_pub(&pub_tx, "done", &json!({"cmd":"generate"}));
    });

    {
        let mut workers = state.workers.lock().unwrap();
        workers.insert("generate".to_string(), worker);
    }

    let resolved = resolve_output(&cfg, state);
    json!({"ok": true, "out_ports": [resolved]})
}

pub fn generate_pink(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "generate_pink");
    let level_dbfs = cmd.get("level_dbfs").and_then(Value::as_f64).unwrap_or(-10.0);
    let cfg        = state.cfg.lock().unwrap().clone();

    let out_port = vec![resolve_output(&cfg, state)];

    let pub_tx = state.pub_tx.clone();
    let fake   = state.fake_audio;

    let worker = spawn_worker(state, "generate_pink", move |stop| {
        let mut eng = make_engine(fake);
        if let Err(e) = eng.start(&out_port, None) {
            send_pub(&pub_tx, "error", &json!({"cmd":"generate_pink","message":format!("{e}")}));
            return;
        }
        let amp = ac_core::shared::generator::dbfs_to_amplitude(level_dbfs);
        eng.set_pink(amp);
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        eng.set_silence();
        eng.stop();
        send_pub(&pub_tx, "done", &json!({"cmd":"generate_pink"}));
    });

    {
        let mut workers = state.workers.lock().unwrap();
        workers.insert("generate_pink".to_string(), worker);
    }

    let resolved = resolve_output(&cfg, state);
    json!({"ok": true, "out_ports": [resolved]})
}
