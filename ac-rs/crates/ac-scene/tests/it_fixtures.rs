//! AC1 (readout truth), AC4 (wire/snapshot scene equivalence), AC5
//! (reference-label correctness) against the frozen fixtures — the
//! checked-in `.acsnap` (`a10688c7…`) and the captured
//! `transfer-frame-v2.json` derived from it (`regenerate_fixture.rs`,
//! "same underlying data" per AC4's premise).

use ac_core::snapshot::read_acsnap;
use ac_core::visualize::weighting_curves::WeightingCurve;
use ac_scene::{Scene, Source, WireFrame};
use std::path::PathBuf;

const FREQ_RANGE: (f64, f64) = (20.0, 20_000.0);
const DB_RANGE: (f64, f64) = (-140.0, 0.0);

fn acsnap_fixture_path() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../tests/fixtures/snapshot-fixture-v1.acsnap"
    ))
}

fn wire_fixture_path() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../tests/fixtures/transfer-frame-v2.json"
    ))
}

fn load_wire_frame() -> WireFrame {
    let text = std::fs::read_to_string(wire_fixture_path()).expect(
        "tests/fixtures/transfer-frame-v2.json must exist — regenerate via \
         `cargo test -p ac-scene --test regenerate_fixture -- --ignored`",
    );
    serde_json::from_str(&text).expect("deserialize captured frame fixture")
}

fn load_snapshot_scene() -> Scene {
    let bytes = std::fs::read(acsnap_fixture_path()).expect("read checked-in .acsnap fixture");
    let snap = read_acsnap(&bytes).expect("parse .acsnap");
    let d = snap
        .derive_pair(0, WeightingCurve::Z, None)
        .expect("derive_pair on fixture");
    Scene::from_pair_derivation(&d, "meas_0", "ref", snap.meta.sr, FREQ_RANGE, DB_RANGE)
}

// AC1: character-for-character cursor readout truth, hand-derived.
//
// Hand-derivation (independent of the code under test, first
// principles): the fixture's meas channel is `tone(1kHz, amp=0.25) +
// 0.5*broadband[i-200]`, with `vrms_at_0dbfs_in=1.5` applied to
// `meas_spectrum` (not to raw `h1.meas_amp`). The column nearest 1 kHz
// reads the tone's own peak amplitude inflated by the 3-tap Hann
// leakage kernel: `sqrt(0.5^2+0.25^2+0.25^2)/0.5 = 1.22474`. Predicted
// column amplitude: `0.25 * 1.5 * 1.22474 = 0.45928` ->
// `20*log10(0.45928) = -6.757 dB`. This is the same fixture and the
// same -6.75 dB-class quantity independently re-derived three times
// already in this stack (M1.5 build, M1.5 QA follow-up) — reused here
// for the display path's formatting composition, not re-litigated from
// scratch a fourth time.
#[test]
fn ac1_cursor_readout_is_character_for_character_correct() {
    let frame = load_wire_frame();
    let scene = Scene::from_wire_frame(&frame, FREQ_RANGE, DB_RANGE);

    let readout = scene.cursor_readout(1_000.0).expect("cursor readout");
    // Column's actual centre frequency (log-spaced grid, not exactly
    // 1000 Hz) and level, both must appear verbatim.
    let nearest = frame
        .spec_freqs
        .iter()
        .zip(frame.meas_spectrum.iter())
        .min_by(|(fa, _), (fb, _)| {
            (*fa - 1_000.0_f64)
                .abs()
                .partial_cmp(&(*fb - 1_000.0_f64).abs())
                .unwrap()
        })
        .unwrap();
    let expected_level_db = 20.0 * nearest.1.log10();
    assert!(
        (expected_level_db - (-6.757)).abs() < 0.05,
        "hand-derived prediction and fixture-read value disagree: {expected_level_db}"
    );
    let expected = format!("{:.0} Hz: {:.2} dB SPL", nearest.0, expected_level_db);
    assert_eq!(readout, expected);
}

// AC1: SPL readout, hand-derived from first principles (not read back
// from the code under test).
//
// spl is computed from RAW (voltage-uncalibrated) `h1.meas_amp` —
// unlike `meas_spectrum`, it does not carry the 1.5x voltage-cal scale
// (`pair_derivation.rs`'s `spl` closure runs before `meas_amp_wire`'s
// voltage scaling). Ideal (uninflated) power-sum in the code's
// peak-amplitude^2 convention (a full-scale sine of peak amplitude A
// contributes A^2 = 2*variance):
//   tone:      variance = 0.25^2/2 = 0.03125   -> ideal 2*0.03125 = 0.0625
//   broadband: variance = (0.5*0.3)^2/3 = 0.0075 -> ideal 2*0.0075  = 0.015
//   ideal total = 0.0775
// The same 3-tap Hann kernel that inflates a single bin-exact tone's
// column by 1.2247x (power ratio 1.5x) also inflates broadband content
// by the window's coherent-gain-vs-noise-gain ratio, mean(w^2)/mean(w)^2
// = 0.375/0.25 = 1.5 exactly (10*log10(1.5) = 1.76 dB) — the same
// recurring constant, same root cause, not a coincidence.
//   power_sum = 1.5 * 0.0775 = 0.11625 -> 10*log10(0.11625) = -9.347 dB
// SPL offset: PISTONPHONE_REF_SPL(94) - mic_sensitivity(-26) = 120 dB.
//   predicted spl = -9.347 + 120 = 110.65 dB
#[test]
fn ac1_spl_readout_matches_first_principles_derivation() {
    let frame = load_wire_frame();
    let scene = Scene::from_wire_frame(&frame, FREQ_RANGE, DB_RANGE);

    let predicted_spl = 110.65;
    assert!(
        (frame.spl.unwrap() - predicted_spl).abs() < 0.05,
        "fixture spl={:?} disagrees with first-principles prediction {predicted_spl}",
        frame.spl
    );

    let expected = format!("{:.2} dB SPL (Z, fast)", frame.spl.unwrap());
    assert_eq!(scene.readouts.spl, Some(expected));
}

// AC4: wire-built and snapshot-built scenes, from the same underlying
// data, must be equivalent — trace coordinates within float tolerance,
// strings exact, except the one field that structurally cannot agree
// (architect review, decisions 3/3b): the SPL readout's integration
// clause. The SPL *value* portion must still agree (3b), reusing
// M1.5's I-B convergence proof that a time-integrated estimator and a
// single-window derivation agree on stationary content.
#[test]
fn ac4_wire_and_snapshot_scenes_are_equivalent_except_the_integration_tag() {
    let frame = load_wire_frame();
    let wire_scene = Scene::from_wire_frame(&frame, FREQ_RANGE, DB_RANGE);
    let snap_scene = load_snapshot_scene();

    assert_eq!(wire_scene.traces.len(), snap_scene.traces.len());
    for (w, s) in wire_scene.traces.iter().zip(snap_scene.traces.iter()) {
        assert_eq!(w.points.len(), s.points.len());
        for (wp, sp) in w.points.iter().zip(s.points.iter()) {
            assert!((wp.0 - sp.0).abs() < 1e-9, "x mismatch: {wp:?} vs {sp:?}");
            assert!((wp.1 - sp.1).abs() < 1e-9, "y mismatch: {wp:?} vs {sp:?}");
        }
    }
    assert_eq!(wire_scene.freq_axis.ticks, snap_scene.freq_axis.ticks);
    assert_eq!(wire_scene.db_axis.ticks, snap_scene.db_axis.ticks);

    // Same numeric value...
    let wire_spl_str = wire_scene.readouts.spl.as_ref().unwrap();
    let snap_spl_str = snap_scene.readouts.spl.as_ref().unwrap();
    let wire_value = wire_spl_str.split(" dB SPL").next().unwrap();
    let snap_value = snap_spl_str.split(" dB SPL").next().unwrap();
    assert_eq!(wire_value, snap_value, "SPL value must agree across paths");
    // ...but only the wire path carries an integration clause.
    assert!(wire_spl_str.ends_with("(Z, fast)"));
    assert!(snap_spl_str.ends_with("(Z)"));
    assert!(!snap_spl_str.contains("fast"));
}

// AC5: reference-label correctness, both directions.
#[test]
fn ac5_reference_label_decided_only_by_spl_cal_presence() {
    let frame = load_wire_frame();
    let calibrated_scene = Scene::from_wire_frame(&frame, FREQ_RANGE, DB_RANGE);
    assert!(calibrated_scene
        .cursor_readout(1_000.0)
        .unwrap()
        .ends_with("dB SPL"));

    let mut uncal_frame = frame.clone();
    uncal_frame.spl = None;
    let uncal_scene = Scene::from_wire_frame(&uncal_frame, FREQ_RANGE, DB_RANGE);
    assert!(uncal_scene
        .cursor_readout(1_000.0)
        .unwrap()
        .ends_with("dBFS"));
    assert_eq!(uncal_scene.readouts.spl, None);

    // Snapshot-derived path, both directions.
    assert!(load_snapshot_scene()
        .cursor_readout(1_000.0)
        .unwrap()
        .ends_with("dB SPL"));
    assert_eq!(calibrated_scene.traces[0].provenance.source, Source::Live);
}
