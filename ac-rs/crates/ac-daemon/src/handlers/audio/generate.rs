//! `generate` / `generate_pink` — continuous tone / pink-noise output.

use std::sync::atomic::Ordering;

use serde_json::{json, Value};

use ac_core::shared::emission_level::{DEFAULT_LEVEL_DBFS, MAX_EMISSION_DBFS};

use crate::server::ServerState;

use crate::handlers::checks::{self, Gate, LevelUnit};

use super::super::{
    busy_guard, cfg_guard, emission_guard, make_engine_for_state, resolve_output,
    resolve_output_by_channel, send_pub, spawn_worker, wire,
};

/// Resolve the `channels` field in a generate command into playback ports.
/// Empty / missing → `[resolve_output(cfg)]` (the sticky default). Useful
/// for the multi-channel "shotgun" form (`ac generate sine 0-17 ...`),
/// which is the only practical workaround when DAC chip enumeration
/// reorders ports across reboots and the user doesn't yet know which
/// JACK index is the analog one this session.
///
/// Returns `Err` with a human-readable message when *any* channel is
/// out of range — the caller surfaces it as a `400` reply instead of
/// silently connecting to a non-existent port name and producing no
/// audio. An explicitly supplied list with any invalid element (#431) is
/// refused the same way: it never falls back to the configured output.
fn resolve_channels(
    cmd_name: &str,
    cmd: &Value,
    cfg: &ac_core::config::Config,
    state: &ServerState,
) -> Result<Vec<String>, String> {
    let channels = wire::opt_u32_array(cmd, "channels")
        .map_err(|e| {
            e.refusal(
                &format!("{cmd_name} not started"),
                &[("stimulus", "silent")],
            )
        })?
        .unwrap_or_default();
    if channels.is_empty() {
        return Ok(vec![resolve_output(cfg, state)?]);
    }
    let mut ports: Vec<String> = channels
        .iter()
        .map(|&c| resolve_output_by_channel(cfg, state, c))
        .collect::<Result<Vec<_>, _>>()?;
    ports.dedup();
    Ok(ports)
}

pub fn generate(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "generate");
    cfg_guard!(state);
    let freq_hz = cmd.get("freq_hz").and_then(Value::as_f64).unwrap_or(1000.0);
    let level_dbfs = cmd
        .get("level_dbfs")
        .and_then(Value::as_f64)
        .unwrap_or(DEFAULT_LEVEL_DBFS);
    let cfg = state.cfg.lock().unwrap().clone();
    // #459: `generate` puts a stimulus on a physical output, so the
    // requested level is refused here, before it ever reaches the engine,
    // if it is above the fixed maximum — never clamped down to it.
    let level_dbfs = emission_guard!(state, &cfg, level_dbfs);
    let level_unit = match LevelUnit::from_request(cmd) {
        Ok(u) => u,
        Err(e) => return e,
    };
    let pair_cal = match checks::gate_pair_cal(&cfg, level_unit) {
        Ok(c) => c,
        Err(e) => return e,
    };
    let gate = Gate::plan(state, &cfg, "generate", pair_cal, level_unit, false);

    let out_ports = match resolve_channels("generate", cmd, &cfg, state) {
        Ok(p) => p,
        Err(e) => return json!({"ok": false, "error": e}),
    };

    let pub_tx = state.pub_tx.clone();
    let mut eng = match make_engine_for_state(state) {
        Ok(eng) => eng,
        Err(e) => return json!({"ok": false, "error": e}),
    };
    let backend = eng.backend_name();
    let ports_for_worker = out_ports.clone();

    let mut reply = json!({
        "ok": true,
        "out_ports": out_ports,
        "level_dbfs": level_dbfs,
        "max_dbfs": MAX_EMISSION_DBFS,
        "backend": backend,
    });
    gate.write_reply(&mut reply);

    let worker = spawn_worker(state, "generate", move |stop| {
        if gate.run(&pub_tx, true).is_err() {
            return;
        }
        if let Err(e) = eng.start(&ports_for_worker, None) {
            send_pub(
                &pub_tx,
                "error",
                &json!({"cmd":"generate","message":format!("{e}")}),
            );
            return;
        }
        let amp = ac_core::shared::generator::dbfs_to_amplitude(level_dbfs);
        eng.set_tone(freq_hz, amp);
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        eng.set_silence();
        eng.stop();
        send_pub(
            &pub_tx,
            "done",
            &json!({"cmd":"generate","backend":backend}),
        );
    });

    {
        let mut workers = state.workers.lock().unwrap();
        workers.insert("generate".to_string(), worker);
    }

    reply
}

pub fn generate_pink(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "generate_pink");
    cfg_guard!(state);
    let level_dbfs = cmd
        .get("level_dbfs")
        .and_then(Value::as_f64)
        .unwrap_or(DEFAULT_LEVEL_DBFS);
    let cfg = state.cfg.lock().unwrap().clone();
    // #459: same refusal discipline as `generate` above.
    let level_dbfs = emission_guard!(state, &cfg, level_dbfs);
    let level_unit = match LevelUnit::from_request(cmd) {
        Ok(u) => u,
        Err(e) => return e,
    };
    let pair_cal = match checks::gate_pair_cal(&cfg, level_unit) {
        Ok(c) => c,
        Err(e) => return e,
    };
    let gate = Gate::plan(state, &cfg, "generate_pink", pair_cal, level_unit, false);

    let out_ports = match resolve_channels("generate_pink", cmd, &cfg, state) {
        Ok(p) => p,
        Err(e) => return json!({"ok": false, "error": e}),
    };

    let pub_tx = state.pub_tx.clone();
    let mut eng = match make_engine_for_state(state) {
        Ok(eng) => eng,
        Err(e) => return json!({"ok": false, "error": e}),
    };
    let backend = eng.backend_name();
    let ports_for_worker = out_ports.clone();

    let mut reply = json!({
        "ok": true,
        "out_ports": out_ports,
        "level_dbfs": level_dbfs,
        "max_dbfs": MAX_EMISSION_DBFS,
        "backend": backend,
    });
    gate.write_reply(&mut reply);

    let worker = spawn_worker(state, "generate_pink", move |stop| {
        if gate.run(&pub_tx, true).is_err() {
            return;
        }
        if let Err(e) = eng.start(&ports_for_worker, None) {
            send_pub(
                &pub_tx,
                "error",
                &json!({"cmd":"generate_pink","message":format!("{e}")}),
            );
            return;
        }
        let amp = ac_core::shared::generator::dbfs_to_amplitude(level_dbfs);
        eng.set_pink(amp);
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        eng.set_silence();
        eng.stop();
        send_pub(
            &pub_tx,
            "done",
            &json!({"cmd":"generate_pink","backend":backend}),
        );
    });

    {
        let mut workers = state.workers.lock().unwrap();
        workers.insert("generate_pink".to_string(), worker);
    }

    reply
}
