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
    // Flip each worker's stop flag first — without dropping the lock so we
    // don't race with `spawn_worker` on the main thread — then move the
    // handles out so we can join them without the workers map locked.
    // Joining here (via `Drop` on `WorkerHandle`) is what makes the reply
    // synchronous with respect to the busy guard: the next command we
    // receive on the REP socket is guaranteed to see an empty workers map
    // and can start an `Exclusive`-group worker like `transfer_stream`.
    let mut joined: Vec<(String, crate::workers::WorkerHandle)> = Vec::new();
    {
        let mut workers = state.workers.lock().unwrap();
        if let Some(name) = target {
            if let Some(w) = workers.get(name) {
                w.stop();
            }
            if let Some(handle) = workers.remove(name) {
                joined.push((name.to_string(), handle));
            }
        } else {
            for w in workers.values() {
                w.stop();
            }
            for (name, handle) in workers.drain() {
                joined.push((name, handle));
            }
        }
    }
    let stopped: Vec<String> = joined.iter().map(|(n, _)| n.clone()).collect();
    drop(joined); // runs Drop → joins the worker threads
    json!({"ok": true, "stopped": stopped})
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

pub fn set_analysis_mode(state: &ServerState, cmd: &Value) -> Value {
    let mode = match cmd.get("mode").and_then(Value::as_str) {
        Some(m) => m,
        None => return json!({"ok": false, "error": "missing 'mode' field"}),
    };
    if mode != "fft" && mode != "cwt" {
        return json!({
            "ok": false,
            "error": format!("invalid mode '{mode}': expected 'fft' or 'cwt'"),
        });
    }
    *state.analysis_mode.lock().unwrap() = mode.to_string();
    if let Some(s) = cmd.get("sigma").and_then(Value::as_f64) {
        let s = (s as f32).clamp(5.0, 24.0);
        *state.cwt_sigma.lock().unwrap() = s;
    }
    if let Some(n) = cmd.get("n_scales").and_then(Value::as_u64) {
        let n = (n as usize).clamp(64, 2048);
        *state.cwt_n_scales.lock().unwrap() = n;
    }
    let sigma = *state.cwt_sigma.lock().unwrap();
    let n_scales = *state.cwt_n_scales.lock().unwrap();
    json!({"ok": true, "mode": mode, "sigma": sigma, "n_scales": n_scales})
}

pub fn get_analysis_mode(state: &ServerState) -> Value {
    let mode = state.analysis_mode.lock().unwrap().clone();
    let sigma = *state.cwt_sigma.lock().unwrap();
    let n_scales = *state.cwt_n_scales.lock().unwrap();
    json!({"ok": true, "mode": mode, "sigma": sigma, "n_scales": n_scales})
}

/// Live-tune `interval` and/or `fft_n` on a running `monitor_spectrum` worker.
/// Rejects if no monitor is active (the worker owns the Arc; without it the
/// change has nothing to pick up).
pub fn set_monitor_params(state: &ServerState, cmd: &Value) -> Value {
    let req_interval = cmd.get("interval").and_then(Value::as_f64);
    let req_fft_n = cmd.get("fft_n").and_then(Value::as_u64).map(|v| v as u32);

    if let Some(i) = req_interval {
        if !(i > 0.0 && i <= 60.0) {
            return json!({"ok": false, "error": "interval must be > 0 and <= 60"});
        }
    }
    if let Some(n) = req_fft_n {
        if !n.is_power_of_two() || n < 256 || n > 131_072 {
            return json!({"ok": false, "error": "fft_n must be power of 2 in [256, 131072]"});
        }
    }

    let mut mp = state.monitor_params.lock().unwrap();
    if !mp.active {
        return json!({"ok": false, "error": "no active monitor"});
    }
    if let Some(i) = req_interval { mp.interval = i; }
    if let Some(n) = req_fft_n    { mp.fft_n = n; }
    json!({"ok": true, "interval": mp.interval, "fft_n": mp.fft_n})
}

/// Set or clear the tuner search-range override for a channel. The
/// `monitor_spectrum` worker applies the lock to the matching per-channel
/// `TunerState` on its next tick. `clear: true` drops the override and
/// restores the default `(40, 2000) Hz` window.
pub fn tuner_range(state: &ServerState, cmd: &Value) -> Value {
    let channel = match cmd.get("channel").and_then(Value::as_u64) {
        Some(v) => v as u32,
        None => return json!({"ok": false, "error": "missing 'channel'"}),
    };
    let clear = cmd.get("clear").and_then(Value::as_bool).unwrap_or(false);
    let mut map = state.tuner_range_locks.lock().unwrap();
    if clear {
        map.remove(&channel);
        return json!({"ok": true, "channel": channel, "range_hz": serde_json::Value::Null});
    }
    let lo = cmd.get("lo_hz").and_then(Value::as_f64);
    let hi = cmd.get("hi_hz").and_then(Value::as_f64);
    match (lo, hi) {
        (Some(lo), Some(hi)) if lo > 0.0 && hi > lo => {
            map.insert(channel, (lo, hi));
            json!({"ok": true, "channel": channel, "range_hz": [lo, hi]})
        }
        _ => json!({"ok": false, "error": "lo_hz and hi_hz required, with 0 < lo < hi"}),
    }
}

/// Override the tuner detector config. All fields optional — omitted
/// values keep their current setting. `min_level_dbfs` accepts `null` to
/// clear the gate. The `monitor_spectrum` worker reads the shared config
/// every tick and pushes it into each per-channel `TunerState` via
/// `set_config`, so a change takes effect on the next publish cycle.
pub fn tuner_config(state: &ServerState, cmd: &Value) -> Value {
    let mut g = state.tuner_config.lock().unwrap();
    if let Some(v) = cmd.get("trigger_delta_db").and_then(Value::as_f64) {
        if !v.is_finite() || v < 0.0 {
            return json!({"ok": false, "error": "trigger_delta_db must be ≥ 0"});
        }
        g.trigger_delta_db = v as f32;
    }
    if let Some(v) = cmd.get("min_confidence").and_then(Value::as_f64) {
        if !(0.0..=1.0).contains(&v) {
            return json!({"ok": false, "error": "min_confidence must be in [0,1]"});
        }
        g.min_confidence = v;
    }
    if let Some(v) = cmd.get("min_level_dbfs") {
        g.min_level_dbfs = match v {
            Value::Null => None,
            Value::Number(n) => {
                let f = n.as_f64().unwrap_or(f64::NAN);
                if !f.is_finite() {
                    return json!({"ok": false, "error": "min_level_dbfs must be finite or null"});
                }
                Some(f as f32)
            }
            _ => return json!({"ok": false, "error": "min_level_dbfs must be number or null"}),
        };
    }
    json!({
        "ok": true,
        "trigger_delta_db": g.trigger_delta_db,
        "min_confidence":   g.min_confidence,
        "min_level_dbfs":   g.min_level_dbfs,
    })
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
