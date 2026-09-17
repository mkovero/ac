//! Non-audio commands: status, control, devices, setup, calibration metadata,
//! DMM passthrough, server bind-mode toggles.

use serde_json::{json, Value};

use std::collections::HashMap;

use ac_core::shared::calibration::{Calibration, DeviceEpoch, EnumerationCheck, TauEntry};

use crate::server::ServerState;

use super::{cached_capture_ports, cached_playback_ports, read_dmm_vrms, refresh_port_cache, wire};

/// The four channel fields of a `setup` update, each validated (#431).
/// Outer `None` = field absent (keep); the nullable references carry
/// `Some(None)` for `null` (clear).
struct SetupChannels {
    output: Option<u32>,
    input: Option<u32>,
    reference: Option<Option<u32>>,
    reference_output: Option<Option<u32>>,
}

fn parse_setup_channels(update: &Value) -> Result<SetupChannels, wire::WireError> {
    Ok(SetupChannels {
        output: wire::opt_u32(update, "output_channel")?,
        input: wire::opt_u32(update, "input_channel")?,
        reference: wire::opt_nullable_u32(update, "reference_channel")?,
        reference_output: wire::opt_nullable_u32(update, "reference_output_channel")?,
    })
}

pub fn status(state: &ServerState) -> Value {
    let workers = state.workers.lock().unwrap();
    let running: Option<String> = workers.keys().next().cloned();
    let listen_mode = state.listen_mode.lock().unwrap().clone();
    let cfg = state.cfg.lock().unwrap();
    let (backend_required, backend_available, backend) =
        crate::audio::backend_status(state.fake_audio, cfg.backend.as_deref());
    json!({
        "ok":            true,
        "busy":          !workers.is_empty(),
        "running_cmd":   running,
        "src_mtime":     state.src_mtime,
        "listen_mode":   listen_mode,
        "server_enabled": true,
        // Identity (#385) — lets a client tell this daemon apart from one
        // spawned under a different HOME/config on the same hardcoded port.
        "home":          state.home.clone(),
        "config_path":   state.config_path.display().to_string(),
        "pid":           state.pid,
        "started_at":    state.started_at.clone(),
        "spawn_mode":    state.spawn_mode.clone(),
        "backend_required": backend_required,
        "backend_available": backend_available,
        "backend": backend,
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
    let no_workers_remain = {
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
        workers.is_empty()
    };
    let stopped: Vec<String> = joined.iter().map(|(n, _)| n.clone()).collect();
    drop(joined); // runs Drop → joins the worker threads
    let mut reply = json!({"ok": true, "stopped": stopped});
    if no_workers_remain {
        reply["stimulus"] = json!("silent");
    }
    reply
}

pub fn devices(state: &ServerState) -> Value {
    // `devices` is the documented hardware rescan trigger — always refresh.
    refresh_port_cache(state);
    let cfg = state.cfg.lock().unwrap().clone();
    let playback = cached_playback_ports(state);
    let capture = cached_capture_ports(state);
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
        "reference_output_channel": cfg.reference_output_channel,
        "reference_output_port":    cfg.reference_output_port,
    })
}

pub fn setup(state: &ServerState, cmd: &Value) -> Value {
    let update = match cmd.get("update") {
        Some(u) => u,
        None => return json!({"ok": false, "error": "missing 'update' field"}),
    };

    // #472: the report directory is validated before anything else in the
    // update is applied, so a refusal leaves the whole config as it was —
    // "setting not changed" is true for every key in the command.
    let report_dir_update: Option<Option<std::path::PathBuf>> = match update.get("report_dir") {
        None => None,
        Some(Value::Null) => Some(None),
        Some(v) => {
            let raw = v.as_str().unwrap_or_default();
            if let Err(reason) = validate_report_dir(raw) {
                return report_dir_refusal(raw, &reason);
            }
            Some(Some(std::path::PathBuf::from(raw)))
        }
    };

    // #433: a spool path is resolved beneath the daemon's own spool root and
    // checked for ownership before anything else is applied. A refusal
    // changes nothing and removes nothing.
    let spool_update: Option<Option<std::path::PathBuf>> = match update.get("snapshot_spool_dir") {
        None => None,
        Some(Value::Null) => Some(None),
        Some(v) => {
            let raw = v.as_str().unwrap_or_default();
            match validate_spool_dir(raw) {
                Ok(leaf) => Some(Some(leaf)),
                Err(rejection) => return spool_dir_refusal(&rejection),
            }
        }
    };

    // #431: every channel field is parsed before the config is copied, so a
    // malformed one refuses the update with nothing applied — not even a
    // valid sibling channel in the same request.
    let channels = match parse_setup_channels(update) {
        Ok(c) => c,
        Err(e) => {
            return json!({"ok": false,
                "error": e.refusal("setup rejected", &[("config", "unchanged")])})
        }
    };

    // Every change is made on a copy and committed to `state.cfg` only after
    // it is on disk (#430), so a failed save leaves memory and disk agreeing
    // on the last-good config. The lock is not held across the save:
    // `dispatch()` is the only writer of `state.cfg` and runs on one thread.
    let mut cfg = state.cfg.lock().unwrap().clone();

    if let Some(dir) = report_dir_update {
        cfg.report_dir = dir;
    }

    // Explicit channel selection invalidates any prior sticky port override.
    // Without this, a stale `*_port` in config.json silently overrides the
    // new channel and routes audio to (or from) the wrong place.
    // `tests/it_loopback_ir.rs` seeds sticky ports directly into config.json
    // and never calls `setup`, so it's unaffected.
    if let Some(v) = channels.output {
        cfg.output_channel = v;
        cfg.output_port = None;
    }
    if let Some(v) = channels.input {
        cfg.input_channel = v;
        cfg.input_port = None;
    }
    if let Some(v) = channels.reference {
        cfg.reference_channel = v;
        cfg.reference_port = None;
    }
    // The reference *output* leg is a playback index and is configured
    // separately from `reference_channel` (#225) — updating one must never
    // move the other.
    if let Some(v) = channels.reference_output {
        cfg.reference_output_channel = v;
        cfg.reference_output_port = None;
    }
    if let Some(v) = update.get("dbu_ref_vrms").and_then(Value::as_f64) {
        cfg.dbu_ref_vrms = v;
    }
    if let Some(v) = update.get("server_enabled").and_then(Value::as_bool) {
        cfg.server_enabled = v;
    }
    if let Some(v) = update.get("backend") {
        if v.is_null() {
            cfg.backend = None;
        } else if let Some(raw) = v.as_str() {
            let canonical = match ac_core::config::canonical_backend(Some(raw)) {
                Ok(Some(v)) => v,
                Ok(None) => unreachable!("a string canonicalizes to a backend"),
                Err(e) => return json!({"ok": false, "error": e}),
            };
            cfg.backend = Some(canonical.to_string());
        } else {
            return json!({"ok": false, "error":
                "backend must be jack, cpal, fake, or null"});
        }
    }
    if update.get("dmm_host").is_some() {
        cfg.dmm_host = update["dmm_host"].as_str().map(str::to_string);
    }
    if let Some(v) = update.get("server_idle_timeout_secs") {
        if v.is_null() {
            cfg.server_idle_timeout_secs = None;
        } else if let Some(n) = v.as_u64() {
            cfg.server_idle_timeout_secs = if n == 0 { None } else { Some(n) };
        }
    }
    // Snapshot backend (handoff: snapshot-backend M1, deliverable 1/2).
    if let Some(v) = update.get("snapshot_ring_s").and_then(Value::as_f64) {
        if v > 0.0 {
            cfg.snapshot_ring_s = v;
        }
    }
    if let Some(dir) = spool_update {
        cfg.snapshot_spool_dir = dir;
    }
    // Room temperature for the delay readout's ms → m conversion (#243).
    // `null` clears it back to the conventional 343 m/s, which is a
    // different statement from any temperature the operator could type.
    if let Some(v) = update.get("temperature_c") {
        if v.is_null() {
            cfg.temperature_c = None;
        } else if let Some(t) = v.as_f64() {
            cfg.temperature_c = Some(t);
        }
    }

    // `update: {}` is a read (`ac generate`, the GPIO handler): it never
    // writes, so it cannot fail on storage and cannot overwrite a config file
    // the per-request reload found unreadable.
    let has_updates = update.as_object().is_some_and(|m| !m.is_empty());
    let mut saved_path = None;
    if has_updates {
        match ac_core::config::save(&cfg, Some(&state.config_path)) {
            Ok(final_cfg) => {
                *state.cfg.lock().unwrap() = final_cfg.clone();
                cfg = final_cfg;
                saved_path = Some(state.config_path.display().to_string());
            }
            Err(e) => return json!({"ok": false, "error": setup_not_saved(&e)}),
        }
    }
    let cfg_value = serde_json::to_value(&cfg).unwrap_or_default();
    // #459: the fixed emission maximum, at the top level (beside `config`,
    // not inside it — `config` is the config file, and the maximum is a
    // build constant; putting it there would make it look settable). This
    // is the one place the maximum is visible without starting an
    // emission — `setup` is never refused, retired key or not, so it stays
    // reachable exactly when an operator needs to find out what the
    // maximum is.
    let mut reply = json!({
        "ok": true,
        "config": cfg_value,
        "max_dbfs": ac_core::shared::emission_level::MAX_EMISSION_DBFS,
    });
    if let Some(path) = saved_path {
        reply["saved"] = json!(path);
    }
    // #472: whether the configured report directory can be written, beside
    // `config` for the same reason as `max_dbfs` — it is not a config
    // value. Checked on every call, so a directory removed after it was set
    // shows up in the read-out before a capture, not after one.
    if let Some(dir) = cfg.report_dir.as_ref() {
        reply["report_dir_status"] = match probe_dir_writable(dir) {
            Ok(()) => json!({"writable": true}),
            Err(e) => json!({"writable": false, "error": e.to_string()}),
        };
    }
    reply
}

/// Operator-facing refusal for a `setup` update that was not persisted
/// (#430). Laid out like the `calibration unreadable` refusal: continuation
/// lines are indented to sit under the text after `  error: `.
fn setup_not_saved(e: &ac_core::config::SaveError) -> String {
    use ac_core::config::SaveError;
    match e {
        SaveError::Write { .. } => format!(
            "setup not saved \u{2014} configuration unchanged\n\
             \x20        file   {}\n\
             \x20        cause  {:#}",
            e.path().display(),
            e.cause()
        ),
        SaveError::Unreadable { .. } => format!(
            "setup not saved \u{2014} existing configuration is unreadable\n\
             \x20        file   {}\n\
             \x20        cause  {:#}\n\
             \x20        data   existing file preserved",
            e.path().display(),
            e.cause()
        ),
    }
}

/// Refusal reason for a non-absolute `report_dir`: a relative path or a `~`
/// means nothing to a daemon that may not share the client's cwd or `$HOME`.
const REPORT_DIR_NOT_ABSOLUTE: &str = "must be an absolute path on the daemon host";

/// Whether `raw` can be stored as the report directory (#472): absolute,
/// existing, and writable by this process. Never creates it — a typo must
/// not become a working archive in the wrong place. The reason is the
/// `std::io::Error` text as the OS gave it, so it names what was observed
/// and nothing more.
fn validate_report_dir(raw: &str) -> Result<(), String> {
    let path = std::path::Path::new(raw);
    if raw.is_empty() || !path.is_absolute() {
        return Err(REPORT_DIR_NOT_ABSOLUTE.to_string());
    }
    probe_dir_writable(path).map_err(|e| e.to_string())
}

/// Write probe shared by setup-time validation and `report_dir_status`:
/// `create_new` a dot-prefixed file in `dir`, then remove it (a failed
/// removal is ignored — a leftover probe file is harmless and visible).
fn probe_dir_writable(dir: &std::path::Path) -> std::io::Result<()> {
    let meta = std::fs::metadata(dir)?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let probe = dir.join(format!(".ac-write-probe-{}-{nanos}", std::process::id()));
    // On a plain file the open fails with the OS's own "Not a directory".
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)?;
    let _ = std::fs::remove_file(&probe);
    if !meta.is_dir() {
        return Err(std::io::Error::from(std::io::ErrorKind::NotADirectory));
    }
    Ok(())
}

fn report_dir_refusal(path: &str, reason: &str) -> Value {
    json!({
        "ok": false,
        "error": format!("report-dir {path}: {reason} \u{2014} setting not changed"),
        "refused": {"key": "report_dir", "path": path, "reason": reason},
    })
}

/// Resolve a requested `snapshot_spool_dir` to an absolute child of the
/// spool root and check that an existing directory there is daemon-owned
/// (#433). Inspects only; the leaf is created when a session first uses it.
fn validate_spool_dir(raw: &str) -> Result<std::path::PathBuf, ac_core::config::SpoolRejection> {
    let root = ac_core::config::snapshot_spool_root();
    let leaf = ac_core::config::resolve_snapshot_spool_leaf(&root, std::path::Path::new(raw))?;
    ac_core::config::inspect_snapshot_spool_leaf(&root, &leaf)?;
    Ok(leaf)
}

fn spool_dir_refusal(r: &ac_core::config::SpoolRejection) -> Value {
    json!({
        "ok": false,
        "error": r.message(),
        "refused": {
            "key": "snapshot_spool_dir",
            "path": r.requested,
            "reason": r.reason,
            "allowed_root": r.allowed.display().to_string(),
        },
    })
}

/// `tau_history` as sent on the wire (#461): each stored entry verbatim,
/// plus `current_enumeration_check` — how the entry's epoch relates to the
/// one its backend is in *now*, computed at reply time. Named `current_*` so
/// it cannot be mistaken for a stored field. The epoch is sampled once per
/// backend per reply, never cached across replies.
fn tau_history_with_live_check(history: &[TauEntry]) -> Vec<Value> {
    let mut epochs: HashMap<&str, DeviceEpoch> = HashMap::new();
    history
        .iter()
        .map(|entry| {
            let backend = entry.conditions.backend.as_str();
            let current = epochs
                .entry(backend)
                .or_insert_with(|| crate::audio::epoch::current_epoch(backend));
            let check = EnumerationCheck::of(entry.enumeration.as_ref(), current);
            let mut v = json!(entry);
            v["current_enumeration_check"] = json!(check);
            v
        })
        .collect()
}

pub fn get_calibration(state: &ServerState, cmd: &Value) -> Value {
    let (cfg_out, cfg_in) = {
        let cfg = state.cfg.lock().unwrap();
        (cfg.output_channel, cfg.input_channel)
    };
    let pick = |field: &str, default: u32| wire::opt_u32(cmd, field).map(|v| v.unwrap_or(default));
    let (out_ch, in_ch) = match (
        pick("output_channel", cfg_out),
        pick("input_channel", cfg_in),
    ) {
        (Ok(o), Ok(i)) => (o, i),
        (Err(e), _) | (_, Err(e)) => {
            return json!({"ok": false,
                "error": e.refusal("calibration lookup rejected", &[])})
        }
    };

    match Calibration::load(out_ch, in_ch, None) {
        Err(e) => json!({"ok": false, "error": format!("{e}")}),
        Ok(None) => json!({"ok": true, "found": false}),
        Ok(Some(cal)) => json!({
            "ok":                                true,
            "found":                             true,
            "key":                               cal.key(),
            "vrms_at_0dbfs_out":                 cal.vrms_at_0dbfs_out,
            "vrms_at_0dbfs_in":                  cal.vrms_at_0dbfs_in,
            "ref_dbfs":                          cal.ref_dbfs,
            "mic_sensitivity_dbfs_at_94db_spl":  cal.mic_sensitivity_dbfs_at_94db_spl,
            "mic_response":                      cal.mic_response,
            "tau_history":                       tau_history_with_live_check(&cal.tau_history),
        }),
    }
}

pub fn list_calibrations(_state: &ServerState) -> Value {
    match Calibration::load_all(None) {
        Err(e) => json!({"ok": false, "error": format!("{e}")}),
        Ok(cals) => {
            let list: Vec<Value> = cals
                .iter()
                .map(|c| {
                    json!({
                        "key":                               c.key(),
                        "vrms_at_0dbfs_out":                 c.vrms_at_0dbfs_out,
                        "vrms_at_0dbfs_in":                  c.vrms_at_0dbfs_in,
                        "mic_sensitivity_dbfs_at_94db_spl":  c.mic_sensitivity_dbfs_at_94db_spl,
                        "mic_response":                      c.mic_response,
                        "tau_history":                       tau_history_with_live_check(&c.tau_history),
                    })
                })
                .collect();
            json!({"ok": true, "calibrations": list})
        }
    }
}

pub fn dmm_read(state: &ServerState) -> Value {
    let cfg = state.cfg.lock().unwrap();
    let host = match &cfg.dmm_host {
        Some(h) => h.clone(),
        None => {
            return json!({"ok": false,
                    "error": "no DMM configured on server — run: ac setup dmm <host>"})
        }
    };
    drop(cfg);
    match read_dmm_vrms(&host, 3) {
        Some(v) => json!({"ok": true, "vrms": v, "idn": null}),
        None => json!({"ok": false, "error": format!("DMM at {host} did not respond")}),
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
    if mode != "fft" && mode != "cwt" && mode != "cqt" && mode != "reassigned" {
        return json!({
            "ok": false,
            "error": format!(
                "invalid mode '{mode}': expected 'fft', 'cwt', 'cqt', or 'reassigned'"
            ),
        });
    }
    *state.analysis_mode.lock().unwrap() = mode.to_string();
    if let Some(s) = cmd.get("sigma").and_then(Value::as_f64) {
        let s = (s as f32).clamp(5.0, 24.0);
        *state.cwt_sigma.lock().unwrap() = s;
    }
    if let Some(n) = cmd.get("n_scales").and_then(Value::as_u64) {
        let n = (n as usize).clamp(64, 8192);
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

/// Toggle/set the per-tick `fractional_octave` frame published alongside
/// `cwt`. `bpo == 0` disables; `1/3/6/12/24` enable. Effect is server-global
/// and the next `monitor_spectrum` tick picks it up live (no worker
/// restart). Persists across worker restart, reset on daemon restart.
pub fn set_ioct_bpo(state: &ServerState, cmd: &Value) -> Value {
    let raw = match cmd.get("bpo") {
        Some(v) => v,
        None => return json!({"ok": false, "error": "missing 'bpo' field"}),
    };
    // #431: checked, never narrowed — 4294967299 used to wrap to 3.
    // The echo comes from `WireError::received`, which is length-bounded;
    // the raw value would copy an arbitrarily long string into the reply.
    let refuse = |shown: String| {
        json!({"ok": false,
        "error": format!("invalid bpo {shown}: expected 0, 1, 3, 6, 12, or 24")})
    };
    let new = match wire::u32_value(raw, "bpo") {
        Ok(0) => None,
        Ok(b @ (1 | 3 | 6 | 12 | 24)) => Some(b),
        Ok(b) => return refuse(b.to_string()),
        Err(e) => return refuse(e.received),
    };
    let bpo = new.unwrap_or(0);
    *state.ioct_bpo.lock().unwrap() = new;
    json!({"ok": true, "bpo": bpo})
}

/// Set the frequency-weighting curve applied to every band level in
/// the live `fractional_octave` / `fractional_octave_leq` frames.
/// One of `off`, `a`, `c`, `z`. `off` and `z` are functionally the
/// same (identity curve); both are accepted so the protocol can
/// distinguish "user hasn't picked one" from "user explicitly picked
/// Z" in UI affordances. Case-insensitive; the reply echoes the
/// canonical lowercase form. Server-global; picked up by the next
/// monitor tick.
pub fn set_band_weighting(state: &ServerState, cmd: &Value) -> Value {
    let mode = match cmd.get("mode").and_then(Value::as_str) {
        Some(m) => m.to_ascii_lowercase(),
        None => return json!({"ok": false, "error": "missing 'mode' field"}),
    };
    match mode.as_str() {
        "off" | "a" | "c" | "z" => {}
        other => {
            return json!({"ok": false,
            "error": format!("invalid mode {other:?}: expected off, a, c, or z")})
        }
    }
    *state.band_weighting.lock().unwrap() = mode.clone();
    json!({"ok": true, "mode": mode})
}

pub fn get_band_weighting(state: &ServerState) -> Value {
    let mode = state.band_weighting.lock().unwrap().clone();
    json!({"ok": true, "mode": mode})
}

/// Set the per-band time-integration mode applied to the live
/// `fractional_octave` frames: `off`, `fast` (τ=125 ms), `slow` (τ=1 s),
/// or `leq` (unbounded). When non-`off`, the monitor worker publishes a
/// `fractional_octave_leq` frame after each `fractional_octave` frame
/// carrying the integrated levels. Server-global; picked up by the next
/// monitor tick without a worker restart.
pub fn set_time_integration(state: &ServerState, cmd: &Value) -> Value {
    let mode = match cmd.get("mode").and_then(Value::as_str) {
        Some(m) => m.to_ascii_lowercase(),
        None => return json!({"ok": false, "error": "missing 'mode' field"}),
    };
    match mode.as_str() {
        "off" | "fast" | "slow" | "leq" => {}
        other => {
            return json!({"ok": false,
            "error": format!("invalid mode {other:?}: expected off, fast, slow, or leq")})
        }
    }
    *state.time_integration_mode.lock().unwrap() = mode.clone();
    // Entering leq from any other mode starts a fresh accumulation;
    // the worker checks the reset flag on its next tick.
    if mode == "leq" {
        state
            .leq_reset_request
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
    json!({"ok": true, "mode": mode})
}

pub fn get_time_integration(state: &ServerState) -> Value {
    let mode = state.time_integration_mode.lock().unwrap().clone();
    json!({"ok": true, "mode": mode})
}

/// Zero the Leq accumulators on the next monitor tick. Fast/slow modes
/// don't need explicit reset — they re-prime from their next input on
/// their own. Safe to call when no monitor is active; the flag is held
/// until a worker consumes it.
pub fn reset_leq(state: &ServerState) -> Value {
    state
        .leq_reset_request
        .store(true, std::sync::atomic::Ordering::Relaxed);
    json!({"ok": true})
}

/// Zero the per-channel BS.1770-5 loudness state (LKFS-I, LRA, dBTP) on
/// the next monitor tick. Momentary / short-term windows re-prime from
/// their next input on their own. Safe to call with no monitor active —
/// the flag is one-shot and held until a worker consumes it.
pub fn reset_loudness(state: &ServerState) -> Value {
    state
        .loudness_reset_request
        .store(true, std::sync::atomic::Ordering::Relaxed);
    json!({"ok": true})
}

/// Live-tune `interval` and/or `fft_n` on a running `monitor_spectrum` worker.
/// Rejects if no monitor is active (the worker owns the Arc; without it the
/// change has nothing to pick up).
pub fn set_monitor_params(state: &ServerState, cmd: &Value) -> Value {
    let req_interval = cmd.get("interval").and_then(Value::as_f64);
    // #431: a present `fft_n` that is not a u32 gets the domain refusal,
    // ahead of the `no active monitor` check — 4294967552 used to wrap to 256.
    let req_fft_n = match wire::opt_u32(cmd, "fft_n") {
        Ok(n) => n,
        Err(_) => return json!({"ok": false, "error": "fft_n must be power of 2 in [256, 131072]"}),
    };

    if let Some(i) = req_interval {
        if !(i > 0.0 && i <= 60.0) {
            return json!({"ok": false, "error": "interval must be > 0 and <= 60"});
        }
    }
    if let Some(n) = req_fft_n {
        if !n.is_power_of_two() || !(256..=131_072).contains(&n) {
            return json!({"ok": false, "error": "fft_n must be power of 2 in [256, 131072]"});
        }
    }

    let mut mp = state.monitor_params.lock().unwrap();
    if !mp.active {
        return json!({"ok": false, "error": "no active monitor"});
    }
    if let Some(i) = req_interval {
        mp.interval = i;
    }
    if let Some(n) = req_fft_n {
        mp.fft_n = n;
    }
    // `lf_fft_n` / `crossover_hz` are daemon-owned constants the UI can't set
    // in this issue — echo them read-only so the LF resolution label stays
    // single-sourced from the daemon (#142).
    json!({
        "ok": true,
        "interval": mp.interval,
        "fft_n": mp.fft_n,
        "lf_fft_n": mp.lf_fft_n,
        "crossover_hz": mp.crossover_hz,
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
        // Identity (#385) — same fields as `status`.
        "home":          state.home.clone(),
        "config_path":   state.config_path.display().to_string(),
        "pid":           state.pid,
        "started_at":    state.started_at.clone(),
        "spawn_mode":    state.spawn_mode.clone(),
    })
}
