use serde_json::json;
use serde_json::Value;

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

#[test]
fn named_stop_does_not_claim_silence_while_output_remains() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let generate = c.call(json!({
        "cmd": "generate",
        "freq_hz": 1000.0,
        "level_dbfs": -40.0,
    }));
    assert_eq!(generate["ok"], json!(true), "generate rejected: {generate}");
    let monitor = c.call(json!({
        "cmd": "monitor_spectrum",
        "interval": 0.2,
        "fft_n": 8192,
    }));
    assert_eq!(monitor["ok"], json!(true), "monitor rejected: {monitor}");

    let stopped_monitor = c.call(json!({"cmd": "stop", "name": "monitor_spectrum"}));
    assert_eq!(
        stopped_monitor["stopped"],
        json!(["monitor_spectrum"]),
        "{stopped_monitor}"
    );
    assert!(
        stopped_monitor.get("stimulus").is_none(),
        "generate still drives output, so silence must not be attested: {stopped_monitor}"
    );

    let stopped_generate = c.call(json!({"cmd": "stop", "name": "generate"}));
    assert_eq!(stopped_generate["stopped"], json!(["generate"]));
    assert_eq!(stopped_generate["stimulus"], json!("silent"));
}

fn assert_budget_rejection(c: &Client<'_>, request: Value, field: &str) {
    let reply = c.call(request);
    assert_eq!(reply["ok"], json!(false), "request must fail: {reply}");
    let error = reply["error"].as_str().unwrap_or_default();
    assert!(error.contains(field), "error must name {field}: {error:?}");
    assert!(
        error.contains("not started") && error.contains("stimulus  silent"),
        "rejection must confirm no audio was emitted: {error:?}"
    );
    let status = c.call(json!({"cmd": "status"}));
    assert_eq!(status["busy"], json!(false), "worker spawned: {status}");
}

#[test]
fn plot_family_rejects_resource_budgets_before_spawn() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    assert_budget_rejection(
        &c,
        json!({"cmd":"plot", "start_hz":100.0, "stop_hz":1000.0, "ppd":10001}),
        "point",
    );
    assert_budget_rejection(&c, json!({"cmd":"plot", "duration":60.001}), "duration");
    assert_budget_rejection(&c, json!({"cmd":"plot_level", "steps":10001}), "steps");
    assert_budget_rejection(&c, json!({"cmd":"plot_level", "duration":0.0}), "duration");
    assert_budget_rejection(&c, json!({"cmd":"plot_ir", "tail_s":60.001}), "tail_s");
    assert_budget_rejection(
        &c,
        json!({"cmd":"plot_ir", "n_harmonics":33}),
        "n_harmonics",
    );
    assert_budget_rejection(
        &c,
        json!({"cmd":"plot_ir", "window_len":1048577}),
        "window_len",
    );
    assert_budget_rejection(&c, json!({"cmd":"plot_ir", "duration":"NaN"}), "duration");
    assert_budget_rejection(&c, json!({"cmd":"plot", "ppd":u64::MAX}), "point");
}

// ---------------------------------------------------------------------------
// Multi-time-window ladder (handoff-mtw-live-spectrum)
// ---------------------------------------------------------------------------
