//! Wire values are either exactly valid or refuse the whole request (#431).
//!
//! Every case here was accepted before: an invalid array element was
//! filtered out, an invalid channel list fell back to the configured
//! default, and a value above u32 was narrowed into a different one.

use serde_json::{json, Value};

use crate::common::{Client, Daemon};

/// Seeded config with distinctive channels, so a refused `setup` can be
/// shown to have left them alone.
fn seeded_cfg() -> Value {
    json!({
        "device": 0,
        "output_channel": 4,
        "input_channel": 0,
        "reference_channel": 3,
        "dbu_ref_vrms": 0.774_596_67,
        "range_start_hz": 20.0,
        "range_stop_hz": 20_000.0,
        "server_enabled": false,
    })
}

fn assert_refused(r: &Value, what: &str, needles: &[&str]) {
    assert_eq!(r["ok"], json!(false), "{what} must be refused: {r}");
    let err = r["error"].as_str().unwrap_or_default();
    for n in needles {
        assert!(err.contains(n), "{what}: error must contain {n:?}: {err:?}");
    }
}

fn assert_idle(c: &Client<'_>, what: &str) {
    let s = c.call(json!({"cmd": "status"}));
    assert_eq!(
        s["busy"],
        json!(false),
        "{what}: a refused request must not leave a worker running: {s}"
    );
}

fn generate(c: &Client<'_>, cmd: &str, channels: Value) -> Value {
    c.call(json!({
        "cmd": cmd, "freq_hz": 1000.0, "level_dbfs": -40.0, "channels": channels,
    }))
}

#[test]
fn generate_refuses_invalid_only_channel_list() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    for cmd in ["generate", "generate_pink"] {
        let r = generate(&c, cmd, json!(["bad"]));
        assert_refused(
            &r,
            cmd,
            &[
                &format!("{cmd} not started"),
                "channels[0] must be an integer",
                "received  \"bad\"",
                "stimulus  silent",
            ],
        );
        assert_idle(&c, cmd);
    }
}

#[test]
fn generate_refuses_mixed_valid_and_invalid_channels() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = generate(&c, "generate", json!([2, "bad"]));
    assert_refused(
        &r,
        "generate [2, \"bad\"]",
        &["channels[1]", "stimulus  silent"],
    );
    assert_idle(&c, "generate [2, \"bad\"]");
}

#[test]
fn generate_refuses_channel_above_u32() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = generate(&c, "generate", json!([4294967296u64]));
    assert_refused(
        &r,
        "generate [4294967296]",
        &[
            "channels[0] is outside 0\u{2013}4294967295",
            "received  4294967296",
        ],
    );
    assert_idle(&c, "generate [4294967296]");
}

/// `ac-cli` sends `channels: []` to mean "the configured output" — that
/// contract must survive the stricter parsing.
#[test]
fn generate_empty_channel_list_still_means_default() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = generate(&c, "generate", json!([]));
    assert_eq!(
        r["ok"],
        json!(true),
        "channels: [] must play on the default: {r}"
    );
    c.call(json!({"cmd": "stop"}));
}

#[test]
fn monitor_refuses_invalid_channel_lists() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    for (channels, field) in [
        (json!(["bad"]), "channels[0]"),
        (json!([1, "bad"]), "channels[1]"),
        (json!([4294967297u64]), "channels[0]"),
    ] {
        let r = c.call(json!({"cmd": "monitor_spectrum", "channels": channels}));
        assert_refused(
            &r,
            &format!("monitor_spectrum {channels}"),
            &["monitor not started", field],
        );
        assert_idle(&c, "monitor_spectrum");
    }
}

#[test]
fn monitor_refuses_incomplete_fake_tone() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd": "monitor_spectrum",
        "fake_tones": [
            {"freq_hz": 1000.0, "level_dbfs": -20.0},
            {"freq_hz": 2000.0},
        ],
    }));
    assert_refused(&r, "fake_tones", &["fake_tones[1].level_dbfs"]);
    assert_idle(&c, "fake_tones");
}

/// 4294967552 = 2^32 + 256: narrowed, it passed the power-of-two check.
#[test]
fn fft_n_above_u32_is_refused_not_wrapped() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({"cmd": "monitor_spectrum", "fft_n": 4294967552u64}));
    assert_refused(&r, "monitor_spectrum fft_n", &["fft_n must be power of 2"]);
    assert_idle(&c, "monitor_spectrum fft_n");

    let r = c.call(json!({"cmd": "set_monitor_params", "fft_n": 4294967552u64}));
    assert_refused(
        &r,
        "set_monitor_params fft_n",
        &["fft_n must be power of 2"],
    );
}

fn curve_points() -> (Vec<Value>, Vec<Value>) {
    let log_min = 100.0_f64.ln();
    let log_max = 10_000.0_f64.ln();
    (0..32)
        .map(|i| {
            let t = i as f64 / 31.0;
            (
                json!((log_min + t * (log_max - log_min)).exp()),
                json!(3.0 * t),
            )
        })
        .unzip()
}

/// The case the old length check structurally could not catch: invalid
/// elements at *different* indices leave two equal-length survivors, the
/// frequencies still increase, and a shifted curve was saved `ok: true`.
/// So the stored curve is asserted, not just `ok`.
#[test]
fn mic_curve_invalid_elements_at_different_indices_leave_curve_unchanged() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let (freqs, gains) = curve_points();
    let r = c.call(json!({
        "cmd": "calibrate_mic_curve", "op": "set", "input_channel": 1,
        "freqs_hz": freqs, "gain_db": gains,
    }));
    assert_eq!(r["ok"], json!(true), "baseline set failed: {r}");
    let lookup = json!({"cmd": "get_calibration", "input_channel": 1});
    let before = c.call(lookup.clone());
    assert_eq!(
        before["mic_response"]["freqs_hz"].as_array().map(Vec::len),
        Some(32)
    );

    let (mut bad_freqs, mut bad_gains) = curve_points();
    bad_freqs[3] = Value::Null;
    bad_gains[7] = json!("x");
    let r = c.call(json!({
        "cmd": "calibrate_mic_curve", "op": "set", "input_channel": 1,
        "freqs_hz": bad_freqs, "gain_db": bad_gains,
    }));
    assert_refused(
        &r,
        "shifted mic curve",
        &[
            "mic curve not saved",
            "freqs_hz[3] must be a finite number",
            "received      null",
            "paired field  gain_db[3] = ",
            "data          existing curve unchanged",
        ],
    );

    let after = c.call(lookup);
    assert_eq!(
        after["mic_response"], before["mic_response"],
        "a refused upload must leave the stored curve byte-for-byte"
    );
}

#[test]
fn mic_curve_names_gain_with_its_frequency_partner() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let (freqs, mut gains) = curve_points();
    gains[0] = json!("x");
    let r = c.call(json!({
        "cmd": "calibrate_mic_curve", "op": "set", "input_channel": 1,
        "freqs_hz": freqs, "gain_db": gains,
    }));
    assert_refused(
        &r,
        "gain_db[0]",
        &[
            "gain_db[0] must be a finite number",
            "paired field  freqs_hz[0] = 100.000 Hz",
        ],
    );
}

#[test]
fn mic_curve_mismatched_lengths_name_both() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let (freqs, mut gains) = curve_points();
    gains.pop();
    let r = c.call(json!({
        "cmd": "calibrate_mic_curve", "op": "set", "input_channel": 1,
        "freqs_hz": freqs, "gain_db": gains,
    }));
    assert_refused(&r, "mismatched", &["freqs_hz: 32", "gain_db: 31"]);
}

/// 4294967297 narrowed to channel 1 and keyed the curve under `in1`.
#[test]
fn mic_curve_refuses_input_channel_above_u32() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let (freqs, gains) = curve_points();
    let r = c.call(json!({
        "cmd": "calibrate_mic_curve", "op": "set", "input_channel": 4294967297u64,
        "freqs_hz": freqs, "gain_db": gains,
    }));
    assert_refused(&r, "input_channel overflow", &["input_channel is outside"]);
    let r = c.call(json!({"cmd": "get_calibration", "input_channel": 1}));
    assert_eq!(r["found"], json!(false), "no in1 entry may be created: {r}");

    let r = c.call(json!({
        "cmd": "calibrate_mic_curve", "op": "clear", "input_channel": "x",
    }));
    assert_refused(&r, "clear with bad channel", &["mic curve not cleared"]);
}

#[test]
fn get_calibration_refuses_channel_above_u32() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({"cmd": "get_calibration", "output_channel": 4294967296u64}));
    assert_refused(
        &r,
        "get_calibration",
        &["calibration lookup rejected", "output_channel"],
    );
}

fn config_of(c: &Client<'_>) -> Value {
    let r = c.call(json!({"cmd": "setup", "update": {}}));
    assert_eq!(r["ok"], json!(true), "{r}");
    r["config"].clone()
}

#[test]
fn setup_refuses_channel_above_u32_and_applies_nothing() {
    let d = Daemon::spawn_with_config(Some(seeded_cfg()));
    let c = Client::new(&d);

    let r = c.call(json!({"cmd": "setup", "update": {"output_channel": 4294967296u64}}));
    assert_refused(
        &r,
        "setup output_channel",
        &[
            "setup rejected \u{2014} output_channel is outside 0\u{2013}4294967295",
            "received  4294967296",
            "config    unchanged",
        ],
    );
    assert_eq!(config_of(&c)["output_channel"], json!(4));

    // A valid sibling in the same request is not applied either.
    let r = c.call(json!({
        "cmd": "setup", "update": {"input_channel": 2, "output_channel": 4294967296u64},
    }));
    assert_refused(&r, "setup mixed", &["output_channel"]);
    let cfg = config_of(&c);
    assert_eq!(cfg["input_channel"], json!(0), "{cfg}");
    assert_eq!(cfg["output_channel"], json!(4), "{cfg}");
}

#[test]
fn setup_refuses_malformed_reference_channel() {
    let d = Daemon::spawn_with_config(Some(seeded_cfg()));
    let c = Client::new(&d);
    let r = c.call(json!({"cmd": "setup", "update": {"reference_channel": "x"}}));
    assert_refused(
        &r,
        "reference_channel \"x\"",
        &["reference_channel must be an integer"],
    );
    assert_eq!(config_of(&c)["reference_channel"], json!(3));

    // `null` still clears.
    let r = c.call(json!({"cmd": "setup", "update": {"reference_channel": null}}));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(r["config"]["reference_channel"], Value::Null);
}

/// 4294967299 = 2^32 + 3: narrowed, it was accepted as bpo 3.
#[test]
fn ioct_bpo_above_u32_is_refused() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({"cmd": "set_ioct_bpo", "bpo": 4294967299u64}));
    assert_refused(&r, "set_ioct_bpo", &["invalid bpo 4294967299"]);
    let r = c.call(json!({"cmd": "set_ioct_bpo", "bpo": 3}));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(r["bpo"], json!(3));
}
