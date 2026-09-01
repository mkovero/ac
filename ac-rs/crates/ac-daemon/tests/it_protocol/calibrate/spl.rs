//! `calibrate_spl` — the pistonphone reference capture.

use super::reply_vrms;
use crate::common::{Client, Daemon};
use serde_json::json;
use std::time::Duration;

// ---------------------------------------------------------------------------
// server_enable / server_disable — toggle listen_mode between local and
// public and check the reported bind_addr. #52.
// ---------------------------------------------------------------------------

#[test]
fn calibrate_spl_records_capture_dbfs() {
    // End-to-end SPL cal flow:
    //   1. send `calibrate_spl`,
    //   2. respond to `cal_prompt` (any reply ⇒ proceed),
    //   3. wait for `cal_done` carrying `mic_sensitivity_dbfs_at_94db_spl`.
    //
    // The fake backend's `capture_block` returns a 0.1-amplitude sine, so
    // the captured RMS ≈ 0.0707 → ≈ -23 dBFS. Verify the cal_done payload
    // sits in that range (±2 dB headroom for the second-harmonic tracer
    // the fake adds and rounding).
    let d = Daemon::spawn();
    let c = Client::new(&d);

    // Tell the daemon which channel to probe; pick something non-zero so
    // a regression that drops the field would show up as wrong-key writes.
    let r = c.call(json!({
        "cmd":           "calibrate_spl",
        "input_channel": 2,
        "capture_s":     0.2,
    }));
    assert_eq!(r["ok"], json!(true), "calibrate_spl ack: {r}");

    // Wait for the prompt, then release the worker.
    let prompt = c
        .wait_for_topic("cal_prompt", Duration::from_secs(3))
        .expect("no cal_prompt within 3 s");
    assert_eq!(prompt["kind"], json!("spl"), "prompt kind: {prompt}");

    let r = reply_vrms(&c, None);
    assert_eq!(r["ok"], json!(true));

    let done = c
        .wait_for_topic("cal_done", Duration::from_secs(5))
        .expect("no cal_done within 5 s");
    let dbfs = done["mic_sensitivity_dbfs_at_94db_spl"]
        .as_f64()
        .expect("dbfs field missing");
    assert!(
        (-26.0..-19.0).contains(&dbfs),
        "captured dBFS {dbfs} outside fake-backend window",
    );
    assert!(done["key"].as_str().unwrap_or("").contains("_in2"));
}
