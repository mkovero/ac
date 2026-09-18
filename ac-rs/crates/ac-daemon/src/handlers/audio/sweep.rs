//! `sweep_level` / `sweep_frequency` — drive output over a stepped range
//! and publish per-point analysis frames. Output-only generators, reached
//! from the CLI via `ac generate level` / `ac generate frequency` (#282;
//! `ac sweep ir`, which captures and analyses, moved to `plot_ir` in
//! `plot.rs`).

use std::sync::atomic::Ordering;

use serde_json::{json, Value};

use ac_core::shared::emission_level::{
    DEFAULT_LEVEL_DBFS, DEFAULT_RAMP_START_DBFS, DEFAULT_RAMP_STOP_DBFS, MAX_EMISSION_DBFS,
};

use crate::server::ServerState;

use crate::handlers::checks::{self, Gate, LevelUnit};

use super::super::{
    busy_guard, cfg_guard, emission_guard, emission_range_guard, make_engine_for_state,
    resolve_output, send_pub, spawn_worker,
};

pub fn sweep_level(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "sweep_level");
    cfg_guard!(state);
    let freq_hz = match cmd.get("freq_hz").and_then(Value::as_f64) {
        Some(v) => v,
        None => return json!({"ok": false, "error": "missing freq_hz"}),
    };
    let start_dbfs = cmd
        .get("start_dbfs")
        .and_then(Value::as_f64)
        .unwrap_or(DEFAULT_RAMP_START_DBFS);
    let stop_dbfs = cmd
        .get("stop_dbfs")
        .and_then(Value::as_f64)
        .unwrap_or(DEFAULT_RAMP_STOP_DBFS);
    let duration = cmd.get("duration").and_then(Value::as_f64).unwrap_or(1.0);
    let cfg = state.cfg.lock().unwrap().clone();
    // #459: both endpoints checked up front — see `plot_level`'s identical
    // reasoning. The per-point clamp the ramp loop below used to carry is
    // gone; every point between two in-range endpoints is in range.
    let (start_dbfs, stop_dbfs) = emission_range_guard!(state, &cfg, start_dbfs, stop_dbfs);
    let level_unit = match LevelUnit::from_request(cmd) {
        Ok(u) => u,
        Err(e) => return e,
    };
    let pair_cal = match checks::gate_pair_cal(&cfg, level_unit) {
        Ok(c) => c,
        Err(e) => return e,
    };
    let gate = Gate::plan(state, &cfg, "sweep_level", pair_cal, level_unit, false);
    let out_port = match resolve_output(&cfg, state) {
        Ok(p) => p,
        Err(e) => return json!({"ok": false, "error": e}),
    };
    let out_port_reply = out_port.clone();

    let pub_tx = state.pub_tx.clone();
    let mut eng = match make_engine_for_state(state) {
        Ok(eng) => eng,
        Err(e) => return json!({"ok": false, "error": e}),
    };
    let backend = eng.backend_name();

    let mut reply = json!({
        "ok": true,
        "out_port": out_port_reply,
        "start_dbfs": start_dbfs,
        "stop_dbfs": stop_dbfs,
        "max_dbfs": MAX_EMISSION_DBFS,
        "backend": backend,
    });
    gate.write_reply(&mut reply);

    let worker = spawn_worker(state, "sweep_level", move |stop| {
        if gate.run(&pub_tx, true).is_err() {
            return;
        }
        if let Err(e) = eng.start(&[out_port], None) {
            send_pub(
                &pub_tx,
                "error",
                &json!({"cmd":"sweep_level","message":format!("{e}")}),
            );
            return;
        }
        let start_amp = ac_core::shared::generator::dbfs_to_amplitude(start_dbfs);
        eng.set_tone(freq_hz, start_amp);
        let t0 = std::time::Instant::now();
        while !stop.load(Ordering::Relaxed) {
            let elapsed = t0.elapsed().as_secs_f64();
            if elapsed >= duration {
                break;
            }
            let t = elapsed / duration;
            let db = start_dbfs + (stop_dbfs - start_dbfs) * t;
            eng.set_tone(freq_hz, ac_core::shared::generator::dbfs_to_amplitude(db));
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        eng.set_silence();
        eng.stop();
        send_pub(
            &pub_tx,
            "done",
            &json!({"cmd":"sweep_level","backend":backend}),
        );
    });

    {
        let mut workers = state.workers.lock().unwrap();
        workers.insert("sweep_level".to_string(), worker);
    }
    reply
}

pub fn sweep_frequency(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "sweep_frequency");
    cfg_guard!(state);
    let start_hz = cmd.get("start_hz").and_then(Value::as_f64).unwrap_or(20.0);
    let stop_hz = cmd
        .get("stop_hz")
        .and_then(Value::as_f64)
        .unwrap_or(20_000.0);
    let level_dbfs = cmd
        .get("level_dbfs")
        .and_then(Value::as_f64)
        .unwrap_or(DEFAULT_LEVEL_DBFS);
    let duration = cmd.get("duration").and_then(Value::as_f64).unwrap_or(1.0);
    let cfg = state.cfg.lock().unwrap().clone();
    // #459: `sweep_frequency` puts a stimulus on a physical output.
    let level_dbfs = emission_guard!(state, &cfg, level_dbfs);
    let level_unit = match LevelUnit::from_request(cmd) {
        Ok(u) => u,
        Err(e) => return e,
    };
    let pair_cal = match checks::gate_pair_cal(&cfg, level_unit) {
        Ok(c) => c,
        Err(e) => return e,
    };
    let gate = Gate::plan(state, &cfg, "sweep_frequency", pair_cal, level_unit, false);
    let out_port = match resolve_output(&cfg, state) {
        Ok(p) => p,
        Err(e) => return json!({"ok": false, "error": e}),
    };
    let out_port_reply = out_port.clone();
    let amplitude = ac_core::shared::generator::dbfs_to_amplitude(level_dbfs);

    let pub_tx = state.pub_tx.clone();
    let mut eng = match make_engine_for_state(state) {
        Ok(eng) => eng,
        Err(e) => return json!({"ok": false, "error": e}),
    };
    let backend = eng.backend_name();

    let mut reply = json!({
        "ok": true,
        "out_port": out_port_reply,
        "level_dbfs": level_dbfs,
        "max_dbfs": MAX_EMISSION_DBFS,
        "backend": backend,
    });
    gate.write_reply(&mut reply);

    let worker = spawn_worker(state, "sweep_frequency", move |stop| {
        if gate.run(&pub_tx, true).is_err() {
            return;
        }
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
        send_pub(
            &pub_tx,
            "done",
            &json!({"cmd":"sweep_frequency","backend":backend}),
        );
    });

    {
        let mut workers = state.workers.lock().unwrap();
        workers.insert("sweep_frequency".to_string(), worker);
    }
    reply
}
