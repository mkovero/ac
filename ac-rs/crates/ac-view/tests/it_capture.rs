//! `S` end to end (#256): a live fake session, `capture::capture_into`
//! against it, a file on disk that reopens to the same stored run.

#[path = "support.rs"]
mod support;

use std::time::Duration;

use ac_core::visualize::weighting_curves::WeightingCurve;
use ac_view::session::Session;
use ac_view::zmq_client::{Client, Endpoint};
use support::{alloc_home, DaemonProcess};

#[test]
fn a_capture_is_written_and_reopens_as_the_same_run() {
    let daemon = DaemonProcess::spawn();
    let endpoint = Endpoint {
        host: "127.0.0.1".to_string(),
        ctrl_port: daemon.ctrl_port,
        data_port: daemon.data_port,
    };
    let mut session = Session::new(Client::connect(&endpoint).expect("connect"));
    session
        .launch(&[(0, 1)], WeightingCurve::Z, "fast")
        .expect("launch transfer_stream");
    // Let the capture ring hold a few Welch segments.
    std::thread::sleep(Duration::from_secs(3));

    let dir = alloc_home().join("captures");
    let capture_client = Client::connect(&endpoint).expect("connect (capture)");
    let settings = ac_view::view::SlotSettings {
        smoothing: ac_scene::Smoothing::Oct6,
        invert: true,
        offset_db: 3.0,
    };
    let captured =
        ac_view::capture::capture_into(&capture_client, &dir, 4, 0, settings).expect("capture");
    // #702: live's settings are saved beside the capture, and read back.
    assert_eq!(
        ac_view::capture::read_settings(&captured.path),
        Some(settings)
    );
    assert!(captured.run.ir.is_some(), "a stored run carries its IR");

    assert!(captured.path.starts_with(&dir));
    assert!(
        captured.path.exists(),
        "{} not written",
        captured.path.display()
    );
    let name = captured.path.file_name().unwrap().to_string_lossy();
    assert!(name.ends_with(".acsnap") && !name.contains(':'), "{name}");
    assert!(name.starts_with("slot4-"), "{name}");
    assert_eq!(captured.run.label, "slot 4");
    assert_eq!(captured.slot, 4);

    let reopened = ac_view::snapshot_flow::open_stored_transfer_run(&captured.path, 0)
        .expect("reopen the written file");
    assert_eq!(reopened.captured_at_utc, captured.run.captured_at_utc);
    assert_eq!(
        reopened.derivation.h1.freqs,
        captured.run.derivation.h1.freqs
    );
    assert_eq!(
        reopened.derivation.h1.magnitude_db,
        captured.run.derivation.h1.magnitude_db
    );

    // `F`'s loader, on its thread, into another slot.
    let loaded = ac_view::capture::spawn_open(captured.path.clone(), 7)
        .recv()
        .expect("loader answered")
        .expect("load the written file");
    assert!(loaded.opened);
    assert_eq!(loaded.slot, 7);
    assert_eq!(loaded.run.label, "slot 7");
    // `F` reopens the slot with the settings it was stored with (#702).
    assert_eq!(loaded.run.smoothing, settings.smoothing);
    assert!(loaded.run.invert);
    assert_eq!(loaded.run.offset_db, settings.offset_db);
    assert_eq!(
        loaded.run.derivation.h1.magnitude_db,
        captured.run.derivation.h1.magnitude_db
    );
}
