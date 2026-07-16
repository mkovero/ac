//! Regenerator for `tests/fixtures/transfer-frame-v2.json` (deliverable
//! 5). Not spun up via a live daemon session — derived directly from
//! the checked-in `.acsnap` fixture's decoded audio through the same
//! `Snapshot::derive_pair` entry point the offline path already uses
//! (D8: no reimplementation), so this frame's numbers are guaranteed to
//! come from "the same underlying data" the `.acsnap` fixture's own
//! derivation uses (AC4's premise) — not from an independently-tuned
//! stimulus that merely looks similar.
//!
//! `cargo test -p ac-scene --test regenerate_fixture -- --ignored`

use ac_core::snapshot::read_acsnap;
use ac_core::visualize::weighting_curves::WeightingCurve;
use serde_json::json;
use std::path::PathBuf;

fn acsnap_fixture_path() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../tests/fixtures/snapshot-fixture-v1.acsnap"
    ))
}

pub(crate) fn wire_fixture_path() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../tests/fixtures/transfer-frame-v2.json"
    ))
}

#[test]
#[ignore = "regenerates tests/fixtures/transfer-frame-v2.json — run manually"]
fn generate_captured_frame_fixture() {
    let bytes = std::fs::read(acsnap_fixture_path()).expect(
        "tests/fixtures/snapshot-fixture-v1.acsnap must exist — regenerate via \
         `cargo test -p ac-core --lib snapshot::tests::generate_snapshot_fixture -- --ignored`",
    );
    let snap = read_acsnap(&bytes).expect("parse checked-in .acsnap fixture");

    let pair_idx = 0;
    let (meas_ch, ref_ch) = snap.meta.session.pairs[pair_idx];
    let weighting = WeightingCurve::from_tag(&snap.meta.per_channel[meas_ch as usize].weighting)
        .expect("valid weighting tag in fixture meta");
    let integration = snap.meta.per_channel[meas_ch as usize].integration.clone();

    let d = snap
        .derive_pair(pair_idx, weighting, None)
        .expect("derive_pair on checked-in fixture");

    let meas_cal = snap.meta.per_channel[meas_ch as usize].calibration.as_ref();
    let ref_cal = snap.meta.per_channel[ref_ch as usize].calibration.as_ref();
    let voltage_tag = |c: Option<&ac_core::shared::calibration::Calibration>| {
        if c.and_then(|c| c.vrms_at_0dbfs_in).is_some() {
            "on"
        } else {
            "none"
        }
    };
    let spl_tag = |c: Option<&ac_core::shared::calibration::Calibration>| {
        if c.and_then(|c| c.spl_offset_db()).is_some() {
            "on"
        } else {
            "none"
        }
    };
    let mic_curve_tag = |c: Option<&ac_core::shared::calibration::Calibration>| {
        if c.and_then(|c| c.mic_response.as_ref()).is_some() {
            "on"
        } else {
            "none"
        }
    };

    eprintln!(
        "spl={:?} meas_spectrum[nearest 1kHz]={:?} spec_freqs.len()={}",
        d.spl,
        d.spec_freqs
            .iter()
            .zip(d.meas_spectrum.iter())
            .min_by(|(fa, _), (fb, _)| (*fa - 1000.0_f64)
                .abs()
                .partial_cmp(&(*fb - 1000.0_f64).abs())
                .unwrap())
            .unwrap(),
        d.spec_freqs.len(),
    );

    let frame = json!({
        "type": "transfer_stream",
        "cmd": "transfer_stream",
        "freqs": d.h1.freqs,
        "magnitude_db": d.h1.magnitude_db,
        "phase_deg": d.h1.phase_deg,
        "coherence": d.h1.coherence,
        "re": d.h1.re,
        "im": d.h1.im,
        "delay_samples": d.h1.delay_samples,
        "delay_ms": d.h1.delay_ms,
        "meas_channel": meas_ch,
        "ref_channel": ref_ch,
        "sr": snap.meta.sr,
        "mic_correction": mic_curve_tag(meas_cal),
        "spec_freqs": d.spec_freqs,
        "meas_spectrum": d.meas_spectrum,
        "ref_spectrum": d.ref_spectrum,
        "spl": d.spl,
        "spl_weighting": d.spl_weighting.tag(),
        "spl_integration": integration,
        "cal_tags": {
            "meas": {
                "voltage": voltage_tag(meas_cal),
                "spl": spl_tag(meas_cal),
                "mic_curve": mic_curve_tag(meas_cal),
            },
            "ref": {
                "voltage": voltage_tag(ref_cal),
                "spl": spl_tag(ref_cal),
                "mic_curve": "none",
            },
        },
    });

    let text = serde_json::to_string_pretty(&frame).expect("serialize frame");
    std::fs::write(wire_fixture_path(), &text).expect("write fixture file");
    eprintln!("wrote {}", wire_fixture_path().display());
}
