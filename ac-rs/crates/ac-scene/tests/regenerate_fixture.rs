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
//!
//! The fixture it writes is checked on every run by
//! `wire_fixture_on_disk_is_current` (#271) — a regeneration that changes the
//! numbers shows up in the diff instead of passing unnoticed.

use ac_core::snapshot::{read_acsnap, write_acsnap, ChannelMeta, Snapshot};
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
    let frame = build_wire_frame();
    let text = serde_json::to_string_pretty(&frame).expect("serialize frame");
    std::fs::write(wire_fixture_path(), &text).expect("write fixture file");
    eprintln!("wrote {}", wire_fixture_path().display());
}

/// #271: the fixture on disk must still be what `build_wire_frame` produces.
///
/// Without this, a derivation change that updates the writer and every reader
/// together leaves the committed fixture describing numbers neither side
/// produces, and `it_fixtures.rs` keeps passing against it. Silent, and the
/// fixture stops being a reference while still looking like one.
///
/// **Compared with a tolerance, not exactly, and the tolerance is the finding.**
/// This artefact is a pure function of committed inputs — derived from the
/// checked-in `.acsnap` through `derive_pair`, no capture and no clock — so an
/// exact comparison looked correct when this test was written. It is not: the
/// derivation is not bit-reproducible across builds. Measured against the
/// committed fixture on first run:
///
/// | field | max abs Δ |
/// |---|---|
/// | `coherence` | 1.1e-16 |
/// | `magnitude_db` | 8.9e-16 dB |
/// | `meas_spectrum` | 3.5e-18 |
/// | `spec_freqs` | 3.6e-12 Hz (relative 1.8e-13) |
///
/// Every one is last-bit: FFT ordering, FMA contraction and library versions
/// move a `f64` at that scale and none of it is drift. **A test that failed on
/// 1e-16 would be deleted rather than debugged**, taking the check with it —
/// which is the failure mode #271 was filed to avoid, arriving from the
/// direction the issue did not anticipate.
///
/// `1e-9` relative sits six orders above the observed noise and six below any
/// derivation change worth catching — a changed window, normalisation or
/// weighting moves these by 1e-6 at the very least, usually far more. It is
/// not a number to tighten: tightening it re-introduces ULP flakiness, and
/// that is what the table above is here to say.
///
/// Structure, keys, strings and integers are still compared **exactly**.
#[test]
fn wire_fixture_on_disk_is_current() {
    /// Relative tolerance for f64 arrays, with an absolute floor for values
    /// near zero. See the doc comment: sized against measured ULP noise, not
    /// fitted to an observed discrepancy.
    const REL_TOL: f64 = 1e-9;
    const ABS_FLOOR: f64 = 1e-12;

    fn close(a: f64, b: f64) -> bool {
        let d = (a - b).abs();
        d <= ABS_FLOOR || d <= REL_TOL * a.abs().max(b.abs())
    }

    /// `Some(description)` when the two differ beyond tolerance.
    fn diff(expected: &serde_json::Value, actual: &serde_json::Value) -> Option<String> {
        match (expected, actual) {
            (serde_json::Value::Array(e), serde_json::Value::Array(a)) => {
                if e.len() != a.len() {
                    return Some(format!("length {} vs {}", e.len(), a.len()));
                }
                let mut worst: Option<(usize, f64, f64)> = None;
                for (i, (ev, av)) in e.iter().zip(a.iter()).enumerate() {
                    match (ev.as_f64(), av.as_f64()) {
                        (Some(x), Some(y)) => {
                            if !close(x, y) {
                                let d = (x - y).abs();
                                if worst.map(|(_, wx, wy)| d > (wx - wy).abs()) != Some(false) {
                                    worst = Some((i, x, y));
                                }
                            }
                        }
                        _ => {
                            if let Some(d) = diff(ev, av) {
                                return Some(format!("[{i}] {d}"));
                            }
                        }
                    }
                }
                worst.map(|(i, x, y)| {
                    format!(
                        "[{i}] {x} vs {y} (Δ {:e}, tolerance {REL_TOL:e} relative)",
                        (x - y).abs()
                    )
                })
            }
            (serde_json::Value::Object(e), serde_json::Value::Object(a)) => {
                for (k, ev) in e {
                    match a.get(k) {
                        None => return Some(format!("`{k}` missing from the fixture")),
                        Some(av) => {
                            if let Some(d) = diff(ev, av) {
                                return Some(format!("`{k}`: {d}"));
                            }
                        }
                    }
                }
                a.keys()
                    .find(|k| !e.contains_key(k.as_str()))
                    .map(|k| format!("fixture carries `{k}`, which this code no longer produces"))
            }
            (serde_json::Value::Number(e), serde_json::Value::Number(a)) => {
                match (e.as_f64(), a.as_f64()) {
                    (Some(x), Some(y)) if close(x, y) => None,
                    _ if e == a => None,
                    _ => Some(format!("{e} vs {a}")),
                }
            }
            _ if expected == actual => None,
            _ => Some(format!("{expected} vs {actual}")),
        }
    }

    let expected = build_wire_frame();
    let text = std::fs::read_to_string(wire_fixture_path()).expect(
        "tests/fixtures/transfer-frame-v2.json must exist — regenerate with \
         `cargo test -p ac-scene --test regenerate_fixture -- --ignored`",
    );
    let on_disk: serde_json::Value =
        serde_json::from_str(&text).expect("committed wire fixture must parse as JSON");

    if let Some(d) = diff(&expected, &on_disk) {
        panic!(
            "the committed transfer-frame-v2.json fixture is stale: {d}. Regenerate with \
             `cargo test -p ac-scene --test regenerate_fixture -- --ignored`, and check what \
             changed before committing it — `it_fixtures.rs` has been asserting against the old \
             content, so whatever moved has been unobserved since."
        );
    }
}

/// The fixture's content, shared by the regenerator and the currency check so
/// the two cannot drift apart.
fn build_wire_frame() -> serde_json::Value {
    build_wire_frame_from(&read_fixture_snapshot())
}

fn read_fixture_snapshot() -> Snapshot {
    let bytes = std::fs::read(acsnap_fixture_path()).expect(
        "tests/fixtures/snapshot-fixture-v1.acsnap must exist — regenerate via \
         `cargo test -p ac-core --lib snapshot::tests::generate_snapshot_fixture -- --ignored`",
    );
    read_acsnap(&bytes).expect("parse checked-in .acsnap fixture")
}

/// Metadata for the channel whose session id is `input_channel`.
///
/// Mirrors `Snapshot::channel_index_for` in `ac-core` (private there): an id is
/// a capture-port index, not a FLAC stream position, so the position is found
/// by matching the id. A miss panics naming the id — it never falls back to
/// `input_channel as usize`, which is the defect #523 removed.
fn channel_meta_for(snap: &Snapshot, input_channel: u32) -> &ChannelMeta {
    let pos = snap
        .meta
        .per_channel
        .iter()
        .position(|c| c.input_channel == input_channel)
        .unwrap_or_else(|| {
            panic!("no per_channel entry has input_channel {input_channel} in the snapshot")
        });
    &snap.meta.per_channel[pos]
}

/// The frame derived from `snap`'s first pair. `build_wire_frame` passes the
/// checked-in fixture; the #523 regression tests pass modified copies of it.
fn build_wire_frame_from(snap: &Snapshot) -> serde_json::Value {
    let pair_idx = 0;
    let (meas_ch, ref_ch) = snap.meta.session.pairs[pair_idx];
    let meas_meta = channel_meta_for(snap, meas_ch);
    let ref_meta = channel_meta_for(snap, ref_ch);
    let weighting = WeightingCurve::from_tag(&meas_meta.weighting)
        .expect("valid weighting tag in fixture meta");
    let integration = meas_meta.integration.clone();

    let d = snap
        .derive_pair(pair_idx, weighting, None)
        .expect("derive_pair on checked-in fixture");

    let meas_cal = meas_meta.calibration.as_ref();
    let ref_cal = ref_meta.calibration.as_ref();
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
        // Shape parity with the live frame. A snapshot-derived frame has
        // no analysis sequence — it is one derivation of one capture, not
        // a position in a stream — so the field is present and null.
        "analysis_seq": serde_json::Value::Null,
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

    frame
}

/// Round-trip `snap` through `write_acsnap` → `read_acsnap`, so the reader's
/// metadata validation (unique ids, pairs that resolve) checks the modified
/// snapshot is a legal one before a test derives from it.
fn round_trip(snap: Snapshot) -> Snapshot {
    let (bytes, _) = write_acsnap(&snap.meta, &snap.channels).expect("write modified snapshot");
    read_acsnap(&bytes).expect("modified snapshot must pass read_acsnap validation")
}

/// #523: stream order reversed, ids kept. Position 0 is now the ref (id 1) and
/// position 1 the meas (id 0), so indexing `per_channel` by id reads the ref's
/// calibration for meas and swaps `cal_tags` / changes `spl`. Resolving by id
/// gives the unmodified frame exactly — same build, same samples, same
/// calibration, so no cross-build tolerance applies.
#[test]
fn frame_resolves_permuted_channels_by_input_id() {
    let base = read_fixture_snapshot();
    let expected = build_wire_frame_from(&base);

    assert_eq!(base.meta.session.pairs[0], (0, 1));
    assert_eq!(base.meta.per_channel[0].input_channel, 0);
    assert_eq!(base.meta.per_channel[1].input_channel, 1);
    assert_ne!(
        base.meta.per_channel[0].calibration, base.meta.per_channel[1].calibration,
        "precondition: meas and ref calibration must differ, or reading the wrong \
         channel's metadata is invisible and this test is vacuous"
    );

    let mut snap = base;
    snap.meta.per_channel.reverse();
    snap.meta.channel_map.reverse();
    snap.channels.reverse();
    let snap = round_trip(snap);
    assert_eq!(snap.meta.per_channel[0].input_channel, 1);

    assert_eq!(build_wire_frame_from(&snap), expected);
}

/// #523: ids that are not positions — triage's `[3, 1]`. Meas relabelled 0→3,
/// ref keeps 1, positions unchanged. Indexing by id panics on `per_channel[3]`;
/// resolving by id gives the unmodified frame except the two channel ids.
#[test]
fn frame_resolves_out_of_range_input_ids() {
    let base = read_fixture_snapshot();
    let expected = build_wire_frame_from(&base);

    let mut snap = base;
    assert_eq!(snap.meta.per_channel[0].input_channel, 0);
    snap.meta.per_channel[0].input_channel = 3;
    snap.meta.session.pairs[0] = (3, 1);
    let snap = round_trip(snap);

    let mut frame = build_wire_frame_from(&snap);
    assert_eq!(frame["meas_channel"], 3);
    assert_eq!(frame["ref_channel"], 1);

    let obj = frame.as_object_mut().expect("frame is an object");
    obj.insert("meas_channel".into(), expected["meas_channel"].clone());
    obj.insert("ref_channel".into(), expected["ref_channel"].clone());
    assert_eq!(frame, expected);
}
