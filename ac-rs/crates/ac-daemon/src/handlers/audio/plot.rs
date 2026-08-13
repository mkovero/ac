//! `plot` / `plot_level` — run a sweep, collect per-point analysis frames,
//! and emit a `done` with the full dataset so the CLI can render a PNG.
//! `plot_ir` — Farina log-sweep impulse-response capture (moved here from
//! `sweep.rs` by #282: it captures and analyses, so it belongs with its
//! `plot`/`plot_level` siblings rather than the pure generators).

use std::sync::atomic::Ordering;

use serde_json::{json, Value};

use ac_core::measurement::filterbank::Filterbank;
use ac_core::measurement::report::{
    FrequencyResponsePoint, IntegrationParams, MeasurementData, MeasurementMethod,
    MeasurementPayload, MeasurementReport, PositionSnapshot, ProcessingChain, StimulusParams,
    SCHEMA_VERSION,
};
use ac_core::measurement::sweep::{
    check_tail_decay, citation as sweep_citation, deconvolve_full, extract_irs, inverse_sweep,
    log_sweep, SweepParams,
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

pub fn plot(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "plot");
    let start_hz = cmd.get("start_hz").and_then(Value::as_f64).unwrap_or(20.0);
    let stop_hz = cmd
        .get("stop_hz")
        .and_then(Value::as_f64)
        .unwrap_or(20_000.0);
    let level_dbfs = cmd
        .get("level_dbfs")
        .and_then(Value::as_f64)
        .unwrap_or(-10.0);
    let ppd = cmd.get("ppd").and_then(Value::as_u64).unwrap_or(10) as usize;
    let duration = cmd.get("duration").and_then(Value::as_f64).unwrap_or(1.0);
    let bpo = cmd.get("bpo").and_then(Value::as_u64).map(|v| v as usize);
    let cfg = state.cfg.lock().unwrap().clone();

    let out_port = match resolve_output(&cfg, state) {
        Ok(p) => p,
        Err(e) => return json!({"ok": false, "error": e}),
    };
    let in_port = match resolve_input(&cfg, state) {
        Ok(p) => p,
        Err(e) => return json!({"ok": false, "error": e}),
    };
    let out_port_reply = out_port.clone();
    let in_port_reply = in_port.clone();

    let pub_tx = state.pub_tx.clone();
    let fake = state.fake_audio;
    let out_ch = cfg.output_channel;
    let in_ch = cfg.input_channel;
    // Processing-context shared state — same Arc clones the monitor
    // worker uses so #97 + #98 wire the same envelope onto Tier 1.
    let mic_corr_enabled = state.mic_correction_enabled.clone();
    let band_weighting_shared = state.band_weighting.clone();
    let time_integration_shared = state.time_integration_mode.clone();

    let report_dir = cfg.report_dir.clone();
    // Environment provenance (#280): flows the setup temperature into
    // the report rather than collecting it a second time. `position`
    // stays `None` when no temperature is configured — nothing else
    // here is knowable without operator entry.
    let temperature_c = cfg.temperature_c;

    let worker = spawn_worker(state, "plot", move |stop| {
        let cal = Calibration::load(out_ch, in_ch, None).ok().flatten();
        let mic_curve_opt = cal.as_ref().and_then(|c| c.mic_response.clone());
        let spl_offset = cal.as_ref().and_then(Calibration::spl_offset_db);
        let freqs = super::super::log_freq_points(start_hz, stop_hz, ppd);
        let amplitude = ac_core::shared::generator::dbfs_to_amplitude(level_dbfs);

        let mut eng = make_engine(fake);
        if let Err(e) = eng.start(&[out_port], Some(&in_port)) {
            send_pub(
                &pub_tx,
                "error",
                &json!({"cmd":"plot","message":format!("{e}")}),
            );
            return;
        }
        let sr = eng.sample_rate();

        let mut n = 0usize;
        let mut xruns = 0u32;
        let mut points: Vec<FrequencyResponsePoint> = Vec::with_capacity(freqs.len());
        let mut concat_capture: Vec<f32> = Vec::new();
        for freq in &freqs {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let dur = f64::max(duration, 3.0 / freq);
            eng.set_tone(*freq, amplitude);
            let _ = eng.capture_block(0.1);
            let samples = match eng.capture_block(dur) {
                Ok(s) => s,
                Err(e) => {
                    send_pub(
                        &pub_tx,
                        "error",
                        &json!({"cmd":"plot","message":format!("{e}")}),
                    );
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
                    let time_int = time_integration_shared.lock().unwrap().clone();
                    let ctx = Tier1Ctx {
                        mic_correction: mc_tag,
                        spl_offset_db: spl_offset,
                        weighting: &weighting,
                        time_integration: &time_int,
                        smoothing_bpo: None,
                    };
                    points.push(FrequencyResponsePoint {
                        freq_hz: *freq,
                        fundamental_dbfs: r.fundamental_dbfs,
                        thd_pct: r.thd_pct,
                        thdn_pct: r.thdn_pct,
                        noise_floor_dbfs: r.noise_floor_dbfs,
                        linear_rms: r.linear_rms,
                        clipping: r.clipping,
                        ac_coupled: r.ac_coupled,
                    });
                    let frame = sweep_point_frame(
                        &r,
                        cal.as_ref(),
                        n,
                        "plot",
                        level_dbfs,
                        Some(*freq),
                        &ctx,
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

        let timestamp = ac_core::shared::time::now_utc_iso8601();
        // Snapshot the processing-chain state at report-build time so a
        // re-loaded report tells the reader what was active during this
        // capture (#105). `mic_correction_applied` reads true exactly
        // when the per-point loop above ran `apply_mic_curve_to_analysis`
        // — the same predicate as the wire frame's `mic_correction` tag.
        let mc_applied = mic_curve_opt.is_some() && mic_corr_enabled.load(Ordering::Relaxed);
        let chain = ProcessingChain {
            weighting: band_weighting_shared.lock().unwrap().clone(),
            smoothing_bpo: None,
            time_integration: time_integration_shared.lock().unwrap().clone(),
            mic_correction_applied: mc_applied,
        };
        let position = temperature_c.map(|t| PositionSnapshot {
            temperature_c: Some(t),
            ..Default::default()
        });
        let report = MeasurementReport {
            schema_version: SCHEMA_VERSION,
            ac_version: env!("CARGO_PKG_VERSION").to_string(),
            timestamp_utc: timestamp.clone(),
            method: MeasurementMethod::SteppedSine { n_points: n },
            stimulus: StimulusParams {
                sample_rate_hz: sr,
                f_start_hz: start_hz,
                f_stop_hz: stop_hz,
                level_dbfs,
                n_points: n,
            },
            integration: IntegrationParams {
                duration_s: duration,
                window: "hann".into(),
            },
            calibration: snapshot_from_cal(cal.as_ref()),
            position,
            data: vec![MeasurementPayload {
                data: MeasurementData::FrequencyResponse { points },
                standard: vec![thd::citation()],
                gate: None,
            }],
            notes: None,
            processing_chain: chain.clone(),
        };

        send_pub(
            &pub_tx,
            "data",
            &json!({
                "type":     "measurement/frequency_response/complete",
                "cmd":      "plot",
                "n_points": n,
                "xruns":    xruns,
            }),
        );

        match serde_json::to_value(&report) {
            Ok(report_json) => {
                send_pub(
                    &pub_tx,
                    "data",
                    &json!({
                        "type":   "measurement/report",
                        "cmd":    "plot",
                        "report": report_json,
                    }),
                );
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
                chain.clone(),
                temperature_c,
            );
        }

        send_pub(
            &pub_tx,
            "done",
            &json!({"cmd":"plot","n_points":n,"xruns":xruns}),
        );
    });

    {
        let mut workers = state.workers.lock().unwrap();
        workers.insert("plot".to_string(), worker);
    }
    json!({"ok": true, "out_port": out_port_reply, "in_port": in_port_reply})
}

pub fn plot_level(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "plot_level");
    let freq_hz = cmd.get("freq_hz").and_then(Value::as_f64).unwrap_or(1000.0);
    let start_dbfs = cmd
        .get("start_dbfs")
        .and_then(Value::as_f64)
        .unwrap_or(-40.0);
    let stop_dbfs = cmd.get("stop_dbfs").and_then(Value::as_f64).unwrap_or(0.0);
    let steps = cmd.get("steps").and_then(Value::as_u64).unwrap_or(26) as usize;
    let duration = cmd.get("duration").and_then(Value::as_f64).unwrap_or(1.0);
    let cfg = state.cfg.lock().unwrap().clone();

    let out_port = match resolve_output(&cfg, state) {
        Ok(p) => p,
        Err(e) => return json!({"ok": false, "error": e}),
    };
    let in_port = match resolve_input(&cfg, state) {
        Ok(p) => p,
        Err(e) => return json!({"ok": false, "error": e}),
    };
    let out_port_reply = out_port.clone();
    let in_port_reply = in_port.clone();

    let pub_tx = state.pub_tx.clone();
    let fake = state.fake_audio;
    let out_ch = cfg.output_channel;
    let in_ch = cfg.input_channel;
    let mic_corr_enabled = state.mic_correction_enabled.clone();
    let band_weighting_shared = state.band_weighting.clone();
    let time_integration_shared = state.time_integration_mode.clone();

    let worker = spawn_worker(state, "plot_level", move |stop| {
        let cal = Calibration::load(out_ch, in_ch, None).ok().flatten();
        let mic_curve_opt = cal.as_ref().and_then(|c| c.mic_response.clone());
        let spl_offset = cal.as_ref().and_then(Calibration::spl_offset_db);
        let levels = super::super::linspace(start_dbfs, stop_dbfs, steps);

        let mut eng = make_engine(fake);
        if let Err(e) = eng.start(&[out_port], Some(&in_port)) {
            send_pub(
                &pub_tx,
                "error",
                &json!({"cmd":"plot_level","message":format!("{e}")}),
            );
            return;
        }
        let sr = eng.sample_rate();

        let mut n = 0usize;
        let mut xruns = 0u32;
        for &level_dbfs in &levels {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let amplitude = ac_core::shared::generator::dbfs_to_amplitude(level_dbfs);
            eng.set_tone(freq_hz, amplitude);
            let _ = eng.capture_block(0.1);
            let samples = match eng.capture_block(duration) {
                Ok(s) => s,
                Err(e) => {
                    send_pub(
                        &pub_tx,
                        "error",
                        &json!({"cmd":"plot_level","message":format!("{e}")}),
                    );
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
                    let time_int = time_integration_shared.lock().unwrap().clone();
                    let ctx = Tier1Ctx {
                        mic_correction: mc_tag,
                        spl_offset_db: spl_offset,
                        weighting: &weighting,
                        time_integration: &time_int,
                        smoothing_bpo: None,
                    };
                    let frame = sweep_point_frame(
                        &r,
                        cal.as_ref(),
                        n,
                        "plot_level",
                        level_dbfs,
                        Some(freq_hz),
                        &ctx,
                    );
                    send_pub(&pub_tx, "data", &frame);
                    n += 1;
                }
                Err(e) => eprintln!("plot_level: analyze error at {level_dbfs}dBFS: {e}"),
            }
        }
        eng.set_silence();
        eng.stop();
        send_pub(
            &pub_tx,
            "done",
            &json!({"cmd":"plot_level","n_points":n,"xruns":xruns}),
        );
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
    chain: ProcessingChain,
    temperature_c: Option<f64>,
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

    send_pub(
        pub_tx,
        "measurement/spectrum_bands",
        &json!({
            "cmd":         "plot",
            "bpo":         bpo,
            "class":       fb.class().label(),
            "centres_hz":  centres.clone(),
            "levels_dbfs": levels.clone(),
        }),
    );

    let position = temperature_c.map(|t| PositionSnapshot {
        temperature_c: Some(t),
        ..Default::default()
    });
    let bands_report = MeasurementReport {
        schema_version: SCHEMA_VERSION,
        ac_version: env!("CARGO_PKG_VERSION").to_string(),
        timestamp_utc: timestamp.to_string(),
        method: MeasurementMethod::SteppedSine {
            n_points: centres.len(),
        },
        stimulus: StimulusParams {
            sample_rate_hz: sr,
            f_start_hz: start_hz,
            f_stop_hz: stop_hz,
            level_dbfs,
            n_points: centres.len(),
        },
        integration: IntegrationParams {
            duration_s: duration,
            window: "butterworth-bp".into(),
        },
        calibration: snapshot_from_cal(cal),
        position,
        data: vec![MeasurementPayload {
            data: MeasurementData::SpectrumBands {
                bpo: bpo as u32,
                class: fb.class().label().to_string(),
                centres_hz: centres,
                levels_dbfs: levels,
            },
            standard: vec![Filterbank::citation()],
            gate: None,
        }],
        notes: None,
        processing_chain: chain,
    };

    match serde_json::to_value(&bands_report) {
        Ok(report_json) => {
            send_pub(
                pub_tx,
                "measurement/report",
                &json!({
                    "cmd":    "plot",
                    "report": report_json,
                }),
            );
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

/// `plot_ir` — Farina exponential log-sweep impulse-response measurement.
/// Renamed from `sweep_ir` by #282 (CLI moved to `ac plot ir`): the wire
/// `cmd` now follows its new `plot` family, matching `plot`/`plot_level`.
///
/// Generates an ESS at `level_dbfs`, plays it out via the audio engine,
/// synchronously captures `duration + tail_s` of the measurement input,
/// deconvolves via the normalized inverse filter, gates the linear IR and
/// the first few pre-impulse harmonic IRs, and emits them as a
/// `measurement/impulse_response` frame plus a full `MeasurementReport`.
/// The report's `notes` carry the ISO 18233 §6.3.2 post-hoc tail-decay
/// verdict (issue #282 acceptance criterion 6) — see
/// `ac_core::measurement::sweep::check_tail_decay`.
///
/// Today only the fake backend implements `play_and_capture`; real JACK /
/// CPAL buffer-playback is tracked as a follow-up. See ARCHITECTURE.md.
pub fn plot_ir(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "plot_ir");
    let f1_hz = cmd.get("f1_hz").and_then(Value::as_f64).unwrap_or(20.0);
    let f2_hz = cmd.get("f2_hz").and_then(Value::as_f64).unwrap_or(20_000.0);
    let duration = cmd.get("duration").and_then(Value::as_f64).unwrap_or(1.0);
    let level_dbfs = cmd
        .get("level_dbfs")
        .and_then(Value::as_f64)
        .unwrap_or(-6.0);
    let tail_s = cmd.get("tail_s").and_then(Value::as_f64).unwrap_or(0.5);
    let n_harmonics = cmd.get("n_harmonics").and_then(Value::as_u64).unwrap_or(5) as usize;
    let window_len = cmd
        .get("window_len")
        .and_then(Value::as_u64)
        .unwrap_or(4096) as usize;

    let cfg = state.cfg.lock().unwrap().clone();
    let out_port = match resolve_output(&cfg, state) {
        Ok(p) => p,
        Err(e) => return json!({"ok": false, "error": e}),
    };
    let in_port = match resolve_input(&cfg, state) {
        Ok(p) => p,
        Err(e) => return json!({"ok": false, "error": e}),
    };
    let out_port_reply = out_port.clone();
    let out_ch = cfg.output_channel;
    let in_ch = cfg.input_channel;
    let report_dir = cfg.report_dir.clone();
    let temperature_c = cfg.temperature_c;

    let pub_tx = state.pub_tx.clone();
    let fake = state.fake_audio;

    let worker = spawn_worker(state, "plot_ir", move |_stop| {
        // Calibration snapshot — the IR itself is NOT yet mic-curve-
        // corrected (the FIR-based deep correction is tracked as a
        // follow-up to #97; the curve provenance is preserved in the
        // report so a downstream tool can apply correction post hoc).
        let cal = Calibration::load(out_ch, in_ch, None).ok().flatten();

        let mut eng = make_engine(fake);
        if let Err(e) = eng.start(&[out_port], Some(&in_port)) {
            send_pub(
                &pub_tx,
                "error",
                &json!({"cmd":"plot_ir","message":format!("{e}")}),
            );
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
                send_pub(
                    &pub_tx,
                    "error",
                    &json!({"cmd":"plot_ir","message":format!("{e}")}),
                );
                return;
            }
        };
        let amp = ac_core::shared::generator::dbfs_to_amplitude(level_dbfs) as f32;
        let scaled: Vec<f32> = sweep.iter().map(|&s| s * amp).collect();

        let captured = match eng.play_and_capture(&scaled, tail_s) {
            Ok(c) => c,
            Err(e) => {
                send_pub(
                    &pub_tx,
                    "error",
                    &json!({"cmd":"plot_ir","message":format!("{e}")}),
                );
                return;
            }
        };

        let inv = match inverse_sweep(&params) {
            Ok(v) => v,
            Err(e) => {
                send_pub(
                    &pub_tx,
                    "error",
                    &json!({"cmd":"plot_ir","message":format!("{e}")}),
                );
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

        // ISO 18233 §6.3.2 post-hoc tail-decay check (#282 acceptance
        // criterion 6) — runs against the unscaled-amplitude `full` buffer
        // before it is windowed down to `irs.linear`/`irs.harmonics`, since
        // the tail past the linear-IR peak is exactly what the criterion
        // asks about and `extract_irs` below discards everything outside
        // `window_len`.
        let decay_note = match check_tail_decay(&full, &params, tail_s) {
            Ok(check) => check.note(),
            Err(e) => format!("ISO 18233 \u{a7}6.3.2 tail-decay check could not be evaluated: {e}"),
        };

        let irs = match extract_irs(&full, &params, n_harmonics.max(1), window_len) {
            Ok(r) => r,
            Err(e) => {
                send_pub(
                    &pub_tx,
                    "error",
                    &json!({"cmd":"plot_ir","message":format!("{e}")}),
                );
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
        send_pub(
            &pub_tx,
            "measurement/impulse_response",
            &json!({
                "cmd": "plot_ir",
                "data": &data,
            }),
        );

        let timestamp = ac_core::shared::time::now_utc_iso8601();
        let position = temperature_c.map(|t| PositionSnapshot {
            temperature_c: Some(t),
            ..Default::default()
        });
        let report = MeasurementReport {
            schema_version: SCHEMA_VERSION,
            ac_version: env!("CARGO_PKG_VERSION").to_string(),
            timestamp_utc: timestamp.clone(),
            method: MeasurementMethod::SweptSine {
                f1_hz,
                f2_hz,
                duration_s: duration,
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
            position,
            data: vec![MeasurementPayload {
                data,
                standard: vec![sweep_citation()],
                gate: None,
            }],
            notes: Some(decay_note),
            // plot_ir's IR isn't yet mic-curve-corrected (deferred from
            // #97 to a follow-up). The snapshot still captures the curve
            // provenance via `calibration.mic_response`; downstream
            // tools can apply the correction post-hoc.
            processing_chain: ProcessingChain {
                mic_correction_applied: false,
                ..Default::default()
            },
        };
        send_pub(
            &pub_tx,
            "measurement/report",
            &json!({
                "cmd": "plot_ir",
                "report": &report,
            }),
        );

        // Persist alongside `plot`/`plot_level` when a report directory is
        // configured — before #282 nothing wrote `sweep_ir`'s report to
        // disk, so `ac report` had no Farina input to render.
        if let Some(ref dir) = report_dir {
            let filename = format!("{timestamp}-plot_ir.json").replace(':', "-");
            let path = dir.join(filename);
            if let Err(e) = report.write_to(&path) {
                eprintln!("plot_ir: report write error ({}): {e}", path.display());
            }
        }

        eng.stop();
        send_pub(&pub_tx, "done", &json!({"cmd":"plot_ir"}));
    });

    {
        let mut workers = state.workers.lock().unwrap();
        workers.insert("plot_ir".to_string(), worker);
    }
    json!({"ok": true, "out_port": out_port_reply})
}
