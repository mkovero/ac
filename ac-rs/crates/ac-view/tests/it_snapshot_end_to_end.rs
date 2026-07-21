//! AC4: snapshot end-to-end.
//!
//! Two parts, deliberately split:
//!
//! 1. **Live mechanics** (trigger → sha256-verified fetch → open):
//!    against a real `--fake-audio` daemon, through `ac-view`'s own
//!    `snapshot_flow::trigger_and_fetch` — proves the network/chunking/
//!    verification path actually works. `Client::fetch_snapshot`
//!    itself bails on a sha256 mismatch, so a successful return *is*
//!    the verification.
//! 2. **Re-derivation under a different weighting**: "reuse M1.5's
//!    hand-derived offset; no new derivation needed" (AC4's own
//!    wording) — this test builds the identical bin-exact 100 Hz tone
//!    snapshot M1.5's own
//!    `derive_pair_reprocesses_correctly_under_a_different_weighting_than_capture_time`
//!    test already validated against IEC 61672-1 Table 2's
//!    `A(100 Hz) = -19.1 dB`, and re-derives it through `ac-view`'s own
//!    `snapshot_flow::rederive_scene` — testing this crate's
//!    orchestration code, not re-deriving the physics.

#[path = "support.rs"]
mod support;

use std::f64::consts::PI;

use ac_core::shared::calibration::Calibration;
use ac_core::snapshot::{write_acsnap, ChannelMeta, SessionMeta, SnapshotMeta, FORMAT_VERSION};
use ac_core::visualize::weighting_curves::WeightingCurve;
use ac_view::snapshot_flow::{rederive_scene, trigger_and_fetch};
use ac_view::zmq_client::{Client, Endpoint};
use serde_json::json;
use support::DaemonProcess;

const FREQ_RANGE: (f64, f64) = (20.0, 20_000.0);
const DB_RANGE: (f64, f64) = (-140.0, 0.0);

#[test]
fn live_trigger_fetch_and_open_succeeds_with_sha256_verification() {
    let daemon = DaemonProcess::spawn();
    let endpoint = Endpoint {
        host: "127.0.0.1".to_string(),
        ctrl_port: daemon.ctrl_port,
        data_port: daemon.data_port,
    };
    let client = Client::connect(&endpoint).expect("connect");

    let r = client
        .call(&json!({
            "cmd": "transfer_stream", "meas_channel": 0, "ref_channel": 1,
        }))
        .expect("transfer_stream call");
    assert_eq!(r["ok"], json!(true), "transfer_stream start: {r}");

    // Give the worker at least one full ring-fill iteration before
    // triggering, same margin M1's own snapshot tests use.
    std::thread::sleep(std::time::Duration::from_secs_f64(3.3));

    let snap = trigger_and_fetch(&client).expect(
        "trigger_and_fetch failed — either the trigger, the chunked fetch, \
         or the sha256 verification inside Client::fetch_snapshot",
    );
    let _ = client.call(&json!({"cmd": "stop"}));

    assert_eq!(snap.meta.format_version, FORMAT_VERSION);
    assert!(!snap.channels.is_empty());
    assert!(
        !snap.channels[0].is_empty(),
        "fetched snapshot has no samples"
    );
}

#[test]
fn rederive_scene_under_a_different_weighting_matches_m1_5_known_offset() {
    // Identical recipe to ac-core's
    // derive_pair_reprocesses_correctly_under_a_different_weighting_than_capture_time
    // (snapshot/mod.rs) — bin-exact 100 Hz tone, SPL-calibrated meas
    // channel, captured at Z weighting.
    let sr = 48_000u32;
    let n = 3 * sr as usize;
    let f0 = 100.0_f64;
    let tone: Vec<f32> = (0..n)
        .map(|i| (0.3 * (2.0 * PI * f0 * i as f64 / sr as f64).sin()) as f32)
        .collect();

    let meas_cal = Calibration {
        output_channel: 0,
        input_channel: 0,
        ref_freq: 1000.0,
        vrms_at_0dbfs_out: None,
        vrms_at_0dbfs_in: None,
        ref_dbfs: -10.0,
        mic_sensitivity_dbfs_at_94db_spl: Some(-20.0),
        mic_response: None,
    };
    let meta = SnapshotMeta {
        format_version: FORMAT_VERSION,
        sr,
        channel_map: vec!["meas_0".to_string(), "ref".to_string()],
        per_channel: vec![
            ChannelMeta {
                role: "meas_0".to_string(),
                input_channel: 0,
                weighting: "Z".to_string(),
                integration: "fast".to_string(),
                calibration: Some(meas_cal),
            },
            ChannelMeta {
                role: "ref".to_string(),
                input_channel: 1,
                weighting: "Z".to_string(),
                integration: "fast".to_string(),
                calibration: None,
            },
        ],
        session: SessionMeta {
            pairs: vec![(0, 1)],
            delay_samples: vec![0],
            nperseg: sr as usize,
        },
        captured_at_utc: "2026-07-16T00:00:00Z".to_string(),
        daemon_version: "test".to_string(),
        ring_duration_s: n as f64 / sr as f64,
    };

    let (bytes, _sha256) =
        write_acsnap(&meta, &[tone.clone(), tone]).expect("write in-memory snapshot");
    let snap = ac_core::snapshot::read_acsnap(&bytes).expect("read it back");

    let scene_z = rederive_scene(&snap, 0, WeightingCurve::Z, FREQ_RANGE, DB_RANGE)
        .expect("rederive under Z");
    let scene_a = rederive_scene(&snap, 0, WeightingCurve::A, FREQ_RANGE, DB_RANGE)
        .expect("rederive under A");

    let parse_spl = |s: &str| -> f64 { s.split(' ').next().unwrap().parse().unwrap() };
    let spl_z = parse_spl(scene_z.readouts.spl.as_ref().unwrap());
    let spl_a = parse_spl(scene_a.readouts.spl.as_ref().unwrap());
    let offset = spl_a - spl_z;

    // Reused, not re-derived: IEC 61672-1 Table 2's A(100 Hz) = -19.1 dB
    // (already standards-verified in weighting_curves::tests, already
    // reproduced once by M1.5's own equivalent test to within 0.042 dB).
    // Measured here (tightened bound first, per this project's
    // standing discipline): 0.040 dB — matches M1.5's own figure
    // closely enough to be the same effect, not a new one. 0.1 dB
    // leaves a real margin over that without being a fresh guess.
    let delta = (offset - (-19.1)).abs();
    assert!(
        delta < 0.1,
        "A-vs-Z offset at 100 Hz was {offset:.3} dB, expected -19.1 dB \
         (IEC 61672-1 Table 2, reused from M1.5) — Δ={delta:.3}"
    );
}
