//! `calibrate_mic_curve` — uploading and clearing a response curve.

use crate::common::{Client, Daemon};
use serde_json::json;

#[test]
fn calibrate_mic_curve_set_then_clear() {
    // End-to-end: upload a synthetic curve, verify cal entry is written,
    // verify the `loaded` count comes back; then `op = clear` and verify
    // the count drops to zero.
    let d = Daemon::spawn();
    let c = Client::new(&d);

    // Synthetic 32-point curve, log-spaced 100..10k Hz, +0..+3 dB ramp.
    let mut freqs = Vec::with_capacity(32);
    let mut gains = Vec::with_capacity(32);
    let log_min = 100.0_f64.ln();
    let log_max = 10_000.0_f64.ln();
    for i in 0..32 {
        let t = i as f64 / 31.0;
        freqs.push((log_min + t * (log_max - log_min)).exp());
        gains.push(3.0 * t);
    }

    let r = c.call(json!({
        "cmd":           "calibrate_mic_curve",
        "op":            "set",
        "input_channel": 1,
        "freqs_hz":      freqs,
        "gain_db":       gains,
        "source_path":   "/tmp/synthetic.frd",
    }));
    assert_eq!(r["ok"], json!(true), "set failed: {r}");
    assert_eq!(r["loaded"], json!(32));
    assert!(r["key"].as_str().unwrap_or("").contains("_in1"));

    // Sparse curve: should be rejected (under MIN_POINTS).
    let r = c.call(json!({
        "cmd":           "calibrate_mic_curve",
        "op":            "set",
        "input_channel": 1,
        "freqs_hz":      [100.0, 200.0, 300.0],
        "gain_db":       [0.0, 0.5, 1.0],
    }));
    assert_eq!(r["ok"], json!(false));
    assert!(
        r["error"].as_str().unwrap_or("").contains("too sparse"),
        "{r}"
    );

    // Clear.
    let r = c.call(json!({
        "cmd":           "calibrate_mic_curve",
        "op":            "clear",
        "input_channel": 1,
    }));
    assert_eq!(r["ok"], json!(true));
    assert_eq!(r["loaded"], json!(0));
}
