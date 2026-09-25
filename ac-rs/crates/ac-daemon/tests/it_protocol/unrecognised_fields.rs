//! A top-level request field the command does not read refuses the whole
//! request, before anything runs (#628).
//!
//! Every case here was accepted before: the unknown field was dropped and
//! the command ran on that knob's default, with no signal (#615 sent
//! `interval_ms` to `monitor_spectrum` and ran at the 0.2 s default).

use serde_json::{json, Value};

use crate::common::{Client, Daemon};

fn assert_refused_naming(r: &Value, cmd: &str, field: &str) {
    assert_eq!(
        r["ok"],
        json!(false),
        "{cmd} + {field} must be refused: {r}"
    );
    let err = r["error"].as_str().unwrap_or_default();
    assert!(
        err.starts_with(&format!("{cmd}: ")),
        "error must name the command first: {err:?}"
    );
    assert!(
        err.contains(&format!("'{field}'")),
        "error must name the field: {err:?}"
    );
    assert!(err.contains("command not run"), "{err:?}");
    assert_eq!(r["unrecognised_fields"], json!([field]), "{r}");
}

fn assert_idle(c: &Client<'_>, what: &str) {
    let s = c.call(json!({"cmd": "status"}));
    assert_eq!(s["ok"], json!(true), "{what}: status after refusal: {s}");
    assert_eq!(
        s["busy"],
        json!(false),
        "{what}: a refused request must not leave a worker running: {s}"
    );
}

#[test]
fn monitor_spectrum_refuses_interval_ms() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd": "monitor_spectrum", "freq_hz": 1000.0, "interval_ms": 100,
    }));
    assert_refused_naming(&r, "monitor_spectrum", "interval_ms");
    let accepted: Vec<&str> = r["accepted_fields"]
        .as_array()
        .expect("accepted_fields array")
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(accepted.contains(&"interval"), "{r}");
    assert!(
        r["error"].as_str().unwrap().contains("\n         reads   "),
        "{r}"
    );
    assert_idle(&c, "monitor_spectrum");
}

#[test]
fn generate_with_an_unrecognised_field_emits_nothing() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd": "generate", "freq_hz": 1000.0, "level_dbfs": -40.0, "level_db": -40.0,
    }));
    assert_refused_naming(&r, "generate", "level_db");
    assert_idle(&c, "generate");
}

#[test]
fn status_refuses_a_stray_field_without_a_reads_line() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({"cmd": "status", "verbose": true}));
    assert_refused_naming(&r, "status", "verbose");
    assert_eq!(r["accepted_fields"], json!([]), "{r}");
    assert!(
        !r["error"].as_str().unwrap().contains("reads"),
        "a command that reads nothing prints no reads line: {r}"
    );
}

#[test]
fn quit_with_a_stray_field_leaves_the_daemon_running() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({"cmd": "quit", "now": true}));
    assert_refused_naming(&r, "quit", "now");
    let s = c.call(json!({"cmd": "status"}));
    assert_eq!(s["ok"], json!(true), "daemon must still answer: {s}");
}

#[test]
fn several_unrecognised_fields_are_all_named() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd": "monitor_spectrum", "interval_ms": 100, "chan": 0, "interval": 0.1,
    }));
    assert_eq!(r["ok"], json!(false), "{r}");
    assert_eq!(
        r["unrecognised_fields"],
        json!(["chan", "interval_ms"]),
        "{r}"
    );
    let err = r["error"].as_str().unwrap();
    assert!(
        err.starts_with("monitor_spectrum: 2 fields not recognised"),
        "{err:?}"
    );
    assert!(
        err.contains("\n         fields  chan  interval_ms"),
        "{err:?}"
    );
    assert_idle(&c, "monitor_spectrum");
}
