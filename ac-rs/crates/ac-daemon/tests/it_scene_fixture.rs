//! Regenerator for `ac-scene`'s **genuinely daemon-emitted**
//! `transfer_stream` frame fixture (QA follow-up item 2 on handoff:
//! ac-scene M2).
//!
//! `ac-scene`'s other fixture (`tests/fixtures/transfer-frame-v2.json`)
//! is derived directly from the checked-in `.acsnap` via
//! `Snapshot::derive_pair`, deliberately — that's what makes the
//! wire-vs-snapshot equivalence test (AC4) sound, since both scenes
//! come from the same underlying data. But it means `ac-scene`'s
//! `WireFrame` deserializer has never actually parsed a frame that came
//! off a real ZMQ socket: field-name drift, JSON number formatting,
//! null handling, and tag-string vocabulary all go untested by a
//! fixture built from Rust struct literals. This regenerator captures
//! **one real DATA frame's raw bytes, verbatim**, from an actual
//! running `ac-daemon --fake-audio` session — the deserializer's actual
//! counterparty.
//!
//! `cargo test -p ac-daemon --test it_scene_fixture -- --ignored`

use std::env;
use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

#[path = "common/mod.rs"]
mod common;

use common::{Client, Daemon};

fn synthetic_curve_flat(peak_db: f64) -> (Vec<f64>, Vec<f64>) {
    let mut freqs = Vec::with_capacity(24);
    let mut gains = Vec::with_capacity(24);
    let log_min = 100.0_f64.ln();
    let log_max = 10_000.0_f64.ln();
    for i in 0..24 {
        let t = i as f64 / 23.0;
        let log_f = log_min + t * (log_max - log_min);
        freqs.push(log_f.exp());
        gains.push(peak_db);
    }
    (freqs, gains)
}

fn fixture_path() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../tests/fixtures/transfer-frame-v2-live.json"
    ))
}

#[test]
#[ignore = "regenerates tests/fixtures/transfer-frame-v2-live.json — run manually, needs a live daemon"]
/// Its output's *shape* is checked on every run by
/// [`live_fixture_on_disk_has_the_shape_the_daemon_still_produces`] (#271).
/// That check found this fixture eight wire fields out of date on its first
/// run — `mtw`, `delay_evidence`, `delay_locked`, `delay_attempts`, `drive`,
/// `meas_peak_dbfs`, `ref_peak_dbfs`, `speed_of_sound_m_s` — which is what the
/// gap looked like in practice.
fn generate_live_captured_frame_fixture() {
    let raw_frame = capture_live_frame();
    fs::write(fixture_path(), &raw_frame).expect("write fixture file");
    eprintln!(
        "wrote {} ({} bytes, verbatim off-wire DATA payload)",
        fixture_path().display(),
        raw_frame.len()
    );
}

/// #271: the committed live fixture must still have the **shape** the daemon
/// produces.
///
/// **This one deliberately does not compare values, and that is the point.**
/// The other two currency checks — `ac-core`'s sha256 on the `.acsnap` and
/// `ac-scene`'s tolerance comparison on `transfer-frame-v2.json` — work because
/// those artefacts are functions of committed inputs. This one is a live
/// capture off a real daemon: its numbers carry capture timing, and a value
/// comparison would be flaky. A flaky test in this suite gets deleted rather
/// than debugged, correctly, and deleting it would leave the fixture uncovered
/// *and* consume the attempt to cover it — a retired test reads as a decision,
/// not an omission.
///
/// What is stable is what the fixture exists to protect: the frame's key set
/// and the `cal_tags` vocabulary, with a full cal stack loaded so every tag
/// exercises its "on" branch. Vocabulary drift is what an all-"none" frame
/// cannot catch, and it is what `ac-scene` parses.
fn capture_live_frame() -> Vec<u8> {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    // Full cal stack loaded (voltage + SPL + mic curve) so the captured
    // frame's `cal_tags` exercises the "on" branch of every tag, not
    // just the "none" defaults — the whole point is to catch vocabulary
    // drift, which an all-"none" frame can't.
    let (freqs, gains) = synthetic_curve_flat(3.0);
    let r = c.call(json!({
        "cmd": "calibrate_mic_curve", "op": "set", "input_channel": 0,
        "freqs_hz": freqs, "gain_db": gains,
    }));
    assert_eq!(r["ok"], json!(true), "calibrate_mic_curve: {r}");

    let r = c.call(json!({"cmd": "calibrate", "ref_dbfs": -10.0,
                           "output_channel": 0, "input_channel": 0}));
    assert_eq!(r["ok"], json!(true), "calibrate start: {r}");
    c.wait_for_topic("cal_prompt", Duration::from_secs(3))
        .expect("voltage cal step 1 prompt");
    let _ = c.call(json!({"cmd": "cal_reply", "vrms": 2.0}));
    c.wait_for_topic("cal_prompt", Duration::from_secs(3))
        .expect("voltage cal step 2 prompt");
    let _ = c.call(json!({"cmd": "cal_reply", "vrms": 2.0}));
    c.wait_for_topic("cal_done", Duration::from_secs(5))
        .expect("voltage cal_done");

    let r = c.call(json!({"cmd": "calibrate_spl", "input_channel": 0, "capture_s": 0.05}));
    assert_eq!(r["ok"], json!(true), "calibrate_spl: {r}");
    c.wait_for_topic("cal_prompt", Duration::from_secs(3))
        .expect("spl cal_prompt");
    let _ = c.call(json!({"cmd": "cal_reply", "vrms": Value::Null}));
    c.wait_for_topic("cal_done", Duration::from_secs(5))
        .expect("spl cal_done");
    while c.recv_pub_raw(50).is_some() {}

    let r = c.call(json!({
        "cmd": "transfer_stream", "meas_channel": 0, "ref_channel": 1,
        "weighting": "A", "integration": "fast",
        "fake_correlated_pair": {"gain": 0.5, "delay_samples": 200},
    }));
    assert_eq!(r["ok"], json!(true), "transfer_stream start: {r}");
    thread::sleep(Duration::from_secs_f64(3.3));

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut raw_frame: Option<Vec<u8>> = None;
    while Instant::now() < deadline {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub_raw(remaining.max(1)) {
            Some((t, payload)) if t == "data" => {
                let v: Value = serde_json::from_slice(&payload).unwrap_or(Value::Null);
                // `n_averages > 0` skips the settling frames a session
                // publishes before its ring holds a Welch segment. They
                // have the same key set — that is checked directly in the
                // daemon's `session_tests` — but empty analysis arrays and
                // a null `spl`, and this fixture's job is to put real
                // numbers and the "on" cal-tag branches in front of
                // `ac-scene`'s deserializer.
                if v["type"] == json!("transfer_stream")
                    && v["n_averages"].as_u64().unwrap_or(0) > 0
                {
                    raw_frame = Some(payload);
                    break;
                }
            }
            Some(_) => continue,
            None => break,
        }
    }
    let _ = c.call(json!({"cmd": "stop"}));
    let raw_frame = raw_frame.expect("no transfer_stream frame within 10 s");

    // Sanity: SPL cal is loaded, so this should be a real number, not
    // null — a fixture with an accidentally-null spl would defeat the
    // whole point of loading full cal state above.
    let parsed: Value = serde_json::from_slice(&raw_frame).unwrap();
    assert!(
        parsed["spl"].is_number(),
        "fixture's spl must be a number (SPL cal was loaded): {parsed}"
    );
    assert_eq!(parsed["cal_tags"]["meas"]["voltage"], json!("on"));
    assert_eq!(parsed["cal_tags"]["meas"]["spl"], json!("on"));
    assert_eq!(parsed["cal_tags"]["meas"]["mic_curve"], json!("on"));

    raw_frame
}

#[test]
fn live_fixture_on_disk_has_the_shape_the_daemon_still_produces() {
    let live: Value =
        serde_json::from_slice(&capture_live_frame()).expect("live frame parses as JSON");
    let text = fs::read_to_string(fixture_path()).expect(
        "tests/fixtures/transfer-frame-v2-live.json must exist — regenerate with \
         `cargo test -p ac-daemon --test it_scene_fixture -- --ignored`",
    );
    let fixture: Value = serde_json::from_str(&text).expect("committed live fixture parses");

    let keys = |v: &Value| -> Vec<String> {
        let mut k: Vec<String> = v
            .as_object()
            .expect("frame is an object")
            .keys()
            .cloned()
            .collect();
        k.sort();
        k
    };
    assert_eq!(
        keys(&fixture),
        keys(&live),
        "the committed live fixture's key set no longer matches what the daemon publishes. \
         Regenerate with `cargo test -p ac-daemon --test it_scene_fixture -- --ignored` — \
         `ac-scene` parses this frame, so a key that appeared or vanished here is a wire \
         change nothing else in the default suite would have caught."
    );

    // Types, not values: a field that changed from a number to a string, or an
    // array to a scalar, breaks every consumer, and no amount of capture
    // jitter can produce that.
    for k in keys(&fixture) {
        let f = &fixture[&k];
        let l = &live[&k];
        let kind = |v: &Value| match v {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        };
        // `spl` is null without SPL cal and a number with it; both runs load
        // the full stack, so a mismatch here is real.
        assert_eq!(
            kind(f),
            kind(l),
            "field `{k}` changed type: fixture has {}, the daemon now publishes {}",
            kind(f),
            kind(l)
        );
    }

    assert_eq!(
        fixture["cal_tags"], live["cal_tags"],
        "the `cal_tags` vocabulary drifted. This is the one part of the frame compared \
         exactly — the tags are a closed string vocabulary, not a measurement, and both runs \
         load the same full cal stack, so any difference is drift rather than jitter."
    );
}
