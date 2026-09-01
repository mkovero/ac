use serde_json::json;
use std::time::Duration;

use crate::common::{Client, Daemon};

/// The remaining #360 call sites (`generate`, `generate_pink`,
/// `sweep_level`, `sweep_frequency`, `plot`, `plot_level`) all echo the
/// applied level on their sync reply, same as `plot_ir`/`calibrate` above
/// and `set_drive` before them. One clamp-above-ceiling check per command
/// — the shared `apply_drive_ceiling` chokepoint itself is unit-tested in
/// `handlers/mod.rs`, so this is coverage that each site actually calls it,
/// not a re-test of the clamp arithmetic.
#[test]
fn generate_and_generate_pink_clamp_level_to_the_ceiling() {
    const CEILING_DBFS: f64 = -20.0;
    let d = Daemon::spawn_with_config(Some(json!({ "drive_max_dbfs": CEILING_DBFS })));
    let c = Client::new(&d);

    let r = c.call(json!({"cmd": "generate", "freq_hz": 1000.0, "level_dbfs": 6.0}));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(r["level_dbfs"], json!(CEILING_DBFS), "{r}");
    let _ = c.call(json!({"cmd": "stop"}));

    let r = c.call(json!({"cmd": "generate_pink", "level_dbfs": 6.0}));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(r["level_dbfs"], json!(CEILING_DBFS), "{r}");
    let _ = c.call(json!({"cmd": "stop"}));
}

#[test]
fn sweep_level_clamps_each_ramp_point_and_echoes_the_applied_range() {
    const CEILING_DBFS: f64 = -20.0;
    let d = Daemon::spawn_with_config(Some(json!({ "drive_max_dbfs": CEILING_DBFS })));
    let c = Client::new(&d);

    // Entire requested range sits above the ceiling — the degenerate case
    // where the ramp's applied shape collapses to a flat line at the
    // ceiling (UX spec, issue #360).
    let r = c.call(json!({
        "cmd": "sweep_level", "freq_hz": 1000.0,
        "start_dbfs": -10.0, "stop_dbfs": 6.0, "duration": 0.2,
    }));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(r["start_dbfs"], json!(CEILING_DBFS), "{r}");
    assert_eq!(r["stop_dbfs"], json!(CEILING_DBFS), "{r}");
    let _ = c.wait_for_topic("done", Duration::from_secs(5));

    // Partial overlap: only the top end is clamped.
    let r = c.call(json!({
        "cmd": "sweep_level", "freq_hz": 1000.0,
        "start_dbfs": -40.0, "stop_dbfs": -10.0, "duration": 0.2,
    }));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(r["start_dbfs"], json!(-40.0), "{r}");
    assert_eq!(r["stop_dbfs"], json!(CEILING_DBFS), "{r}");
    let _ = c.wait_for_topic("done", Duration::from_secs(5));
}

#[test]
fn sweep_frequency_clamps_level_to_the_ceiling() {
    const CEILING_DBFS: f64 = -20.0;
    let d = Daemon::spawn_with_config(Some(json!({ "drive_max_dbfs": CEILING_DBFS })));
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd": "sweep_frequency", "start_hz": 100.0, "stop_hz": 200.0,
        "level_dbfs": 6.0, "duration": 0.2,
    }));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(r["level_dbfs"], json!(CEILING_DBFS), "{r}");
    let _ = c.wait_for_topic("done", Duration::from_secs(5));
}

#[test]
fn plot_clamps_level_to_the_ceiling() {
    const CEILING_DBFS: f64 = -20.0;
    let d = Daemon::spawn_with_config(Some(json!({ "drive_max_dbfs": CEILING_DBFS })));
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd": "plot", "start_hz": 500.0, "stop_hz": 600.0,
        "level_dbfs": 6.0, "ppd": 2, "duration": 0.05,
    }));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(r["level_dbfs"], json!(CEILING_DBFS), "{r}");
    let _ = c.wait_for_topic("done", Duration::from_secs(10));
}

#[test]
fn plot_level_clamps_the_range_and_echoes_it_applied() {
    const CEILING_DBFS: f64 = -20.0;
    let d = Daemon::spawn_with_config(Some(json!({ "drive_max_dbfs": CEILING_DBFS })));
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd": "plot_level", "freq_hz": 1000.0,
        "start_dbfs": -40.0, "stop_dbfs": -10.0, "steps": 3, "duration": 0.05,
    }));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(r["start_dbfs"], json!(-40.0), "{r}");
    assert_eq!(r["stop_dbfs"], json!(CEILING_DBFS), "{r}");
    let _ = c.wait_for_topic("done", Duration::from_secs(10));
}
