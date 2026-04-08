//! Command handlers.  Each function receives the ServerState + parsed JSON command
//! and returns a JSON Value to send as the CTRL reply.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use serde_json::{json, Value};

use ac_core::calibration::Calibration;
use ac_core::config::Config;

use crate::audio::{make_engine, AudioEngine};
use crate::server::ServerState;
use crate::workers::{cmd_group, Group, WorkerHandle};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Check the busy guard and return Err string if a conflict exists.
fn check_busy(state: &ServerState, new_cmd: &str) -> Option<String> {
    let new_group = match cmd_group(new_cmd) {
        Some(g) => g,
        None    => return None, // non-audio command, always allowed
    };

    let workers = state.workers.lock().unwrap();
    if workers.is_empty() {
        return None;
    }

    // Any exclusive running → block everything
    for name in workers.keys() {
        if matches!(cmd_group(name), Some(Group::Exclusive)) {
            return Some(format!("busy: {name} running — send stop first"));
        }
    }

    // New exclusive → block if anything is running
    if new_group == Group::Exclusive {
        let running: Vec<&String> = workers.keys().collect();
        return Some(format!("busy: {} running — send stop first", running[0]));
    }

    // Duplicate group
    for name in workers.keys() {
        if cmd_group(name) == Some(new_group) {
            return Some(format!("busy: {name} running — send stop first"));
        }
    }

    None
}

/// Spawn a worker thread and register it.
fn spawn_worker<F>(state: &ServerState, cmd_name: &str, f: F) -> WorkerHandle
where
    F: FnOnce(Arc<AtomicBool>) + Send + 'static,
{
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let t = thread::spawn(move || f(stop2));
    WorkerHandle { stop_flag: stop, thread: Some(t) }
}

/// Resolve output port: config sticky name, or fall back to channel index in engine list.
fn resolve_output(cfg: &Config, fake_audio: bool) -> String {
    if let Some(p) = &cfg.output_port {
        return p.clone();
    }
    let eng = make_engine(fake_audio);
    let ports = eng.playback_ports();
    let ch = cfg.output_channel as usize;
    ports.get(ch).cloned().unwrap_or_else(|| "system:playback_1".to_string())
}

/// Resolve input port: config sticky name, or fall back to channel index in engine list.
fn resolve_input(cfg: &Config, fake_audio: bool) -> String {
    if let Some(p) = &cfg.input_port {
        return p.clone();
    }
    let eng = make_engine(fake_audio);
    let ports = eng.capture_ports();
    let ch = cfg.input_channel as usize;
    ports.get(ch).cloned().unwrap_or_else(|| "system:capture_1".to_string())
}

// ---------------------------------------------------------------------------
// Non-audio commands
// ---------------------------------------------------------------------------

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
    // Stop all workers
    let mut workers = state.workers.lock().unwrap();
    for w in workers.values_mut() {
        w.stop();
    }
    drop(workers);
    // Signal main loop to exit via _quit flag
    json!({"ok": true, "_quit": true})
}

pub fn stop(state: &ServerState, cmd: &Value) -> Value {
    let target = cmd.get("name").and_then(Value::as_str);
    let mut workers = state.workers.lock().unwrap();
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
    let cfg = state.cfg.lock().unwrap().clone();
    let engine = make_engine(state.fake_audio);
    let playback = engine.playback_ports();
    let capture  = engine.capture_ports();
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
    if let Some(v) = update.get("reference_channel").and_then(Value::as_u64) {
        cfg.reference_channel = Some(v as u32);
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

pub fn list_calibrations(state: &ServerState) -> Value {
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

pub fn dmm_read(_state: &ServerState) -> Value {
    json!({"ok": false, "error": "no DMM configured on server — run: ac setup dmm <host>"})
}

pub fn server_enable(state: &ServerState) -> Value {
    *state.listen_mode.lock().unwrap() = "public".to_string();
    // Signal the main loop to rebind after this reply is sent
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

// ---------------------------------------------------------------------------
// Audio commands — each spawns a worker thread
// ---------------------------------------------------------------------------

macro_rules! busy_guard {
    ($state:expr, $name:expr) => {
        if let Some(msg) = check_busy($state, $name) {
            return json!({"ok": false, "error": msg});
        }
    };
}

pub fn generate(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "generate");
    let freq_hz    = cmd.get("freq_hz")   .and_then(Value::as_f64).unwrap_or(1000.0);
    let level_dbfs = cmd.get("level_dbfs").and_then(Value::as_f64).unwrap_or(-10.0);
    let cfg        = state.cfg.lock().unwrap().clone();

    let out_port = vec![resolve_output(&cfg, state.fake_audio)];

    let pub_tx   = state.pub_tx.clone();
    let fake     = state.fake_audio;

    let worker = spawn_worker(state, "generate", move |stop| {
        let mut eng = make_engine(fake);
        if let Err(e) = eng.start(&out_port, None) {
            send_pub(&pub_tx, "error", &json!({"cmd":"generate","message":format!("{e}")}));
            return;
        }
        let amp = ac_core::generator::dbfs_to_amplitude(level_dbfs);
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

    let resolved = resolve_output(&cfg, state.fake_audio);
    json!({"ok": true, "out_ports": [resolved]})
}

pub fn generate_pink(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "generate_pink");
    let level_dbfs = cmd.get("level_dbfs").and_then(Value::as_f64).unwrap_or(-10.0);
    let cfg        = state.cfg.lock().unwrap().clone();

    let out_port = vec![resolve_output(&cfg, state.fake_audio)];

    let pub_tx = state.pub_tx.clone();
    let fake   = state.fake_audio;

    let worker = spawn_worker(state, "generate_pink", move |stop| {
        let mut eng = make_engine(fake);
        if let Err(e) = eng.start(&out_port, None) {
            send_pub(&pub_tx, "error", &json!({"cmd":"generate_pink","message":format!("{e}")}));
            return;
        }
        let amp = ac_core::generator::dbfs_to_amplitude(level_dbfs);
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

    let resolved = resolve_output(&cfg, state.fake_audio);
    json!({"ok": true, "out_ports": [resolved]})
}

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
    let out_port   = resolve_output(&cfg, state.fake_audio);
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
    let out_port   = resolve_output(&cfg, state.fake_audio);
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

pub fn plot(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "plot");
    let start_hz   = cmd.get("start_hz")  .and_then(Value::as_f64).unwrap_or(20.0);
    let stop_hz    = cmd.get("stop_hz")   .and_then(Value::as_f64).unwrap_or(20_000.0);
    let level_dbfs = cmd.get("level_dbfs").and_then(Value::as_f64).unwrap_or(-10.0);
    let ppd        = cmd.get("ppd")       .and_then(Value::as_u64).unwrap_or(10) as usize;
    let duration   = cmd.get("duration")  .and_then(Value::as_f64).unwrap_or(1.0);
    let cfg        = state.cfg.lock().unwrap().clone();

    let out_port = resolve_output(&cfg, state.fake_audio);
    let in_port  = resolve_input(&cfg, state.fake_audio);
    let out_port_reply = out_port.clone();
    let in_port_reply  = in_port.clone();

    let pub_tx   = state.pub_tx.clone();
    let fake     = state.fake_audio;
    let out_ch   = cfg.output_channel;
    let in_ch    = cfg.input_channel;

    let worker = spawn_worker(state, "plot", move |stop| {
        let cal = Calibration::load(out_ch, in_ch, None).ok().flatten();
        let freqs = log_freq_points(start_hz, stop_hz, ppd);
        let amplitude = ac_core::generator::dbfs_to_amplitude(level_dbfs);

        let mut eng = make_engine(fake);
        if let Err(e) = eng.start(&[out_port], Some(&in_port)) {
            send_pub(&pub_tx, "error", &json!({"cmd":"plot","message":format!("{e}")}));
            return;
        }
        let sr = eng.sample_rate();

        let mut n = 0usize;
        let mut xruns = 0u32;
        for freq in &freqs {
            if stop.load(Ordering::Relaxed) { break; }
            let dur = f64::max(duration, 3.0 / freq); // at least 3 cycles
            eng.set_tone(*freq, amplitude);
            // warmup
            let _ = eng.capture_block(0.1);
            let samples = match eng.capture_block(dur) {
                Ok(s) => s,
                Err(e) => {
                    send_pub(&pub_tx, "error", &json!({"cmd":"plot","message":format!("{e}")}));
                    return;
                }
            };
            xruns += eng.xruns();

            match ac_core::analysis::analyze(&samples, sr, *freq, 10) {
                Ok(r)  => {
                    let frame = sweep_point_frame(&r, cal.as_ref(), n, "plot", level_dbfs, Some(*freq));
                    send_pub(&pub_tx, "data", &frame);
                    n += 1;
                }
                Err(e) => eprintln!("plot: analyze error at {freq}Hz: {e}"),
            }
        }
        eng.set_silence();
        eng.stop();
        send_pub(&pub_tx, "done", &json!({"cmd":"plot","n_points":n,"xruns":xruns}));
    });

    {
        let mut workers = state.workers.lock().unwrap();
        workers.insert("plot".to_string(), worker);
    }
    json!({"ok": true, "out_port": out_port_reply, "in_port": in_port_reply})
}

pub fn plot_level(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "plot_level");
    let freq_hz    = cmd.get("freq_hz")   .and_then(Value::as_f64).unwrap_or(1000.0);
    let start_dbfs = cmd.get("start_dbfs").and_then(Value::as_f64).unwrap_or(-40.0);
    let stop_dbfs  = cmd.get("stop_dbfs") .and_then(Value::as_f64).unwrap_or(0.0);
    let steps      = cmd.get("steps")     .and_then(Value::as_u64).unwrap_or(26) as usize;
    let duration   = cmd.get("duration")  .and_then(Value::as_f64).unwrap_or(1.0);
    let cfg        = state.cfg.lock().unwrap().clone();

    let out_port = resolve_output(&cfg, state.fake_audio);
    let in_port  = resolve_input(&cfg, state.fake_audio);
    let out_port_reply = out_port.clone();
    let in_port_reply  = in_port.clone();

    let pub_tx   = state.pub_tx.clone();
    let fake     = state.fake_audio;
    let out_ch   = cfg.output_channel;
    let in_ch    = cfg.input_channel;

    let worker = spawn_worker(state, "plot_level", move |stop| {
        let cal = Calibration::load(out_ch, in_ch, None).ok().flatten();
        let levels = linspace(start_dbfs, stop_dbfs, steps);

        let mut eng = make_engine(fake);
        if let Err(e) = eng.start(&[out_port], Some(&in_port)) {
            send_pub(&pub_tx, "error", &json!({"cmd":"plot_level","message":format!("{e}")}));
            return;
        }
        let sr = eng.sample_rate();

        let mut n = 0usize;
        let mut xruns = 0u32;
        for &level_dbfs in &levels {
            if stop.load(Ordering::Relaxed) { break; }
            let amplitude = ac_core::generator::dbfs_to_amplitude(level_dbfs);
            eng.set_tone(freq_hz, amplitude);
            let _ = eng.capture_block(0.1);
            let samples = match eng.capture_block(duration) {
                Ok(s) => s,
                Err(e) => {
                    send_pub(&pub_tx, "error", &json!({"cmd":"plot_level","message":format!("{e}")}));
                    return;
                }
            };
            xruns += eng.xruns();

            match ac_core::analysis::analyze(&samples, sr, freq_hz, 10) {
                Ok(r) => {
                    let frame = sweep_point_frame(&r, cal.as_ref(), n, "plot_level",
                                                  level_dbfs, Some(freq_hz));
                    send_pub(&pub_tx, "data", &frame);
                    n += 1;
                }
                Err(e) => eprintln!("plot_level: analyze error at {level_dbfs}dBFS: {e}"),
            }
        }
        eng.set_silence();
        eng.stop();
        send_pub(&pub_tx, "done", &json!({"cmd":"plot_level","n_points":n,"xruns":xruns}));
    });

    {
        let mut workers = state.workers.lock().unwrap();
        workers.insert("plot_level".to_string(), worker);
    }
    json!({"ok": true, "out_port": out_port_reply, "in_port": in_port_reply})
}

pub fn monitor_spectrum(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "monitor_spectrum");
    let freq_hz  = cmd.get("freq_hz") .and_then(Value::as_f64).unwrap_or(1000.0);
    let interval = cmd.get("interval").and_then(Value::as_f64).unwrap_or(0.2);
    let cfg      = state.cfg.lock().unwrap().clone();
    let in_port  = resolve_input(&cfg, state.fake_audio);
    let in_port_reply = in_port.clone();

    let pub_tx = state.pub_tx.clone();
    let fake   = state.fake_audio;
    let out_ch = cfg.output_channel;
    let in_ch  = cfg.input_channel;

    let worker = spawn_worker(state, "monitor_spectrum", move |stop| {
        let cal = Calibration::load(out_ch, in_ch, None).ok().flatten();
        let mut eng = make_engine(fake);
        if let Err(e) = eng.start(&[], Some(&in_port)) {
            send_pub(&pub_tx, "error", &json!({"cmd":"monitor_spectrum","message":format!("{e}")}));
            return;
        }
        let sr = eng.sample_rate();
        let mut current_freq = freq_hz;
        let mut xruns_total = 0u32;

        while !stop.load(Ordering::Relaxed) {
            let samples = match eng.capture_block(interval) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("monitor_spectrum: capture error: {e}");
                    break;
                }
            };
            xruns_total += eng.xruns();

            if let Ok(r) = ac_core::analysis::analyze(&samples, sr, current_freq, 10) {
                current_freq = r.fundamental_hz;
                let in_dbu = cal.as_ref()
                    .and_then(|c| c.in_vrms(r.linear_rms))
                    .map(ac_core::conversions::vrms_to_dbu);
                let (spec, freqs) = downsample(&r.spectrum, &r.freqs, 1000);
                let frame = json!({
                    "type":             "spectrum",
                    "cmd":              "monitor_spectrum",
                    "freq_hz":          current_freq,
                    "sr":               sr,
                    "freqs":            freqs,
                    "spectrum":         spec,
                    "fundamental_dbfs": r.fundamental_dbfs,
                    "thd_pct":          r.thd_pct,
                    "thdn_pct":         r.thdn_pct,
                    "in_dbu":           in_dbu,
                    "clipping":         r.clipping,
                    "xruns":            xruns_total,
                });
                send_pub(&pub_tx, "data", &frame);
            }
        }
        eng.stop();
        send_pub(&pub_tx, "done", &json!({"cmd":"monitor_spectrum"}));
    });

    {
        let mut workers = state.workers.lock().unwrap();
        workers.insert("monitor_spectrum".to_string(), worker);
    }
    json!({"ok": true, "in_port": in_port_reply})
}

pub fn calibrate(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "calibrate");
    // Calibration is complex (interactive DMM prompts).
    // For now return a stub that completes immediately with no readings.
    let cfg    = state.cfg.lock().unwrap().clone();
    let out_ch = cmd.get("output_channel")
        .and_then(Value::as_u64)
        .unwrap_or(cfg.output_channel as u64) as u32;
    let in_ch  = cmd.get("input_channel")
        .and_then(Value::as_u64)
        .unwrap_or(cfg.input_channel as u64) as u32;
    let ref_dbfs = cmd.get("ref_dbfs").and_then(Value::as_f64).unwrap_or(-10.0);

    let pub_tx = state.pub_tx.clone();
    let fake   = state.fake_audio;
    let out_port = resolve_output(&cfg, state.fake_audio);
    let in_port  = resolve_input(&cfg, state.fake_audio);

    let worker = spawn_worker(state, "calibrate", move |stop| {
        // Step 1: output calibration prompt
        send_pub(&pub_tx, "cal_prompt", &json!({
            "step":     1,
            "text":     "Connect DMM to output. Press Enter to skip or enter Vrms reading.",
            "dmm_vrms": null,
        }));

        // Wait for cal_reply (up to 60 s)
        // In the stub we just wait for stop or use a fake reading.
        // Real implementation would wait for a channel signal from cal_reply handler.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !stop.load(Ordering::Relaxed) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        let key = format!("out{out_ch}_in{in_ch}");
        send_pub(&pub_tx, "cal_done", &json!({
            "key":               key,
            "vrms_at_0dbfs_out": null,
            "vrms_at_0dbfs_in":  null,
        }));
        send_pub(&pub_tx, "done", &json!({"cmd":"calibrate"}));
    });

    {
        let mut workers = state.workers.lock().unwrap();
        workers.insert("calibrate".to_string(), worker);
    }
    json!({"ok": true})
}

pub fn cal_reply(_state: &ServerState, _cmd: &Value) -> Value {
    // In the full implementation this would signal the calibrate worker.
    // For now, just acknowledge.
    json!({"ok": true})
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

fn send_pub(tx: &crossbeam_channel::Sender<Vec<u8>>, topic: &str, frame: &Value) {
    let mut msg = topic.as_bytes().to_vec();
    msg.push(b' ');
    msg.extend_from_slice(serde_json::to_vec(frame).unwrap_or_default().as_slice());
    let _ = tx.send(msg);
}

fn log_freq_points(start: f64, stop: f64, ppd: usize) -> Vec<f64> {
    let n_decades = (stop / start).log10();
    let n_points  = (n_decades * ppd as f64).round() as usize;
    let n_points  = n_points.max(2);
    let mut freqs: Vec<f64> = (0..n_points)
        .map(|i| start * (stop / start).powf(i as f64 / (n_points - 1) as f64))
        .collect();
    freqs.dedup_by(|a, b| (*a as u64) == (*b as u64));
    freqs
}

fn linspace(start: f64, stop: f64, n: usize) -> Vec<f64> {
    if n <= 1 { return vec![start]; }
    (0..n).map(|i| start + (stop - start) * i as f64 / (n - 1) as f64).collect()
}

fn downsample(spec: &[f64], freqs: &[f64], max_pts: usize) -> (Vec<f64>, Vec<f64>) {
    if spec.len() <= max_pts {
        return (spec.to_vec(), freqs.to_vec());
    }
    let n = spec.len();
    let indices: Vec<usize> = {
        let mut v: Vec<usize> = (0..max_pts)
            .map(|i| {
                let t = i as f64 / (max_pts - 1) as f64;
                ((n - 1) as f64 * t) as usize
            })
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    let s: Vec<f64> = indices.iter().map(|&i| spec[i]).collect();
    let f: Vec<f64> = indices.iter().map(|&i| freqs[i]).collect();
    (s, f)
}

fn sweep_point_frame(
    r: &ac_core::types::AnalysisResult,
    cal: Option<&Calibration>,
    n: usize,
    cmd_name: &str,
    level_dbfs: f64,
    freq_hz: Option<f64>,
) -> Value {
    let out_vrms = cal.and_then(|c| c.out_vrms(level_dbfs));
    let in_vrms  = cal.and_then(|c| c.in_vrms(r.linear_rms));
    let in_dbu   = in_vrms .map(ac_core::conversions::vrms_to_dbu);
    let out_dbu  = out_vrms.map(ac_core::conversions::vrms_to_dbu);
    let gain_db  = in_dbu.zip(out_dbu).map(|(i, o)| i - o);

    // Skip DC bin (index 0)
    let (spec_ds, freqs_ds) = if r.spectrum.len() > 1 {
        downsample(&r.spectrum[1..], &r.freqs[1..], 1000)
    } else {
        (r.spectrum.clone(), r.freqs.clone())
    };

    let harmonic_levels: Vec<Value> = r.harmonic_levels.iter()
        .map(|&(hz, amp)| json!([hz, amp]))
        .collect();

    let mut frame = json!({
        "type":              "sweep_point",
        "cmd":               cmd_name,
        "n":                 n,
        "drive_db":          level_dbfs,
        "thd_pct":           r.thd_pct,
        "thdn_pct":          r.thdn_pct,
        "fundamental_hz":    r.fundamental_hz,
        "fundamental_dbfs":  r.fundamental_dbfs,
        "linear_rms":        r.linear_rms,
        "harmonic_levels":   harmonic_levels,
        "noise_floor_dbfs":  r.noise_floor_dbfs,
        "spectrum":          spec_ds,
        "freqs":             freqs_ds,
        "clipping":          r.clipping,
        "ac_coupled":        r.ac_coupled,
        "out_vrms":          out_vrms,
        "out_dbu":           out_dbu,
        "in_vrms":           in_vrms,
        "in_dbu":            in_dbu,
        "gain_db":           gain_db,
        "vrms_at_0dbfs_out": cal.and_then(|c| c.vrms_at_0dbfs_out),
        "vrms_at_0dbfs_in":  cal.and_then(|c| c.vrms_at_0dbfs_in),
    });

    if let Some(f) = freq_hz {
        frame["freq_hz"] = json!(f);
    }
    frame
}

// ---------------------------------------------------------------------------
// Helper: resolve reference capture port from config
// ---------------------------------------------------------------------------

fn resolve_ref_input(cfg: &ac_core::config::Config, fake_audio: bool) -> Option<String> {
    let ch = cfg.reference_channel? as usize;
    if let Some(p) = &cfg.reference_port {
        return Some(p.clone());
    }
    let eng = make_engine(fake_audio);
    eng.capture_ports().get(ch).cloned()
}

fn resolve_ref_output(cfg: &ac_core::config::Config, fake_audio: bool) -> String {
    // The reference output is the playback port at reference_channel index.
    // Falls back to the primary output if reference_channel not set.
    if let Some(ch) = cfg.reference_channel {
        let eng = make_engine(fake_audio);
        let ports = eng.playback_ports();
        if let Some(p) = ports.get(ch as usize) {
            return p.clone();
        }
    }
    resolve_output(cfg, fake_audio)
}

// ---------------------------------------------------------------------------
// transfer
// ---------------------------------------------------------------------------

pub fn transfer(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "transfer");

    let cfg = state.cfg.lock().unwrap().clone();

    if cfg.reference_channel.is_none() && cfg.reference_port.is_none() {
        return json!({"ok": false, "error": "reference port not configured — run: ac setup reference <channel>"});
    }

    let level_dbfs   = cmd.get("level_dbfs").and_then(Value::as_f64).unwrap_or(-10.0);
    let out_port     = resolve_output(&cfg, state.fake_audio);
    let in_port      = resolve_input(&cfg, state.fake_audio);
    let ref_port     = match resolve_ref_input(&cfg, state.fake_audio) {
        Some(p) => p,
        None    => in_port.clone(), // fallback: use same as input (loopback)
    };
    let ref_out_port = resolve_ref_output(&cfg, state.fake_audio);

    let pub_tx   = state.pub_tx.clone();
    let fake     = state.fake_audio;
    let out_port_r = out_port.clone();
    let in_port_r  = in_port.clone();
    let ref_port_r = ref_port.clone();

    let worker = spawn_worker(state, "transfer", move |stop| {
        let amplitude = ac_core::generator::dbfs_to_amplitude(level_dbfs);

        let out_ports: Vec<String> = if ref_out_port != out_port {
            vec![out_port.clone(), ref_out_port]
        } else {
            vec![out_port.clone()]
        };

        let mut eng = make_engine(fake);
        if let Err(e) = eng.start(&out_ports, Some(&in_port)) {
            send_pub(&pub_tx, "error", &json!({"cmd":"transfer","message":format!("{e}")}));
            return;
        }
        if let Err(e) = eng.add_ref_input(&ref_port) {
            eprintln!("transfer: warning — ref input {ref_port}: {e}");
        }

        let sr       = eng.sample_rate();
        let duration = ac_core::transfer::capture_duration(16, sr);

        eng.set_pink(amplitude);
        let _ = eng.capture_block(0.2); // warmup flush

        if stop.load(Ordering::Relaxed) {
            eng.set_silence(); eng.stop(); return;
        }

        let (meas, refch) = match eng.capture_stereo(duration) {
            Ok(s)  => s,
            Err(e) => {
                send_pub(&pub_tx, "error", &json!({"cmd":"transfer","message":format!("{e}")}));
                eng.set_silence(); eng.stop(); return;
            }
        };
        let xruns = eng.xruns();
        eng.set_silence();
        eng.stop();

        if stop.load(Ordering::Relaxed) { return; }

        let result = ac_core::transfer::h1_estimate(&refch, &meas, sr);

        // Downsample to ≤2000 points
        let n_pts = result.freqs.len();
        let indices: Vec<usize> = if n_pts > 2000 {
            let mut idx: Vec<usize> = (0..2000)
                .map(|i| (i as f64 * (n_pts - 1) as f64 / 1999.0).round() as usize)
                .collect();
            idx.dedup();
            idx
        } else {
            (0..n_pts).collect()
        };

        let freqs  = indices.iter().map(|&i| result.freqs[i]).collect::<Vec<_>>();
        let mag    = indices.iter().map(|&i| result.magnitude_db[i]).collect::<Vec<_>>();
        let phase  = indices.iter().map(|&i| result.phase_deg[i]).collect::<Vec<_>>();
        let coh    = indices.iter().map(|&i| result.coherence[i]).collect::<Vec<_>>();

        send_pub(&pub_tx, "data", &json!({
            "type":          "transfer_result",
            "cmd":           "transfer",
            "freqs":         freqs,
            "magnitude_db":  mag,
            "phase_deg":     phase,
            "coherence":     coh,
            "delay_samples": result.delay_samples,
            "delay_ms":      result.delay_ms,
            "out_port":      out_port,
            "in_port":       in_port,
            "ref_port":      ref_port,
            "xruns":         xruns,
        }));
        send_pub(&pub_tx, "done", &json!({"cmd":"transfer","xruns":xruns}));
    });

    {
        let mut workers = state.workers.lock().unwrap();
        workers.insert("transfer".to_string(), worker);
    }
    json!({
        "ok":          true,
        "out_port":    out_port_r,
        "in_port":     in_port_r,
        "ref_port":    ref_port_r,
        "ref_out_port": resolve_ref_output(&state.cfg.lock().unwrap(), state.fake_audio),
    })
}

// ---------------------------------------------------------------------------
// probe
// ---------------------------------------------------------------------------

pub fn probe(state: &ServerState, _cmd: &Value) -> Value {
    busy_guard!(state, "probe");

    let fake    = state.fake_audio;
    let pub_tx  = state.pub_tx.clone();
    let cfg     = state.cfg.lock().unwrap().clone();
    let dmm_host = cfg.dmm_host.clone();

    let (playback, capture) = {
        let eng = make_engine(fake);
        (eng.playback_ports(), eng.capture_ports())
    };
    let n_play = playback.len();
    let n_cap  = capture.len();

    let worker = spawn_worker(state, "probe", move |stop| {
        let threshold_rms: f64 = 0.010 / (2.0f64.sqrt()); // 10 mVrms ≈ this linear RMS

        let freq      = 1000.0;
        let amplitude = ac_core::generator::dbfs_to_amplitude(-10.0);

        let mut eng = make_engine(fake);
        if playback.is_empty() {
            send_pub(&pub_tx, "error", &json!({"cmd":"probe","message":"no playback ports"}));
            return;
        }

        if let Err(e) = eng.start(&[playback[0].clone()], None) {
            send_pub(&pub_tx, "error", &json!({"cmd":"probe","message":format!("{e}")}));
            return;
        }
        eng.set_tone(freq, amplitude);
        eng.disconnect_output(&playback[0]);

        // Phase 1: DMM output scan
        let mut analog_channels: Vec<usize> = Vec::new();
        if let Some(ref host) = dmm_host {
            send_pub(&pub_tx, "data", &json!({
                "cmd": "probe", "phase": "output_start", "n_ports": n_play
            }));
            for (i, port) in playback.iter().enumerate() {
                if stop.load(Ordering::Relaxed) { break; }
                eng.connect_output(port).ok();
                std::thread::sleep(std::time::Duration::from_millis(400));
                let vrms = read_dmm_vrms(host, 3);
                eng.disconnect_output(port);
                let is_analog = vrms.map(|v| v > threshold_rms).unwrap_or(false);
                if is_analog { analog_channels.push(i); }
                send_pub(&pub_tx, "data", &json!({
                    "cmd": "probe", "phase": "output",
                    "channel": i, "port": port,
                    "vrms": vrms, "analog": is_analog,
                }));
            }
        } else {
            send_pub(&pub_tx, "data", &json!({
                "cmd": "probe", "phase": "output_skip",
                "message": "no DMM configured — skipping output scan",
            }));
            analog_channels = (0..n_play).collect();
        }

        // Phase 2: Loopback detection
        if !stop.load(Ordering::Relaxed) {
            send_pub(&pub_tx, "data", &json!({
                "cmd": "probe", "phase": "loopback_start",
                "n_outputs": analog_channels.len(), "n_inputs": n_cap,
            }));
        }

        // Connect first capture port as base input for measurement
        if let Some(cap0) = capture.first() {
            eng.reconnect_input(cap0).ok();
        }

        for &out_idx in &analog_channels {
            if stop.load(Ordering::Relaxed) { break; }
            eng.connect_output(&playback[out_idx]).ok();
            std::thread::sleep(std::time::Duration::from_millis(150));

            for (j, cap_port) in capture.iter().enumerate() {
                if stop.load(Ordering::Relaxed) { break; }
                eng.reconnect_input(cap_port).ok();
                eng.flush_capture();
                std::thread::sleep(std::time::Duration::from_millis(50));
                let level_dbfs = match eng.capture_block(0.05) {
                    Ok(data) => {
                        let rms = (data.iter().map(|&x| (x as f64).powi(2)).sum::<f64>()
                            / data.len().max(1) as f64).sqrt();
                        20.0 * rms.max(1e-12).log10()
                    }
                    Err(_) => -120.0,
                };
                if level_dbfs > -30.0 {
                    send_pub(&pub_tx, "data", &json!({
                        "cmd": "probe", "phase": "loopback",
                        "out_ch": out_idx, "out_port": &playback[out_idx],
                        "in_ch": j, "in_port": cap_port,
                        "level_dbfs": (level_dbfs * 10.0).round() / 10.0,
                    }));
                }
            }
            eng.disconnect_output(&playback[out_idx]);
        }

        eng.set_silence();
        eng.stop();
        send_pub(&pub_tx, "done", &json!({
            "cmd": "probe",
            "analog_channels": analog_channels,
        }));
    });

    {
        let mut workers = state.workers.lock().unwrap();
        workers.insert("probe".to_string(), worker);
    }
    json!({ "ok": true, "n_playback": n_play, "n_capture": n_cap })
}

/// Best-effort DMM read over SCPI TCP (port 5025).
fn read_dmm_vrms(host: &str, n: usize) -> Option<f64> {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    let mut sum = 0.0f64;
    let mut count = 0usize;
    for _ in 0..n {
        let mut stream = TcpStream::connect_timeout(
            &format!("{host}:5025").parse().ok()?,
            std::time::Duration::from_secs(2),
        ).ok()?;
        stream.write_all(b"MEAS:VOLT:AC?\n").ok()?;
        let mut buf = [0u8; 64];
        let bytes = stream.read(&mut buf).ok()?;
        let s = std::str::from_utf8(&buf[..bytes]).ok()?.trim().to_string();
        if let Ok(v) = s.parse::<f64>() {
            sum += v;
            count += 1;
        }
    }
    if count > 0 { Some(sum / count as f64) } else { None }
}

// ---------------------------------------------------------------------------
// test_hardware
// ---------------------------------------------------------------------------

pub fn test_hardware(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "test_hardware");

    let cfg = state.cfg.lock().unwrap().clone();

    if cfg.reference_channel.is_none() && cfg.reference_port.is_none() {
        return json!({"ok": false, "error": "reference channel not configured — run: ac setup reference <channel>"});
    }

    let dmm_mode     = cmd.get("dmm").and_then(Value::as_bool).unwrap_or(false);
    let out_port     = resolve_output(&cfg, state.fake_audio);
    let in_port      = resolve_input(&cfg, state.fake_audio);
    let ref_port     = match resolve_ref_input(&cfg, state.fake_audio) {
        Some(p) => p,
        None    => in_port.clone(),
    };
    let ref_out_port = resolve_ref_output(&cfg, state.fake_audio);

    let pub_tx       = state.pub_tx.clone();
    let fake         = state.fake_audio;
    let dmm_host     = cfg.dmm_host.clone();
    let out_ch       = cfg.output_channel;
    let in_ch        = cfg.input_channel;

    let out_port_r     = out_port.clone();
    let in_port_r      = in_port.clone();
    let ref_port_r     = ref_port.clone();
    let ref_out_port_r = ref_out_port.clone();

    let worker = spawn_worker(state, "test_hardware", move |stop| {
        let out_ports: Vec<String> = if ref_out_port != out_port {
            vec![out_port.clone(), ref_out_port]
        } else {
            vec![out_port.clone()]
        };

        let mut eng = make_engine(fake);
        if let Err(e) = eng.start(&out_ports, Some(&in_port)) {
            send_pub(&pub_tx, "error", &json!({"cmd":"test_hardware","message":format!("{e}")}));
            return;
        }
        let sr = eng.sample_rate();

        let mut tests_run  = 0usize;
        let mut tests_pass = 0usize;

        macro_rules! emit {
            ($r:expr) => {{
                if $r.pass { tests_pass += 1; }
                tests_run += 1;
                send_pub(&pub_tx, "data", &json!({
                    "type": "test_result", "cmd": "test_hardware",
                    "name": $r.name, "pass": $r.pass,
                    "detail": $r.detail, "tolerance": $r.tolerance,
                }));
            }};
        }

        if !stop.load(Ordering::Relaxed) {
            emit!(hw_noise_floor(&mut *eng, &in_port, &ref_port, sr));
        }
        if !stop.load(Ordering::Relaxed) {
            emit!(hw_level_linearity(&mut *eng, &in_port, sr));
        }
        if !stop.load(Ordering::Relaxed) {
            emit!(hw_thd_floor(&mut *eng, &in_port, sr));
        }
        if !stop.load(Ordering::Relaxed) {
            emit!(hw_freq_response(&mut *eng, &in_port, sr));
        }
        if !stop.load(Ordering::Relaxed) {
            emit!(hw_channel_match(&mut *eng, &in_port, &ref_port, sr));
        }
        if !stop.load(Ordering::Relaxed) {
            emit!(hw_repeatability(&mut *eng, &in_port, sr));
        }

        // DMM tests (only if configured and requested)
        let mut dmm_run = 0usize; let mut dmm_pass = 0usize;
        if dmm_mode {
            if let Some(ref host) = dmm_host {
                let cal = Calibration::load(out_ch, in_ch, None).ok().flatten();

                macro_rules! emit_dmm {
                    ($r:expr) => {{
                        if $r.pass { dmm_pass += 1; }
                        dmm_run += 1;
                        send_pub(&pub_tx, "data", &json!({
                            "type": "test_result", "cmd": "test_hardware", "dmm": true,
                            "name": $r.name, "pass": $r.pass,
                            "detail": $r.detail, "tolerance": $r.tolerance,
                        }));
                    }};
                }

                if !stop.load(Ordering::Relaxed) {
                    emit_dmm!(hw_dmm_absolute(&mut *eng, host, cal.as_ref()));
                }
                if !stop.load(Ordering::Relaxed) {
                    emit_dmm!(hw_dmm_tracking(&mut *eng, host, cal.as_ref()));
                }
                if !stop.load(Ordering::Relaxed) {
                    emit_dmm!(hw_dmm_freq_response(&mut *eng, host));
                }
            }
        }

        eng.set_silence();
        eng.stop();
        send_pub(&pub_tx, "done", &json!({
            "cmd": "test_hardware",
            "tests_run": tests_run, "tests_pass": tests_pass,
            "dmm_run": dmm_run, "dmm_pass": dmm_pass,
            "xruns": eng.xruns(),
        }));
    });

    {
        let mut workers = state.workers.lock().unwrap();
        workers.insert("test_hardware".to_string(), worker);
    }
    json!({
        "ok": true,
        "out_port":     out_port_r,
        "ref_out_port": ref_out_port_r,
        "in_port":      in_port_r,
        "ref_port":     ref_port_r,
    })
}

// ---------------------------------------------------------------------------
// test_dut
// ---------------------------------------------------------------------------

pub fn test_dut(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "test_dut");

    let cfg = state.cfg.lock().unwrap().clone();

    if cfg.reference_channel.is_none() && cfg.reference_port.is_none() {
        return json!({"ok": false, "error": "reference channel not configured — run: ac setup reference <channel>"});
    }

    let compare_mode = cmd.get("compare").and_then(Value::as_bool).unwrap_or(false);
    let level_dbfs   = cmd.get("level_dbfs").and_then(Value::as_f64).unwrap_or(-20.0);
    let out_port     = resolve_output(&cfg, state.fake_audio);
    let in_port      = resolve_input(&cfg, state.fake_audio);
    let ref_port     = match resolve_ref_input(&cfg, state.fake_audio) {
        Some(p) => p,
        None    => in_port.clone(),
    };
    let ref_out_port = resolve_ref_output(&cfg, state.fake_audio);
    let out_ch       = cfg.output_channel;
    let in_ch        = cfg.input_channel;

    let pub_tx       = state.pub_tx.clone();
    let fake         = state.fake_audio;
    let dut_reply_tx = state.dut_reply_tx.clone();

    let out_port_r     = out_port.clone();
    let in_port_r      = in_port.clone();
    let ref_port_r     = ref_port.clone();
    let ref_out_port_r = ref_out_port.clone();

    let worker = spawn_worker(state, "test_dut", move |stop| {
        let out_ports: Vec<String> = if ref_out_port != out_port {
            vec![out_port.clone(), ref_out_port]
        } else {
            vec![out_port.clone()]
        };

        let mut eng = make_engine(fake);
        if let Err(e) = eng.start(&out_ports, Some(&in_port)) {
            send_pub(&pub_tx, "error", &json!({"cmd":"test_dut","message":format!("{e}")}));
            return;
        }
        if let Err(e) = eng.add_ref_input(&ref_port) {
            eprintln!("test_dut: ref input {ref_port}: {e}");
        }

        let sr  = eng.sample_rate();
        let cal = Calibration::load(out_ch, in_ch, None).ok().flatten();
        let mut tests_done = 0usize;

        macro_rules! emit {
            ($r:expr) => {{
                tests_done += 1;
                send_pub(&pub_tx, "data", &json!({
                    "type": "test_result", "cmd": "test_dut",
                    "name": $r.name, "pass": $r.pass,
                    "detail": $r.detail, "tolerance": $r.tolerance,
                }));
            }};
            ($r:expr, $tag:expr) => {{
                tests_done += 1;
                send_pub(&pub_tx, "data", &json!({
                    "type": "test_result", "cmd": "test_dut", "tag": $tag,
                    "name": $r.name, "pass": $r.pass,
                    "detail": $r.detail, "tolerance": $r.tolerance,
                }));
            }};
        }

        let run_suite = |eng: &mut Box<dyn crate::audio::AudioEngine>, tag: &str| {
            let mut n = 0usize;
            let _ = (eng, tag, &cal, &pub_tx, sr, level_dbfs); // suppress unused warnings
            n
        };
        let _ = run_suite; // not used — inline below instead

        // Run DUT suite
        if !stop.load(Ordering::Relaxed) {
            emit!(dut_noise_floor(&mut *eng, sr, cal.as_ref()), "dut");
        }
        if !stop.load(Ordering::Relaxed) {
            emit!(dut_gain(&mut *eng, level_dbfs, sr, cal.as_ref()), "dut");
        }
        if !stop.load(Ordering::Relaxed) {
            emit!(dut_thd_vs_level(&mut *eng, sr, cal.as_ref()), "dut");
        }
        if !stop.load(Ordering::Relaxed) {
            emit!(dut_freq_response(&mut *eng, level_dbfs, sr, cal.as_ref()), "dut");
        }
        if !stop.load(Ordering::Relaxed) {
            emit!(dut_clipping_point(&mut *eng, sr, cal.as_ref()), "dut");
        }

        if compare_mode && !stop.load(Ordering::Relaxed) {
            // Register our reply channel and wait for dut_reply
            let (tx, rx) = crossbeam_channel::bounded(1);
            *dut_reply_tx.lock().unwrap() = Some(tx);

            send_pub(&pub_tx, "data", &json!({
                "type": "dut_compare_prompt", "cmd": "test_dut",
                "message": "Bypass DUT and press Enter",
            }));

            // Wait up to 5 min for user to press Enter
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
            loop {
                if stop.load(Ordering::Relaxed) { break; }
                if std::time::Instant::now() > deadline { break; }
                if rx.try_recv().is_ok() { break; }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            *dut_reply_tx.lock().unwrap() = None;

            if !stop.load(Ordering::Relaxed) {
                // Bypass suite
                if !stop.load(Ordering::Relaxed) {
                    emit!(dut_noise_floor(&mut *eng, sr, cal.as_ref()), "bypass");
                }
                if !stop.load(Ordering::Relaxed) {
                    emit!(dut_gain(&mut *eng, level_dbfs, sr, cal.as_ref()), "bypass");
                }
                if !stop.load(Ordering::Relaxed) {
                    emit!(dut_thd_vs_level(&mut *eng, sr, cal.as_ref()), "bypass");
                }
                if !stop.load(Ordering::Relaxed) {
                    emit!(dut_freq_response(&mut *eng, level_dbfs, sr, cal.as_ref()), "bypass");
                }
                if !stop.load(Ordering::Relaxed) {
                    emit!(dut_clipping_point(&mut *eng, sr, cal.as_ref()), "bypass");
                }
            }
        }

        eng.set_silence();
        eng.stop();
        let xruns = eng.xruns();
        send_pub(&pub_tx, "done", &json!({
            "cmd": "test_dut",
            "tests_run": tests_done, "compare": compare_mode, "xruns": xruns,
        }));
    });

    {
        let mut workers = state.workers.lock().unwrap();
        workers.insert("test_dut".to_string(), worker);
    }
    json!({
        "ok": true,
        "out_port":     out_port_r,
        "ref_out_port": ref_out_port_r,
        "in_port":      in_port_r,
        "ref_port":     ref_port_r,
    })
}

pub fn dut_reply(state: &ServerState) -> Value {
    let tx = state.dut_reply_tx.lock().unwrap();
    if let Some(ref t) = *tx {
        let _ = t.send(());
    }
    json!({"ok": true})
}

// ---------------------------------------------------------------------------
// Hardware test functions (port of ac/test.py)
// ---------------------------------------------------------------------------

struct TestResult {
    name:      String,
    pass:      bool,
    detail:    String,
    tolerance: String,
}

impl TestResult {
    fn new(name: &str, pass: bool, detail: String, tolerance: &str) -> Self {
        Self { name: name.to_string(), pass, detail, tolerance: tolerance.to_string() }
    }
}

fn capture_rms(eng: &mut dyn AudioEngine, duration: f64) -> f64 {
    match eng.capture_block(duration) {
        Ok(data) => {
            let sum_sq: f64 = data.iter().map(|&x| (x as f64).powi(2)).sum();
            (sum_sq / data.len().max(1) as f64).sqrt()
        }
        Err(_) => 0.0,
    }
}

fn rms_to_dbfs(rms: f64) -> f64 {
    20.0 * rms.max(1e-12).log10()
}

fn analyze_mono(eng: &mut dyn AudioEngine, freq: f64, duration: f64, sr: u32) -> Option<ac_core::types::AnalysisResult> {
    let dur = duration.max(20.0 / freq.max(1.0)); // at least 20 cycles
    let _ = eng.capture_block(0.05); // brief flush
    eng.flush_capture();
    std::thread::sleep(std::time::Duration::from_millis(50));
    let samples = eng.capture_block(dur).ok()?;
    ac_core::analysis::analyze(&samples, sr, freq, 10).ok()
}

// ---- Hardware tests ----

fn hw_noise_floor(eng: &mut dyn AudioEngine, in_a: &str, in_b: &str, sr: u32) -> TestResult {
    eng.set_silence();
    std::thread::sleep(std::time::Duration::from_millis(100));
    let mut floors = vec![];
    for port in [in_a, in_b] {
        eng.reconnect_input(port).ok();
        eng.flush_capture();
        std::thread::sleep(std::time::Duration::from_millis(50));
        let rms = capture_rms(eng, 0.5);
        floors.push(rms_to_dbfs(rms));
    }
    let pass = floors.iter().all(|&d| d < -80.0);
    TestResult::new(
        "Noise floor",
        pass,
        format!("{:.1} dBFS / {:.1} dBFS", floors[0], floors[1]),
        "< -80 dBFS",
    )
}

fn hw_level_linearity(eng: &mut dyn AudioEngine, in_port: &str, sr: u32) -> TestResult {
    let levels: Vec<i32> = (-42..=-5).step_by(6).collect();
    eng.reconnect_input(in_port).ok();
    let mut measured: Vec<Option<f64>> = Vec::new();
    for &level in &levels {
        let amp = ac_core::generator::dbfs_to_amplitude(level as f64);
        eng.set_tone(1000.0, amp);
        eng.flush_capture();
        std::thread::sleep(std::time::Duration::from_millis(100));
        let r = analyze_mono(eng, 1000.0, 1.0, sr);
        measured.push(r.map(|x| x.fundamental_dbfs));
    }

    let valid: Vec<(i32, f64)> = levels.iter().copied().zip(measured.iter())
        .filter_map(|(l, m)| m.map(|v| (l, v)))
        .collect();

    let monotonic = valid.windows(2).all(|w| w[0].1 < w[1].1);
    let deltas: Vec<(i32, i32, f64)> = valid.windows(2)
        .map(|w| (w[0].0, w[1].0, w[1].1 - w[0].1))
        .collect();
    let max_step_err = deltas.iter().enumerate()
        .map(|(i, &(_, _, d))| {
            let tol = if i == deltas.len().saturating_sub(1) { 1.5 } else { 1.0 };
            (d - 6.0).abs() / tol
        })
        .fold(0.0f64, f64::max);

    let pass = monotonic && max_step_err <= 1.0;
    let step_detail = deltas.iter().map(|(a, b, d)| format!("{a}→{b}:{d:.2}")).collect::<Vec<_>>().join(", ");
    TestResult::new(
        "Level linearity",
        pass,
        format!("[{step_detail}]"),
        "monotonic, step error < 1 dB (1.5 dB top step)",
    )
}

fn hw_thd_floor(eng: &mut dyn AudioEngine, in_port: &str, sr: u32) -> TestResult {
    let levels: &[f64] = &[-40.0, -30.0, -20.0, -10.0, -3.0];
    eng.reconnect_input(in_port).ok();
    let mut results: Vec<(f64, f64, f64)> = Vec::new();
    for &level in levels {
        let amp = ac_core::generator::dbfs_to_amplitude(level);
        eng.set_tone(1000.0, amp);
        if let Some(r) = analyze_mono(eng, 1000.0, 1.0, sr) {
            results.push((level, r.thd_pct, r.thdn_pct));
        }
    }
    let best = results.iter().map(|&(_, t, _)| t).fold(f64::INFINITY, f64::min);
    let parts = results.iter().map(|(l, t, _)| format!("{l:.0}:{t:.4}%")).collect::<Vec<_>>().join(", ");
    TestResult::new(
        "THD floor (1 kHz)",
        best < 0.05,
        format!("best {best:.4}%  [{parts}]"),
        "best THD < 0.05%",
    )
}

fn hw_freq_response(eng: &mut dyn AudioEngine, in_port: &str, sr: u32) -> TestResult {
    let freqs: &[f64] = &[50.0, 100.0, 500.0, 1000.0, 5000.0, 10000.0, 20000.0];
    let amp = ac_core::generator::dbfs_to_amplitude(-10.0);
    eng.reconnect_input(in_port).ok();
    let mut results: Vec<(f64, f64)> = Vec::new();
    for &freq in freqs {
        eng.set_tone(freq, amp);
        if let Some(r) = analyze_mono(eng, freq, 0.5, sr) {
            results.push((freq, r.fundamental_dbfs));
        }
    }
    if results.len() < 2 {
        return TestResult::new("Frequency response", false, "insufficient data".to_string(), "");
    }
    let ref_db = results.iter().find(|&&(f, _)| f == 1000.0).map(|&(_, d)| d)
        .unwrap_or(results[0].1);
    let deviations: Vec<(f64, f64)> = results.iter().map(|&(f, d)| (f, d - ref_db)).collect();
    let max_dev = deviations.iter().map(|&(_, d)| d.abs()).fold(0.0f64, f64::max);
    let parts = deviations.iter().map(|(f, d)| format!("{f:.0}Hz:{d:+.2}dB")).collect::<Vec<_>>().join(", ");
    TestResult::new(
        "Frequency response",
        max_dev < 1.0,
        format!("max deviation {max_dev:.2} dB  [{parts}]"),
        "< 1.0 dB vs 1 kHz ref",
    )
}

fn hw_channel_match(eng: &mut dyn AudioEngine, in_a: &str, in_b: &str, sr: u32) -> TestResult {
    let amp = ac_core::generator::dbfs_to_amplitude(-10.0);
    eng.set_tone(1000.0, amp);
    let mut measurements: Vec<(String, f64, f64)> = Vec::new();
    for (label, port) in [("A", in_a), ("B", in_b)] {
        eng.reconnect_input(port).ok();
        eng.flush_capture();
        std::thread::sleep(std::time::Duration::from_millis(100));
        if let Some(r) = analyze_mono(eng, 1000.0, 1.0, sr) {
            measurements.push((label.to_string(), r.fundamental_dbfs, r.thd_pct));
        }
    }
    if measurements.len() < 2 {
        return TestResult::new("Channel match", false, "measurement failed".to_string(), "");
    }
    let delta_db  = (measurements[0].1 - measurements[1].1).abs();
    let delta_thd = (measurements[0].2 - measurements[1].2).abs();
    TestResult::new(
        "Channel match",
        delta_db < 0.5 && delta_thd < 0.01,
        format!("delta level: {delta_db:.3} dB  delta THD: {delta_thd:.4}%"),
        "level < 0.5 dB, THD < 0.01%",
    )
}

fn hw_repeatability(eng: &mut dyn AudioEngine, in_port: &str, sr: u32) -> TestResult {
    let amp = ac_core::generator::dbfs_to_amplitude(-10.0);
    eng.set_tone(1000.0, amp);
    eng.reconnect_input(in_port).ok();
    let mut levels: Vec<f64> = Vec::new();
    let mut thds: Vec<f64>   = Vec::new();
    for _ in 0..5 {
        eng.flush_capture();
        std::thread::sleep(std::time::Duration::from_millis(20));
        if let Some(r) = analyze_mono(eng, 1000.0, 1.0, sr) {
            levels.push(r.fundamental_dbfs);
            thds.push(r.thd_pct);
        }
    }
    if levels.len() < 3 {
        return TestResult::new("Repeatability", false, "insufficient measurements".to_string(), "");
    }
    let level_std = std_dev(&levels);
    let thd_std   = std_dev(&thds);
    TestResult::new(
        "Repeatability",
        level_std < 0.05 && thd_std < 0.005,
        format!("level sigma={level_std:.4} dB  THD sigma={thd_std:.6}%  ({}x)", levels.len()),
        "level sigma < 0.05 dB, THD sigma < 0.005%",
    )
}

// ---- DMM hardware tests ----

fn hw_dmm_absolute(eng: &mut dyn AudioEngine, host: &str, cal: Option<&Calibration>) -> TestResult {
    let amp = ac_core::generator::dbfs_to_amplitude(-10.0);
    eng.set_tone(1000.0, amp);
    std::thread::sleep(std::time::Duration::from_millis(500));
    let vrms_dmm = match read_dmm_vrms(host, 5) {
        Some(v) => v,
        None    => return TestResult::new("DMM absolute level", false, "DMM read failed".to_string(), ""),
    };
    let vrms_pred = match cal.and_then(|c| c.out_vrms(-10.0)) {
        Some(v) => v,
        None    => return TestResult::new("DMM absolute level", false, "no output calibration".to_string(), "requires calibration"),
    };
    let err_pct = (vrms_dmm - vrms_pred).abs() / vrms_pred * 100.0;
    TestResult::new(
        "DMM absolute level",
        err_pct < 1.0,
        format!("DMM: {:.3} mVrms  predicted: {:.3} mVrms  delta: {err_pct:.2}%",
            vrms_dmm * 1000.0, vrms_pred * 1000.0),
        "< 1% error",
    )
}

fn hw_dmm_tracking(eng: &mut dyn AudioEngine, host: &str, cal: Option<&Calibration>) -> TestResult {
    let levels: &[f64] = &[-40.0, -30.0, -20.0, -10.0, -6.0, -3.0, 0.0];
    let mut max_err = 0.0f64;
    let mut n_pts = 0usize;
    for &level in levels {
        let amp = ac_core::generator::dbfs_to_amplitude(level);
        eng.set_tone(1000.0, amp);
        std::thread::sleep(std::time::Duration::from_millis(400));
        if let (Some(vrms_dmm), Some(vrms_pred)) = (
            read_dmm_vrms(host, 3),
            cal.and_then(|c| c.out_vrms(level)),
        ) {
            let err = (vrms_dmm - vrms_pred).abs() / vrms_pred * 100.0;
            max_err = max_err.max(err);
            n_pts += 1;
        }
    }
    TestResult::new(
        "DMM level tracking",
        max_err < 2.0 && n_pts >= 5,
        format!("max error {max_err:.2}% over {n_pts} points"),
        "< 2% error at all levels",
    )
}

fn hw_dmm_freq_response(eng: &mut dyn AudioEngine, host: &str) -> TestResult {
    let freqs: &[f64] = &[100.0, 1000.0, 5000.0, 10000.0, 20000.0];
    let amp = ac_core::generator::dbfs_to_amplitude(-10.0);
    let mut readings: Vec<(f64, f64)> = Vec::new();
    for &freq in freqs {
        eng.set_tone(freq, amp);
        std::thread::sleep(std::time::Duration::from_millis(500));
        if let Some(v) = read_dmm_vrms(host, 3) {
            readings.push((freq, v));
        }
    }
    if readings.len() < 3 {
        return TestResult::new("DMM freq response", false, "insufficient readings".to_string(), "");
    }
    let ref_v = readings.iter().find(|&&(f, _)| f == 1000.0).map(|&(_, v)| v)
        .unwrap_or(readings[0].1);
    let deviations: Vec<(f64, f64)> = readings.iter()
        .map(|&(f, v)| (f, 20.0 * (v / ref_v.max(1e-12)).log10()))
        .collect();
    let max_dev = deviations.iter().map(|&(_, d)| d.abs()).fold(0.0f64, f64::max);
    let parts = deviations.iter().map(|(f, d)| format!("{f:.0}Hz:{d:+.2}dB")).collect::<Vec<_>>().join(", ");
    TestResult::new(
        "DMM freq response",
        max_dev < 1.0,
        format!("max deviation {max_dev:.2} dB  [{parts}]"),
        "< 1.0 dB vs 1 kHz ref",
    )
}

// ---------------------------------------------------------------------------
// DUT test functions (port of ac/test.py run_dut_*)
// ---------------------------------------------------------------------------

fn dut_noise_floor(eng: &mut dyn AudioEngine, sr: u32, cal: Option<&Calibration>) -> TestResult {
    eng.set_silence();
    std::thread::sleep(std::time::Duration::from_millis(200));
    let rms   = capture_rms(eng, 1.0);
    let dbfs  = rms_to_dbfs(rms);
    let label = cal_dbu_str(dbfs, cal, false);
    TestResult::new("Noise floor", true, label, "DUT output noise")
}

fn dut_gain(eng: &mut dyn AudioEngine, level_dbfs: f64, sr: u32, cal: Option<&Calibration>) -> TestResult {
    let amp = ac_core::generator::dbfs_to_amplitude(level_dbfs);
    eng.set_tone(1000.0, amp);
    std::thread::sleep(std::time::Duration::from_millis(200));
    let (meas, refch) = match eng.capture_stereo(1.0) {
        Ok(s)  => s,
        Err(e) => return TestResult::new("Gain", false, format!("capture failed: {e}"), ""),
    };
    let r_meas = match ac_core::analysis::analyze(&meas, sr, 1000.0, 10) {
        Ok(r)  => r,
        Err(_) => return TestResult::new("Gain", false, "no signal at measurement input".to_string(), ""),
    };
    let r_ref = match ac_core::analysis::analyze(&refch, sr, 1000.0, 10) {
        Ok(r)  => r,
        Err(_) => return TestResult::new("Gain", false, "no signal at reference input".to_string(), ""),
    };
    let gain = r_meas.fundamental_dbfs - r_ref.fundamental_dbfs;
    let ref_str  = cal_out_dbu_str(r_ref.fundamental_dbfs, cal);
    let meas_str = cal_dbu_str(r_meas.fundamental_dbfs, cal, false);
    TestResult::new(
        "Gain",
        true,
        format!("{gain:+.1} dB  (ref: {ref_str} → meas: {meas_str})"),
        "at 1 kHz",
    )
}

fn dut_thd_vs_level(eng: &mut dyn AudioEngine, sr: u32, cal: Option<&Calibration>) -> TestResult {
    let levels: &[f64] = &[-40.0, -30.0, -20.0, -10.0, -6.0, -3.0];
    let mut results: Vec<(f64, f64, f64, f64)> = Vec::new(); // (level, thd, thdn, gain)
    for &level in levels {
        let amp = ac_core::generator::dbfs_to_amplitude(level);
        eng.set_tone(1000.0, amp);
        std::thread::sleep(std::time::Duration::from_millis(100));
        if let Ok((meas, refch)) = eng.capture_stereo(1.0) {
            let r_meas = ac_core::analysis::analyze(&meas, sr, 1000.0, 10).ok();
            let r_ref  = ac_core::analysis::analyze(&refch, sr, 1000.0, 10).ok();
            if let (Some(rm), Some(rr)) = (r_meas, r_ref) {
                let gain = rm.fundamental_dbfs - rr.fundamental_dbfs;
                results.push((level, rm.thd_pct, rm.thdn_pct, gain));
            }
        }
    }
    if results.is_empty() {
        return TestResult::new("THD vs level", false, "no valid measurements".to_string(), "");
    }
    let best_thd = results.iter().map(|&(_, t, _, _)| t).fold(f64::INFINITY, f64::min);
    let parts = results.iter().map(|(l, t, _, g)| {
        let drive = cal_out_dbu_str(*l, cal);
        format!("{drive}:{t:.4}%/{g:+.1}dB")
    }).collect::<Vec<_>>().join(", ");
    TestResult::new(
        "THD vs level",
        true,
        format!("best {best_thd:.4}%  [{parts}]"),
        "THD%/gain at each drive level",
    )
}

fn dut_freq_response(eng: &mut dyn AudioEngine, level_dbfs: f64, sr: u32, cal: Option<&Calibration>) -> TestResult {
    let amp = ac_core::generator::dbfs_to_amplitude(level_dbfs);
    eng.set_pink(amp);
    std::thread::sleep(std::time::Duration::from_millis(300));
    let (meas, refch) = match eng.capture_stereo(4.0) {
        Ok(s)  => s,
        Err(e) => return TestResult::new("Frequency response", false, format!("capture failed: {e}"), ""),
    };
    eng.set_silence();
    let result = ac_core::transfer::h1_estimate(&refch, &meas, sr);
    let freqs = &result.freqs;
    let mag   = &result.magnitude_db;
    let coh   = &result.coherence;

    let band: Vec<usize> = (0..freqs.len()).filter(|&i| freqs[i] >= 50.0 && freqs[i] <= 20000.0).collect();
    if band.is_empty() {
        return TestResult::new("Frequency response", false, "no data in 50-20kHz".to_string(), "");
    }

    let mag_band: Vec<f64> = band.iter().map(|&i| mag[i]).collect();
    let coh_band: Vec<f64> = band.iter().map(|&i| coh[i]).collect();
    let ref_db  = median(&mag_band);
    let dev_pos = mag_band.iter().copied().fold(f64::NEG_INFINITY, f64::max) - ref_db;
    let dev_neg = mag_band.iter().copied().fold(f64::INFINITY, f64::min) - ref_db;
    let avg_coh = coh_band.iter().sum::<f64>() / coh_band.len() as f64;
    let level_str = cal_out_dbu_str(level_dbfs, cal);

    TestResult::new(
        "Frequency response",
        true,
        format!("{dev_pos:+.1}/{dev_neg:+.1} dB  (50-20kHz, coh {avg_coh:.3}, delay {:.2}ms)  at {level_str}",
            result.delay_ms),
        "H1 transfer function",
    )
}

fn dut_clipping_point(eng: &mut dyn AudioEngine, sr: u32, cal: Option<&Calibration>) -> TestResult {
    let levels: Vec<f64> = (-30..=0).step_by(3).map(|x| x as f64).collect();
    let mut last_clean = None::<f64>;
    let mut clip_level = None::<f64>;

    for level in &levels {
        let amp = ac_core::generator::dbfs_to_amplitude(*level);
        eng.set_tone(1000.0, amp);
        std::thread::sleep(std::time::Duration::from_millis(100));
        let (meas, _) = match eng.capture_stereo(0.5) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if let Ok(r) = ac_core::analysis::analyze(&meas, sr, 1000.0, 10) {
            if r.thd_pct > 1.0 || r.clipping {
                clip_level = Some(*level);
                break;
            }
            last_clean = Some(*level);
        }
    }
    eng.set_silence();

    match clip_level {
        Some(lv) => {
            let onset = cal_out_dbu_str(lv, cal);
            let clean = last_clean.map(|l| cal_out_dbu_str(l, cal)).unwrap_or_else(|| "?".to_string());
            TestResult::new("Clipping point", true, format!("onset at {onset} (last clean: {clean})"), "THD > 1% threshold")
        }
        None => match last_clean {
            Some(lv) => {
                let clean = cal_out_dbu_str(lv, cal);
                TestResult::new("Clipping point", true, format!("clean through {clean} (no clipping detected)"), "THD > 1% threshold")
            }
            None => TestResult::new("Clipping point", false, "no valid measurements".to_string(), ""),
        },
    }
}

// ---------------------------------------------------------------------------
// Calibration unit helpers (for display strings in DUT tests)
// ---------------------------------------------------------------------------

fn cal_dbu_str(dbfs: f64, cal: Option<&Calibration>, use_output: bool) -> String {
    let vrms_ref = if use_output {
        cal.and_then(|c| c.vrms_at_0dbfs_out)
    } else {
        cal.and_then(|c| c.vrms_at_0dbfs_in)
    };
    if let Some(ref_vrms) = vrms_ref {
        let vrms = ref_vrms * 10f64.powf(dbfs / 20.0);
        let dbu  = ac_core::conversions::vrms_to_dbu(vrms);
        format!("{dbu:+.1} dBu")
    } else {
        format!("{dbfs:.1} dBFS")
    }
}

fn cal_out_dbu_str(dbfs: f64, cal: Option<&Calibration>) -> String {
    cal_dbu_str(dbfs, cal, true)
}

// ---------------------------------------------------------------------------
// Math utilities
// ---------------------------------------------------------------------------

fn std_dev(vals: &[f64]) -> f64 {
    if vals.len() < 2 { return 0.0; }
    let mean = vals.iter().sum::<f64>() / vals.len() as f64;
    let var  = vals.iter().map(|&x| (x - mean).powi(2)).sum::<f64>() / (vals.len() - 1) as f64;
    var.sqrt()
}

fn median(vals: &[f64]) -> f64 {
    if vals.is_empty() { return 0.0; }
    let mut sorted = vals.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = sorted.len();
    if n % 2 == 0 {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    } else {
        sorted[n / 2]
    }
}
