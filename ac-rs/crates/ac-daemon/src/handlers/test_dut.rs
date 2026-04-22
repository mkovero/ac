//! DUT qualification suite: noise floor, gain, THD vs level, frequency
//! response, clipping point. Optional compare-mode reruns the suite with
//! the DUT bypassed for delta reporting.

use std::sync::atomic::Ordering;

use serde_json::{json, Value};

use ac_core::shared::calibration::Calibration;

use crate::audio::{make_engine, AudioEngine};
use crate::server::ServerState;

use super::{
    busy_guard, cal_dbu_str, cal_out_dbu_str, capture_rms, median, resolve_input,
    resolve_output, resolve_ref_input, resolve_ref_output, rms_to_dbfs, send_pub,
    spawn_worker, TestResult,
};

pub fn test_dut(state: &ServerState, cmd: &Value) -> Value {
    busy_guard!(state, "test_dut");

    let cfg = state.cfg.lock().unwrap().clone();

    if cfg.reference_channel.is_none() && cfg.reference_port.is_none() {
        return json!({"ok": false, "error": "reference channel not configured — run: ac setup reference <channel>"});
    }

    let compare_mode = cmd.get("compare").and_then(Value::as_bool).unwrap_or(false);
    let level_dbfs   = cmd.get("level_dbfs").and_then(Value::as_f64).unwrap_or(-20.0);
    let out_port     = resolve_output(&cfg, state);
    let in_port      = resolve_input(&cfg, state);
    let ref_port     = match resolve_ref_input(&cfg, state) {
        Some(p) => p,
        None    => in_port.clone(),
    };
    let ref_out_port = resolve_ref_output(&cfg, state);
    let out_ch       = cfg.output_channel;
    let in_ch        = cfg.input_channel;

    let pub_tx       = state.pub_tx.clone();
    let fake         = state.fake_audio;
    let dut_reply_tx = state.dut_reply_tx.clone();

    let out_port_r     = out_port.clone();
    let in_port_r      = in_port.clone();
    let ref_port_r     = ref_port.clone();
    let ref_out_port_r = ref_out_port.clone();

    let worker = spawn_worker(state, "test_dut", move |stop| {
        let out_ports: Vec<String> = if ref_out_port != out_port {
            vec![out_port.clone(), ref_out_port]
        } else {
            vec![out_port.clone()]
        };

        let mut eng = make_engine(fake);
        if !eng.supports_routing() {
            send_pub(&pub_tx, "error", &json!({
                "cmd":     "test_dut",
                "message": format!("{} backend does not support port routing", eng.backend_name()),
            }));
            return;
        }
        if let Err(e) = eng.start(&out_ports, Some(&in_port)) {
            send_pub(&pub_tx, "error", &json!({"cmd":"test_dut","message":format!("{e}")}));
            return;
        }
        if let Err(e) = eng.add_ref_input(&ref_port) {
            eprintln!("test_dut: ref input {ref_port}: {e}");
        }

        let sr  = eng.sample_rate();
        let cal = Calibration::load(out_ch, in_ch, None).ok().flatten();
        let mut tests_done = 0usize;

        macro_rules! emit {
            ($r:expr) => {{
                tests_done += 1;
                send_pub(&pub_tx, "data", &json!({
                    "type": "test_result", "cmd": "test_dut",
                    "name": $r.name, "pass": $r.pass,
                    "detail": $r.detail, "tolerance": $r.tolerance,
                }));
            }};
            ($r:expr, $tag:expr) => {{
                tests_done += 1;
                send_pub(&pub_tx, "data", &json!({
                    "type": "test_result", "cmd": "test_dut", "tag": $tag,
                    "name": $r.name, "pass": $r.pass,
                    "detail": $r.detail, "tolerance": $r.tolerance,
                }));
            }};
        }

        // Run DUT suite
        if !stop.load(Ordering::Relaxed) {
            emit!(dut_noise_floor(&mut *eng, sr, cal.as_ref()), "dut");
        }
        if !stop.load(Ordering::Relaxed) {
            emit!(dut_gain(&mut *eng, level_dbfs, sr, cal.as_ref()), "dut");
        }
        if !stop.load(Ordering::Relaxed) {
            emit!(dut_thd_vs_level(&mut *eng, sr, cal.as_ref()), "dut");
        }
        if !stop.load(Ordering::Relaxed) {
            emit!(dut_freq_response(&mut *eng, level_dbfs, sr, cal.as_ref()), "dut");
        }
        if !stop.load(Ordering::Relaxed) {
            emit!(dut_clipping_point(&mut *eng, sr, cal.as_ref()), "dut");
        }

        if compare_mode && !stop.load(Ordering::Relaxed) {
            let (tx, rx) = crossbeam_channel::bounded(1);
            *dut_reply_tx.lock().unwrap() = Some(tx);

            send_pub(&pub_tx, "data", &json!({
                "type": "dut_compare_prompt", "cmd": "test_dut",
                "message": "Bypass DUT and press Enter",
            }));

            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
            loop {
                if stop.load(Ordering::Relaxed) { break; }
                if std::time::Instant::now() > deadline { break; }
                if rx.try_recv().is_ok() { break; }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            *dut_reply_tx.lock().unwrap() = None;

            if !stop.load(Ordering::Relaxed) {
                if !stop.load(Ordering::Relaxed) {
                    emit!(dut_noise_floor(&mut *eng, sr, cal.as_ref()), "bypass");
                }
                if !stop.load(Ordering::Relaxed) {
                    emit!(dut_gain(&mut *eng, level_dbfs, sr, cal.as_ref()), "bypass");
                }
                if !stop.load(Ordering::Relaxed) {
                    emit!(dut_thd_vs_level(&mut *eng, sr, cal.as_ref()), "bypass");
                }
                if !stop.load(Ordering::Relaxed) {
                    emit!(dut_freq_response(&mut *eng, level_dbfs, sr, cal.as_ref()), "bypass");
                }
                if !stop.load(Ordering::Relaxed) {
                    emit!(dut_clipping_point(&mut *eng, sr, cal.as_ref()), "bypass");
                }
            }
        }

        eng.set_silence();
        eng.stop();
        let xruns = eng.xruns();
        send_pub(&pub_tx, "done", &json!({
            "cmd": "test_dut",
            "tests_run": tests_done, "compare": compare_mode, "xruns": xruns,
        }));
    });

    {
        let mut workers = state.workers.lock().unwrap();
        workers.insert("test_dut".to_string(), worker);
    }
    json!({
        "ok": true,
        "out_port":     out_port_r,
        "ref_out_port": ref_out_port_r,
        "in_port":      in_port_r,
        "ref_port":     ref_port_r,
    })
}

pub fn dut_reply(state: &ServerState) -> Value {
    let tx = state.dut_reply_tx.lock().unwrap();
    if let Some(ref t) = *tx {
        let _ = t.send(());
    }
    json!({"ok": true})
}

// ---- DUT test functions (port of ac/test.py run_dut_*) ----

fn dut_noise_floor(eng: &mut dyn AudioEngine, _sr: u32, cal: Option<&Calibration>) -> TestResult {
    eng.set_silence();
    std::thread::sleep(std::time::Duration::from_millis(200));
    let rms   = capture_rms(eng, 1.0);
    let dbfs  = rms_to_dbfs(rms);
    let label = cal_dbu_str(dbfs, cal, false);
    TestResult::new("Noise floor", true, label, "DUT output noise")
}

fn dut_gain(eng: &mut dyn AudioEngine, level_dbfs: f64, sr: u32, cal: Option<&Calibration>) -> TestResult {
    let amp = ac_core::shared::generator::dbfs_to_amplitude(level_dbfs);
    eng.set_tone(1000.0, amp);
    std::thread::sleep(std::time::Duration::from_millis(200));
    let (meas, refch) = match eng.capture_stereo(1.0) {
        Ok(s)  => s,
        Err(e) => return TestResult::new("Gain", false, format!("capture failed: {e}"), ""),
    };
    let r_meas = match ac_core::measurement::thd::analyze(&meas, sr, 1000.0, 10) {
        Ok(r)  => r,
        Err(_) => return TestResult::new("Gain", false, "no signal at measurement input".to_string(), ""),
    };
    let r_ref = match ac_core::measurement::thd::analyze(&refch, sr, 1000.0, 10) {
        Ok(r)  => r,
        Err(_) => return TestResult::new("Gain", false, "no signal at reference input".to_string(), ""),
    };
    let gain = r_meas.fundamental_dbfs - r_ref.fundamental_dbfs;
    let ref_str  = cal_out_dbu_str(r_ref.fundamental_dbfs, cal);
    let meas_str = cal_dbu_str(r_meas.fundamental_dbfs, cal, false);
    TestResult::new(
        "Gain",
        true,
        format!("{gain:+.1} dB  (ref: {ref_str} → meas: {meas_str})"),
        "at 1 kHz",
    )
}

fn dut_thd_vs_level(eng: &mut dyn AudioEngine, sr: u32, cal: Option<&Calibration>) -> TestResult {
    let levels: &[f64] = &[-40.0, -30.0, -20.0, -10.0, -6.0, -3.0];
    let mut results: Vec<(f64, f64, f64, f64)> = Vec::new(); // (level, thd, thdn, gain)
    for &level in levels {
        let amp = ac_core::shared::generator::dbfs_to_amplitude(level);
        eng.set_tone(1000.0, amp);
        std::thread::sleep(std::time::Duration::from_millis(100));
        if let Ok((meas, refch)) = eng.capture_stereo(1.0) {
            let r_meas = ac_core::measurement::thd::analyze(&meas, sr, 1000.0, 10).ok();
            let r_ref  = ac_core::measurement::thd::analyze(&refch, sr, 1000.0, 10).ok();
            if let (Some(rm), Some(rr)) = (r_meas, r_ref) {
                let gain = rm.fundamental_dbfs - rr.fundamental_dbfs;
                results.push((level, rm.thd_pct, rm.thdn_pct, gain));
            }
        }
    }
    if results.is_empty() {
        return TestResult::new("THD vs level", false, "no valid measurements".to_string(), "");
    }
    let best_thd = results.iter().map(|&(_, t, _, _)| t).fold(f64::INFINITY, f64::min);
    let parts = results.iter().map(|(l, t, _, g)| {
        let drive = cal_out_dbu_str(*l, cal);
        format!("{drive}:{t:.4}%/{g:+.1}dB")
    }).collect::<Vec<_>>().join(", ");
    TestResult::new(
        "THD vs level",
        true,
        format!("best {best_thd:.4}%  [{parts}]"),
        "THD%/gain at each drive level",
    )
}

fn dut_freq_response(eng: &mut dyn AudioEngine, level_dbfs: f64, sr: u32, cal: Option<&Calibration>) -> TestResult {
    let amp = ac_core::shared::generator::dbfs_to_amplitude(level_dbfs);
    eng.set_pink(amp);
    std::thread::sleep(std::time::Duration::from_millis(300));
    let (meas, refch) = match eng.capture_stereo(4.0) {
        Ok(s)  => s,
        Err(e) => return TestResult::new("Frequency response", false, format!("capture failed: {e}"), ""),
    };
    eng.set_silence();
    let result = ac_core::visualize::transfer::h1_estimate(&refch, &meas, sr);
    let freqs = &result.freqs;
    let mag   = &result.magnitude_db;
    let coh   = &result.coherence;

    let band: Vec<usize> = (0..freqs.len()).filter(|&i| freqs[i] >= 50.0 && freqs[i] <= 20000.0).collect();
    if band.is_empty() {
        return TestResult::new("Frequency response", false, "no data in 50-20kHz".to_string(), "");
    }

    let mag_band: Vec<f64> = band.iter().map(|&i| mag[i]).collect();
    let coh_band: Vec<f64> = band.iter().map(|&i| coh[i]).collect();
    let ref_db  = median(&mag_band);
    let dev_pos = mag_band.iter().copied().fold(f64::NEG_INFINITY, f64::max) - ref_db;
    let dev_neg = mag_band.iter().copied().fold(f64::INFINITY, f64::min) - ref_db;
    let avg_coh = coh_band.iter().sum::<f64>() / coh_band.len() as f64;
    let level_str = cal_out_dbu_str(level_dbfs, cal);

    TestResult::new(
        "Frequency response",
        true,
        format!("{dev_pos:+.1}/{dev_neg:+.1} dB  (50-20kHz, coh {avg_coh:.3}, delay {:.2}ms)  at {level_str}",
            result.delay_ms),
        "H1 transfer function",
    )
}

fn dut_clipping_point(eng: &mut dyn AudioEngine, sr: u32, cal: Option<&Calibration>) -> TestResult {
    let levels: Vec<f64> = (-30..=0).step_by(3).map(|x| x as f64).collect();
    let mut last_clean = None::<f64>;
    let mut clip_level = None::<f64>;

    for level in &levels {
        let amp = ac_core::shared::generator::dbfs_to_amplitude(*level);
        eng.set_tone(1000.0, amp);
        std::thread::sleep(std::time::Duration::from_millis(100));
        let (meas, _) = match eng.capture_stereo(0.5) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if let Ok(r) = ac_core::measurement::thd::analyze(&meas, sr, 1000.0, 10) {
            if r.thd_pct > 1.0 || r.clipping {
                clip_level = Some(*level);
                break;
            }
            last_clean = Some(*level);
        }
    }
    eng.set_silence();

    match clip_level {
        Some(lv) => {
            let onset = cal_out_dbu_str(lv, cal);
            let clean = last_clean.map(|l| cal_out_dbu_str(l, cal)).unwrap_or_else(|| "?".to_string());
            TestResult::new("Clipping point", true, format!("onset at {onset} (last clean: {clean})"), "THD > 1% threshold")
        }
        None => match last_clean {
            Some(lv) => {
                let clean = cal_out_dbu_str(lv, cal);
                TestResult::new("Clipping point", true, format!("clean through {clean} (no clipping detected)"), "THD > 1% threshold")
            }
            None => TestResult::new("Clipping point", false, "no valid measurements".to_string(), ""),
        },
    }
}
