//! Audio commands: tone/noise generation, level/frequency sweeps, plot helpers,
//! live spectrum monitor.

use std::sync::atomic::Ordering;

use serde_json::{json, Value};

use ac_core::calibration::Calibration;

use crate::audio::make_engine;
use crate::server::ServerState;

use super::{
    busy_guard, downsample, resolve_input, resolve_output, send_pub, spawn_worker,
    sweep_point_frame,
};

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
        let freqs = super::log_freq_points(start_hz, stop_hz, ppd);
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
        let levels = super::linspace(start_dbfs, stop_dbfs, steps);

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
