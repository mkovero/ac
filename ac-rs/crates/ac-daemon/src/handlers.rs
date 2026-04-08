//! Command handlers.  Each function receives the ServerState + parsed JSON command
//! and returns a JSON Value to send as the CTRL reply.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use serde_json::{json, Value};

use ac_core::calibration::Calibration;
use ac_core::config::Config;

use crate::audio::make_engine;
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

/// Get output port name from config.
fn output_port(cfg: &Config) -> Option<String> {
    cfg.output_port.clone()
}

/// Get input port name from config.
fn input_port(cfg: &Config) -> Option<String> {
    cfg.input_port.clone()
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
    json!({"ok": true, "bind_addr": "*", "listen_mode": "public"})
}

pub fn server_disable(state: &ServerState) -> Value {
    *state.listen_mode.lock().unwrap() = "local".to_string();
    json!({"ok": true, "bind_addr": "127.0.0.1", "listen_mode": "local"})
}

pub fn server_connections(state: &ServerState) -> Value {
    let listen_mode = state.listen_mode.lock().unwrap().clone();
    json!({
        "ok":            true,
        "listen_mode":   listen_mode,
        "ctrl_endpoint": "tcp://127.0.0.1:5556",
        "data_endpoint": "tcp://127.0.0.1:5557",
        "clients":       [],
        "workers":       []
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

    let out_port   = match output_port(&cfg) {
        Some(p) => vec![p],
        None    => vec!["system:playback_1".to_string()],
    };

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

    let out_ports: Vec<String> = cfg.output_port.into_iter().collect();
    json!({"ok": true, "out_ports": out_ports})
}

pub fn generate_pink(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "generate_pink");
    let level_dbfs = cmd.get("level_dbfs").and_then(Value::as_f64).unwrap_or(-10.0);
    let cfg        = state.cfg.lock().unwrap().clone();

    let out_port = match output_port(&cfg) {
        Some(p) => vec![p],
        None    => vec!["system:playback_1".to_string()],
    };

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

    json!({"ok": true, "out_ports": [cfg.output_port]})
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
    let out_port   = cfg.output_port.clone().unwrap_or_else(|| "system:playback_1".to_string());
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
    let out_port   = cfg.output_port.clone().unwrap_or_else(|| "system:playback_1".to_string());
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

    let out_port = cfg.output_port.clone().unwrap_or_else(|| "system:playback_1".to_string());
    let in_port  = cfg.input_port .clone().unwrap_or_else(|| "system:capture_1".to_string());
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

    let out_port = cfg.output_port.clone().unwrap_or_else(|| "system:playback_1".to_string());
    let in_port  = cfg.input_port .clone().unwrap_or_else(|| "system:capture_1".to_string());
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
    let in_port  = cfg.input_port.clone().unwrap_or_else(|| "system:capture_1".to_string());
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
    let out_port = cfg.output_port.clone().unwrap_or_else(|| "system:playback_1".to_string());
    let in_port  = cfg.input_port .clone().unwrap_or_else(|| "system:capture_1".to_string());

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
