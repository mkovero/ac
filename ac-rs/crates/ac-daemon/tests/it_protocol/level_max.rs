//! #459 — the single global emission ceiling, enforced by refusal.
//!
//! Replaces `level_clamp.rs`: #360's clamp-and-report shape is gone, and a
//! level above `ac_core::shared::emission_level::MAX_EMISSION_DBFS`
//! (0.0 dBFS, full scale) now gets `{"ok": false, ...}` with `max_dbfs` on
//! it, never a quietly-lowered success. One test per wire command listed
//! under the design's ZMQ impact item 1, at +0.1 dBFS (refused) and at
//! exactly 0.0 dBFS (accepted) — the shared check arithmetic
//! itself is unit-tested in `ac-core`; this is coverage that each site
//! actually calls it. Red case: put `.min(MAX)` back into any one handler
//! and that handler's own pair of tests fails.

use std::time::Duration;

use serde_json::json;

use crate::common::{Client, Daemon};

const OVER: f64 = 0.1;
const AT_MAX: f64 = 0.0;

fn assert_refused(r: &serde_json::Value) {
    assert_eq!(r["ok"], json!(false), "{r}");
    assert!(
        r["max_dbfs"].as_f64().is_some(),
        "refusal must carry max_dbfs: {r}"
    );
    assert_eq!(r["max_dbfs"], json!(AT_MAX), "{r}");
}

fn assert_accepted_at(r: &serde_json::Value, field: &str) {
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(r[field], json!(AT_MAX), "{r}");
}

#[test]
fn generate_refuses_above_the_maximum_and_accepts_at_it() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let r = c.call(json!({"cmd": "generate", "freq_hz": 1000.0, "level_dbfs": OVER}));
    assert_refused(&r);

    let r = c.call(json!({"cmd": "generate", "freq_hz": 1000.0, "level_dbfs": AT_MAX}));
    assert_accepted_at(&r, "level_dbfs");
    let _ = c.call(json!({"cmd": "stop"}));
}

#[test]
fn generate_accepts_a_typed_level_between_the_old_and_new_maximum() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({"cmd": "generate", "freq_hz": 1000.0, "level_dbfs": -10.0}));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(r["level_dbfs"], json!(-10.0), "{r}");
    let _ = c.call(json!({"cmd": "stop"}));
}

#[test]
fn generate_pink_refuses_above_the_maximum_and_accepts_at_it() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let r = c.call(json!({"cmd": "generate_pink", "level_dbfs": OVER}));
    assert_refused(&r);

    let r = c.call(json!({"cmd": "generate_pink", "level_dbfs": AT_MAX}));
    assert_accepted_at(&r, "level_dbfs");
    let _ = c.call(json!({"cmd": "stop"}));
}

#[test]
fn sweep_frequency_refuses_above_the_maximum_and_accepts_at_it() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let r = c.call(json!({
        "cmd": "sweep_frequency", "start_hz": 100.0, "stop_hz": 200.0,
        "level_dbfs": OVER, "duration": 0.2,
    }));
    assert_refused(&r);

    let r = c.call(json!({
        "cmd": "sweep_frequency", "start_hz": 100.0, "stop_hz": 200.0,
        "level_dbfs": AT_MAX, "duration": 0.2,
    }));
    assert_accepted_at(&r, "level_dbfs");
    let _ = c.wait_for_topic("done", Duration::from_secs(5));
}

#[test]
fn sweep_level_refuses_when_either_endpoint_is_above_the_maximum() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let r = c.call(json!({
        "cmd": "sweep_level", "freq_hz": 1000.0,
        "start_dbfs": -40.0, "stop_dbfs": OVER, "duration": 0.2,
    }));
    assert_refused(&r);

    let r = c.call(json!({
        "cmd": "sweep_level", "freq_hz": 1000.0,
        "start_dbfs": -40.0, "stop_dbfs": AT_MAX, "duration": 0.2,
    }));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(r["start_dbfs"], json!(-40.0), "{r}");
    assert_eq!(r["stop_dbfs"], json!(AT_MAX), "{r}");
    let _ = c.wait_for_topic("done", Duration::from_secs(5));
}

#[test]
fn plot_refuses_above_the_maximum_and_accepts_at_it() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let r = c.call(json!({
        "cmd": "plot", "start_hz": 500.0, "stop_hz": 600.0,
        "level_dbfs": OVER, "ppd": 2, "duration": 0.05,
    }));
    assert_refused(&r);

    let r = c.call(json!({
        "cmd": "plot", "start_hz": 500.0, "stop_hz": 600.0,
        "level_dbfs": AT_MAX, "ppd": 2, "duration": 0.05,
    }));
    assert_accepted_at(&r, "level_dbfs");
    let _ = c.wait_for_topic("done", Duration::from_secs(10));
}

#[test]
fn plot_level_refuses_when_either_endpoint_is_above_the_maximum() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let r = c.call(json!({
        "cmd": "plot_level", "freq_hz": 1000.0,
        "start_dbfs": -40.0, "stop_dbfs": OVER, "steps": 3, "duration": 0.05,
    }));
    assert_refused(&r);

    let r = c.call(json!({
        "cmd": "plot_level", "freq_hz": 1000.0,
        "start_dbfs": -40.0, "stop_dbfs": AT_MAX, "steps": 3, "duration": 0.05,
    }));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(r["start_dbfs"], json!(-40.0), "{r}");
    assert_eq!(r["stop_dbfs"], json!(AT_MAX), "{r}");
    let _ = c.wait_for_topic("done", Duration::from_secs(10));
}

#[test]
fn plot_ir_refuses_above_the_maximum_and_accepts_at_it() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let r = c.call(json!({
        "cmd": "plot_ir", "f1_hz": 500.0, "f2_hz": 1000.0,
        "duration": 0.05, "level_dbfs": OVER,
    }));
    assert_refused(&r);

    let r = c.call(json!({
        "cmd": "plot_ir", "f1_hz": 500.0, "f2_hz": 1000.0,
        "duration": 0.05, "level_dbfs": AT_MAX,
    }));
    assert_accepted_at(&r, "level_dbfs");
    let _ = c.wait_for_topic("done", Duration::from_secs(10));
}

#[test]
fn calibrate_refuses_above_the_maximum_and_accepts_at_it() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let r = c.call(json!({"cmd": "calibrate", "ref_dbfs": OVER}));
    assert_refused(&r);

    let r = c.call(json!({"cmd": "calibrate", "ref_dbfs": AT_MAX}));
    assert_accepted_at(&r, "ref_dbfs");
    let _ = c.call(json!({"cmd": "stop"}));
}

#[test]
fn test_dut_refuses_above_the_maximum_and_accepts_at_it() {
    let d = Daemon::spawn_with_config(Some(json!({"reference_channel": 1})));
    let c = Client::with_ctrl_timeout(&d, 15_000);

    let r = c.call(json!({"cmd": "test_dut", "level_dbfs": OVER}));
    assert_refused(&r);

    let r = c.call(json!({"cmd": "test_dut", "level_dbfs": AT_MAX}));
    assert_accepted_at(&r, "level_dbfs");
    let _ = c.call(json!({"cmd": "stop"}));
}

#[test]
fn transfer_stream_drivable_refuses_above_the_maximum_and_accepts_at_it() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let r = c.call(json!({
        "cmd": "transfer_stream", "pairs": [[0, 1]],
        "drivable": true, "level_dbfs": OVER,
    }));
    assert_refused(&r);

    let r = c.call(json!({
        "cmd": "transfer_stream", "pairs": [[0, 1]],
        "drivable": true, "level_dbfs": AT_MAX,
    }));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(r["max_dbfs"], json!(AT_MAX), "{r}");
    let _ = c.call(json!({"cmd": "stop"}));
}

#[test]
fn set_drive_on_refuses_above_the_maximum_and_accepts_at_it() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let launch = c.call(json!({
        "cmd": "transfer_stream", "pairs": [[0, 1]], "drivable": true,
    }));
    assert_eq!(launch["ok"], json!(true), "{launch}");

    let r = c.call(json!({"cmd": "set_drive", "on": true, "level_dbfs": OVER}));
    assert_refused(&r);

    let r = c.call(json!({"cmd": "set_drive", "on": true, "level_dbfs": AT_MAX}));
    assert_accepted_at(&r, "level_dbfs");

    // `on: false` is never checked — turning drive off must never be the
    // one request a refused level could leave stuck on.
    let r = c.call(json!({"cmd": "set_drive", "on": false, "level_dbfs": 6.0}));
    assert_eq!(r["ok"], json!(true), "{r}");

    let _ = c.call(json!({"cmd": "stop"}));
}

/// The retired `drive_max_dbfs` config key. While present, every emitting
/// command refuses and names the key — the #225 defect class (a value
/// that silently stops doing anything) applies here just as it does to
/// one that does the wrong thing.
#[test]
fn retired_drive_max_dbfs_key_refuses_emission_and_names_itself() {
    let d = Daemon::spawn_with_config(Some(json!({"drive_max_dbfs": -10.0})));
    let c = Client::new(&d);

    let r = c.call(json!({"cmd": "generate", "freq_hz": 1000.0, "level_dbfs": AT_MAX}));
    assert_eq!(r["ok"], json!(false), "{r}");
    let err = r["error"].as_str().unwrap_or_default();
    assert!(
        err.contains("drive_max_dbfs"),
        "error must name the retired key, got {err:?}"
    );
}

#[test]
fn probe_reports_its_fixed_level_and_refuses_the_retired_key() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({"cmd": "probe"}));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(
        r["level_dbfs"],
        json!(ac_core::shared::emission_level::SELF_TEST_LEVEL_DBFS)
    );
    assert_eq!(r["max_dbfs"], json!(AT_MAX));
    let _ = c.call(json!({"cmd": "stop"}));

    let d = Daemon::spawn_with_config(Some(json!({"drive_max_dbfs": -10.0})));
    let c = Client::new(&d);
    let r = c.call(json!({"cmd": "probe"}));
    assert_eq!(r["ok"], json!(false), "{r}");
    assert!(r["error"]
        .as_str()
        .unwrap_or_default()
        .contains("drive_max_dbfs"));
}

#[test]
fn test_hardware_refuses_the_retired_key() {
    let d = Daemon::spawn_with_config(Some(json!({
        "drive_max_dbfs": -10.0,
        "reference_channel": 1,
    })));
    let c = Client::new(&d);
    let r = c.call(json!({"cmd": "test_hardware"}));
    assert_eq!(r["ok"], json!(false), "{r}");
    assert!(r["error"]
        .as_str()
        .unwrap_or_default()
        .contains("drive_max_dbfs"));
}
