//! QA follow-up item 2: `tests/fixtures/transfer-frame-v2.json` (used by
//! `it_fixtures.rs`'s AC4 test) is derived from the checked-in `.acsnap`
//! via `derive_pair`, not captured off a real socket — sound for
//! numeric equivalence, but it never proves `WireFrame` can parse what
//! a real daemon actually emits. This file's fixture
//! (`transfer-frame-v2-live.json`, `ac-daemon`'s
//! `it_scene_fixture::generate_live_captured_frame_fixture`) is the
//! verbatim bytes off a real `ac-daemon --fake-audio` session's ZMQ PUB
//! socket, full cal stack loaded (voltage + SPL + mic curve) so every
//! tag vocabulary branch is exercised, not just the "none" defaults.

use ac_scene::{Scene, WireFrame};
use std::path::PathBuf;

const FREQ_RANGE: (f64, f64) = (20.0, 20_000.0);
const DB_RANGE: (f64, f64) = (-140.0, 0.0);

fn live_fixture_path() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../tests/fixtures/transfer-frame-v2-live.json"
    ))
}

#[test]
fn wire_frame_deserializes_a_real_daemon_emitted_frame() {
    let text = std::fs::read_to_string(live_fixture_path()).expect(
        "tests/fixtures/transfer-frame-v2-live.json must exist — regenerate via \
         `cargo test -p ac-daemon --test it_scene_fixture -- --ignored`",
    );

    // First check the *full* schema as a bag of keys, independent of
    // WireFrame's narrow field list — this is the check that would
    // catch a field-name rename WireFrame's own deserialize wouldn't
    // notice (serde silently ignores unknown/missing-but-optional
    // fields; a raw key check doesn't).
    let raw: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
    for key in [
        "type",
        "cmd",
        "freqs",
        "magnitude_db",
        "phase_deg",
        "coherence",
        "re",
        "im",
        "delay_samples",
        "delay_ms",
        "meas_channel",
        "ref_channel",
        "sr",
        "mic_correction",
        "spec_freqs",
        "meas_spectrum",
        "ref_spectrum",
        "spl",
        "spl_weighting",
        "spl_integration",
        "cal_tags",
    ] {
        assert!(raw.get(key).is_some(), "real frame missing key: {key}");
    }
    assert_eq!(raw["type"], serde_json::json!("transfer_stream"));

    // cal_tags vocabulary, straight off the wire — this fixture loaded
    // the full cal stack specifically so all three "on" branches (not
    // just the "none" defaults) are exercised here.
    assert_eq!(raw["cal_tags"]["meas"]["voltage"], serde_json::json!("on"));
    assert_eq!(raw["cal_tags"]["meas"]["spl"], serde_json::json!("on"));
    assert_eq!(
        raw["cal_tags"]["meas"]["mic_curve"],
        serde_json::json!("on")
    );
    assert_eq!(
        raw["cal_tags"]["ref"]["mic_curve"],
        serde_json::json!("none")
    );

    // Now the actual deserializer under test.
    let frame: WireFrame = serde_json::from_str(&text).expect("WireFrame::deserialize");
    assert!(["A", "C", "Z"].contains(&frame.spl_weighting.as_str()));
    assert!(["fast", "slow"].contains(&frame.spl_integration.as_str()));
    assert!(
        frame.spl.is_some(),
        "fixture was captured with SPL cal loaded"
    );
    assert!(!frame.spec_freqs.is_empty());
    assert_eq!(frame.spec_freqs.len(), frame.meas_spectrum.len());
    assert_eq!(frame.spec_freqs.len(), frame.ref_spectrum.len());

    // And the scene actually builds from it — the deserializer's real
    // counterparty end to end, not just a parse check.
    let scene = Scene::from_wire_frame(&frame, FREQ_RANGE, DB_RANGE);
    assert_eq!(scene.traces.len(), 2);
    assert!(!scene.traces[0].points.is_empty());
    assert!(scene.readouts.spl.is_some());
    assert!(scene.cursor_readout(1_000.0).is_some());
}
