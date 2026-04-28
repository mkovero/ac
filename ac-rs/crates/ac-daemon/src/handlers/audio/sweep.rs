//! `sweep_level` / `sweep_frequency` — drive output over a stepped range
//! and publish per-point analysis frames.

use std::sync::atomic::Ordering;

use serde_json::{json, Value};

use ac_core::measurement::report::{
    IntegrationParams, MeasurementData, MeasurementMethod, MeasurementReport, ProcessingChain,
    SCHEMA_VERSION, StimulusParams,
};
use ac_core::measurement::sweep::{
    citation as sweep_citation, deconvolve_full, extract_irs, inverse_sweep, log_sweep,
    SweepParams,
};
use ac_core::shared::calibration::Calibration;

use crate::audio::make_engine;
use crate::server::ServerState;

use super::super::{busy_guard, resolve_input, resolve_output, send_pub, snapshot_from_cal, spawn_worker};

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
        let start_amp = ac_core::shared::generator::dbfs_to_amplitude(start_dbfs);
        eng.set_tone(freq_hz, start_amp);
        let t0 = std::time::Instant::now();
        while !stop.load(Ordering::Relaxed) {
            let elapsed = t0.elapsed().as_secs_f64();
            if elapsed >= duration { break; }
            let t = elapsed / duration;
            let db = start_dbfs + (stop_dbfs - start_dbfs) * t;
            eng.set_tone(freq_hz, ac_core::shared::generator::dbfs_to_amplitude(db));
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
    let amplitude  = ac_core::shared::generator::dbfs_to_amplitude(level_dbfs);

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

/// `sweep_ir` — Farina exponential log-sweep impulse-response measurement.
///
/// Generates an ESS at `level_dbfs`, plays it out via the audio engine,
/// synchronously captures `duration + tail_s` of the measurement input,
/// deconvolves via the normalized inverse filter, gates the linear IR and
/// the first few pre-impulse harmonic IRs, and emits them as a
/// `measurement/impulse_response` frame plus a full `MeasurementReport`.
///
/// Today only the fake backend implements `play_and_capture`; real JACK /
/// CPAL buffer-playback is tracked as a follow-up. See ARCHITECTURE.md.
pub fn sweep_ir(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "sweep_ir");
    let f1_hz      = cmd.get("f1_hz").and_then(Value::as_f64).unwrap_or(20.0);
    let f2_hz      = cmd.get("f2_hz").and_then(Value::as_f64).unwrap_or(20_000.0);
    let duration   = cmd.get("duration").and_then(Value::as_f64).unwrap_or(1.0);
    let level_dbfs = cmd.get("level_dbfs").and_then(Value::as_f64).unwrap_or(-6.0);
    let tail_s     = cmd.get("tail_s").and_then(Value::as_f64).unwrap_or(0.5);
    let n_harmonics = cmd.get("n_harmonics").and_then(Value::as_u64).unwrap_or(5) as usize;
    let window_len = cmd.get("window_len").and_then(Value::as_u64).unwrap_or(4096) as usize;

    let cfg        = state.cfg.lock().unwrap().clone();
    let out_port   = resolve_output(&cfg, state);
    let in_port    = resolve_input(&cfg, state);
    let out_port_reply = out_port.clone();
    let out_ch     = cfg.output_channel;
    let in_ch      = cfg.input_channel;

    let pub_tx = state.pub_tx.clone();
    let fake   = state.fake_audio;

    let worker = spawn_worker(state, "sweep_ir", move |_stop| {
        // Calibration snapshot — the IR itself is NOT yet mic-curve-
        // corrected (the FIR-based deep correction is tracked as a
        // follow-up to #97; the curve provenance is preserved in the
        // report so a downstream tool can apply correction post hoc).
        let cal = Calibration::load(out_ch, in_ch, None).ok().flatten();

        let mut eng = make_engine(fake);
        if let Err(e) = eng.start(&[out_port], Some(&in_port)) {
            send_pub(&pub_tx, "error", &json!({"cmd":"sweep_ir","message":format!("{e}")}));
            return;
        }
        let sr = eng.sample_rate();
        let params = SweepParams {
            f1_hz,
            f2_hz,
            duration_s: duration,
            sample_rate: sr,
        };
        let sweep = match log_sweep(&params) {
            Ok(s) => s,
            Err(e) => {
                send_pub(&pub_tx, "error", &json!({"cmd":"sweep_ir","message":format!("{e}")}));
                return;
            }
        };
        let amp = ac_core::shared::generator::dbfs_to_amplitude(level_dbfs) as f32;
        let scaled: Vec<f32> = sweep.iter().map(|&s| s * amp).collect();

        let captured = match eng.play_and_capture(&scaled, tail_s) {
            Ok(c) => c,
            Err(e) => {
                send_pub(&pub_tx, "error", &json!({"cmd":"sweep_ir","message":format!("{e}")}));
                return;
            }
        };

        let inv = match inverse_sweep(&params) {
            Ok(v) => v,
            Err(e) => {
                send_pub(&pub_tx, "error", &json!({"cmd":"sweep_ir","message":format!("{e}")}));
                return;
            }
        };
        let full = deconvolve_full(&captured, &inv);
        // Re-scale out the stimulus amplitude so the reported IR has unity
        // peak for an identity loopback regardless of `level_dbfs`.
        let full: Vec<f64> = if amp > 0.0 {
            full.iter().map(|v| v / amp as f64).collect()
        } else {
            full
        };
        let irs = match extract_irs(&full, &params, n_harmonics.max(1), window_len) {
            Ok(r) => r,
            Err(e) => {
                send_pub(&pub_tx, "error", &json!({"cmd":"sweep_ir","message":format!("{e}")}));
                return;
            }
        };

        let data = MeasurementData::ImpulseResponse {
            sample_rate_hz: sr,
            f1_hz,
            f2_hz,
            duration_s: duration,
            linear_ir: irs.linear.clone(),
            harmonics: irs.harmonics.clone(),
        };
        send_pub(&pub_tx, "measurement/impulse_response", &json!({
            "cmd": "sweep_ir",
            "data": &data,
        }));

        let report = MeasurementReport {
            schema_version: SCHEMA_VERSION,
            ac_version: env!("CARGO_PKG_VERSION").to_string(),
            timestamp_utc: chrono::Utc::now().to_rfc3339(),
            method: MeasurementMethod::SweptSine {
                f1_hz,
                f2_hz,
                duration_s: duration,
                standard: Some(sweep_citation()),
            },
            stimulus: StimulusParams {
                sample_rate_hz: sr,
                f_start_hz: f1_hz,
                f_stop_hz: f2_hz,
                level_dbfs,
                n_points: 0,
            },
            integration: IntegrationParams {
                duration_s: duration,
                window: "farina-inverse".into(),
            },
            calibration: snapshot_from_cal(cal.as_ref()),
            data,
            notes: None,
            // sweep_ir's IR isn't yet mic-curve-corrected (deferred from
            // #97 to a follow-up). The snapshot still captures the curve
            // provenance via `calibration.mic_response`; downstream
            // tools can apply the correction post-hoc.
            processing_chain: ProcessingChain {
                mic_correction_applied: false,
                ..Default::default()
            },
        };
        send_pub(&pub_tx, "measurement/report", &json!({
            "cmd": "sweep_ir",
            "report": &report,
        }));

        eng.stop();
        send_pub(&pub_tx, "done", &json!({"cmd":"sweep_ir"}));
    });

    {
        let mut workers = state.workers.lock().unwrap();
        workers.insert("sweep_ir".to_string(), worker);
    }
    json!({"ok": true, "out_port": out_port_reply})
}
