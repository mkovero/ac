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
        .launch(0, 1, WeightingCurve::Z, "fast")
        .expect("launch transfer_stream");
    // Let the capture ring hold a few Welch segments.
    std::thread::sleep(Duration::from_secs(3));

    let dir = alloc_home().join("captures");
    let capture_client = Client::connect(&endpoint).expect("connect (capture)");
    let captured = ac_view::capture::capture_into(&capture_client, &dir, 4).expect("capture");

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
}
