//! Config-reload rules a routing command has to honour. `calibrate` is
//! the routing command under test, but the rules are `dispatch`'s: a
//! config edit between two runs must reach the second one, and a config
//! the daemon cannot parse must produce a refusal rather than service
//! against a stale in-memory copy (#370).

use super::run_calibrate_skip_all;
use crate::common::{Client, Daemon};
use serde_json::json;
use std::fs;

/// #370, acceptance criterion 3 (the failing case named in the triage spec):
/// a config.json edit made between two measurements against one long-lived
/// daemon must reach the second one. Before the per-request reload in
/// `dispatch()`, this is exactly the reporter's repro — an auto-spawned
/// daemon outlives the `ac` command that spawned it, so a channel-scan
/// script editing `input_channel` between runs silently re-measured the
/// first channel every time.
#[test]
fn calibrate_picks_up_a_config_edit_made_between_two_runs() {
    let d = Daemon::spawn_with_config(Some(json!({"input_channel": 1})));
    let c = Client::new(&d);

    let done1 = run_calibrate_skip_all(&c, json!({}));
    assert_eq!(
        done1["input_port"],
        json!("fake:capture_1"),
        "first run: {done1}"
    );

    // Same daemon process, no restart — just the config file changing
    // underneath it, exactly as an operator's editor would.
    let cfg_path = d.home.join(".config").join("ac").join("config.json");
    fs::write(
        &cfg_path,
        serde_json::to_vec_pretty(&json!({"input_channel": 2})).unwrap(),
    )
    .expect("rewrite config.json");

    let done2 = run_calibrate_skip_all(&c, json!({}));
    assert_eq!(
        done2["input_port"],
        json!("fake:capture_2"),
        "second run: {done2}"
    );
    assert_ne!(
        done1["input_port"], done2["input_port"],
        "config edit between runs must change the resolved input port"
    );
}

/// #370, acceptance criterion 4: where the running daemon cannot serve the
/// current on-disk config (unparseable JSON, e.g. a file caught mid-write),
/// a routing command must say so and refuse rather than silently serving
/// against the last-known-good in-memory config. Non-routing commands
/// (`status`) stay reachable so the operator can tell what's wrong without
/// a restart.
#[test]
fn routing_command_refuses_when_config_json_is_unparseable() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let cfg_path = d.home.join(".config").join("ac").join("config.json");
    fs::write(&cfg_path, b"{ not json").expect("write malformed config.json");

    let r = c.call(json!({"cmd": "calibrate"}));
    assert_eq!(r["ok"], json!(false), "expected refusal: {r}");
    let err = r["error"].as_str().unwrap_or("");
    assert!(
        err.contains("config.json"),
        "error should name config.json: {r}"
    );
    // `{e:#}` (not `{e}`) on the reload's Err arm: the reply must carry the
    // actual parse failure, not just the file path — that's what makes the
    // refusal diagnosable rather than merely visible.
    assert!(
        err.contains("line") || err.contains("column") || err.to_lowercase().contains("expected"),
        "error should name *why* config.json failed to parse, not just that it did: {r}"
    );

    let s = c.call(json!({"cmd": "status"}));
    assert_eq!(s["ok"], json!(true), "status must still answer: {s}");
}
