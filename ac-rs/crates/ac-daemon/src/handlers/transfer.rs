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
    let out_port     = resolve_output(&cfg, state);
    let in_port      = resolve_input(&cfg, state);
    let ref_port     = match resolve_ref_input(&cfg, state) {
        Some(p) => p,
        None    => in_port.clone(), // fallback: use same as input (loopback)
    };
    let ref_out_port = resolve_ref_output(&cfg, state);

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
        "ref_out_port": resolve_ref_output(&state.cfg.lock().unwrap(), state),
    })
}

/// Streaming variant of `transfer`: loops `h1_estimate` until stop is signalled
/// and publishes a `transfer_stream` frame each iteration. `meas_channel` and
/// `ref_channel` are indices into the capture port list, set explicitly from
/// the client (not from config), so `ac-ui` can switch channel pairs without
/// touching persistent config.
/// Parse the `pairs` and legacy `meas_channel`/`ref_channel` shapes of
/// `transfer_stream` into a canonical pair list. Returns an Err message
/// suitable for `{"ok": false, "error": ...}` on malformed input.
fn parse_transfer_pairs(cmd: &Value) -> Result<Vec<(u32, u32)>, String> {
    if let Some(arr) = cmd.get("pairs").and_then(Value::as_array) {
        if arr.is_empty() {
            return Err("pairs is empty".into());
        }
        let mut out = Vec::with_capacity(arr.len());
        for (i, p) in arr.iter().enumerate() {
            let tuple = p.as_array().ok_or_else(|| format!("pairs[{i}] must be [meas, ref]"))?;
            if tuple.len() != 2 {
                return Err(format!("pairs[{i}] must have exactly 2 elements, got {}", tuple.len()));
            }
            let m = tuple[0].as_u64().ok_or_else(|| format!("pairs[{i}][0] must be unsigned int"))?;
            let r = tuple[1].as_u64().ok_or_else(|| format!("pairs[{i}][1] must be unsigned int"))?;
            out.push((m as u32, r as u32));
        }
        // De-dup identical pairs — harmless but wasteful to publish twice.
        out.sort_unstable();
        out.dedup();
        return Ok(out);
    }
    // Legacy single-pair form.
    let m = cmd.get("meas_channel").and_then(Value::as_u64)
        .ok_or_else(|| "meas_channel required (or use pairs=[[m,r], ...])".to_string())?;
    let r = cmd.get("ref_channel").and_then(Value::as_u64)
        .ok_or_else(|| "ref_channel required (or use pairs=[[m,r], ...])".to_string())?;
    Ok(vec![(m as u32, r as u32)])
}

pub fn transfer_stream(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "transfer_stream");

    // `drive` controls whether the daemon plays pink noise on the output
    // while capturing. Default `false` — the UI wants a purely passive H1
    // estimate against whatever the user is already driving into the inputs.
    // Set `true` to restore the old self-driving behavior (with `level_dbfs`
    // controlling amplitude).
    let drive      = cmd.get("drive").and_then(Value::as_bool).unwrap_or(false);
    let level_dbfs = cmd.get("level_dbfs").and_then(Value::as_f64).unwrap_or(-10.0);

    let pairs = match parse_transfer_pairs(cmd) {
        Ok(p)  => p,
        Err(e) => return json!({"ok": false, "error": e}),
    };

    let cfg = state.cfg.lock().unwrap().clone();
    let capture_ports = super::cached_capture_ports(state);

    // Resolve each unique capture channel to a port name once. `unique_ports`
    // drives the JACK port-registration order; each pair indexes into it.
    let mut unique_chans: Vec<u32> = Vec::new();
    for &(m, r) in &pairs {
        for c in [m, r] {
            if !unique_chans.contains(&c) { unique_chans.push(c); }
        }
    }
    let mut unique_ports = Vec::with_capacity(unique_chans.len());
    for &ch in &unique_chans {
        match capture_ports.get(ch as usize) {
            Some(p) => unique_ports.push(p.clone()),
            None => return json!({"ok": false,
                "error": format!("channel {ch} out of range (n_capture={})", capture_ports.len())}),
        }
    }

    // Per-pair buffer indices into the `Vec<Vec<f32>>` that `capture_multi`
    // returns. Precomputed so the worker loop doesn't re-scan per iteration.
    let pair_idx: Vec<(usize, usize)> = pairs.iter().map(|&(m, r)| {
        let mi = unique_chans.iter().position(|&c| c == m).unwrap();
        let ri = unique_chans.iter().position(|&c| c == r).unwrap();
        (mi, ri)
    }).collect();

    let out_port     = resolve_output(&cfg, state);
    let ref_out_port = resolve_ref_output(&cfg, state);

    // Sync routing-capability check so CPAL-only environments get an
    // immediate REP error instead of a silent worker exit that the UI never
    // sees (the async `send_pub("error", …)` path below is still needed for
    // per-capture failures once the worker is live).
    {
        let probe_eng = make_engine(state.fake_audio);
        if !probe_eng.supports_routing() {
            return json!({
                "ok": false,
                "error": format!("{} backend does not support port routing", probe_eng.backend_name()),
            });
        }
    }

    let pub_tx = state.pub_tx.clone();
    let fake   = state.fake_audio;

    let out_port_r    = out_port.clone();
    let meas_port_r   = unique_ports.first().cloned().unwrap_or_default();
    let ref_port_r    = unique_ports.get(1).cloned().unwrap_or_else(|| meas_port_r.clone());
    let pairs_r       = pairs.clone();

    let worker = spawn_worker(state, "transfer_stream", move |stop| {
        let amplitude = ac_core::generator::dbfs_to_amplitude(level_dbfs);

        // Passive mode (default): open no output ports at all so the daemon
        // doesn't need exclusive access to the playback side — the user is
        // driving the DUT externally. `drive=true` restores the old
        // pink-noise self-stimulus path for the loopback workflow.
        let out_ports: Vec<String> = if !drive {
            Vec::new()
        } else if ref_out_port != out_port {
            vec![out_port.clone(), ref_out_port.clone()]
        } else {
            vec![out_port.clone()]
        };

        let mut eng = make_engine(fake);
        let main_port = unique_ports[0].clone();
        if let Err(e) = eng.start(&out_ports, Some(&main_port)) {
            send_pub(&pub_tx, "error", &json!({"cmd":"transfer_stream","message":format!("{e}")}));
            return;
        }
        for p in &unique_ports[1..] {
            if let Err(e) = eng.add_ref_input(p) {
                eprintln!("transfer_stream: warning — ref input {p}: {e}");
            }
        }

        let sr       = eng.sample_rate();
        // Sliding window: keep the last `target_total` samples per unique
        // channel and recompute H1 every `chunk_secs`. nperseg/step mirror
        // h1_estimate's internal Welch settings.
        let nperseg      = sr as usize;              // 1 Hz bin width
        let step         = nperseg / 2;              // 50% overlap
        let n_averages   = 4;
        let target_total = nperseg + step * (n_averages - 1);
        let chunk_secs   = 0.25;
        let mut rings: Vec<Vec<f32>> = (0..unique_ports.len())
            .map(|_| Vec::with_capacity(target_total + step))
            .collect();

        if drive {
            eng.set_pink(amplitude);
        }
        let _ = eng.capture_block(0.2); // warmup flush

        while !stop.load(Ordering::Relaxed) {
            let bufs = match eng.capture_multi(chunk_secs) {
                Ok(b)  => b,
                Err(e) => {
                    send_pub(&pub_tx, "error", &json!({"cmd":"transfer_stream","message":format!("{e}")}));
                    break;
                }
            };
            if stop.load(Ordering::Relaxed) { break; }

            for (i, buf) in bufs.iter().enumerate() {
                if i >= rings.len() { break; }
                let r = &mut rings[i];
                r.extend_from_slice(buf);
                if r.len() > target_total {
                    let drop = r.len() - target_total;
                    r.drain(..drop);
                }
            }

            // Warm-up: wait for the rings to fill to at least one Welch
            // segment so the first H1 has meaningful coherence.
            if rings.iter().any(|r| r.len() < nperseg) { continue; }

            for (&(meas_ch, ref_ch), &(mi, ri)) in pairs.iter().zip(pair_idx.iter()) {
                let meas = match rings.get(mi) { Some(b) => b.as_slice(), None => continue };
                let refb = match rings.get(ri) { Some(b) => b.as_slice(), None => continue };
                let result = ac_core::transfer::h1_estimate(refb, meas, sr);

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

                let freqs = indices.iter().map(|&i| result.freqs[i]).collect::<Vec<_>>();
                let mag   = indices.iter().map(|&i| result.magnitude_db[i]).collect::<Vec<_>>();
                let phase = indices.iter().map(|&i| result.phase_deg[i]).collect::<Vec<_>>();
                let coh   = indices.iter().map(|&i| result.coherence[i]).collect::<Vec<_>>();

                send_pub(&pub_tx, "data", &json!({
                    "type":          "transfer_stream",
                    "cmd":           "transfer_stream",
                    "freqs":         freqs,
                    "magnitude_db":  mag,
                    "phase_deg":     phase,
                    "coherence":     coh,
                    "delay_samples": result.delay_samples,
                    "delay_ms":      result.delay_ms,
                    "ref_channel":   ref_ch,
                    "meas_channel":  meas_ch,
                    "sr":            sr,
                }));
            }
        }

        if drive {
            eng.set_silence();
        }
        eng.stop();
        send_pub(&pub_tx, "done", &json!({"cmd":"transfer_stream","stopped":true}));
    });

    {
        let mut workers = state.workers.lock().unwrap();
        workers.insert("transfer_stream".to_string(), worker);
    }
    json!({
        "ok":           true,
        "out_port":     out_port_r,
        "meas_port":    meas_port_r,
        "ref_port":     ref_port_r,
        "pairs":        pairs_r,
        // Legacy fields — filled with the first pair so old clients keep working.
        "meas_channel": pairs_r.first().map(|p| p.0).unwrap_or(0),
        "ref_channel":  pairs_r.first().map(|p| p.1).unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pairs_multi() {
        let cmd = json!({ "pairs": [[0, 3], [1, 3], [2, 3]] });
        assert_eq!(parse_transfer_pairs(&cmd).unwrap(), vec![(0, 3), (1, 3), (2, 3)]);
    }

    #[test]
    fn parse_pairs_dedups() {
        let cmd = json!({ "pairs": [[0, 3], [1, 3], [0, 3]] });
        assert_eq!(parse_transfer_pairs(&cmd).unwrap(), vec![(0, 3), (1, 3)]);
    }

    #[test]
    fn parse_pairs_legacy_single() {
        let cmd = json!({ "meas_channel": 0, "ref_channel": 3 });
        assert_eq!(parse_transfer_pairs(&cmd).unwrap(), vec![(0, 3)]);
    }

    #[test]
    fn parse_pairs_empty_errors() {
        let cmd = json!({ "pairs": [] });
        assert!(parse_transfer_pairs(&cmd).is_err());
    }

    #[test]
    fn parse_pairs_malformed_element_errors() {
        let cmd = json!({ "pairs": [[0, 3], [1]] });
        assert!(parse_transfer_pairs(&cmd).is_err());
    }

    #[test]
    fn parse_pairs_missing_fields_errors() {
        let cmd = json!({});
        assert!(parse_transfer_pairs(&cmd).is_err());
    }
}

pub fn probe(state: &ServerState, _cmd: &Value) -> Value {
    busy_guard!(state, "probe");

    let fake    = state.fake_audio;
    let pub_tx  = state.pub_tx.clone();
    let cfg     = state.cfg.lock().unwrap().clone();
    let dmm_host = cfg.dmm_host.clone();

    let (playback, capture) = (
        super::cached_playback_ports(state),
        super::cached_capture_ports(state),
    );
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
