//! Hardware self-tests: noise floor, level linearity, THD floor, frequency
//! response, channel match, repeatability, plus optional DMM-corroborated
//! absolute / tracking / freq-response tests.

use std::sync::atomic::Ordering;

use serde_json::{json, Value};

use ac_core::shared::calibration::Calibration;

use crate::audio::{make_engine, AudioEngine};
use crate::handlers::mic;
use crate::server::ServerState;

use super::{
    analyze_mono, busy_guard, capture_rms, read_dmm_vrms, resolve_input,
    resolve_output, resolve_ref_input, resolve_ref_output, rms_to_dbfs, send_pub,
    spawn_worker, std_dev, TestResult,
};

pub fn test_hardware(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "test_hardware");

    let cfg = state.cfg.lock().unwrap().clone();

    if cfg.reference_channel.is_none() && cfg.reference_port.is_none() {
        return json!({"ok": false, "error": "reference channel not configured — run: ac setup reference <channel>"});
    }

    let dmm_mode     = cmd.get("dmm").and_then(Value::as_bool).unwrap_or(false);
    let out_port     = resolve_output(&cfg, state);
    let in_port      = resolve_input(&cfg, state);
    let ref_port     = match resolve_ref_input(&cfg, state) {
        Some(p) => p,
        None    => in_port.clone(),
    };
    let ref_out_port = resolve_ref_output(&cfg, state);

    let pub_tx       = state.pub_tx.clone();
    let fake         = state.fake_audio;
    let dmm_host     = cfg.dmm_host.clone();
    let out_ch       = cfg.output_channel;
    let in_ch        = cfg.input_channel;
    let mic_corr_enabled = state.mic_correction_enabled.clone();

    let out_port_r     = out_port.clone();
    let in_port_r      = in_port.clone();
    let ref_port_r     = ref_port.clone();
    let ref_out_port_r = ref_out_port.clone();

    let worker = spawn_worker(state, "test_hardware", move |stop| {
        let out_ports: Vec<String> = if ref_out_port != out_port {
            vec![out_port.clone(), ref_out_port]
        } else {
            vec![out_port.clone()]
        };

        let mut eng = make_engine(fake);
        if !eng.supports_routing() {
            send_pub(&pub_tx, "error", &json!({
                "cmd":     "test_hardware",
                "message": format!("{} backend does not support port routing", eng.backend_name()),
            }));
            return;
        }
        if let Err(e) = eng.start(&out_ports, Some(&in_port)) {
            send_pub(&pub_tx, "error", &json!({"cmd":"test_hardware","message":format!("{e}")}));
            return;
        }
        let sr = eng.sample_rate();

        let mut tests_run  = 0usize;
        let mut tests_pass = 0usize;

        // Cal context for the active measurement channel — looked up
        // once per worker, stamped on every emitted `test_result` so
        // downstream readers can tell whether the test ran on a
        // mic-curve'd channel and at what SPL offset (#103).
        let cal_ctx = Calibration::load(out_ch, in_ch, None).ok().flatten();
        let mic_curve_loaded = cal_ctx.as_ref()
            .map(|c| c.mic_response.is_some()).unwrap_or(false);
        let spl_offset_db = cal_ctx.as_ref().and_then(Calibration::spl_offset_db);

        macro_rules! emit {
            ($r:expr) => {{
                if $r.pass { tests_pass += 1; }
                tests_run += 1;
                let mc_tag = mic::mic_correction_tag(
                    mic_curve_loaded,
                    mic_corr_enabled.load(Ordering::Relaxed),
                );
                send_pub(&pub_tx, "data", &json!({
                    "type": "test_result", "cmd": "test_hardware",
                    "name": $r.name, "pass": $r.pass,
                    "detail": $r.detail, "tolerance": $r.tolerance,
                    "mic_correction":   mc_tag,
                    "spl_offset_db":    spl_offset_db,
                }));
            }};
        }

        if !stop.load(Ordering::Relaxed) {
            emit!(hw_noise_floor(&mut *eng, &in_port, &ref_port, sr));
        }
        if !stop.load(Ordering::Relaxed) {
            emit!(hw_level_linearity(&mut *eng, &in_port, sr));
        }
        if !stop.load(Ordering::Relaxed) {
            emit!(hw_thd_floor(&mut *eng, &in_port, sr));
        }
        if !stop.load(Ordering::Relaxed) {
            emit!(hw_freq_response(&mut *eng, &in_port, sr));
        }
        if !stop.load(Ordering::Relaxed) {
            emit!(hw_channel_match(&mut *eng, &in_port, &ref_port, sr));
        }
        if !stop.load(Ordering::Relaxed) {
            emit!(hw_repeatability(&mut *eng, &in_port, sr));
        }

        // DMM tests (only if configured and requested)
        let mut dmm_run = 0usize; let mut dmm_pass = 0usize;
        if dmm_mode {
            if let Some(ref host) = dmm_host {
                let cal = Calibration::load(out_ch, in_ch, None).ok().flatten();

                macro_rules! emit_dmm {
                    ($r:expr) => {{
                        if $r.pass { dmm_pass += 1; }
                        dmm_run += 1;
                        send_pub(&pub_tx, "data", &json!({
                            "type": "test_result", "cmd": "test_hardware", "dmm": true,
                            "name": $r.name, "pass": $r.pass,
                            "detail": $r.detail, "tolerance": $r.tolerance,
                        }));
                    }};
                }

                if !stop.load(Ordering::Relaxed) {
                    emit_dmm!(hw_dmm_absolute(&mut *eng, host, cal.as_ref()));
                }
                if !stop.load(Ordering::Relaxed) {
                    emit_dmm!(hw_dmm_tracking(&mut *eng, host, cal.as_ref()));
                }
                if !stop.load(Ordering::Relaxed) {
                    emit_dmm!(hw_dmm_freq_response(&mut *eng, host));
                }
            }
        }

        eng.set_silence();
        eng.stop();
        send_pub(&pub_tx, "done", &json!({
            "cmd": "test_hardware",
            "tests_run": tests_run, "tests_pass": tests_pass,
            "dmm_run": dmm_run, "dmm_pass": dmm_pass,
            "xruns": eng.xruns(),
        }));
    });

    {
        let mut workers = state.workers.lock().unwrap();
        workers.insert("test_hardware".to_string(), worker);
    }
    json!({
        "ok": true,
        "out_port":     out_port_r,
        "ref_out_port": ref_out_port_r,
        "in_port":      in_port_r,
        "ref_port":     ref_port_r,
    })
}

// ---- Hardware tests ----

fn hw_noise_floor(eng: &mut dyn AudioEngine, in_a: &str, in_b: &str, _sr: u32) -> TestResult {
    eng.set_silence();
    std::thread::sleep(std::time::Duration::from_millis(100));
    let mut floors = vec![];
    for port in [in_a, in_b] {
        eng.reconnect_input(port).ok();
        eng.flush_capture();
        std::thread::sleep(std::time::Duration::from_millis(50));
        let rms = capture_rms(eng, 0.5);
        floors.push(rms_to_dbfs(rms));
    }
    let pass = floors.iter().all(|&d| d < -80.0);
    TestResult::new(
        "Noise floor",
        pass,
        format!("{:.1} dBFS / {:.1} dBFS", floors[0], floors[1]),
        "< -80 dBFS",
    )
}

fn hw_level_linearity(eng: &mut dyn AudioEngine, in_port: &str, sr: u32) -> TestResult {
    let levels: Vec<i32> = (-42..=-5).step_by(6).collect();
    eng.reconnect_input(in_port).ok();
    let mut measured: Vec<Option<f64>> = Vec::new();
    for &level in &levels {
        let amp = ac_core::shared::generator::dbfs_to_amplitude(level as f64);
        eng.set_tone(1000.0, amp);
        eng.flush_capture();
        std::thread::sleep(std::time::Duration::from_millis(100));
        let r = analyze_mono(eng, 1000.0, 1.0, sr);
        measured.push(r.map(|x| x.fundamental_dbfs));
    }

    let valid: Vec<(i32, f64)> = levels.iter().copied().zip(measured.iter())
        .filter_map(|(l, m)| m.map(|v| (l, v)))
        .collect();

    let monotonic = valid.windows(2).all(|w| w[0].1 < w[1].1);
    let deltas: Vec<(i32, i32, f64)> = valid.windows(2)
        .map(|w| (w[0].0, w[1].0, w[1].1 - w[0].1))
        .collect();
    let max_step_err = deltas.iter().enumerate()
        .map(|(i, &(_, _, d))| {
            let tol = if i == deltas.len().saturating_sub(1) { 1.5 } else { 1.0 };
            (d - 6.0).abs() / tol
        })
        .fold(0.0f64, f64::max);

    let pass = monotonic && max_step_err <= 1.0;
    let step_detail = deltas.iter().map(|(a, b, d)| format!("{a}→{b}:{d:.2}")).collect::<Vec<_>>().join(", ");
    TestResult::new(
        "Level linearity",
        pass,
        format!("[{step_detail}]"),
        "monotonic, step error < 1 dB (1.5 dB top step)",
    )
}

fn hw_thd_floor(eng: &mut dyn AudioEngine, in_port: &str, sr: u32) -> TestResult {
    let levels: &[f64] = &[-40.0, -30.0, -20.0, -10.0, -3.0];
    eng.reconnect_input(in_port).ok();
    let mut results: Vec<(f64, f64, f64)> = Vec::new();
    for &level in levels {
        let amp = ac_core::shared::generator::dbfs_to_amplitude(level);
        eng.set_tone(1000.0, amp);
        if let Some(r) = analyze_mono(eng, 1000.0, 1.0, sr) {
            results.push((level, r.thd_pct, r.thdn_pct));
        }
    }
    let best = results.iter().map(|&(_, t, _)| t).fold(f64::INFINITY, f64::min);
    let parts = results.iter().map(|(l, t, _)| format!("{l:.0}:{t:.4}%")).collect::<Vec<_>>().join(", ");
    TestResult::new(
        "THD floor (1 kHz)",
        best < 0.05,
        format!("best {best:.4}%  [{parts}]"),
        "best THD < 0.05%",
    )
}

fn hw_freq_response(eng: &mut dyn AudioEngine, in_port: &str, sr: u32) -> TestResult {
    let freqs: &[f64] = &[50.0, 100.0, 500.0, 1000.0, 5000.0, 10000.0, 20000.0];
    let amp = ac_core::shared::generator::dbfs_to_amplitude(-10.0);
    eng.reconnect_input(in_port).ok();
    let mut results: Vec<(f64, f64)> = Vec::new();
    for &freq in freqs {
        eng.set_tone(freq, amp);
        if let Some(r) = analyze_mono(eng, freq, 0.5, sr) {
            results.push((freq, r.fundamental_dbfs));
        }
    }
    if results.len() < 2 {
        return TestResult::new("Frequency response", false, "insufficient data".to_string(), "");
    }
    let ref_db = results.iter().find(|&&(f, _)| f == 1000.0).map(|&(_, d)| d)
        .unwrap_or(results[0].1);
    let deviations: Vec<(f64, f64)> = results.iter().map(|&(f, d)| (f, d - ref_db)).collect();
    let max_dev = deviations.iter().map(|&(_, d)| d.abs()).fold(0.0f64, f64::max);
    let parts = deviations.iter().map(|(f, d)| format!("{f:.0}Hz:{d:+.2}dB")).collect::<Vec<_>>().join(", ");
    TestResult::new(
        "Frequency response",
        max_dev < 1.0,
        format!("max deviation {max_dev:.2} dB  [{parts}]"),
        "< 1.0 dB vs 1 kHz ref",
    )
}

fn hw_channel_match(eng: &mut dyn AudioEngine, in_a: &str, in_b: &str, sr: u32) -> TestResult {
    let amp = ac_core::shared::generator::dbfs_to_amplitude(-10.0);
    eng.set_tone(1000.0, amp);
    let mut measurements: Vec<(String, f64, f64)> = Vec::new();
    for (label, port) in [("A", in_a), ("B", in_b)] {
        eng.reconnect_input(port).ok();
        eng.flush_capture();
        std::thread::sleep(std::time::Duration::from_millis(100));
        if let Some(r) = analyze_mono(eng, 1000.0, 1.0, sr) {
            measurements.push((label.to_string(), r.fundamental_dbfs, r.thd_pct));
        }
    }
    if measurements.len() < 2 {
        return TestResult::new("Channel match", false, "measurement failed".to_string(), "");
    }
    let delta_db  = (measurements[0].1 - measurements[1].1).abs();
    let delta_thd = (measurements[0].2 - measurements[1].2).abs();
    TestResult::new(
        "Channel match",
        delta_db < 0.5 && delta_thd < 0.01,
        format!("delta level: {delta_db:.3} dB  delta THD: {delta_thd:.4}%"),
        "level < 0.5 dB, THD < 0.01%",
    )
}

fn hw_repeatability(eng: &mut dyn AudioEngine, in_port: &str, sr: u32) -> TestResult {
    let amp = ac_core::shared::generator::dbfs_to_amplitude(-10.0);
    eng.set_tone(1000.0, amp);
    eng.reconnect_input(in_port).ok();
    let mut levels: Vec<f64> = Vec::new();
    let mut thds: Vec<f64>   = Vec::new();
    for _ in 0..5 {
        eng.flush_capture();
        std::thread::sleep(std::time::Duration::from_millis(20));
        if let Some(r) = analyze_mono(eng, 1000.0, 1.0, sr) {
            levels.push(r.fundamental_dbfs);
            thds.push(r.thd_pct);
        }
    }
    if levels.len() < 3 {
        return TestResult::new("Repeatability", false, "insufficient measurements".to_string(), "");
    }
    let level_std = std_dev(&levels);
    let thd_std   = std_dev(&thds);
    TestResult::new(
        "Repeatability",
        level_std < 0.05 && thd_std < 0.005,
        format!("level sigma={level_std:.4} dB  THD sigma={thd_std:.6}%  ({}x)", levels.len()),
        "level sigma < 0.05 dB, THD sigma < 0.005%",
    )
}

// ---- DMM hardware tests ----

fn hw_dmm_absolute(eng: &mut dyn AudioEngine, host: &str, cal: Option<&Calibration>) -> TestResult {
    let amp = ac_core::shared::generator::dbfs_to_amplitude(-10.0);
    eng.set_tone(1000.0, amp);
    std::thread::sleep(std::time::Duration::from_millis(500));
    let vrms_dmm = match read_dmm_vrms(host, 5) {
        Some(v) => v,
        None    => return TestResult::new("DMM absolute level", false, "DMM read failed".to_string(), ""),
    };
    let vrms_pred = match cal.and_then(|c| c.out_vrms(-10.0)) {
        Some(v) => v,
        None    => return TestResult::new("DMM absolute level", false, "no output calibration".to_string(), "requires calibration"),
    };
    let err_pct = (vrms_dmm - vrms_pred).abs() / vrms_pred * 100.0;
    TestResult::new(
        "DMM absolute level",
        err_pct < 1.0,
        format!("DMM: {:.3} mVrms  predicted: {:.3} mVrms  delta: {err_pct:.2}%",
            vrms_dmm * 1000.0, vrms_pred * 1000.0),
        "< 1% error",
    )
}

fn hw_dmm_tracking(eng: &mut dyn AudioEngine, host: &str, cal: Option<&Calibration>) -> TestResult {
    let levels: &[f64] = &[-40.0, -30.0, -20.0, -10.0, -6.0, -3.0, 0.0];
    let mut max_err = 0.0f64;
    let mut n_pts = 0usize;
    for &level in levels {
        let amp = ac_core::shared::generator::dbfs_to_amplitude(level);
        eng.set_tone(1000.0, amp);
        std::thread::sleep(std::time::Duration::from_millis(400));
        if let (Some(vrms_dmm), Some(vrms_pred)) = (
            read_dmm_vrms(host, 3),
            cal.and_then(|c| c.out_vrms(level)),
        ) {
            let err = (vrms_dmm - vrms_pred).abs() / vrms_pred * 100.0;
            max_err = max_err.max(err);
            n_pts += 1;
        }
    }
    TestResult::new(
        "DMM level tracking",
        max_err < 2.0 && n_pts >= 5,
        format!("max error {max_err:.2}% over {n_pts} points"),
        "< 2% error at all levels",
    )
}

fn hw_dmm_freq_response(eng: &mut dyn AudioEngine, host: &str) -> TestResult {
    let freqs: &[f64] = &[100.0, 1000.0, 5000.0, 10000.0, 20000.0];
    let amp = ac_core::shared::generator::dbfs_to_amplitude(-10.0);
    let mut readings: Vec<(f64, f64)> = Vec::new();
    for &freq in freqs {
        eng.set_tone(freq, amp);
        std::thread::sleep(std::time::Duration::from_millis(500));
        if let Some(v) = read_dmm_vrms(host, 3) {
            readings.push((freq, v));
        }
    }
    if readings.len() < 3 {
        return TestResult::new("DMM freq response", false, "insufficient readings".to_string(), "");
    }
    let ref_v = readings.iter().find(|&&(f, _)| f == 1000.0).map(|&(_, v)| v)
        .unwrap_or(readings[0].1);
    let deviations: Vec<(f64, f64)> = readings.iter()
        .map(|&(f, v)| (f, 20.0 * (v / ref_v.max(1e-12)).log10()))
        .collect();
    let max_dev = deviations.iter().map(|&(_, d)| d.abs()).fold(0.0f64, f64::max);
    let parts = deviations.iter().map(|(f, d)| format!("{f:.0}Hz:{d:+.2}dB")).collect::<Vec<_>>().join(", ");
    TestResult::new(
        "DMM freq response",
        max_dev < 1.0,
        format!("max deviation {max_dev:.2} dB  [{parts}]"),
        "< 1.0 dB vs 1 kHz ref",
    )
}
