//! `plot` / `plot_level` — run a sweep, collect per-point analysis frames,
//! and emit a `done` with the full dataset so the CLI can render a PNG.

use std::sync::atomic::Ordering;

use serde_json::{json, Value};

use ac_core::measurement::report::{
    FrequencyResponsePoint, IntegrationParams, MeasurementData, MeasurementMethod,
    MeasurementReport, StandardsCitation, StimulusParams, SCHEMA_VERSION,
};
use ac_core::shared::calibration::Calibration;

use crate::audio::make_engine;
use crate::server::ServerState;

use super::super::{
    busy_guard, resolve_input, resolve_output, send_pub, spawn_worker, sweep_point_frame,
};

/// Publish a per-point frame under both the legacy `sweep_point` name
/// and its tiered equivalent. Callers pass the already-built frame
/// (whose `type` field is `sweep_point`); this helper forwards the
/// legacy copy unchanged and sends a second copy with `type` replaced.
fn publish_sweep_point(
    pub_tx: &crossbeam_channel::Sender<Vec<u8>>,
    mut frame: Value,
) {
    send_pub(pub_tx, "data", &frame);
    frame["type"] = json!("measurement/frequency_response/point");
    send_pub(pub_tx, "data", &frame);
}

fn now_iso8601_utc() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
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

    let report_dir = cfg.report_dir.clone();

    let worker = spawn_worker(state, "plot", move |stop| {
        let cal = Calibration::load(out_ch, in_ch, None).ok().flatten();
        let freqs = super::super::log_freq_points(start_hz, stop_hz, ppd);
        let amplitude = ac_core::shared::generator::dbfs_to_amplitude(level_dbfs);

        let mut eng = make_engine(fake);
        if let Err(e) = eng.start(&[out_port], Some(&in_port)) {
            send_pub(&pub_tx, "error", &json!({"cmd":"plot","message":format!("{e}")}));
            return;
        }
        let sr = eng.sample_rate();

        let mut n = 0usize;
        let mut xruns = 0u32;
        let mut points: Vec<FrequencyResponsePoint> = Vec::with_capacity(freqs.len());
        for freq in &freqs {
            if stop.load(Ordering::Relaxed) { break; }
            let dur = f64::max(duration, 3.0 / freq);
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

            match ac_core::measurement::thd::analyze(&samples, sr, *freq, 10) {
                Ok(r) => {
                    points.push(FrequencyResponsePoint {
                        freq_hz:          *freq,
                        fundamental_dbfs: r.fundamental_dbfs,
                        thd_pct:          r.thd_pct,
                        thdn_pct:         r.thdn_pct,
                        noise_floor_dbfs: r.noise_floor_dbfs,
                        linear_rms:       r.linear_rms,
                        clipping:         r.clipping,
                        ac_coupled:       r.ac_coupled,
                    });
                    let frame = sweep_point_frame(&r, cal.as_ref(), n, "plot", level_dbfs, Some(*freq));
                    publish_sweep_point(&pub_tx, frame);
                    n += 1;
                }
                Err(e) => eprintln!("plot: analyze error at {freq}Hz: {e}"),
            }
        }
        eng.set_silence();
        eng.stop();

        let timestamp = now_iso8601_utc();
        let report = MeasurementReport {
            schema_version: SCHEMA_VERSION,
            ac_version:     env!("CARGO_PKG_VERSION").to_string(),
            timestamp_utc:  timestamp.clone(),
            method: MeasurementMethod::SteppedSine {
                n_points: n,
                standard: Some(StandardsCitation {
                    standard: "IEC 60268-3:2018".into(),
                    clause:   "§14.12".into(),
                    verified: false,
                }),
            },
            stimulus: StimulusParams {
                sample_rate_hz: sr,
                f_start_hz:     start_hz,
                f_stop_hz:      stop_hz,
                level_dbfs,
                n_points:       n,
            },
            integration: IntegrationParams {
                duration_s: duration,
                window:     "hann".into(),
            },
            calibration: None,
            data:        MeasurementData::FrequencyResponse { points },
            notes:       None,
        };

        send_pub(&pub_tx, "data", &json!({
            "type":     "measurement/frequency_response/complete",
            "cmd":      "plot",
            "n_points": n,
            "xruns":    xruns,
        }));

        match serde_json::to_value(&report) {
            Ok(report_json) => {
                send_pub(&pub_tx, "data", &json!({
                    "type":   "measurement/report",
                    "cmd":    "plot",
                    "report": report_json,
                }));
            }
            Err(e) => eprintln!("plot: report serialization error: {e}"),
        }

        if let Some(dir) = report_dir {
            let filename = format!("{timestamp}-plot.json").replace(':', "-");
            let path = dir.join(filename);
            if let Err(e) = report.write_to(&path) {
                eprintln!("plot: report write error ({}): {e}", path.display());
            }
        }

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
        let levels = super::super::linspace(start_dbfs, stop_dbfs, steps);

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
            let amplitude = ac_core::shared::generator::dbfs_to_amplitude(level_dbfs);
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

            match ac_core::measurement::thd::analyze(&samples, sr, freq_hz, 10) {
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
