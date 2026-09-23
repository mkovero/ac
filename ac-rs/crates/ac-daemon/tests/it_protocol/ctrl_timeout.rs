//! The shared harness's CTRL receive timeout is the only bound on a daemon
//! that never replies (#564): nextest sets no `slow-timeout`, so without it a
//! hung handler hangs the gate. This checks that the bound holds and that the
//! failure names the command and the timeout it waited.

use std::process::Command;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::json;

use crate::common::{Client, Daemon};

/// Short, so the test is fast; the harness default is far longer.
const TIMEOUT_MS: i32 = 500;

/// Scheduling slack on top of the timeout under a loaded full-workspace run —
/// provenance: assumed.
const SLACK: Duration = Duration::from_secs(2);

#[cfg(unix)]
fn signal(pid: u32, sig: &str) {
    let status = Command::new("kill")
        .args([sig, &pid.to_string()])
        .status()
        .expect("run kill");
    assert!(status.success(), "kill {sig} {pid} failed: {status}");
}

#[cfg(unix)]
#[test]
fn stopped_daemon_fails_call_within_timeout_naming_command() {
    let d = Arc::new(Daemon::spawn());
    let pid = d.pid();
    let bound = Duration::from_millis(TIMEOUT_MS as u64) + SLACK;

    // The call runs on its own thread so that this test's own deadline, not
    // the socket's, decides when it gives up: a client that blocks forever
    // must turn this test red, not hang it.
    let (tx, rx) = mpsc::channel();
    let worker = {
        let d = Arc::clone(&d);
        thread::spawn(move || {
            let c = Client::with_ctrl_timeout(&d, TIMEOUT_MS);
            // The daemon answers while running, so the Err below comes from
            // the stop, not from a daemon that was never reachable.
            c.try_call(json!({"cmd": "status"}))
                .expect("status before SIGSTOP");
            signal(pid, "-STOP");
            let start = Instant::now();
            let result = c.try_call(json!({"cmd": "status"}));
            let _ = tx.send((result, start.elapsed()));
        })
    };

    let (result, elapsed) = match rx.recv_timeout(bound + Duration::from_secs(1)) {
        Ok(got) => got,
        Err(_) => {
            // The worker is blocked on a socket that will never answer; kill
            // the daemon so the red run does not leave a stopped process.
            signal(pid, "-KILL");
            panic!("CTRL call on a stopped daemon did not return within {bound:?}");
        }
    };
    worker.join().expect("worker thread");

    let err = match result {
        Ok(v) => panic!("stopped daemon replied: {v}"),
        Err(e) => e,
    };
    assert!(
        elapsed < bound,
        "timeout took {elapsed:?}, bound {bound:?} ({TIMEOUT_MS} ms + slack)"
    );
    assert_eq!(err.cmd, "status");
    assert_eq!(err.timeout_ms, TIMEOUT_MS);
    let msg = err.to_string();
    assert!(
        msg.contains("`status`"),
        "message does not name the command: {msg}"
    );
    assert!(
        msg.contains(&format!("{TIMEOUT_MS} ms")),
        "message does not state the timeout: {msg}"
    );
}
