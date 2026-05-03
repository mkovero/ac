//! `generate` / `generate_pink` — continuous tone / pink-noise output.

use std::sync::atomic::Ordering;

use serde_json::{json, Value};

use crate::audio::make_engine;
use crate::server::ServerState;

use super::super::{
    busy_guard, resolve_output, resolve_output_by_channel, send_pub, spawn_worker,
};

/// Resolve the `channels` field in a generate command into playback ports.
/// Empty / missing → `[resolve_output(cfg)]` (the sticky default). Useful
/// for the multi-channel "shotgun" form (`ac generate sine 0-17 ...`),
/// which is the only practical workaround when DAC chip enumeration
/// reorders ports across reboots and the user doesn't yet know which
/// JACK index is the analog one this session.
fn resolve_channels(cmd: &Value, cfg: &ac_core::config::Config, state: &ServerState) -> Vec<String> {
    let channels: Vec<u32> = cmd
        .get("channels")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_u64().map(|u| u as u32)).collect())
        .unwrap_or_default();
    if channels.is_empty() {
        return vec![resolve_output(cfg, state)];
    }
    let mut ports: Vec<String> =
        channels.iter().map(|&c| resolve_output_by_channel(cfg, state, c)).collect();
    ports.dedup();
    ports
}

pub fn generate(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "generate");
    let freq_hz    = cmd.get("freq_hz")   .and_then(Value::as_f64).unwrap_or(1000.0);
    let level_dbfs = cmd.get("level_dbfs").and_then(Value::as_f64).unwrap_or(-10.0);
    let cfg        = state.cfg.lock().unwrap().clone();

    let out_ports = resolve_channels(cmd, &cfg, state);

    let pub_tx   = state.pub_tx.clone();
    let fake     = state.fake_audio;
    let ports_for_worker = out_ports.clone();

    let worker = spawn_worker(state, "generate", move |stop| {
        let mut eng = make_engine(fake);
        if let Err(e) = eng.start(&ports_for_worker, None) {
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

    json!({"ok": true, "out_ports": out_ports})
}

pub fn generate_pink(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "generate_pink");
    let level_dbfs = cmd.get("level_dbfs").and_then(Value::as_f64).unwrap_or(-10.0);
    let cfg        = state.cfg.lock().unwrap().clone();

    let out_ports = resolve_channels(cmd, &cfg, state);

    let pub_tx = state.pub_tx.clone();
    let fake   = state.fake_audio;
    let ports_for_worker = out_ports.clone();

    let worker = spawn_worker(state, "generate_pink", move |stop| {
        let mut eng = make_engine(fake);
        if let Err(e) = eng.start(&ports_for_worker, None) {
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

    json!({"ok": true, "out_ports": out_ports})
}
