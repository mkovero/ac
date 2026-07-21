//! AC7: remote. Connects to two daemon instances on separate `HOME`s
//! (M1's crash-test isolation pattern — proves the tooling exists to
//! simulate "a different machine" without an actual second host) by
//! `host:port` alone, and confirms a client connected to one instance
//! never sees the other's state — the concrete meaning of "no
//! filesystem sharing assumed anywhere" (D6) for a live session.

#[path = "support.rs"]
mod support;

use ac_view::zmq_client::{Client, Endpoint};
use serde_json::json;
use support::DaemonProcess;

#[test]
fn client_connects_to_two_isolated_daemon_instances_by_host_port_alone() {
    let daemon_a = DaemonProcess::spawn();
    let daemon_b = DaemonProcess::spawn();
    assert_ne!(daemon_a.ctrl_port, daemon_b.ctrl_port);

    let endpoint_a = Endpoint {
        host: "127.0.0.1".to_string(),
        ctrl_port: daemon_a.ctrl_port,
        data_port: daemon_a.data_port,
    };
    let endpoint_b = Endpoint {
        host: "127.0.0.1".to_string(),
        ctrl_port: daemon_b.ctrl_port,
        data_port: daemon_b.data_port,
    };

    let client_a = Client::connect(&endpoint_a).expect("connect A");
    let client_b = Client::connect(&endpoint_b).expect("connect B");

    // Start a session only on daemon A.
    let r = client_a
        .call(&json!({"cmd": "transfer_stream", "meas_channel": 0, "ref_channel": 1}))
        .unwrap();
    assert_eq!(r["ok"], json!(true));

    // Daemon B must report no session running — proves client_b's CTRL
    // round trip talks to a genuinely separate daemon process/HOME,
    // not a shared/aliased one.
    let snap_b = client_b.call(&json!({"cmd": "snapshot"})).unwrap();
    assert_eq!(snap_b["ok"], json!(false));
    assert!(snap_b["error"]
        .as_str()
        .unwrap_or("")
        .contains("no transfer_stream session running"));

    let _ = client_a.call(&json!({"cmd": "stop"}));
}

#[test]
fn open_local_snapshot_needs_no_daemon_connection() {
    // D8: opening a local .acsnap is filesystem-only — this test
    // constructs one via ac-core directly (no Client, no Endpoint, no
    // daemon spawned at all) and confirms ac-view's open_local reads
    // it with zero network dependency.
    use ac_core::shared::calibration::Calibration;
    use ac_core::snapshot::{write_acsnap, ChannelMeta, SessionMeta, SnapshotMeta, FORMAT_VERSION};

    let sr = 48_000u32;
    let n = 64usize;
    let samples = vec![0.1f32; n];
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
                calibration: None::<Calibration>,
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
    let (bytes, _) = write_acsnap(&meta, &[samples.clone(), samples]).unwrap();

    let dir = std::env::temp_dir().join(format!("ac-view-open-local-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("test.acsnap");
    std::fs::write(&path, &bytes).unwrap();

    let snap = ac_view::snapshot_flow::open_local(&path).expect("open_local");
    assert_eq!(snap.meta.sr, sr);

    std::fs::remove_dir_all(&dir).ok();
}
