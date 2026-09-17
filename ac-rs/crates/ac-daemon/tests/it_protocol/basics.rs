use serde_json::json;
use serde_json::Value;
use std::time::Duration;
use std::time::Instant;

use crate::common::{Client, Daemon};

#[test]
fn status_replies_ok() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({"cmd":"status"}));
    assert_eq!(r["ok"], json!(true));
    assert_eq!(r["busy"], json!(false));
    assert_eq!(r["listen_mode"], json!("local"));
    assert_eq!(r["backend_required"], json!("fake"));
    assert_eq!(r["backend_available"], json!(true));
    assert_eq!(r["backend"], json!("fake"));
}

/// #385: `status` carries the identity fields a client needs to tell this
/// daemon apart from one squatting the same hardcoded ports under a
/// different `HOME`.
#[test]
fn status_reports_daemon_identity() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({"cmd":"status"}));
    assert_eq!(r["ok"], json!(true));
    assert_eq!(r["backend"], json!("fake"));
    assert_eq!(
        r["home"],
        json!(d.home.display().to_string()),
        "home must be the daemon's own $HOME: {r}"
    );
    assert_eq!(
        r["pid"],
        json!(d.pid()),
        "pid must be this daemon process's own pid: {r}"
    );
    assert_eq!(
        r["spawn_mode"],
        json!("manual"),
        "Daemon::spawn never passes --auto-spawned: {r}"
    );
    let config_path = r["config_path"].as_str().expect("config_path string");
    assert!(
        config_path.ends_with("config.json"),
        "config_path: {config_path}"
    );
    assert!(
        config_path.starts_with(&d.home.display().to_string()),
        "config_path should be under the daemon's HOME: {config_path}"
    );
    let started_at = r["started_at"].as_str().expect("started_at string");
    assert!(
        started_at.ends_with('Z'),
        "started_at should be RFC3339 Zulu: {started_at}"
    );
}

#[test]
fn unknown_command_rejected() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({"cmd":"nope"}));
    assert_eq!(r["ok"], json!(false));
    assert!(r["error"].as_str().unwrap().contains("unknown command"));
}

#[test]
fn devices_lists_ports() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({"cmd":"devices"}));
    assert_eq!(r["ok"], json!(true));
    assert!(!r["playback"].as_array().unwrap().is_empty());
    assert!(!r["capture"].as_array().unwrap().is_empty());
}

#[test]
fn generate_stop_emits_done_frame() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({"cmd":"generate","freq_hz":1000.0,"level_dbfs":-20.0}));
    assert_eq!(r["ok"], json!(true));

    // Should now be busy.
    let s = c.call(json!({"cmd":"status"}));
    assert_eq!(s["busy"], json!(true));
    assert_eq!(s["running_cmd"], json!("generate"));

    // Stop should emit a "done" frame on the PUB channel.
    let _ = c.call(json!({"cmd":"stop"}));
    let done = c
        .wait_for_topic("done", Duration::from_secs(3))
        .expect("no done frame after stop");
    assert_eq!(done["cmd"], json!("generate"));
}

#[test]
fn generate_routes_all_channels_in_request() {
    // Reproduces the post-2026-05-01 reboot scenario on the FF400 rig:
    // DAC chip enumeration order shifted, so the user shotguns
    // `ac generate sine 0-17 ...` to hit *some* analog output. The CLI
    // sent `channels: [0..17]` but the daemon ignored the field and
    // only opened `cfg.output_channel` — so even the shotgun missed.
    // Lock in: every channel in the request must show up in `out_ports`.
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let chans: Vec<u32> = (0..6).collect();
    let r = c.call(json!({
        "cmd": "generate",
        "freq_hz": 1000.0,
        "level_dbfs": -20.0,
        "channels": chans,
    }));
    assert_eq!(r["ok"], json!(true), "generate ack: {r}");
    let ports = r["out_ports"].as_array().expect("out_ports array");
    assert_eq!(
        ports.len(),
        chans.len(),
        "expected {} ports for channels {:?}, got {:?}",
        chans.len(),
        chans,
        ports,
    );
    // Each port name must be unique — otherwise the daemon collapsed
    // distinct channel indices to the sticky default.
    let names: std::collections::HashSet<&str> = ports.iter().filter_map(|v| v.as_str()).collect();
    assert_eq!(
        names.len(),
        ports.len(),
        "duplicate port in out_ports: {ports:?}"
    );

    let _ = c.call(json!({"cmd":"stop"}));
}

#[test]
fn generate_no_channels_falls_back_to_configured_default() {
    // Bare `ac generate sine ...` (no channel spec) must still route to
    // the configured `output_channel` — this path doesn't go through
    // `resolve_output_by_channel` and was a regression risk when adding
    // multi-channel support.
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let r = c.call(json!({"cmd": "generate", "freq_hz": 1000.0, "level_dbfs": -20.0}));
    assert_eq!(r["ok"], json!(true));
    let ports = r["out_ports"].as_array().expect("out_ports array");
    assert_eq!(
        ports.len(),
        1,
        "default-channel generate should give one port"
    );

    let _ = c.call(json!({"cmd":"stop"}));
}

#[test]
fn busy_guard_blocks_duplicate() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    c.call(json!({"cmd":"generate","freq_hz":1000.0,"level_dbfs":-20.0}));
    let dup = c.call(json!({"cmd":"generate","freq_hz":2000.0,"level_dbfs":-20.0}));
    assert_eq!(dup["ok"], json!(false));
    assert!(dup["error"].as_str().unwrap().contains("busy"));
    let _ = c.call(json!({"cmd":"stop"}));
}

#[test]
fn sweep_frequency_publishes_done() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd":"sweep_frequency",
        "start_hz": 100.0,
        "stop_hz":  200.0,
        "level_dbfs": -20.0,
        "duration": 0.3,
    }));
    assert_eq!(r["ok"], json!(true));
    let done = c
        .wait_for_topic("done", Duration::from_secs(5))
        .expect("sweep_frequency never finished");
    assert_eq!(done["cmd"], json!("sweep_frequency"));
}

#[test]
fn plot_with_bpo_emits_spectrum_bands() {
    // Plot with `bpo` set: the daemon runs the concatenated sweep capture
    // through an IEC 61260-1 1/3-octave filterbank and publishes a
    // `measurement/spectrum_bands` frame plus a second `measurement/report`
    // whose `data.kind == spectrum_bands`. Assert the payload is well-formed
    // and the peak band lies inside the stimulus range.
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let start_hz = 200.0;
    let stop_hz = 4_000.0;
    let r = c.call(json!({
        "cmd":        "plot",
        "start_hz":   start_hz,
        "stop_hz":    stop_hz,
        "level_dbfs": -20.0,
        "ppd":        3,
        "duration":   0.2,
        "bpo":        3,
    }));
    assert_eq!(r["ok"], json!(true));

    let mut got_frame = false;
    let mut got_report = false;
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && !(got_frame && got_report) {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "measurement/spectrum_bands" => {
                assert_eq!(v["bpo"], json!(3));
                assert_eq!(v["class"], json!("Class 1"));
                let centres = v["centres_hz"].as_array().expect("centres_hz array");
                let levels = v["levels_dbfs"].as_array().expect("levels_dbfs array");
                assert_eq!(centres.len(), levels.len());
                assert!(!centres.is_empty(), "filterbank produced no bands");
                // Peak band must land near the 1 kHz loopback tone.
                let (peak_idx, _) =
                    levels
                        .iter()
                        .enumerate()
                        .fold((0usize, f64::NEG_INFINITY), |acc, (i, x)| {
                            let v = x.as_f64().unwrap_or(f64::NEG_INFINITY);
                            if v > acc.1 {
                                (i, v)
                            } else {
                                acc
                            }
                        });
                let peak_fc = centres[peak_idx].as_f64().unwrap();
                assert!(
                    (start_hz / 2.0..=stop_hz * 2.0).contains(&peak_fc),
                    "peak band {peak_fc} Hz falls outside sweep range \
                     [{start_hz}, {stop_hz}] (±1 octave)"
                );
                got_frame = true;
            }
            Some((t, v)) if t == "measurement/report" => {
                if v["report"]["data"][0]["data"]["kind"] == json!("spectrum_bands") {
                    assert_eq!(v["report"]["data"][0]["data"]["bpo"], json!(3));
                    assert_eq!(v["report"]["schema_version"], json!(9));
                    got_report = true;
                }
            }
            Some((t, _)) if t == "done" => break,
            Some(_) => continue,
            None => break,
        }
    }
    assert!(got_frame, "never saw measurement/spectrum_bands frame");
    assert!(
        got_report,
        "never saw measurement/report with spectrum_bands data"
    );
}

#[test]
fn plot_frames_carry_processing_context_envelope() {
    // After Phase 3 (#97 + #98) Tier 1 frames must carry the same
    // processing-context envelope Tier 2 monitor frames already do —
    // mic_correction, spl_offset_db, weighting, time_integration,
    // smoothing_bpo. The MeasurementReport's CalibrationSnapshot must
    // record SPL and mic-curve provenance when those are set.
    let d = Daemon::spawn();
    let c = Client::new(&d);

    // Set up SPL cal.
    let r = c.call(json!({"cmd": "calibrate_spl", "input_channel": 0, "capture_s": 0.05}));
    assert_eq!(r["ok"], json!(true));
    let _ = c
        .wait_for_topic("cal_prompt", Duration::from_secs(3))
        .expect("cal_prompt");
    let _ = c.call(json!({"cmd": "cal_reply", "vrms": Value::Null}));
    let _ = c
        .wait_for_topic("cal_done", Duration::from_secs(5))
        .expect("cal_done");

    // Attach a synthetic 24-point mic-curve.
    let mut freqs = Vec::new();
    let mut gains = Vec::new();
    let log_min = 100.0_f64.ln();
    let log_max = 10_000.0_f64.ln();
    for i in 0..24 {
        let t = i as f64 / 23.0;
        freqs.push((log_min + t * (log_max - log_min)).exp());
        gains.push(2.0 * t); // ramp 0..2 dB
    }
    let r = c.call(json!({
        "cmd":           "calibrate_mic_curve",
        "op":            "set",
        "input_channel": 0,
        "freqs_hz":      freqs,
        "gain_db":       gains,
    }));
    assert_eq!(r["ok"], json!(true));

    // Drive a tiny `plot` and grab the first per-point frame + the report.
    let r = c.call(json!({
        "cmd":        "plot",
        "start_hz":   1000.0,
        "stop_hz":    1000.0,
        "level_dbfs": -20.0,
        "ppd":        1,
        "duration":   0.1,
    }));
    assert_eq!(r["ok"], json!(true));

    let mut point_frame: Option<Value> = None;
    let mut report_frame: Option<Value> = None;
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline && !(point_frame.is_some() && report_frame.is_some()) {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "data" => {
                if v["type"] == json!("measurement/frequency_response/point")
                    && point_frame.is_none()
                {
                    point_frame = Some(v);
                } else if v["type"] == json!("measurement/report") && report_frame.is_none() {
                    report_frame = Some(v);
                }
            }
            Some(_) => continue,
            None => break,
        }
    }
    let pf = point_frame.expect("missing per-point frame");
    let rf = report_frame.expect("missing measurement/report");

    // Envelope keys present on the per-point frame (#98).
    assert_eq!(pf["mic_correction"], json!("on"));
    assert!(
        pf["spl_offset_db"].is_f64(),
        "spl_offset_db not f64: {pf:?}"
    );
    assert_eq!(pf["weighting"], json!("off"));
    assert_eq!(pf["time_integration"], json!("off"));
    assert!(
        pf.get("smoothing_bpo").is_some(),
        "smoothing_bpo key missing"
    );

    // CalibrationSnapshot in the report carries SPL + mic_response (#94 →
    // populated here per #97).
    let cal = rf["report"]["calibration"]
        .as_object()
        .expect("calibration block missing");
    assert!(cal["mic_sensitivity_dbfs_at_94db_spl"].is_f64(), "{cal:?}");
    let mr = cal["mic_response"]
        .as_object()
        .expect("mic_response missing");
    assert_eq!(mr["n_points"], json!(24));
    assert!(mr["imported_at"].is_string());
}

/// First array stored under key `key` anywhere in `v`, depth-first.
fn find_array<'a>(v: &'a Value, key: &str) -> Option<&'a Vec<Value>> {
    match v {
        Value::Object(m) => m
            .get(key)
            .and_then(Value::as_array)
            .or_else(|| m.values().find_map(|x| find_array(x, key))),
        Value::Array(a) => a.iter().find_map(|x| find_array(x, key)),
        _ => None,
    }
}

/// #436: mic-correction enablement is a process-wide toggle, but one
/// measurement must use one state for every point and describe that state
/// in its report. The toggle here lands mid-sweep — after point 0 is
/// published and while later points are still pending — and must not
/// reach this sweep.
///
/// A flat +6 dB curve makes the treatment observable in the numbers, not
/// only the tag: the fake loopback is flat, so a sweep that switched
/// correction off part-way would show a ~6 dB step in `fundamental_dbfs`
/// between its points (the rejected per-point read), and its report would
/// claim whichever state was current at the end.
#[test]
fn plot_mic_correction_toggle_mid_sweep_does_not_mix_points() {
    const CURVE_DB: f64 = 6.0;
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let mut freqs = Vec::new();
    let log_min = 100.0_f64.ln();
    let log_max = 10_000.0_f64.ln();
    for i in 0..24 {
        let t = i as f64 / 23.0;
        freqs.push((log_min + t * (log_max - log_min)).exp());
    }
    let r = c.call(json!({
        "cmd":           "calibrate_mic_curve",
        "op":            "set",
        "input_channel": 0,
        "freqs_hz":      freqs,
        "gain_db":       vec![CURVE_DB; 24],
    }));
    assert_eq!(r["ok"], json!(true), "{r}");
    let r = c.call(json!({"cmd": "set_mic_correction_enabled", "enabled": true}));
    assert_eq!(r["enabled"], json!(true), "{r}");

    // 200 Hz .. 2 kHz at 3 ppd: four points, each ≥ 0.4 s of fake capture
    // (0.1 s warm-up + 0.3 s), so the sweep is still running well after
    // the toggle below is acknowledged.
    let r = c.call(json!({
        "cmd":        "plot",
        "start_hz":   200.0,
        "stop_hz":    2000.0,
        "level_dbfs": -20.0,
        "ppd":        3,
        "duration":   0.3,
    }));
    assert_eq!(r["ok"], json!(true), "plot ack: {r}");

    let mut points: Vec<Value> = Vec::new();
    let mut points_after_toggle = 0usize;
    let mut toggled = false;
    let mut report: Option<Value> = None;
    let mut done: Option<Value> = None;
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && done.is_none() {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "data" => {
                if v["type"] == json!("measurement/frequency_response/point") {
                    points.push(v);
                    if toggled {
                        points_after_toggle += 1;
                    } else {
                        let r =
                            c.call(json!({"cmd": "set_mic_correction_enabled", "enabled": false}));
                        assert_eq!(r["ok"], json!(true), "toggle reply: {r}");
                        assert_eq!(r["enabled"], json!(false), "toggle reply: {r}");
                        toggled = true;
                    }
                } else if v["type"] == json!("measurement/report") {
                    report = Some(v);
                }
            }
            Some((t, v)) if t == "done" => done = Some(v),
            Some((t, v)) if t == "error" => panic!("plot error: {v}"),
            Some(_) => continue,
            None => break,
        }
    }
    let done = done.unwrap_or_else(|| panic!("plot never finished; saw {} points", points.len()));
    let report = report.expect("missing measurement/report");
    assert_eq!(done["n_points"], json!(points.len()), "{done}");
    assert!(
        points_after_toggle >= 2,
        "toggle did not land mid-sweep: only {points_after_toggle} point(s) after it \
         of {} total",
        points.len()
    );

    // Uniform tag on every point: the state the sweep was accepted with.
    for p in &points {
        assert_eq!(p["mic_correction"], json!("on"), "point: {p}");
    }

    // Uniform treatment in the numbers: no ~CURVE_DB step between points.
    let levels: Vec<f64> = points
        .iter()
        .map(|p| p["fundamental_dbfs"].as_f64().expect("fundamental_dbfs"))
        .collect();
    let lo = levels.iter().cloned().fold(f64::INFINITY, f64::min);
    let hi = levels.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    assert!(
        hi - lo < CURVE_DB / 2.0,
        "mixed correction across points: fundamental_dbfs = {levels:?}"
    );

    // The report describes exactly those points.
    let rep = &report["report"];
    assert_eq!(
        rep["processing_chain"]["mic_correction_applied"],
        json!(true),
        "report processing_chain: {}",
        rep["processing_chain"]
    );
    let rep_points = find_array(rep, "points").expect("report points");
    assert_eq!(rep_points.len(), points.len());
    for (rp, p) in rep_points.iter().zip(&points) {
        assert_eq!(rp["fundamental_dbfs"], p["fundamental_dbfs"], "{rp} vs {p}");
    }

    // The accepted toggle applies to the next measurement.
    let r = c.call(json!({
        "cmd":        "plot",
        "start_hz":   1000.0,
        "stop_hz":    1000.0,
        "level_dbfs": -20.0,
        "ppd":        1,
        "duration":   0.1,
    }));
    assert_eq!(r["ok"], json!(true), "second plot ack: {r}");
    let next = loop {
        match c.recv_pub(10_000) {
            Some((t, v))
                if t == "data" && v["type"] == json!("measurement/frequency_response/point") =>
            {
                break v
            }
            Some(_) => continue,
            None => panic!("second plot published no point"),
        }
    };
    assert_eq!(next["mic_correction"], json!("off"), "{next}");
}

/// #428: `AudioEngine::xruns()` is cumulative "since start" (see the trait
/// doc), so summing it once per sweep point double-, triple-, ...-counts
/// every xrun that happened before the sweep's last point. One real xrun
/// injected mid-sweep must show up as exactly 1 in the terminal `done`
/// frame, not accumulate across the remaining points.
///
/// The fake backend's `capture_block` consumes one
/// `AC_FAKE_CAPTURE_BLOCK_XRUNS_OVERRIDE` slot per call
/// (`audio/fake/hooks.rs`), and each sweep point issues two calls — a
/// discarded 0.1 s warm-up, then the real capture — so the 4th call
/// (index 3, 0-based) is point 1's real capture.
#[test]
fn plot_reports_session_xrun_delta_not_cumulative_sum() {
    let d = Daemon::spawn_with_env(&[("AC_FAKE_CAPTURE_BLOCK_XRUNS_OVERRIDE", "0,0,0,1")]);
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd":        "plot",
        "start_hz":   100.0,
        "stop_hz":    1000.0,
        "level_dbfs": -20.0,
        "ppd":        3,
        "duration":   0.05,
    }));
    assert_eq!(r["ok"], json!(true), "plot ack: {r}");

    let done = c
        .wait_for_topic("done", Duration::from_secs(10))
        .expect("plot never finished");
    assert_eq!(done["cmd"], json!("plot"));
    assert_eq!(
        done["n_points"],
        json!(3),
        "all 3 points should have completed cleanly: {done}"
    );
    assert_eq!(
        done["xruns"],
        json!(1),
        "session xrun delta must be 1, not a per-point cumulative sum: {done}"
    );
}

// ---------------------------------------------------------------------------
// server_idle_timeout — daemon folds the public bind back to localhost after
// the configured idle CTRL-activity window expires. See issue #58.
// ---------------------------------------------------------------------------
