//! `plot` / `plot_level` — run a sweep, collect per-point analysis frames,
//! and emit a `done` with the full dataset so the CLI can render a PNG.
//! `plot_ir` — Farina log-sweep impulse-response capture (moved here from
//! `sweep.rs` by #282: it captures and analyses, so it belongs with its
//! `plot`/`plot_level` siblings rather than the pure generators).

use std::sync::atomic::Ordering;

use serde_json::{json, Value};

use ac_core::measurement::filterbank::Filterbank;
use ac_core::measurement::report::{
    FrequencyResponsePoint, GateParams, GatedFrequencyResponsePoint, IntegrationParams,
    InterfaceLatency, MeasuredLatency, MeasurementData, MeasurementMethod, MeasurementPayload,
    MeasurementReport, PositionSnapshot, ProcessingChain, StimulusParams, SCHEMA_VERSION,
};
use ac_core::measurement::sweep::{
    check_tail_decay, citation as sweep_citation, deconvolve_full, extract_irs, farina_citation,
    gated_frequency_response, gated_response_citation, inverse_sweep, log_sweep,
    noise_tail_start_s, SweepParams, LINEAR_DECONV_TAIL_NOTE,
};
use ac_core::measurement::thd;
use ac_core::shared::calibration::{Calibration, TauConditions};

use crate::audio::make_engine;
use crate::server::ServerState;

use super::super::{
    apply_drive_ceiling, busy_guard, cfg_guard, resolve_input, resolve_output, send_pub,
    snapshot_from_cal, spawn_worker, sweep_point_frame, Tier1Ctx,
};
use crate::handlers::mic;

pub fn plot(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "plot");
    cfg_guard!(state);
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
    // #360: `plot` puts a stimulus on a physical output, so it is clamped
    // to the session ceiling here — the same discipline `set_drive` has
    // always had.
    let level_dbfs = apply_drive_ceiling(cfg.drive_max_dbfs, level_dbfs);

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
                n_averages: None,
            },
            calibration: snapshot_from_cal(cal.as_ref()),
            position,
            // A stepped-sine sweep records no arrival, so there is
            // nothing here for a τ to correct (#283).
            interface_latency: None,
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
    json!({
        "ok": true,
        "out_port": out_port_reply,
        "in_port": in_port_reply,
        "level_dbfs": level_dbfs,
    })
}

pub fn plot_level(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "plot_level");
    cfg_guard!(state);
    let freq_hz = cmd.get("freq_hz").and_then(Value::as_f64).unwrap_or(1000.0);
    let start_dbfs = cmd
        .get("start_dbfs")
        .and_then(Value::as_f64)
        .unwrap_or(-40.0);
    let stop_dbfs = cmd.get("stop_dbfs").and_then(Value::as_f64).unwrap_or(0.0);
    let steps = cmd.get("steps").and_then(Value::as_u64).unwrap_or(26) as usize;
    let duration = cmd.get("duration").and_then(Value::as_f64).unwrap_or(1.0);
    let cfg = state.cfg.lock().unwrap().clone();
    let ceiling = cfg.drive_max_dbfs;

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
    // Applied endpoints echoed on the sync reply — see `sweep_level`'s
    // identical reasoning (#360): monotone under a `min` clamp, so this is
    // exactly the range the sweep's levels actually cover.
    let start_dbfs_applied = apply_drive_ceiling(ceiling, start_dbfs);
    let stop_dbfs_applied = apply_drive_ceiling(ceiling, stop_dbfs);

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
        // Raw request shape — each computed level is clamped individually
        // below (#360), not the endpoints here, so a range whose top end
        // exceeds the ceiling flattens there rather than shifting the
        // whole shape.
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
        for &level_req in &levels {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            // Applied value (#360) — the frame below carries this, not the
            // raw request, so `drive_db` on the wire always matches what
            // reached the engine.
            let level_dbfs = apply_drive_ceiling(ceiling, level_req);
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
    json!({
        "ok": true,
        "out_port": out_port_reply,
        "in_port": in_port_reply,
        "start_dbfs": start_dbfs_applied,
        "stop_dbfs": stop_dbfs_applied,
    })
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
            n_averages: None,
        },
        calibration: snapshot_from_cal(cal),
        position,
        // Band levels carry no arrival for a τ to correct (#283).
        interface_latency: None,
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

/// Look up the τ (interface latency) matching `cond` exactly, and flatten
/// the result into the archival form. Never measures, never falls back to
/// a nearest entry: [`Calibration::tau_for`] refuses on any condition
/// mismatch, and that refusal — with the differing fields named — is what
/// gets recorded, so a reader can see why no distance was derived (#281,
/// #283). A capture with no calibration at all is the same case as a
/// calibration with no matching τ, and says so.
fn resolve_tau(cal: Option<&Calibration>, cond: &TauConditions) -> InterfaceLatency {
    let Some(cal) = cal else {
        return InterfaceLatency::Unavailable {
            reason: format!(
                "no calibration stored for this channel pair \u{2014} run `ac calibrate` with \
                 loopback patched to measure \u{3c4} for device {} / {} backend",
                cond.device, cond.backend
            ),
        };
    };
    match cal.tau_for(cond) {
        Ok(e) => InterfaceLatency::Measured(MeasuredLatency {
            tau_s: e.tau_s,
            measured_at: e.measured_at.clone(),
            method: e.method.clone(),
            backend: e.conditions.backend.clone(),
            sample_rate_hz: e.conditions.sample_rate,
            period_size: e.conditions.period_size,
            output_port: e.conditions.output_port.clone(),
            input_port: e.conditions.input_port.clone(),
        }),
        Err(refusal) => InterfaceLatency::Unavailable {
            reason: refusal.message(),
        },
    }
}

/// `plot_ir` — Farina exponential log-sweep impulse-response measurement.
/// Renamed from `sweep_ir` by #282 (CLI moved to `ac plot ir`): the wire
/// `cmd` now follows its new `plot` family, matching `plot`/`plot_level`.
///
/// Generates an ESS at `level_dbfs` (clamped to `drive_max_dbfs`, #360),
/// plays it out via the audio engine,
/// synchronously captures `duration + tail_s` of the measurement input,
/// deconvolves via the normalized inverse filter, gates the linear IR and
/// the first few pre-impulse harmonic IRs, and emits them as a
/// `measurement/impulse_response` frame plus a full `MeasurementReport`.
/// The report's `notes` carry the ISO 18233 §6.3.2 post-hoc tail-decay
/// verdict (issue #282 acceptance criterion 6) — see
/// `ac_core::measurement::sweep::check_tail_decay`.
///
/// Fake, JACK, and CPAL backends all implement `play_and_capture`
/// (`jack_backend.rs`, `cpal_backend.rs`) — only the default trait impl
/// bails. See ARCHITECTURE.md.
pub fn plot_ir(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "plot_ir");
    cfg_guard!(state);
    let f1_hz = cmd.get("f1_hz").and_then(Value::as_f64).unwrap_or(20.0);
    let f2_hz = cmd.get("f2_hz").and_then(Value::as_f64).unwrap_or(20_000.0);
    let duration = cmd.get("duration").and_then(Value::as_f64).unwrap_or(1.0);
    let level_dbfs = cmd
        .get("level_dbfs")
        .and_then(Value::as_f64)
        .unwrap_or(-6.0);
    let tail_s = cmd.get("tail_s").and_then(Value::as_f64).unwrap_or(0.5);
    let n_harmonics = cmd.get("n_harmonics").and_then(Value::as_u64).unwrap_or(5) as usize;
    // 4096 is a request, not a promise: `extract_irs` clamps each order's
    // gate down to the spacing of its own nearest neighbour, so the linear
    // IR keeps the full 4096 (its neighbour, order 2, sits ~4816 samples
    // away at these defaults) while the tighter high orders get 2818 /
    // 1999 / 1551 / 1551. Those lengths are not silent — they ride out in
    // the `measurement/impulse_response` envelope and, when any order was
    // shortened, in the report notes. See issue #278.
    let window_len = cmd
        .get("window_len")
        .and_then(Value::as_u64)
        .unwrap_or(4096) as usize;

    let cfg = state.cfg.lock().unwrap().clone();
    // #360: `plot_ir` had no clamp at all — the module doc on
    // `tests/it_loopback_ir.rs` documented this gap outright. Every other
    // field derived from `level_dbfs` below (the report's `StimulusParams`,
    // the actual played amplitude) now derives from the applied value.
    let level_dbfs = apply_drive_ceiling(cfg.drive_max_dbfs, level_dbfs);
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
    let device = cfg.device;
    let tau_out_port = out_port.clone();
    let tau_in_port = in_port.clone();
    let mic_corr_enabled = state.mic_correction_enabled.clone();

    let pub_tx = state.pub_tx.clone();
    let fake = state.fake_audio;

    let worker = spawn_worker(state, "plot_ir", move |_stop| {
        // Calibration snapshot. The linear IR itself is never mic-curve
        // corrected — arrival estimation and gating (`extract_irs`,
        // `gated_frequency_response` below) run on the raw, uncorrected
        // capture. Correction is applied afterward, in the frequency
        // domain, to the *derived* gated spectrum only (#285) — see the
        // `gated_points` correction step below for why: `MicCurveFir`'s
        // linear-phase group delay would otherwise move the IR peak the
        // gate is anchored to.
        let cal = Calibration::load(out_ch, in_ch, None).ok().flatten();
        let mic_curve_opt = cal.as_ref().and_then(|c| c.mic_response.clone());

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

        // τ (#281) resolved here, while the engine is live: `period_size()`
        // and `backend_name()` are part of the exact-match key, and a τ
        // measured under any other tuple is a different number. Looked up
        // rather than measured — `plot_ir` must not silently re-run a
        // calibration step. Both outcomes are recorded; "no τ" is the
        // provenance that stops a distance being derived downstream (#283).
        let interface_latency = Some(resolve_tau(
            cal.as_ref(),
            &TauConditions {
                device,
                backend: eng.backend_name().to_string(),
                sample_rate: sr,
                period_size: eng.period_size(),
                output_port: tau_out_port,
                input_port: tau_in_port,
            },
        ));

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
        let mut notes = vec![decay_note];

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
        if let Some(note) = irs.clamp_note() {
            notes.push(note);
        }
        // A third, independent statement (#283). The §6.3.2 verdict above
        // is *measured* from this capture's tail and the #278 clamp note
        // describes this run's gate lengths; the §B.5 note is a
        // *structural* consequence of deconvolving linearly, true
        // whatever those two came back as. None implies the others, so
        // all three are emitted.
        notes.push(LINEAR_DECONV_TAIL_NOTE.to_string());

        // Gate length order 1 (the linear IR) actually got — see the
        // `GateParams` below. Falls back to the requested length only if
        // the per-order vec is somehow empty, which `extract_irs` does
        // not do for `n_harmonics >= 1`.
        let linear_gate_len = irs.window_len_for(1).unwrap_or(window_len);

        let data = MeasurementData::ImpulseResponse {
            sample_rate_hz: sr,
            f1_hz,
            f2_hz,
            duration_s: duration,
            linear_ir: irs.linear.clone(),
            harmonics: irs.harmonics.clone(),
            // Derivable from the sweep parameters, not estimated — see
            // `noise_tail_start_s`'s doc (#284).
            noise_tail_start_s: Some(noise_tail_start_s(&params)),
        };
        // Gate lengths ride alongside `data` rather than inside it: a
        // consumer that assumes every IR is `window_len` long would read
        // the clamped harmonics wrong, and `MeasurementData` is a
        // versioned schema this doesn't need to bump.
        send_pub(
            &pub_tx,
            "measurement/impulse_response",
            &json!({
                "cmd": "plot_ir",
                "data": &data,
                "window_len_requested": irs.window_len_requested,
                "window_len_used": irs.window_len_used,
            }),
        );

        // Gated frequency response (#284): magnitude + phase from a Tukey-
        // tapered FFT of the linear IR. The gate starts at the linear IR's
        // peak-reference sample (`linear_ir.len() / 2`, per #278) and runs
        // for half the linear gate's length — the causal half that
        // actually holds post-arrival samples; a gate starting there and
        // running the *full* window would read past the end of `irs.linear`
        // on its trailing half. Default shape is Tukey α=0.25, matching the
        // string already used in #280's fixtures/UX mockup.
        const GATED_RESPONSE_ALPHA: f64 = 0.25;
        let gated_gate_len = (linear_gate_len / 2).max(2);
        let gated_gate_length_s = gated_gate_len as f64 / sr as f64;
        let mut gated_raw = gated_frequency_response(
            &irs.linear,
            sr,
            0.0,
            gated_gate_length_s,
            GATED_RESPONSE_ALPHA,
        );
        // Mic-curve correction (#285): applied here, to the already-gated
        // derived spectrum, in the frequency domain — the same
        // `mic::apply_mic_curve_inplace_f64` subtraction `plot`/
        // `plot_level` use on `AnalysisResult.spectrum`. `irs.linear`
        // above (and the arrival/gate it was extracted with) is never
        // touched: by this point gating is already done, so there is no
        // time axis left for a FIR's group delay to disturb.
        let mc_enabled = mic_corr_enabled.load(Ordering::Relaxed);
        if mc_enabled {
            if let Some(curve) = &mic_curve_opt {
                mic::apply_mic_curve_to_gated_response(curve, &mut gated_raw);
            }
        }
        let mc_applied = mic_curve_opt.is_some() && mc_enabled;
        if mc_applied {
            // (#306 QA correctness issue 1) `processing_chain.mic_correction_
            // applied` is one report-wide flag, but this report carries two
            // payloads with different correction status: the linear IR /
            // harmonics are never corrected (see the comment at the top of
            // this worker), only the gated frequency response is. Without
            // this note, a reader trusting the report-wide flag (per its own
            // doc comment on `ProcessingChain::mic_correction_applied`) could
            // wrongly assume `linear_ir` is corrected too.
            notes.push(
                "mic correction applied to the gated frequency-response payload only; \
                 the linear impulse response and harmonics are uncorrected raw capture data"
                    .to_string(),
            );
        }
        let gated_points: Vec<GatedFrequencyResponsePoint> = gated_raw
            .into_iter()
            .map(|p| GatedFrequencyResponsePoint {
                freq_hz: p.freq_hz,
                magnitude_db: p.magnitude_db,
                phase_deg: p.phase_deg,
            })
            .collect();
        let gated_payload = MeasurementPayload {
            data: MeasurementData::GatedFrequencyResponse {
                points: gated_points,
            },
            // Quasi-anechoic frequency response cites the Farina
            // theoretical basis (preprint only, not `sweep_citation()` —
            // that also packs in ISO 18233:2006 Annex B, which does not
            // apply to this payload per the architect's #284 decision 4;
            // see `farina_citation`'s doc) plus AES17-2020 Annex A.4.5 for
            // the gating method itself.
            standard: vec![farina_citation(), gated_response_citation()],
            gate: Some(GateParams {
                gate_start_s: 0.0,
                gate_length_s: gated_gate_length_s,
                window_kind: format!("tukey{GATED_RESPONSE_ALPHA:.2}"),
                f_low_hz: 1.0 / gated_gate_length_s,
            }),
        };

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
                n_averages: None,
            },
            calibration: snapshot_from_cal(cal.as_ref()),
            position,
            interface_latency,
            data: vec![
                MeasurementPayload {
                    data,
                    standard: vec![sweep_citation()],
                    // `extract_irs` gates a rectangular window centred on the
                    // sweep endpoint — the zero-delay reference — so the gate
                    // opens half a window before it. Recorded rather than left
                    // implicit: #280's rule is that `f_low_hz` is stored, not
                    // recomputed by the reader.
                    //
                    // The length here is the one order 1 *actually* got, not
                    // the one requested: #278 clamps each order to its nearest
                    // neighbour's spacing, so on a short sweep the linear IR's
                    // gate can come back shorter than `window_len`. Recording
                    // the request would archive a gate that never ran and an
                    // `f_low_hz` the payload does not meet.
                    gate: Some(GateParams {
                        gate_start_s: -((linear_gate_len / 2) as f64) / sr as f64,
                        gate_length_s: linear_gate_len as f64 / sr as f64,
                        window_kind: "rectangular".into(),
                        f_low_hz: sr as f64 / linear_gate_len as f64,
                    }),
                },
                gated_payload,
            ],
            // One statement per line: each note is an independent claim
            // and the CLI prints them one to a row, so they must not run
            // together into a single paragraph (#283).
            notes: Some(notes.join("\n")),
            // Reports the truth (#285): `true` exactly when a curve was
            // loaded *and* the global mic-correction toggle was on for
            // this capture — the same predicate `gated_raw`'s correction
            // step above used. The linear IR itself is still uncorrected
            // (see the comment at the top of this worker); this flag
            // describes the gated frequency-response payload only.
            processing_chain: ProcessingChain {
                mic_correction_applied: mc_applied,
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
            let stem = format!("{timestamp}-plot_ir").replace(':', "-");
            let path = dir.join(format!("{stem}.json"));
            if let Err(e) = report.write_to(&path) {
                eprintln!("plot_ir: report write error ({}): {e}", path.display());
            }
            // CSV alongside the JSON, via the report's own IR branch —
            // the same pair `plot`/`plot_level` leave behind, so a
            // spreadsheet reader never has to go through `ac report`
            // (#283).
            let csv_path = dir.join(format!("{stem}.csv"));
            if let Err(e) = std::fs::write(&csv_path, report.to_csv()) {
                eprintln!("plot_ir: CSV write error ({}): {e}", csv_path.display());
            }
        }

        eng.stop();
        send_pub(&pub_tx, "done", &json!({"cmd":"plot_ir"}));
    });

    {
        let mut workers = state.workers.lock().unwrap();
        workers.insert("plot_ir".to_string(), worker);
    }
    json!({"ok": true, "out_port": out_port_reply, "level_dbfs": level_dbfs})
}
