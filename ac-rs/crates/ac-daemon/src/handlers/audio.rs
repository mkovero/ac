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

    let out_port = vec![resolve_output(&cfg, state)];

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

    let resolved = resolve_output(&cfg, state);
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
    let out_port   = resolve_output(&cfg, state);
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
    let out_port   = resolve_output(&cfg, state);
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

    let out_port = resolve_output(&cfg, state);
    let in_port  = resolve_input(&cfg, state);
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

    let out_port = resolve_output(&cfg, state);
    let in_port  = resolve_input(&cfg, state);
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

    let channels: Vec<u32> = cmd.get("channels")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(Value::as_u64).map(|v| v as u32).collect())
        .filter(|v: &Vec<u32>| !v.is_empty())
        .unwrap_or_else(|| vec![cfg.input_channel]);

    let in_ports: Vec<String> = channels.iter()
        .map(|&ch| {
            let mut cfg_override = cfg.clone();
            cfg_override.input_channel = ch;
            cfg_override.input_port = None; // force index-based resolution
            resolve_input(&cfg_override, state)
        })
        .collect();
    let primary_in_port = in_ports.first().cloned().unwrap_or_default();

    let pub_tx = state.pub_tx.clone();
    let fake   = state.fake_audio;
    let out_ch = cfg.output_channel;
    let n_channels = channels.len() as u32;
    let channels_worker = channels.clone();
    let in_ports_worker = in_ports.clone();
    let analysis_mode = state.analysis_mode.clone();
    let cwt_sigma_shared = state.cwt_sigma.clone();
    let cwt_n_scales_shared = state.cwt_n_scales.clone();

    let worker = spawn_worker(state, "monitor_spectrum", move |stop| {
        let cals: Vec<Option<Calibration>> = channels_worker.iter()
            .map(|&ch| Calibration::load(out_ch, ch, None).ok().flatten())
            .collect();
        let mut eng = make_engine(fake);
        let start_port = in_ports_worker.first().map(String::as_str);
        if let Err(e) = eng.start(&[], start_port) {
            send_pub(&pub_tx, "error", &json!({"cmd":"monitor_spectrum","message":format!("{e}")}));
            return;
        }
        let sr = eng.sample_rate();
        let mut current_freqs: Vec<f64> = vec![freq_hz; channels_worker.len()];
        let mut xruns_total = 0u32;

        // CWT state: recomputed when sigma/n_scales change.
        let mut cwt_sigma = *cwt_sigma_shared.lock().unwrap();
        let mut cwt_n_scales = *cwt_n_scales_shared.lock().unwrap();
        let (mut cwt_scales, mut cwt_freqs) = ac_core::cwt::log_scales(
            ac_core::cwt::DEFAULT_F_MIN,
            ac_core::cwt::default_f_max(sr),
            cwt_n_scales,
            sr,
            cwt_sigma,
        );

        // Sliding ring buffer for CWT: holds ~0.5 s of audio per channel so
        // low-frequency wavelets (20 Hz @ sigma=12 ≈ 0.6 s support) see
        // enough data. Short 50 ms captures feed the ring; the CWT runs on
        // the full ring each tick giving ~20 Hz update rate.
        let ring_cap = (sr as f64 * 0.15).ceil() as usize; // 0.15 s — enough for 20 Hz
        let cwt_tick = 0.02_f64; // 20 ms capture per CWT tick
        let mut cwt_rings: Vec<std::collections::VecDeque<f32>> =
            channels_worker.iter().map(|_| std::collections::VecDeque::with_capacity(ring_cap)).collect();
        let mut cwt_log_counter = 0u32;

        let per_channel_interval = if channels_worker.len() > 1 {
            interval / channels_worker.len() as f64
        } else {
            interval
        };

        while !stop.load(Ordering::Relaxed) {
            let mode = analysis_mode.lock().unwrap().clone();
            let is_cwt = mode == "cwt";

            // Check for live CWT param changes.
            if is_cwt {
                let new_sigma = *cwt_sigma_shared.lock().unwrap();
                let new_n = *cwt_n_scales_shared.lock().unwrap();
                if (new_sigma - cwt_sigma).abs() > 0.01 || new_n != cwt_n_scales {
                    cwt_sigma = new_sigma;
                    cwt_n_scales = new_n;
                    let (s, f) = ac_core::cwt::log_scales(
                        ac_core::cwt::DEFAULT_F_MIN,
                        ac_core::cwt::default_f_max(sr),
                        cwt_n_scales,
                        sr,
                        cwt_sigma,
                    );
                    cwt_scales = s;
                    cwt_freqs = f;
                }
            }

            for (idx, &channel) in channels_worker.iter().enumerate() {
                if stop.load(Ordering::Relaxed) { break; }
                if channels_worker.len() > 1 {
                    if let Err(e) = eng.reconnect_input(&in_ports_worker[idx]) {
                        send_pub(&pub_tx, "error", &json!({
                            "cmd":     "monitor_spectrum",
                            "message": format!("reconnect ch{channel}: {e}"),
                        }));
                        continue;
                    }
                    eng.flush_capture();
                }
                let cap_dur = if is_cwt { cwt_tick } else { per_channel_interval };
                let samples = match eng.capture_block(cap_dur) {
                    Ok(s) => s,
                    Err(e) => {
                        send_pub(&pub_tx, "error", &json!({
                            "cmd":     "monitor_spectrum",
                            "message": format!("capture error on ch{channel}: {e}"),
                        }));
                        return;
                    }
                };
                xruns_total += eng.xruns();

                if is_cwt {
                    let ring = &mut cwt_rings[idx];
                    ring.extend(samples.iter());
                    while ring.len() > ring_cap {
                        ring.pop_front();
                    }
                    if ring.len() < 256 {
                        continue;
                    }
                    let t0 = std::time::Instant::now();
                    let buf = ring.make_contiguous();
                    let mags = ac_core::cwt::morlet_cwt(
                        buf,
                        sr,
                        &cwt_scales,
                        cwt_sigma,
                    );
                    cwt_log_counter += 1;
                    if cwt_log_counter % 50 == 1 {
                        eprintln!(
                            "cwt ch{channel}: {:.1}ms, ring={}, scales={}",
                            t0.elapsed().as_secs_f64() * 1000.0,
                            buf.len(),
                            cwt_scales.len(),
                        );
                    }
                    let ts_ns = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos() as u64)
                        .unwrap_or(0);
                    let frame = json!({
                        "type":        "cwt",
                        "cmd":         "monitor_spectrum",
                        "channel":     channel,
                        "n_channels":  n_channels,
                        "sr":          sr,
                        "magnitudes":  mags,
                        "frequencies": cwt_freqs,
                        "timestamp":   ts_ns,
                        "xruns":       xruns_total,
                    });
                    send_pub(&pub_tx, "data", &frame);
                } else {
                    let frame = match ac_core::analysis::analyze(&samples, sr, current_freqs[idx], 10) {
                        Ok(r) => {
                            current_freqs[idx] = r.fundamental_hz;
                            let cal = cals[idx].as_ref();
                            let in_dbu = cal
                                .and_then(|c| c.in_vrms(r.linear_rms))
                                .map(ac_core::conversions::vrms_to_dbu);
                            let (spec, freqs) = downsample(&r.spectrum, &r.freqs, 1000);
                            json!({
                                "type":             "spectrum",
                                "cmd":              "monitor_spectrum",
                                "channel":          channel,
                                "n_channels":       n_channels,
                                "freq_hz":          r.fundamental_hz,
                                "sr":               sr,
                                "freqs":            freqs,
                                "spectrum":         spec,
                                "fundamental_dbfs": r.fundamental_dbfs,
                                "thd_pct":          r.thd_pct,
                                "thdn_pct":         r.thdn_pct,
                                "in_dbu":           in_dbu,
                                "clipping":         r.clipping,
                                "xruns":            xruns_total,
                            })
                        }
                        Err(_) => {
                            let (spec, freqs) = ac_core::analysis::spectrum_only(&samples, sr);
                            let (spec, freqs) = downsample(&spec, &freqs, 1000);
                            json!({
                                "type":             "spectrum",
                                "cmd":              "monitor_spectrum",
                                "channel":          channel,
                                "n_channels":       n_channels,
                                "sr":               sr,
                                "freqs":            freqs,
                                "spectrum":         spec,
                                "xruns":            xruns_total,
                            })
                        }
                    };
                    send_pub(&pub_tx, "data", &frame);
                }
            }
        }
        eng.stop();
        send_pub(&pub_tx, "done", &json!({"cmd":"monitor_spectrum"}));
    });

    {
        let mut workers = state.workers.lock().unwrap();
        workers.insert("monitor_spectrum".to_string(), worker);
    }
    json!({
        "ok": true,
        "in_port":   primary_in_port,
        "in_ports":  in_ports,
        "channels":  channels,
    })
}
