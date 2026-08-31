//! `calibrate`, `calibrate_spl`, `calibrate_mic_curve` and the
//! calibration query commands, over the wire.
//!
//! One file per layer, matching `handlers/calibrate/` on the daemon side:
//! [`voltage`] the two DMM prompts, [`tau`] the interface-latency sweep
//! the same run piggybacks on, [`spl`] the pistonphone reference,
//! [`mic_curve`] the response curve, [`query`] what `get_calibration` /
//! `list_calibrations` report back. [`routing`] covers the config-reload
//! rules that `calibrate` happens to be the routing command for.

mod mic_curve;
mod query;
mod routing;
mod spl;
mod tau;
mod voltage;

use serde_json::json;
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;

use crate::common::{Client, Daemon};

/// Wait for the next `cal_prompt` and assert which step it is.
///
/// Prompts arrive in order, so a test that answers the wrong one would
/// otherwise hang in the *next* `wait_for_topic` with nothing to say
/// about what it actually got.
fn expect_prompt(c: &Client, step: u64) -> Value {
    let p = c
        .wait_for_topic("cal_prompt", Duration::from_secs(5))
        .unwrap_or_else(|| panic!("no cal_prompt for step {step}"));
    assert_eq!(p["step"], json!(step), "wrong prompt step: {p}");
    p
}

/// Answer the pending prompt with a reading, or with `None` to skip it.
///
/// A skip sends `vrms: null`, which since #279 means "I did not measure
/// this" — distinct from [`reply_clear`], and required not to overwrite a
/// stored value.
fn reply_vrms(c: &Client, vrms: Option<f64>) -> Value {
    c.call(json!({"cmd": "cal_reply", "vrms": vrms}))
}

/// Answer the pending prompt with `clear: true` — the only request that
/// erases a stored value, and it has to be asked for by name (#279).
fn reply_clear(c: &Client) -> Value {
    c.call(json!({"cmd": "cal_reply", "clear": true}))
}

/// The run's terminal `cal_done` payload.
fn expect_cal_done(c: &Client) -> Value {
    c.wait_for_topic("cal_done", Duration::from_secs(5))
        .expect("cal_done frame")
}

/// Cancel the run at the pending prompt. The daemon treats `q` as a stop,
/// so nothing from this run reaches disk.
fn reply_cancel(c: &Client) -> Value {
    c.call(json!({"cmd": "stop"}))
}

/// Drive a `calibrate` run to completion, skipping every voltage prompt
/// (`vrms: null`), and return the `cal_done` payload. `cmd` supplies any
/// extra fields (`output_channel` / `input_channel` / `ref_dbfs`) merged
/// into the `calibrate` request.
fn run_calibrate_skip_all(c: &Client, mut cmd: Value) -> Value {
    cmd["cmd"] = json!("calibrate");
    let r = c.call(cmd);
    assert_eq!(r["ok"], json!(true), "calibrate ack: {r}");
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((topic, _)) if topic == "cal_prompt" => {
                let _ = c.call(json!({"cmd":"cal_reply", "vrms": null}));
            }
            Some((topic, payload)) if topic == "cal_done" => return payload,
            Some(_) => continue,
            None => break,
        }
    }
    panic!("calibrate run never reached cal_done");
}

/// Seed a `cal.json` entry with both voltage legs set, at a `ref_dbfs`
/// deliberately different from the one the test's `calibrate` run uses.
fn seed_voltage_cal(d: &Daemon, out_vrms: f64, in_vrms: f64, ref_dbfs: f64) -> PathBuf {
    let path = d.home.join(".config").join("ac").join("cal.json");
    let seeded = json!({
        "out0_in0": {
            "output_channel":                   0,
            "input_channel":                    0,
            "ref_freq":                         1000.0,
            "vrms_at_0dbfs_out":                out_vrms,
            "vrms_at_0dbfs_in":                 in_vrms,
            "ref_dbfs":                         ref_dbfs,
            "mic_sensitivity_dbfs_at_94db_spl": -32.5,
            "mic_response":                     null,
        }
    });
    fs::write(&path, serde_json::to_vec_pretty(&seeded).unwrap()).expect("seed cal.json");
    path
}

fn read_cal_entry(path: &PathBuf) -> Value {
    let raw = fs::read_to_string(path).expect("read cal.json");
    let all: Value = serde_json::from_str(&raw).expect("parse cal.json");
    all["out0_in0"].clone()
}
