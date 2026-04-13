//! Non-audio commands: status, control, devices, setup, calibration metadata,
//! DMM passthrough, server bind-mode toggles.

use serde_json::{json, Value};

use ac_core::calibration::Calibration;

use crate::server::ServerState;

use super::{cached_capture_ports, cached_playback_ports, read_dmm_vrms, refresh_port_cache};

pub fn status(state: &ServerState) -> Value {
    let workers = state.workers.lock().unwrap();
    let running: Option<String> = workers.keys().next().cloned();
    let listen_mode = state.listen_mode.lock().unwrap().clone();
    json!({
        "ok":            true,
        "busy":          !workers.is_empty(),
        "running_cmd":   running,
        "src_mtime":     state.src_mtime,
        "listen_mode":   listen_mode,
        "server_enabled": true
    })
}

pub fn quit(state: &ServerState) -> Value {
    let mut workers = state.workers.lock().unwrap();
    for w in workers.values_mut() {
        w.stop();
    }
    drop(workers);
    json!({"ok": true, "_quit": true})
}

pub fn stop(state: &ServerState, cmd: &Value) -> Value {
    let target = cmd.get("name").and_then(Value::as_str);
    let workers = state.workers.lock().unwrap();
    if let Some(name) = target {
        if let Some(w) = workers.get(name) {
            w.stop();
        }
    } else {
        for w in workers.values() {
            w.stop();
        }
    }
    json!({"ok": true})
}

pub fn devices(state: &ServerState) -> Value {
    // `devices` is the documented hardware rescan trigger — always refresh.
    refresh_port_cache(state);
    let cfg      = state.cfg.lock().unwrap().clone();
    let playback = cached_playback_ports(state);
    let capture  = cached_capture_ports(state);
    json!({
        "ok":                true,
        "playback":          playback,
        "capture":           capture,
        "output_channel":    cfg.output_channel,
        "input_channel":     cfg.input_channel,
        "output_port":       cfg.output_port,
        "input_port":        cfg.input_port,
        "reference_channel": cfg.reference_channel,
        "reference_port":    cfg.reference_port,
    })
}

pub fn setup(state: &ServerState, cmd: &Value) -> Value {
    let update = match cmd.get("update") {
        Some(u) => u,
        None    => return json!({"ok": false, "error": "missing 'update' field"}),
    };

    let mut cfg = state.cfg.lock().unwrap();

    if let Some(v) = update.get("output_channel").and_then(Value::as_u64) {
        cfg.output_channel = v as u32;
    }
    if let Some(v) = update.get("input_channel").and_then(Value::as_u64) {
        cfg.input_channel = v as u32;
    }
    if let Some(v) = update.get("reference_channel") {
        if v.is_null() {
            cfg.reference_channel = None;
        } else if let Some(n) = v.as_u64() {
            cfg.reference_channel = Some(n as u32);
        }
    }
    if let Some(v) = update.get("dbu_ref_vrms").and_then(Value::as_f64) {
        cfg.dbu_ref_vrms = v;
    }
    if let Some(v) = update.get("server_enabled").and_then(Value::as_bool) {
        cfg.server_enabled = v;
    }
    if let Some(v) = update.get("backend").and_then(Value::as_str) {
        cfg.backend = Some(v.to_string());
    }
    if update.get("dmm_host").is_some() {
        cfg.dmm_host = update["dmm_host"].as_str().map(str::to_string);
    }

    let cfg_value = serde_json::to_value(&*cfg).unwrap_or_default();
    if let Err(e) = ac_core::config::save(&*cfg, None) {
        eprintln!("setup: save failed: {e}");
    }
    json!({"ok": true, "config": cfg_value})
}

pub fn get_calibration(state: &ServerState, cmd: &Value) -> Value {
    let cfg = state.cfg.lock().unwrap();
    let out_ch = cmd.get("output_channel")
        .and_then(Value::as_u64)
        .unwrap_or(cfg.output_channel as u64) as u32;
    let in_ch = cmd.get("input_channel")
        .and_then(Value::as_u64)
        .unwrap_or(cfg.input_channel as u64) as u32;
    drop(cfg);

    match Calibration::load(out_ch, in_ch, None) {
        Err(e) => json!({"ok": false, "error": format!("{e}")}),
        Ok(None) => json!({"ok": true, "found": false}),
        Ok(Some(cal)) => json!({
            "ok":                true,
            "found":             true,
            "key":               cal.key(),
            "vrms_at_0dbfs_out": cal.vrms_at_0dbfs_out,
            "vrms_at_0dbfs_in":  cal.vrms_at_0dbfs_in,
            "ref_dbfs":          cal.ref_dbfs,
        }),
    }
}

pub fn list_calibrations(_state: &ServerState) -> Value {
    match Calibration::load_all(None) {
        Err(e) => json!({"ok": false, "error": format!("{e}")}),
        Ok(cals) => {
            let list: Vec<Value> = cals.iter().map(|c| json!({
                "key":               c.key(),
                "vrms_at_0dbfs_out": c.vrms_at_0dbfs_out,
                "vrms_at_0dbfs_in":  c.vrms_at_0dbfs_in,
            })).collect();
            json!({"ok": true, "calibrations": list})
        }
    }
}

pub fn dmm_read(state: &ServerState) -> Value {
    let cfg = state.cfg.lock().unwrap();
    let host = match &cfg.dmm_host {
        Some(h) => h.clone(),
        None => return json!({"ok": false,
                    "error": "no DMM configured on server — run: ac setup dmm <host>"}),
    };
    drop(cfg);
    match read_dmm_vrms(&host, 3) {
        Some(v) => json!({"ok": true, "vrms": v, "idn": null}),
        None    => json!({"ok": false, "error": format!("DMM at {host} did not respond")}),
    }
}

pub fn server_enable(state: &ServerState) -> Value {
    *state.listen_mode.lock().unwrap() = "public".to_string();
    let _ = state.rebind_tx.send("*".to_string());
    json!({"ok": true, "bind_addr": "*", "listen_mode": "public"})
}

pub fn server_disable(state: &ServerState) -> Value {
    *state.listen_mode.lock().unwrap() = "local".to_string();
    let _ = state.rebind_tx.send("127.0.0.1".to_string());
    json!({"ok": true, "bind_addr": "127.0.0.1", "listen_mode": "local"})
}

pub fn server_connections(state: &ServerState) -> Value {
    let listen_mode = state.listen_mode.lock().unwrap().clone();
    let (ctrl_ep, data_ep) = if listen_mode == "public" {
        (
            format!("tcp://*:{}", state.ctrl_port),
            format!("tcp://*:{}", state.data_port),
        )
    } else {
        (
            format!("tcp://127.0.0.1:{}", state.ctrl_port),
            format!("tcp://127.0.0.1:{}", state.data_port),
        )
    };
    let workers: Vec<String> = state.workers.lock().unwrap().keys().cloned().collect();
    json!({
        "ok":            true,
        "listen_mode":   listen_mode,
        "ctrl_endpoint": ctrl_ep,
        "data_endpoint": data_ep,
        "clients":       [],
        "workers":       workers,
    })
}
