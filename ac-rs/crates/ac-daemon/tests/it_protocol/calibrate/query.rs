//! `get_calibration` / `list_calibrations` — what the three layers look
//! like coming back out.

use super::reply_vrms;
use crate::common::{Client, Daemon};
use serde_json::json;
use std::fs;
use std::time::Duration;

#[test]
fn get_and_list_calibrations_return_all_three_layers() {
    // After loading voltage cal (via the existing `calibrate` fake-mode
    // path is awkward — easier to just inject via `calibrate_spl` +
    // `calibrate_mic_curve` which write their own fields), `get_calibration`
    // and `list_calibrations` must return the SPL field and the
    // mic_response provenance, matching the schema documented in ZMQ.md.
    let d = Daemon::spawn();
    let c = Client::new(&d);

    // SPL cal — spawn worker, prompt arrives, we reply, daemon captures.
    let r = c.call(json!({
        "cmd": "calibrate_spl",
        "input_channel": 0,
        "capture_s": 0.1,
    }));
    assert_eq!(r["ok"], json!(true));
    let _ = c
        .wait_for_topic("cal_prompt", Duration::from_secs(3))
        .expect("cal_prompt");
    reply_vrms(&c, None);
    let _ = c
        .wait_for_topic("cal_done", Duration::from_secs(5))
        .expect("cal_done");

    // Mic-curve — synthetic 24-point curve.
    let mut freqs = Vec::new();
    let mut gains = Vec::new();
    let log_min = 100.0_f64.ln();
    let log_max = 10_000.0_f64.ln();
    for i in 0..24 {
        let t = i as f64 / 23.0;
        freqs.push((log_min + t * (log_max - log_min)).exp());
        gains.push(2.5 * t);
    }
    let r = c.call(json!({
        "cmd":           "calibrate_mic_curve",
        "op":            "set",
        "input_channel": 0,
        "freqs_hz":      freqs,
        "gain_db":       gains,
        "source_path":   "/tmp/synthetic.frd",
    }));
    assert_eq!(r["ok"], json!(true));

    // get_calibration must surface both new fields.
    let r = c.call(json!({"cmd": "get_calibration", "input_channel": 0}));
    assert_eq!(r["ok"], json!(true));
    assert_eq!(r["found"], json!(true));
    assert!(
        r["mic_sensitivity_dbfs_at_94db_spl"].is_f64(),
        "missing or wrong-typed mic_sensitivity_dbfs_at_94db_spl in: {r}"
    );
    let mr = r["mic_response"].as_object().expect("mic_response object");
    assert_eq!(mr["freqs_hz"].as_array().unwrap().len(), 24);
    assert_eq!(mr["gain_db"].as_array().unwrap().len(), 24);
    assert_eq!(mr["source_path"], json!("/tmp/synthetic.frd"));
    assert!(mr["imported_at"].is_string());

    // list_calibrations must surface them too — find the entry we just wrote.
    let r = c.call(json!({"cmd": "list_calibrations"}));
    assert_eq!(r["ok"], json!(true));
    let cals = r["calibrations"].as_array().expect("calibrations array");
    let entry = cals
        .iter()
        .find(|e| e["key"].as_str() == Some("out0_in0"))
        .expect("out0_in0 entry not in list");
    assert!(
        entry["mic_sensitivity_dbfs_at_94db_spl"].is_f64(),
        "list_calibrations entry missing mic_sensitivity field: {entry}"
    );
    assert!(
        entry["mic_response"].is_object(),
        "list_calibrations entry missing mic_response: {entry}"
    );
}

/// #297: `get_calibration` / `list_calibrations` must surface `tau_history`
/// — before this issue neither reply carried the field at all, so no client
/// could show it. Seed a key with two τ entries and a second key with none,
/// and assert both the present-array and the absent-array (`[]`) shapes
/// round-trip over the wire per ZMQ.md.
#[test]
fn get_and_list_calibrations_carry_tau_history() {
    let d = Daemon::spawn();
    let cal_path = d.home.join(".config").join("ac").join("cal.json");
    let seeded = json!({
        "out0_in0": {
            "output_channel":                   0,
            "input_channel":                    0,
            "ref_freq":                         1000.0,
            "vrms_at_0dbfs_out":                null,
            "vrms_at_0dbfs_in":                 null,
            "ref_dbfs":                         -10.0,
            "mic_sensitivity_dbfs_at_94db_spl": null,
            "mic_response":                     null,
            "tau_history": [
                {
                    "conditions": {
                        "device":      0,
                        "backend":     "jack",
                        "sample_rate": 48000,
                        "period_size": 1024,
                        "output_port": "system:playback_1",
                        "input_port":  "system:capture_2"
                    },
                    "tau_s":       0.0011931,
                    "measured_at": "2026-08-01T00:00:00Z",
                    "method":      "farina_short_ess"
                },
                {
                    "conditions": {
                        "device":      0,
                        "backend":     "jack",
                        "sample_rate": 48000,
                        "period_size": 128,
                        "output_port": "system:playback_1",
                        "input_port":  "system:capture_2"
                    },
                    "tau_s":       0.0025,
                    "measured_at": "2026-08-15T09:12:03Z",
                    "method":      "farina_short_ess"
                }
            ]
        },
        "out1_in1": {
            "output_channel":                   1,
            "input_channel":                    1,
            "ref_freq":                         1000.0,
            "vrms_at_0dbfs_out":                null,
            "vrms_at_0dbfs_in":                 null,
            "ref_dbfs":                         -10.0,
            "mic_sensitivity_dbfs_at_94db_spl": null,
            "mic_response":                     null,
            "tau_history":                      []
        }
    });
    fs::write(&cal_path, serde_json::to_vec_pretty(&seeded).unwrap()).expect("seed cal.json");

    let c = Client::new(&d);

    // get_calibration on the key with history: both entries present.
    let r = c.call(json!({"cmd": "get_calibration", "output_channel": 0, "input_channel": 0}));
    assert_eq!(r["ok"], json!(true));
    let history = r["tau_history"].as_array().expect("tau_history array");
    assert_eq!(history.len(), 2, "wire reply: {r}");
    assert_eq!(history[1]["tau_s"], json!(0.0025));
    assert_eq!(
        history[1]["conditions"]["period_size"],
        json!(128),
        "conditions must round-trip: {r}"
    );

    // get_calibration on the key with no history: empty array, not absent.
    let r = c.call(json!({"cmd": "get_calibration", "output_channel": 1, "input_channel": 1}));
    assert_eq!(r["ok"], json!(true));
    assert_eq!(
        r["tau_history"],
        json!([]),
        "unmeasured key must reply with [], not omit the field: {r}"
    );

    // list_calibrations must carry the same shape for both keys.
    let r = c.call(json!({"cmd": "list_calibrations"}));
    assert_eq!(r["ok"], json!(true));
    let cals = r["calibrations"].as_array().expect("calibrations array");
    let with_history = cals
        .iter()
        .find(|e| e["key"].as_str() == Some("out0_in0"))
        .expect("out0_in0 entry not in list");
    assert_eq!(
        with_history["tau_history"].as_array().map(|a| a.len()),
        Some(2),
        "list_calibrations: {with_history}"
    );
    let without_history = cals
        .iter()
        .find(|e| e["key"].as_str() == Some("out1_in1"))
        .expect("out1_in1 entry not in list");
    assert_eq!(
        without_history["tau_history"],
        json!([]),
        "list_calibrations: {without_history}"
    );
}
