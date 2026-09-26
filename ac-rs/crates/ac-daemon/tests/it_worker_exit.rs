//! ZMQ regressions for #432: `transfer_stream` and `monitor_spectrum`
//! publish state before their worker's setup finishes, and that state must
//! be cleared on every way the worker can end — setup failure, panic,
//! explicit stop — not only at the bottom of the normal path.
//!
//! The failure paths are reached through the fake backend's opt-in fault
//! hooks (`audio/fake/hooks.rs`):
//!
//! * `AC_FAKE_ENGINE_OPEN_FAIL_CALLS` — `make_engine`'s fake arm fails.
//!   In a fresh daemon, transfer's launch fills the capture-port cache
//!   (call 0) and the playback-port cache (call 1), opens its plan probe
//!   (call 2), and then opens the worker's engine (call 3).
//! * `AC_FAKE_START_FAIL_CALLS` — `FakeEngine::start` fails. The probe
//!   engine is never started, so the worker's start is call 0 for both
//!   commands.
//! * `AC_FAKE_CAPTURE_PANIC_CALLS` — a fake capture panics. Transfer's
//!   warmup is call 0 and its first loop drain is call 1, after the snapshot
//!   ring is published; monitor's first capture is call 0.
//!
//! Every case that uses a hook also asserts the hook's variable name shows up
//! in the `error` frame or the daemon log. The hooks are indexed by call
//! count, so an index that drifts onto a call that no longer exists would
//! otherwise pass without testing anything.
//!
//! What these do not cover: a panicked worker still publishes no terminal
//! frame (so the panic cases poll state rather than wait for `error`), and
//! a relaunch sent immediately after `error` can see `busy` until the server
//! loop reaps the finished thread, which is why relaunches are retried.

use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

#[path = "common/mod.rs"]
mod common;

use common::{Client, Daemon};

/// How long a test waits for a finished worker's state to be cleared, or for
/// a hook's marker to appear. A deadline, not a delay. Provenance: assumed.
const DEADLINE: Duration = Duration::from_secs(2);

/// Out of range on the fake backend's 20 capture ports.
const OUT_OF_RANGE_CH: u32 = 99;

const NO_TRANSFER: &str = "no transfer_stream session running";
const NO_MONITOR: &str = "no active monitor";

const OPEN_FAIL: &str = "AC_FAKE_ENGINE_OPEN_FAIL_CALLS";
const START_FAIL: &str = "AC_FAKE_START_FAIL_CALLS";
const CAPTURE_PANIC: &str = "AC_FAKE_CAPTURE_PANIC_CALLS";

fn transfer_cmd() -> Value {
    json!({
        "cmd": "transfer_stream", "meas_channel": 0, "ref_channel": 1,
        "weighting": "Z", "integration": "fast",
    })
}

fn monitor_cmd() -> Value {
    json!({"cmd": "monitor_spectrum", "interval": 0.1, "fft_n": 4096})
}

/// `on: false` is never refused for its level, so the only refusal this can
/// draw is "no session".
fn set_drive_off() -> Value {
    json!({"cmd": "set_drive", "on": false, "level_dbfs": -40.0})
}

fn set_monitor_params() -> Value {
    json!({"cmd": "set_monitor_params", "interval": 0.1, "fft_n": 4096})
}

/// Poll `cmd` until it is refused with `error`, or fail at the deadline with
/// the last reply.
fn assert_refused_within(c: &Client, cmd: Value, error: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let r = c.call(cmd.clone());
        if r["ok"] == json!(false) && r["error"] == json!(error) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{} still not refused with {error:?} after {DEADLINE:?}; last reply: {r}\nlog:\n{}",
            cmd["cmd"],
            c.daemon().log_tail()
        );
        thread::sleep(Duration::from_millis(20));
    }
}

fn assert_transfer_state_cleared(c: &Client) {
    assert_refused_within(c, set_drive_off(), NO_TRANSFER);
    assert_refused_within(c, json!({"cmd": "set_delay", "samples": null}), NO_TRANSFER);
    assert_refused_within(c, json!({"cmd": "snapshot"}), NO_TRANSFER);
}

/// The next `error` frame must be this command's and name `marker`.
fn assert_error_frame_names(c: &Client, cmd: &str, marker: &str) {
    let frame = c
        .wait_for_topic("error", Duration::from_secs(5))
        .unwrap_or_else(|| panic!("no error frame; log:\n{}", c.daemon().log_tail()));
    assert_eq!(frame["cmd"], json!(cmd), "{frame}");
    let msg = frame["message"].as_str().unwrap_or_default();
    assert!(
        msg.contains(marker),
        "error frame should come from the {marker} hook, got: {frame}"
    );
}

/// A panic publishes nothing, so the daemon log is the only proof the hook
/// fired.
fn assert_log_names_within(d: &Daemon, marker: &str) {
    let deadline = Instant::now() + DEADLINE;
    while !d.log().contains(marker) {
        assert!(
            Instant::now() < deadline,
            "daemon log never named {marker}:\n{}",
            d.log_tail()
        );
        thread::sleep(Duration::from_millis(20));
    }
}

/// A second launch of `cmd` is accepted once the failed worker is reaped —
/// the busy guard does not hold a dead session forever.
fn assert_relaunch_accepted(c: &Client, cmd: Value) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let r = c.call(cmd.clone());
        if r["ok"] == json!(true) {
            let _ = c.call(json!({"cmd": "stop"}));
            return;
        }
        assert!(
            Instant::now() < deadline,
            "relaunch of {} still refused after {DEADLINE:?}: {r}",
            cmd["cmd"]
        );
        thread::sleep(Duration::from_millis(20));
    }
}

// ---------------------------------------------------------------------
// transfer_stream
// ---------------------------------------------------------------------

#[test]
fn transfer_engine_open_failure_clears_session_state() {
    let d = Daemon::spawn_with_env(&[(OPEN_FAIL, "3")]);
    let c = Client::new(&d);
    let r = c.call(transfer_cmd());
    assert_eq!(r["ok"], json!(true), "{r}");

    assert_error_frame_names(&c, "transfer_stream", OPEN_FAIL);
    assert_transfer_state_cleared(&c);
    assert_relaunch_accepted(&c, transfer_cmd());
}

#[test]
fn transfer_start_failure_clears_session_state() {
    let d = Daemon::spawn_with_env(&[(START_FAIL, "0")]);
    let c = Client::new(&d);
    let r = c.call(transfer_cmd());
    assert_eq!(r["ok"], json!(true), "{r}");

    assert_error_frame_names(&c, "transfer_stream", START_FAIL);
    assert_transfer_state_cleared(&c);
    assert_relaunch_accepted(&c, transfer_cmd());
}

#[test]
fn transfer_worker_panic_clears_session_state() {
    let d = Daemon::spawn_with_env(&[(CAPTURE_PANIC, "1")]);
    let c = Client::new(&d);
    let r = c.call(transfer_cmd());
    assert_eq!(r["ok"], json!(true), "{r}");

    assert_log_names_within(&d, CAPTURE_PANIC);
    assert_transfer_state_cleared(&c);
    // The daemon survived the worker's panic and still accepts work.
    assert_relaunch_accepted(&c, transfer_cmd());
}

/// Green before #432 too: input resolution runs before anything is
/// published. Pins that ordering.
#[test]
fn transfer_input_resolution_refusal_publishes_nothing() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd": "transfer_stream", "meas_channel": OUT_OF_RANGE_CH, "ref_channel": 1,
        "weighting": "Z", "integration": "fast",
    }));
    assert_eq!(r["ok"], json!(false), "{r}");

    assert_transfer_state_cleared(&c);
}

/// Green before #432 too: pins that the guard kept explicit stop clearing
/// the session's state.
#[test]
fn transfer_explicit_stop_clears_session_state() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    assert_eq!(c.call(transfer_cmd())["ok"], json!(true));
    let r = c.call(set_drive_off());
    assert_eq!(
        r["ok"],
        json!(true),
        "live session must accept set_drive: {r}"
    );

    assert_eq!(c.call(json!({"cmd": "stop"}))["ok"], json!(true));
    assert_transfer_state_cleared(&c);
}

// ---------------------------------------------------------------------
// monitor_spectrum
// ---------------------------------------------------------------------

#[test]
fn monitor_start_failure_clears_active() {
    let d = Daemon::spawn_with_env(&[(START_FAIL, "0")]);
    let c = Client::new(&d);
    let r = c.call(monitor_cmd());
    assert_eq!(r["ok"], json!(true), "{r}");

    assert_error_frame_names(&c, "monitor_spectrum", START_FAIL);
    assert_refused_within(&c, set_monitor_params(), NO_MONITOR);
}

#[test]
fn monitor_worker_panic_clears_active() {
    let d = Daemon::spawn_with_env(&[(CAPTURE_PANIC, "0")]);
    let c = Client::new(&d);
    let r = c.call(monitor_cmd());
    assert_eq!(r["ok"], json!(true), "{r}");

    assert_log_names_within(&d, CAPTURE_PANIC);
    assert_refused_within(&c, set_monitor_params(), NO_MONITOR);
}

/// Green before #432 too: input resolution runs before `active` is set.
#[test]
fn monitor_input_resolution_refusal_leaves_monitor_inactive() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({"cmd": "monitor_spectrum", "channels": [OUT_OF_RANGE_CH]}));
    assert_eq!(r["ok"], json!(false), "{r}");

    assert_refused_within(&c, set_monitor_params(), NO_MONITOR);
}

/// Green before #432 too: pins that the guard kept explicit stop clearing
/// `active`.
#[test]
fn monitor_explicit_stop_clears_active() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    assert_eq!(c.call(monitor_cmd())["ok"], json!(true));
    let r = c.call(set_monitor_params());
    assert_eq!(r["ok"], json!(true), "live monitor must accept params: {r}");

    assert_eq!(c.call(json!({"cmd": "stop"}))["ok"], json!(true));
    assert_refused_within(&c, set_monitor_params(), NO_MONITOR);
}
