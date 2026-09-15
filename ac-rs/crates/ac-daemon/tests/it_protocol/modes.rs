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

// ---------------------------------------------------------------------------
// Loudness settle helper — #446.
//
// The two tests below used to collect `measurement/loudness` frames for a
// fixed 1500 ms wall-clock window and assert on the *last* one seen. Under
// CPU contention that window can end before even the first momentary-bearing
// frame arrives (each `--fake-audio` tick advances the stream by exactly
// `interval` regardless of how long the tick's own work takes — see
// `src/audio/fake/mod.rs` — but wall time per tick grows with load), so the
// test went red on load rather than on a defect.
//
// `LoudnessState::momentary()` (`ac-core/src/measurement/loudness/state.rs`)
// is `-inf` until 4 x 100 ms tiles have been pushed, so the first
// momentary-bearing frame is always tick 2 of the session — a stream-time
// fact, not a wall-clock one. The helper below waits on that stream
// progress instead of a deadline: it returns the momentary-bearing frame
// `loudness_settle_ticks()` frames after the first one (so the returned 400 ms
// block sits after the first block, clear of the 512-tap FIR start-up), and
// only uses wall time as a hang guard.

/// `monitor_spectrum` interval used by the loudness tests, pinned explicitly
/// in the request rather than left to the daemon default — so a future
/// change to that default cannot silently shift which frame gets asserted.
const LOUDNESS_INTERVAL_S: f64 = 0.2;

/// Ticks after the first momentary-bearing frame to wait before asserting.
/// `ceil(400 ms / interval)`: a momentary window is 4 x 100 ms tiles
/// (`MOMENTARY_TILES`, `ac-core/src/measurement/loudness/state.rs`). At the
/// pinned interval this is 2, so the returned frame is the one whose 400 ms
/// block starts after the first block ends.
fn loudness_settle_ticks() -> usize {
    (0.400 / LOUDNESS_INTERVAL_S).ceil() as usize
}

/// Upper bound on how long one session may take to settle. This is a hang
/// guard, not a target: a passing run ends as soon as the settle condition
/// is met, however long or short that takes. 30 s matches the longest
/// existing `it_protocol` guards (`mtw.rs`, `warmup.rs`).
const LOUDNESS_HANG_GUARD: Duration = Duration::from_secs(30);

/// Start `monitor_spectrum`, wait for the momentary-bearing
/// `measurement/loudness` frame `loudness_settle_ticks()` ticks after the
/// first one, stop the session, and wait for its `done` boundary. Returns
/// that frame.
fn wait_for_settled_loudness(c: &Client) -> Value {
    let settle_ticks = loudness_settle_ticks();
    let start = Instant::now();
    let r = c.call(json!({
        "cmd":      "monitor_spectrum",
        "freq_hz":  1000.0,
        "interval": LOUDNESS_INTERVAL_S,
    }));
    assert_eq!(r["ok"], json!(true), "monitor_spectrum ack not ok: {r}");

    let deadline = start + LOUDNESS_HANG_GUARD;
    let mut momentary_seen: Vec<Value> = Vec::new();
    let mut spectrum_frames: u64 = 0;
    let mut null_loudness_frames: u64 = 0;
    let mut first_momentary_wall: Option<Duration> = None;
    let mut first_momentary_tick: Option<u64> = None;

    while Instant::now() < deadline && momentary_seen.len() <= settle_ticks {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "data" && v["type"] == json!("visualize/spectrum") => {
                spectrum_frames += 1;
            }
            Some((t, v)) if t == "data" && v["type"] == json!("measurement/loudness") => {
                if v["momentary_lkfs"].is_f64() {
                    if first_momentary_wall.is_none() {
                        first_momentary_wall = Some(start.elapsed());
                        first_momentary_tick = Some(spectrum_frames);
                    }
                    momentary_seen.push(v);
                } else {
                    null_loudness_frames += 1;
                }
            }
            Some(_) => continue,
            None => break,
        }
    }

    let _ = c.call(json!({"cmd": "stop"}));
    let done = c.wait_for_topic("done", Duration::from_secs(5));
    assert!(
        done.is_some(),
        "no 'done' frame after stop — daemon log:\n{}",
        c.daemon().log_tail()
    );

    assert!(
        !momentary_seen.is_empty(),
        "no momentary_lkfs-bearing measurement/loudness frame within {LOUDNESS_HANG_GUARD:?}: \
         saw {spectrum_frames} visualize/spectrum frame(s) and {null_loudness_frames} \
         measurement/loudness frame(s) with momentary_lkfs: null. daemon log:\n{}",
        c.daemon().log_tail()
    );
    assert!(
        momentary_seen.len() > settle_ticks,
        "only {} momentary_lkfs frame(s) within {LOUDNESS_HANG_GUARD:?}, need {} \
         (first + {settle_ticks} settle tick(s)). daemon log:\n{}",
        momentary_seen.len(),
        settle_ticks + 1,
        c.daemon().log_tail()
    );

    eprintln!(
        "loudness settle: first momentary frame at {:?} wall, visualize/spectrum tick {} \
         (interval={LOUDNESS_INTERVAL_S}s, M={settle_ticks})",
        first_momentary_wall.unwrap(),
        first_momentary_tick.unwrap()
    );

    momentary_seen.into_iter().nth(settle_ticks).unwrap()
}

#[test]
fn loudness_lkfs_drops_by_curve_db_when_mic_correction_on() {
    // #104 (Phase 6): with the per-sample inverse-curve FIR running
    // BEFORE K-weighting, a flat +3 dB mic-curve attenuates the audio
    // by 3 dB → LKFS / true_peak drop by 3 dB. Without the FIR the
    // LKFS would be unchanged from baseline (the cheap "tag-only"
    // alternative this issue rejected).
    let d = Daemon::spawn();
    let c = Client::new(&d);

    // Baseline — no curve loaded.
    let baseline = wait_for_settled_loudness(&c);
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

    // With curve loaded → FIR runs before K-weighting → LKFS drops.
    let corrected = wait_for_settled_loudness(&c);
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
    let baseline = wait_for_settled_loudness(&c);
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

    // Re-run monitor; FIR is bypassed.
    let off = wait_for_settled_loudness(&c);
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
