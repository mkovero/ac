use serde_json::json;
use serde_json::Value;
use std::time::{Duration, Instant};

use crate::common::{Client, Daemon};

/// The fake backend exposes 20 playback and 20 capture ports (indices 0..19),
/// so this index cannot resolve on any backend under test.
const OUT_OF_RANGE_CH: u32 = 99;

/// Config with an out-of-range channel. `*_port` is left unset so resolution
/// falls through to the index path — the sticky-name path was never affected.
fn cfg_with_channel(key: &str, ch: u32) -> Value {
    json!({
        "device": 0,
        "output_channel": if key == "output_channel" { ch } else { 4 },
        "input_channel": if key == "input_channel" { ch } else { 0 },
        "reference_channel": if key == "reference_channel" { ch } else { 3 },
        "dbu_ref_vrms": 0.774_596_67,
        "range_start_hz": 20.0,
        "range_stop_hz": 20_000.0,
        "server_enabled": false,
    })
}

fn assert_out_of_range_error(r: &Value, cmd: &str) {
    assert_eq!(
        r["ok"],
        json!(false),
        "{cmd} must fail on an out-of-range channel, replied: {r}"
    );
    let err = r["error"].as_str().unwrap_or_default();
    assert!(
        err.contains("out of range") || err.contains("no physical"),
        "{cmd} error should say the channel is out of range, got: {err:?}"
    );
    // The operator's next question is "then what should I have said?" — the
    // available ports must be named, and the fabricated fallbacks must not
    // appear anywhere in the reply.
    assert!(
        err.contains("fake:"),
        "{cmd} error should list the available ports, got: {err:?}"
    );
    let whole = r.to_string();
    assert!(
        !whole.contains("system:playback_1") && !whole.contains("system:capture_1"),
        "{cmd} reply must not contain a fabricated port name: {whole}"
    );
}

/// **The drive-path case from #206.** A mistyped `output_channel` used to
/// silently retarget the stimulus to `system:playback_1` — noise leaving an
/// output the operator did not choose. It must refuse instead.
#[test]
fn generate_refuses_an_out_of_range_output_channel() {
    let d = Daemon::spawn_with_config(Some(cfg_with_channel("output_channel", OUT_OF_RANGE_CH)));
    let c = Client::new(&d);
    let r = c.call(json!({"cmd":"generate","freq_hz":1000.0,"level_dbfs":-40.0}));
    assert_out_of_range_error(&r, "generate");

    // And nothing was started: the busy guard must still be clear.
    let s = c.call(json!({"cmd":"status"}));
    assert_eq!(
        s["busy"],
        json!(false),
        "a refused generate must not leave a worker running: {s}"
    );
}

#[test]
fn transfer_stream_refuses_an_out_of_range_output_channel() {
    let d = Daemon::spawn_with_config(Some(cfg_with_channel("output_channel", OUT_OF_RANGE_CH)));
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd": "transfer_stream", "pairs": [[0, 1]], "drivable": true
    }));
    assert_out_of_range_error(&r, "transfer_stream");
}

#[test]
fn monitor_spectrum_refuses_an_out_of_range_input_channel() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    // Explicit `channels` goes through the same resolution path.
    let r = c.call(json!({"cmd":"monitor_spectrum","channels":[OUT_OF_RANGE_CH]}));
    assert_out_of_range_error(&r, "monitor_spectrum");
}

#[test]
fn plot_refuses_an_out_of_range_input_channel() {
    let d = Daemon::spawn_with_config(Some(cfg_with_channel("input_channel", OUT_OF_RANGE_CH)));
    let c = Client::new(&d);
    let r = c.call(json!({"cmd":"plot","freq_start":100.0,"freq_stop":1000.0,"ppd":2}));
    assert_out_of_range_error(&r, "plot");
}

#[test]
fn sweep_refuses_an_out_of_range_output_channel() {
    let d = Daemon::spawn_with_config(Some(cfg_with_channel("output_channel", OUT_OF_RANGE_CH)));
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd":"sweep_frequency","freq_start":100.0,"freq_stop":1000.0,"level_dbfs":-40.0
    }));
    assert_out_of_range_error(&r, "sweep_frequency");
}

/// A configured-but-missing *reference* channel used to present as "no
/// reference": `resolve_ref_input` returned `None` for both "not configured"
/// and "out of range", so the measurement ran single-ended while the operator
/// believed a reference was wired in.
#[test]
fn test_dut_refuses_an_out_of_range_reference_channel() {
    let d = Daemon::spawn_with_config(Some(cfg_with_channel("reference_channel", OUT_OF_RANGE_CH)));
    let c = Client::new(&d);
    let r = c.call(json!({"cmd":"test_dut","level_dbfs":-40.0}));
    assert_out_of_range_error(&r, "test_dut");
}

/// The sticky-name path is unaffected: an explicit `*_port` bypasses index
/// resolution entirely and must keep working even when the channel index
/// alongside it is nonsense.
#[test]
fn explicit_sticky_port_still_bypasses_channel_resolution() {
    let mut cfg = cfg_with_channel("output_channel", OUT_OF_RANGE_CH);
    cfg["output_port"] = json!("fake:playback_2");
    let d = Daemon::spawn_with_config(Some(cfg));
    let c = Client::new(&d);
    let r = c.call(json!({"cmd":"generate","freq_hz":1000.0,"level_dbfs":-40.0}));
    assert_eq!(
        r["ok"],
        json!(true),
        "an explicit output_port must still be honoured: {r}"
    );
    c.call(json!({"cmd":"stop"}));
}

/// In-range channels must be entirely unaffected — the fix must not have made
/// a working configuration fail.
#[test]
fn in_range_channels_are_unaffected() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({"cmd":"generate","freq_hz":1000.0,"level_dbfs":-40.0,"channels":[2]}));
    assert_eq!(r["ok"], json!(true), "in-range generate must work: {r}");
    c.call(json!({"cmd":"stop"}));
}

/// #428: `duration: 0` makes `plot`'s per-point capture length
/// `max(duration, 3.0 / freq)`, which falls under `analyze`'s 256-sample
/// minimum for any point above 562.5 Hz at the fake backend's 48 kHz rate
/// (`3.0 / 562.5 * 48_000 == 256`). A sweep that fails partway through
/// must not archive the successful prefix as a complete measurement: it
/// must terminate on `error`, carrying how much of the request actually
/// completed, and never reach `done` or `measurement/report` for this run.
#[test]
fn plot_duration_zero_fails_atomically_instead_of_archiving_partial_sweep() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd":        "plot",
        "start_hz":   100.0,
        "stop_hz":    2000.0,
        "level_dbfs": -20.0,
        "ppd":        5,
        "duration":   0.0,
    }));
    assert_eq!(r["ok"], json!(true), "plot ack: {r}");

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut error_frame: Option<Value> = None;
    while Instant::now() < deadline && error_frame.is_none() {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "error" => error_frame = Some(v),
            Some((t, _)) if t == "done" => {
                panic!("a sweep that failed partway through must not reach done")
            }
            Some((_, v)) if v["type"] == json!("measurement/report") => panic!(
                "a sweep that failed partway through must not archive a measurement/report: {v}"
            ),
            Some(_) => continue,
            None => break,
        }
    }
    let err = error_frame.expect("plot never published a terminal error");
    assert_eq!(err["cmd"], json!("plot"), "frame: {err}");
    let message = err["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("256"),
        "error message should name the analyzer's 256-sample minimum, got: {message:?}"
    );
    let requested = err["requested_points"]
        .as_u64()
        .expect("requested_points missing");
    let completed = err["completed_points"]
        .as_u64()
        .expect("completed_points missing");
    assert!(
        completed < requested,
        "completed_points ({completed}) should be less than requested_points \
         ({requested}) — some points at/below 562.5 Hz must have succeeded \
         before the failure: {err}"
    );
    assert!(
        completed > 0,
        "the low-frequency prefix should have completed: {err}"
    );
}

// ---------------------------------------------------------------------------
// Multi-time-window ladder (handoff-mtw-live-spectrum)
// ---------------------------------------------------------------------------
