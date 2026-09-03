use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::common::{Client, Daemon};

const CORRUPT_CAL: &[u8] = br#"{"out0_in0": not-json}"#;
const UNREADABLE_CAL: &[u8] = &[0xff];

fn cal_path(d: &Daemon) -> PathBuf {
    d.home.join(".config").join("ac").join("cal.json")
}

fn write_corrupt_cal(d: &Daemon) -> PathBuf {
    let path = cal_path(d);
    fs::write(&path, CORRUPT_CAL).expect("write corrupt calibration store");
    path
}

fn assert_refused(reply: &Value, path: &Path, operation: &str, scope: Option<&str>) {
    assert_eq!(reply["ok"], false, "operation was not refused: {reply}");
    let error = reply["error"]
        .as_str()
        .unwrap_or_else(|| panic!("refusal error was not a string: {reply}"));
    assert!(
        error.starts_with(&format!("calibration unreadable — {operation} not started")),
        "wrong refusal: {error}"
    );
    if let Some(scope) = scope {
        assert!(error.contains(&format!("\n         scope  {scope}")));
    }
    assert!(error.contains(&format!("\n         store  {}", path.display())));
    assert!(error.contains("\n         cause  parsing "));
    assert!(error.contains("\n         data   existing file preserved"));
}

fn assert_store_preserved(path: &Path) {
    assert_eq!(
        fs::read(path).expect("read calibration store after refusal"),
        CORRUPT_CAL
    );
}

fn configure_reference(client: &Client<'_>) {
    let reply = client.call(json!({
        "cmd": "setup",
        "update": {"reference_channel": 1}
    }));
    assert_eq!(reply["ok"], true, "reference setup failed: {reply}");
}

#[test]
fn corrupt_cal_refuses_every_plot_read_and_preserves_store() {
    let d = Daemon::spawn();
    let client = Client::new(&d);
    let path = write_corrupt_cal(&d);

    for cmd in ["plot", "plot_level", "plot_ir"] {
        let reply = client.call(json!({"cmd": cmd}));
        assert_refused(&reply, &path, "measurement", None);
    }
    assert_store_preserved(&path);
}

#[test]
fn unreadable_cal_refuses_measurement_and_preserves_store() {
    let d = Daemon::spawn();
    let client = Client::new(&d);
    let path = cal_path(&d);
    fs::write(&path, UNREADABLE_CAL).expect("write non-UTF-8 calibration store");

    let reply = client.call(json!({"cmd": "plot"}));
    assert_eq!(reply["ok"], false, "operation was not refused: {reply}");
    let error = reply["error"]
        .as_str()
        .unwrap_or_else(|| panic!("refusal error was not a string: {reply}"));
    assert!(error.contains(&format!("\n         store  {}", path.display())));
    assert!(error.contains("\n         cause  reading "));
    assert!(error.contains("\n         data   existing file preserved"));
    assert_eq!(
        fs::read(&path).expect("read calibration store after refusal"),
        UNREADABLE_CAL
    );
}

#[test]
fn corrupt_cal_refuses_monitor_and_preserves_store() {
    let d = Daemon::spawn();
    let client = Client::new(&d);
    let path = write_corrupt_cal(&d);

    let reply = client.call(json!({"cmd": "monitor_spectrum", "channels": [0, 1]}));
    assert_refused(&reply, &path, "measurement", None);
    assert_store_preserved(&path);
}

#[test]
fn corrupt_cal_refuses_transfer_and_preserves_store() {
    let d = Daemon::spawn();
    let client = Client::new(&d);
    let path = write_corrupt_cal(&d);

    let reply = client.call(json!({
        "cmd": "transfer_stream",
        "pairs": [[0, 1]]
    }));
    assert_refused(&reply, &path, "transfer", Some("all requested pairs"));
    assert_store_preserved(&path);
}

#[test]
fn corrupt_cal_refuses_dut_test_and_preserves_store() {
    let d = Daemon::spawn();
    let client = Client::new(&d);
    configure_reference(&client);
    let path = write_corrupt_cal(&d);

    let reply = client.call(json!({"cmd": "test_dut"}));
    assert_refused(&reply, &path, "measurement", None);
    assert_store_preserved(&path);
}

#[test]
fn corrupt_cal_refuses_hardware_test_and_preserves_store() {
    let d = Daemon::spawn();
    let client = Client::new(&d);
    configure_reference(&client);
    let path = write_corrupt_cal(&d);

    let reply = client.call(json!({"cmd": "test_hardware"}));
    assert_refused(&reply, &path, "measurement", None);
    assert_store_preserved(&path);
}
