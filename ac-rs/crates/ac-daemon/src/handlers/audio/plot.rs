//! `plot` / `plot_level` — run a sweep, collect per-point analysis frames,
//! and emit a `done` with the full dataset so the CLI can render a PNG.

use std::sync::atomic::Ordering;

use serde_json::{json, Value};

use ac_core::measurement::filterbank::Filterbank;
use ac_core::measurement::report::{
    FrequencyResponsePoint, IntegrationParams, MeasurementData, MeasurementMethod,
    MeasurementReport, StimulusParams, SCHEMA_VERSION,
};
use ac_core::measurement::thd;
use ac_core::shared::calibration::Calibration;

use crate::audio::make_engine;
use crate::server::ServerState;

use super::super::{
    busy_guard, resolve_input, resolve_output, send_pub, snapshot_from_cal, spawn_worker,
    sweep_point_frame, Tier1Ctx,
};
use crate::handlers::mic;

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
    let bpo        = cmd.get("bpo")       .and_then(Value::as_u64).map(|v| v as usize);
    let cfg        = state.cfg.lock().unwrap().clone();

    let out_port = resolve_output(&cfg, state);
    let in_port  = resolve_input(&cfg, state);
    let out_port_reply = out_port.clone();
    let in_port_reply  = in_port.clone();

    let pub_tx   = state.pub_tx.clone();
    let fake     = state.fake_audio;
    let out_ch   = cfg.output_channel;
    let in_ch    = cfg.input_channel;
    // Processing-context shared state — same Arc clones the monitor
    // worker uses so #97 + #98 wire the same envelope onto Tier 1.
    let mic_corr_enabled         = state.mic_correction_enabled.clone();
    let band_weighting_shared    = state.band_weighting.clone();
    let time_integration_shared  = state.time_integration_mode.clone();

    let report_dir = cfg.report_dir.clone();

    let worker = spawn_worker(state, "plot", move |stop| {
        let cal = Calibration::load(out_ch, in_ch, None).ok().flatten();
        let mic_curve_opt = cal.as_ref().and_then(|c| c.mic_response.clone());
        let spl_offset    = cal.as_ref().and_then(Calibration::spl_offset_db);
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
        let mut concat_capture: Vec<f32> = Vec::new();
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
                Ok(mut r) => {
                    // Mic-curve correction (#97): applied to the analysis
                    // result in place — spectrum bins, fundamental, harmonics,
                    // THD recomputed. linear_rms / noise_floor untouched (see
                    // `mic::apply_mic_curve_to_analysis` doc).
                    let mc_enabled = mic_corr_enabled.load(Ordering::Relaxed);
                    if mc_enabled {
                        if let Some(curve) = &mic_curve_opt {
                            mic::apply_mic_curve_to_analysis(curve, &mut r);
                        }
                    }
                    let mc_tag = mic::mic_correction_tag(mic_curve_opt.is_some(), mc_enabled);
                    let weighting = band_weighting_shared.lock().unwrap().clone();
                    let time_int  = time_integration_shared.lock().unwrap().clone();
                    let ctx = Tier1Ctx {
                        mic_correction:   mc_tag,
                        spl_offset_db:    spl_offset,
                        weighting:        &weighting,
                        time_integration: &time_int,
                        smoothing_bpo:    None,
                    };
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
                    let frame = sweep_point_frame(
                        &r, cal.as_ref(), n, "plot", level_dbfs, Some(*freq), &ctx,
                    );
                    send_pub(&pub_tx, "data", &frame);
                    n += 1;
                }
                Err(e) => eprintln!("plot: analyze error at {freq}Hz: {e}"),
            }
            if bpo.is_some() {
                concat_capture.extend_from_slice(&samples);
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
                standard: Some(thd::citation()),
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
            calibration: snapshot_from_cal(cal.as_ref()),
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

        if let Some(ref dir) = report_dir {
            let filename = format!("{timestamp}-plot.json").replace(':', "-");
            let path = dir.join(filename);
            if let Err(e) = report.write_to(&path) {
                eprintln!("plot: report write error ({}): {e}", path.display());
            }
        }

        if let Some(bpo) = bpo {
            emit_spectrum_bands(
                &pub_tx,
                &concat_capture,
                sr,
                bpo,
                start_hz,
                stop_hz,
                level_dbfs,
                duration,
                &timestamp,
                report_dir.as_deref(),
                cal.as_ref(),
            );
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
    let mic_corr_enabled        = state.mic_correction_enabled.clone();
    let band_weighting_shared   = state.band_weighting.clone();
    let time_integration_shared = state.time_integration_mode.clone();

    let worker = spawn_worker(state, "plot_level", move |stop| {
        let cal = Calibration::load(out_ch, in_ch, None).ok().flatten();
        let mic_curve_opt = cal.as_ref().and_then(|c| c.mic_response.clone());
        let spl_offset    = cal.as_ref().and_then(Calibration::spl_offset_db);
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
                Ok(mut r) => {
                    let mc_enabled = mic_corr_enabled.load(Ordering::Relaxed);
                    if mc_enabled {
                        if let Some(curve) = &mic_curve_opt {
                            mic::apply_mic_curve_to_analysis(curve, &mut r);
                        }
                    }
                    let mc_tag = mic::mic_correction_tag(mic_curve_opt.is_some(), mc_enabled);
                    let weighting = band_weighting_shared.lock().unwrap().clone();
                    let time_int  = time_integration_shared.lock().unwrap().clone();
                    let ctx = Tier1Ctx {
                        mic_correction:   mc_tag,
                        spl_offset_db:    spl_offset,
                        weighting:        &weighting,
                        time_integration: &time_int,
                        smoothing_bpo:    None,
                    };
                    let frame = sweep_point_frame(
                        &r, cal.as_ref(), n, "plot_level", level_dbfs, Some(freq_hz), &ctx,
                    );
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

/// Run the concatenated sweep capture through an IEC 61260-1 Class 1
/// filterbank at the requested BPO, publish a `measurement/spectrum_bands`
/// frame and a second `measurement/report` whose data payload is the
/// `SpectrumBands` variant. Errors and non-fatal rejections (e.g. the
/// band grid would clash with Nyquist) are logged and swallowed — the
/// primary frequency-response report has already been emitted.
#[allow(clippy::too_many_arguments)]
fn emit_spectrum_bands(
    pub_tx: &crossbeam_channel::Sender<Vec<u8>>,
    samples: &[f32],
    sr: u32,
    bpo: usize,
    start_hz: f64,
    stop_hz: f64,
    level_dbfs: f64,
    duration: f64,
    timestamp: &str,
    report_dir: Option<&std::path::Path>,
    cal: Option<&Calibration>,
) {
    let f_max = (sr as f64 * 0.45).min(stop_hz.max(start_hz));
    let f_min = start_hz.max(1.0);
    let fb = match Filterbank::new(sr, bpo, f_min, f_max) {
        Ok(fb) => fb,
        Err(e) => {
            eprintln!("plot: filterbank init failed: {e}");
            return;
        }
    };
    let levels = fb.process(samples);
    let centres: Vec<f64> = fb.centres_hz().to_vec();

    send_pub(pub_tx, "measurement/spectrum_bands", &json!({
        "cmd":         "plot",
        "bpo":         bpo,
        "class":       fb.class().label(),
        "centres_hz":  centres.clone(),
        "levels_dbfs": levels.clone(),
    }));

    let bands_report = MeasurementReport {
        schema_version: SCHEMA_VERSION,
        ac_version:     env!("CARGO_PKG_VERSION").to_string(),
        timestamp_utc:  timestamp.to_string(),
        method: MeasurementMethod::SteppedSine {
            n_points: centres.len(),
            standard: Some(Filterbank::citation()),
        },
        stimulus: StimulusParams {
            sample_rate_hz: sr,
            f_start_hz:     start_hz,
            f_stop_hz:      stop_hz,
            level_dbfs,
            n_points:       centres.len(),
        },
        integration: IntegrationParams {
            duration_s: duration,
            window:     "butterworth-bp".into(),
        },
        calibration: snapshot_from_cal(cal),
        data: MeasurementData::SpectrumBands {
            bpo:         bpo as u32,
            class:       fb.class().label().to_string(),
            centres_hz:  centres,
            levels_dbfs: levels,
        },
        notes: None,
    };

    match serde_json::to_value(&bands_report) {
        Ok(report_json) => {
            send_pub(pub_tx, "measurement/report", &json!({
                "cmd":    "plot",
                "report": report_json,
            }));
        }
        Err(e) => eprintln!("plot: bands report serialization error: {e}"),
    }

    if let Some(dir) = report_dir {
        let filename = format!("{timestamp}-plot-bands.json").replace(':', "-");
        let path = dir.join(filename);
        if let Err(e) = bands_report.write_to(&path) {
            eprintln!("plot: bands report write error ({}): {e}", path.display());
        }
    }
}
