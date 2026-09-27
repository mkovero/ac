//! `test_dut`'s `done` frame says when a `stop` cut the suite short (#619).
//!
//! `tests_run` counts only the checks that ran, so without `stopped` a
//! suite stopped after passing checks is indistinguishable from a complete
//! one, and `ac test dut` would exit 0 on it.

use std::time::Duration;

use serde_json::json;

#[path = "common/mod.rs"]
mod common;

use common::{Client, Daemon};

#[test]
fn a_stop_mid_suite_is_reported_in_done() {
    let d = Daemon::spawn_with_config(Some(json!({"reference_channel": 1})));
    let c = Client::new(&d);
    let ack = c.call(json!({"cmd": "test_dut"}));
    assert_eq!(ack["ok"], true, "{ack}");

    let first = c
        .wait_for_topic("data", Duration::from_secs(60))
        .expect("first test_result");
    assert_eq!(first["type"], "test_result", "{first}");
    let stop = c.call(json!({"cmd": "stop", "name": "test_dut"}));
    assert_eq!(stop["ok"], true, "{stop}");

    let done = c
        .wait_for_topic("done", Duration::from_secs(60))
        .expect("done after stop");
    assert_eq!(done["cmd"], "test_dut", "{done}");
    assert_eq!(done["stopped"], true, "{done}");
    assert_eq!(done["bypass_confirmed"], false, "{done}");
    let run = done["tests_run"].as_u64().expect("tests_run");
    assert!(
        (1..5).contains(&run),
        "a stopped suite ran {run} of 5: {done}"
    );
}
