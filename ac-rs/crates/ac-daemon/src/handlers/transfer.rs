//! Reference-plane commands: H1 transfer estimation and channel probe.
//! Both depend on engine port routing and the configured reference channel.

use std::sync::atomic::Ordering;

use serde_json::{json, Value};

use crate::audio::make_engine;
use crate::server::ServerState;

use super::{
    busy_guard, read_dmm_vrms, resolve_input, resolve_output, resolve_ref_input,
    resolve_ref_output, send_pub, spawn_worker,
};

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
        if !eng.supports_routing() {
            send_pub(&pub_tx, "error", &json!({
                "cmd":     "transfer",
                "message": format!("{} backend does not support port routing", eng.backend_name()),
            }));
            return;
        }
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
            eng.set_silence(); eng.stop();
            send_pub(&pub_tx, "done", &json!({"cmd":"transfer","stopped":true}));
            return;
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

        if stop.load(Ordering::Relaxed) {
            send_pub(&pub_tx, "done", &json!({"cmd":"transfer","stopped":true}));
            return;
        }

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
        if !eng.supports_routing() {
            send_pub(&pub_tx, "error", &json!({
                "cmd":     "probe",
                "message": format!("{} backend does not support port routing", eng.backend_name()),
            }));
            return;
        }
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
