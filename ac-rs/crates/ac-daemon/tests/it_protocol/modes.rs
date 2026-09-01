use serde_json::json;
use serde_json::Value;
use std::time::Duration;
use std::time::Instant;

use crate::common::{Client, Daemon};

#[test]
fn time_integration_default_is_off() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({"cmd": "get_time_integration"}));
    assert_eq!(r["ok"], json!(true));
    assert_eq!(r["mode"], json!("off"));
}

#[test]
fn time_integration_accepts_valid_modes() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    for mode in ["off", "fast", "slow", "leq"] {
        let r = c.call(json!({"cmd": "set_time_integration", "mode": mode}));
        assert_eq!(r["ok"], json!(true), "set {mode} failed: {r}");
        assert_eq!(r["mode"], json!(mode));
        let g = c.call(json!({"cmd": "get_time_integration"}));
        assert_eq!(g["mode"], json!(mode));
    }
}

#[test]
fn time_integration_rejects_invalid_mode() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({"cmd": "set_time_integration", "mode": "impulse"}));
    assert_eq!(r["ok"], json!(false));
    assert!(r["error"].as_str().unwrap_or("").contains("invalid mode"));
    // Mode should not have changed.
    let g = c.call(json!({"cmd": "get_time_integration"}));
    assert_eq!(g["mode"], json!("off"));
}

#[test]
fn time_integration_mode_is_case_insensitive() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({"cmd": "set_time_integration", "mode": "SLOW"}));
    assert_eq!(r["ok"], json!(true));
    assert_eq!(r["mode"], json!("slow"));
}

#[test]
fn reset_leq_accepted_when_idle() {
    // No active monitor — the reset flag is latched for the next worker.
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({"cmd": "reset_leq"}));
    assert_eq!(r["ok"], json!(true));
}

// ---------------------------------------------------------------------------
// Band weighting (A/C/Z) — IEC 61672-style curves applied to each
// fractional-octave band before publish. See issue #61.
// ---------------------------------------------------------------------------

#[test]
fn band_weighting_default_is_off() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({"cmd": "get_band_weighting"}));
    assert_eq!(r["ok"], json!(true));
    assert_eq!(r["mode"], json!("off"));
}

#[test]
fn band_weighting_accepts_valid_modes() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    for mode in ["off", "a", "c", "z"] {
        let r = c.call(json!({"cmd": "set_band_weighting", "mode": mode}));
        assert_eq!(r["ok"], json!(true), "set {mode} failed: {r}");
        assert_eq!(r["mode"], json!(mode));
        let g = c.call(json!({"cmd": "get_band_weighting"}));
        assert_eq!(g["mode"], json!(mode));
    }
}

#[test]
fn band_weighting_rejects_invalid_mode() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({"cmd": "set_band_weighting", "mode": "b"}));
    assert_eq!(r["ok"], json!(false));
    assert!(r["error"].as_str().unwrap_or("").contains("invalid mode"));
    let g = c.call(json!({"cmd": "get_band_weighting"}));
    assert_eq!(g["mode"], json!("off"));
}

#[test]
fn band_weighting_mode_is_case_insensitive() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({"cmd": "set_band_weighting", "mode": "A"}));
    assert_eq!(r["ok"], json!(true));
    assert_eq!(r["mode"], json!("a"));
}

// ---------------------------------------------------------------------------
// transfer_stream — ports of the pytest scenarios deleted when the Python
// runtime was removed. See issue #52.
// ---------------------------------------------------------------------------

#[test]
fn set_mic_correction_enabled_round_trips() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({"cmd": "set_mic_correction_enabled", "enabled": false}));
    assert_eq!(r["ok"], json!(true));
    assert_eq!(r["enabled"], json!(false));
    let r = c.call(json!({"cmd": "set_mic_correction_enabled", "enabled": true}));
    assert_eq!(r["enabled"], json!(true));
}

#[test]
fn loudness_lkfs_drops_by_curve_db_when_mic_correction_on() {
    // #104 (Phase 6): with the per-sample inverse-curve FIR running
    // BEFORE K-weighting, a flat +3 dB mic-curve attenuates the audio
    // by 3 dB → LKFS / true_peak drop by 3 dB. Without the FIR the
    // LKFS would be unchanged from baseline (the cheap "tag-only"
    // alternative this issue rejected).
    fn last_loudness(c: &Client, dur_ms: u64) -> Value {
        let r = c.call(json!({"cmd": "monitor_spectrum", "freq_hz": 1000.0}));
        assert_eq!(r["ok"], json!(true));
        let deadline = Instant::now() + Duration::from_millis(dur_ms);
        let mut last: Option<Value> = None;
        while Instant::now() < deadline {
            let remaining = deadline
                .saturating_duration_since(Instant::now())
                .as_millis() as i32;
            match c.recv_pub(remaining.max(1)) {
                Some((t, v))
                    if t == "data"
                        && v["type"] == json!("measurement/loudness")
                        && v["momentary_lkfs"].is_f64() =>
                {
                    last = Some(v);
                }
                Some(_) => continue,
                None => break,
            }
        }
        let _ = c.call(json!({"cmd": "stop"}));
        // Drain trailing frames.
        let drain = Instant::now() + Duration::from_millis(300);
        while Instant::now() < drain {
            if c.recv_pub(50).is_none() {
                break;
            }
        }
        last.expect("no measurement/loudness frame with momentary_lkfs in window")
    }

    let d = Daemon::spawn();
    let c = Client::new(&d);

    // Baseline — no curve loaded.
    let baseline = last_loudness(&c, 1500);
    let baseline_lkfs = baseline["momentary_lkfs"].as_f64().unwrap();
    assert_eq!(
        baseline["mic_correction"],
        json!("none"),
        "baseline tag must be 'none': {baseline}"
    );

    // Load a flat +3 dB mic-curve.
    let mut freqs = Vec::new();
    let mut gains = Vec::new();
    let log_min = 100.0_f64.ln();
    let log_max = 10_000.0_f64.ln();
    for i in 0..24 {
        let t = i as f64 / 23.0;
        freqs.push((log_min + t * (log_max - log_min)).exp());
        gains.push(3.0);
    }
    let r = c.call(json!({
        "cmd":           "calibrate_mic_curve",
        "op":            "set",
        "input_channel": 0,
        "freqs_hz":      freqs,
        "gain_db":       gains,
    }));
    assert_eq!(r["ok"], json!(true));

    // Drain anything that came in between monitor sessions.
    while c.recv_pub(50).is_some() {}

    // With curve loaded → FIR runs before K-weighting → LKFS drops.
    let corrected = last_loudness(&c, 1500);
    let corrected_lkfs = corrected["momentary_lkfs"].as_f64().unwrap();
    assert_eq!(
        corrected["mic_correction"],
        json!("on"),
        "corrected tag must be 'on': {corrected}"
    );

    let delta = baseline_lkfs - corrected_lkfs;
    assert!(
        (delta - 3.0).abs() < 0.5,
        "expected ≈ 3 dB LKFS drop, got Δ={delta:.3} dB \
         (baseline={baseline_lkfs:.2}, corrected={corrected_lkfs:.2})"
    );
    // True-peak shifts the same way (FIR runs before the 4× polyphase
    // oversampler that produces dBTP).
    let baseline_tp = baseline["true_peak_dbtp"].as_f64().unwrap_or(f64::NAN);
    let corrected_tp = corrected["true_peak_dbtp"].as_f64().unwrap_or(f64::NAN);
    if baseline_tp.is_finite() && corrected_tp.is_finite() {
        let tp_delta = baseline_tp - corrected_tp;
        assert!(
            (tp_delta - 3.0).abs() < 0.7,
            "expected ≈ 3 dB true-peak drop, got Δ={tp_delta:.3} dB"
        );
    }
}

#[test]
fn loudness_unchanged_when_mic_correction_toggled_off() {
    // Curve loaded but global toggle off → FIR bypassed, LKFS reads
    // the same as the no-curve baseline. Tag flips to "off".
    let d = Daemon::spawn();
    let c = Client::new(&d);

    // Baseline.
    let r = c.call(json!({"cmd": "monitor_spectrum", "freq_hz": 1000.0}));
    assert_eq!(r["ok"], json!(true));
    let baseline = {
        let deadline = Instant::now() + Duration::from_millis(1500);
        let mut last: Option<Value> = None;
        while Instant::now() < deadline {
            let r = deadline
                .saturating_duration_since(Instant::now())
                .as_millis() as i32;
            match c.recv_pub(r.max(1)) {
                Some((t, v))
                    if t == "data"
                        && v["type"] == json!("measurement/loudness")
                        && v["momentary_lkfs"].is_f64() =>
                {
                    last = Some(v);
                }
                Some(_) => continue,
                None => break,
            }
        }
        let _ = c.call(json!({"cmd": "stop"}));
        let drain = Instant::now() + Duration::from_millis(300);
        while Instant::now() < drain {
            if c.recv_pub(50).is_none() {
                break;
            }
        }
        last.expect("no baseline loudness frame")
    };
    let baseline_lkfs = baseline["momentary_lkfs"].as_f64().unwrap();

    // Load the curve, then disable the toggle.
    let mut freqs = Vec::new();
    let mut gains = Vec::new();
    let log_min = 100.0_f64.ln();
    let log_max = 10_000.0_f64.ln();
    for i in 0..24 {
        let t = i as f64 / 23.0;
        freqs.push((log_min + t * (log_max - log_min)).exp());
        gains.push(3.0);
    }
    let r = c.call(json!({
        "cmd":           "calibrate_mic_curve",
        "op":            "set",
        "input_channel": 0,
        "freqs_hz":      freqs,
        "gain_db":       gains,
    }));
    assert_eq!(r["ok"], json!(true));
    let r = c.call(json!({"cmd": "set_mic_correction_enabled", "enabled": false}));
    assert_eq!(r["ok"], json!(true));
    while c.recv_pub(50).is_some() {}

    // Re-run monitor; FIR is bypassed.
    let r = c.call(json!({"cmd": "monitor_spectrum", "freq_hz": 1000.0}));
    assert_eq!(r["ok"], json!(true));
    let off = {
        let deadline = Instant::now() + Duration::from_millis(1500);
        let mut last: Option<Value> = None;
        while Instant::now() < deadline {
            let r = deadline
                .saturating_duration_since(Instant::now())
                .as_millis() as i32;
            match c.recv_pub(r.max(1)) {
                Some((t, v))
                    if t == "data"
                        && v["type"] == json!("measurement/loudness")
                        && v["momentary_lkfs"].is_f64() =>
                {
                    last = Some(v);
                }
                Some(_) => continue,
                None => break,
            }
        }
        let _ = c.call(json!({"cmd": "stop"}));
        last.expect("no off-mode loudness frame")
    };
    let off_lkfs = off["momentary_lkfs"].as_f64().unwrap();
    assert_eq!(
        off["mic_correction"],
        json!("off"),
        "tag must be 'off' when toggle disables FIR: {off}"
    );
    let delta = (baseline_lkfs - off_lkfs).abs();
    assert!(
        delta < 0.3,
        "FIR should be bypassed: expected LKFS ≈ baseline, Δ={delta:.3} dB"
    );
}

#[test]
fn set_analysis_mode_rejects_garbage() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({"cmd": "set_analysis_mode", "mode": "wavelet-of-doom"}));
    assert_eq!(r["ok"], json!(false));
    let err = r["error"].as_str().unwrap_or("");
    assert!(err.contains("invalid mode"), "got {err}");
}
