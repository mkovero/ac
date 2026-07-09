//! Display-truth harness (T2/T3) end-to-end — #170.
//!
//! Spawns a real, isolated `ac-daemon --fake-audio` + `ac-ui
//! --headless-test` pair (same subprocess-spawn idiom as
//! `it_views_smoke.rs`) and asserts against the actual JSON the harness
//! produces. `#[ignore]`'d by default — needs a wgpu adapter (software
//! Vulkan/lavapipe or a real GPU); runbook only:
//!
//!   cargo test -p ac-ui --test it_display_truth -- --include-ignored
//!
//! This is the CI-equivalent gate `ac-cli`'s `run_software` (`ac test
//! software`) wraps for human use; this test drives the same two binaries
//! directly so a harness regression fails fast without going through the
//! CLI's table-printing layer.

use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

const AC_UI: &str = env!("CARGO_BIN_EXE_ac-ui");

/// `ac-daemon` has no library target, so Cargo can't add it as an
/// `ac-ui` dev-dependency to populate `CARGO_BIN_EXE_ac-daemon` the way
/// `it_protocol.rs` does from inside ac-daemon's own package. Instead,
/// resolve it as a sibling of this test binary's own `ac-ui` executable —
/// both land in the same `target/<profile>/` directory in this workspace.
/// Requires `ac-daemon` to have been built already (`cargo build -p
/// ac-daemon` or a full workspace build) before running this
/// `--include-ignored` runbook test.
fn ac_daemon_bin() -> std::path::PathBuf {
    let p = std::path::Path::new(AC_UI).with_file_name("ac-daemon");
    assert!(
        p.exists(),
        "ac-daemon binary not found at {} — build it first: cargo build -p ac-daemon",
        p.display()
    );
    p
}

fn free_port_pair() -> (u16, u16) {
    let a = TcpListener::bind("127.0.0.1:0").unwrap();
    let b = TcpListener::bind("127.0.0.1:0").unwrap();
    (
        a.local_addr().unwrap().port(),
        b.local_addr().unwrap().port(),
    )
}

struct Daemon {
    child: Child,
}

impl Daemon {
    fn spawn(ctrl_port: u16, data_port: u16) -> Self {
        let child = Command::new(ac_daemon_bin())
            .args([
                "--local",
                "--fake-audio",
                "--ctrl-port",
                &ctrl_port.to_string(),
                "--data-port",
                &data_port.to_string(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn ac-daemon");
        let ctx = zmq::Context::new();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if Instant::now() > deadline {
                panic!("ac-daemon never came up on ctrl port {ctrl_port}");
            }
            std::thread::sleep(Duration::from_millis(50));
            let s = ctx.socket(zmq::REQ).unwrap();
            s.set_linger(0).ok();
            s.set_rcvtimeo(300).ok();
            s.set_sndtimeo(300).ok();
            if s.connect(&format!("tcp://127.0.0.1:{ctrl_port}")).is_err() {
                continue;
            }
            if s.send(r#"{"cmd":"status"}"#, 0).is_err() {
                continue;
            }
            if let Ok(Ok(reply)) = s.recv_string(0) {
                if let Ok(v) = serde_json::from_str::<Value>(&reply) {
                    if v.get("ok").and_then(Value::as_bool) == Some(true) {
                        break;
                    }
                }
            }
        }
        Self { child }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Run `ac-ui --headless-test` against `daemon` and return its parsed JSON
/// result. Panics (rather than silently treating a crash as "no results")
/// if the process didn't exit cleanly with JSON on stdout — this test's
/// entire job is to notice that.
fn run_headless_test(ctrl_port: u16, data_port: u16) -> Value {
    let output = Command::new(AC_UI)
        .args([
            "--headless-test",
            "--ctrl",
            &format!("tcp://127.0.0.1:{ctrl_port}"),
            "--connect",
            &format!("tcp://127.0.0.1:{data_port}"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn ac-ui --headless-test");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!(
            "ac-ui --headless-test did not produce parseable JSON \
             (status={:?}): {e}\nstdout: {stdout}\nstderr tail: {}",
            output.status.code(),
            stderr.lines().rev().take(40).collect::<Vec<_>>().join("\n"),
        )
    })
}

fn find_check<'a>(results: &'a [Value], name_substr: &str) -> Option<&'a Value> {
    results.iter().find(|r| {
        r.get("name")
            .and_then(Value::as_str)
            .is_some_and(|n| n.contains(name_substr))
    })
}

#[test]
#[ignore = "spawns ac-daemon + ac-ui; needs a wgpu adapter — runbook only (#170)"]
fn display_truth_harness_runs_and_reports_i1_i4() {
    let (ctrl_port, data_port) = free_port_pair();
    let _daemon = Daemon::spawn(ctrl_port, data_port);
    let result = run_headless_test(ctrl_port, data_port);

    let results = result
        .get("results")
        .and_then(Value::as_array)
        .expect("results array");
    assert!(
        !results.is_empty(),
        "expected at least one check, got none: {result}"
    );

    // I1: buffer-level tone accuracy at the LF/HF crossover must pass on
    // current main (no known-open bug affects it).
    let i1 = find_check(results, "I1 buffer @ LF/HF-crossover low side")
        .unwrap_or_else(|| panic!("missing I1 LF/HF check in {results:?}"));
    assert_eq!(
        i1.get("pass").and_then(Value::as_bool),
        Some(true),
        "I1 buffer check should pass on current main: {i1}"
    );

    // I4: bounded output must pass on current main for a two-tone fake
    // capture (the known-open HF-garbage bug is exercised separately by
    // the fixture corpus in headless.rs's unit tests, not by a live
    // capture at these frequencies).
    let i4 = find_check(results, "I4 bounded output (two-tone LF/HF capture)")
        .unwrap_or_else(|| panic!("missing I4 check in {results:?}"));
    assert_eq!(
        i4.get("pass").and_then(Value::as_bool),
        Some(true),
        "I4 bounded-output check should pass on current main: {i4}"
    );
}

/// Ember Y-axis orientation, permanent I3 assertion (#172). Was tracked as
/// `known_open_bug_ember_orientation_currently_fails_i3` while the bug was
/// open (`ember_display.wgsl` sampled the substrate texture without the
/// screen/deposit row flip `waterfall.wgsl` already applies, so the louder
/// tone rendered lower on screen instead of higher). Converted to a normal
/// pass assertion now that the flip is in place.
#[test]
#[ignore = "spawns ac-daemon + ac-ui; needs a wgpu adapter — runbook only (#170)"]
fn ember_orientation_passes_i3() {
    let (ctrl_port, data_port) = free_port_pair();
    let _daemon = Daemon::spawn(ctrl_port, data_port);
    let result = run_headless_test(ctrl_port, data_port);
    let results = result
        .get("results")
        .and_then(Value::as_array)
        .expect("results array");
    let ember_i3 = find_check(results, "I3 orientation (SpectrumEmber)")
        .unwrap_or_else(|| panic!("missing SpectrumEmber I3 check in {results:?}"));
    assert_eq!(
        ember_i3.get("pass").and_then(Value::as_bool),
        Some(true),
        "ember I3 orientation should pass post-fix: {ember_i3}"
    );
}
