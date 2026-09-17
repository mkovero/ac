//! #385 — `ensure_server` must refuse (and never send `quit` to) a daemon
//! whose `HOME` differs from the caller's own, even though the daemon
//! answers `status` fine and looks, on the wire, exactly like a normal one.
//!
//! Drives the real `ac` binary against a real `ac-daemon --fake-audio`, the
//! same pattern `it_plot_ir.rs` uses — a wire-level assertion on `spawn.rs`
//! alone cannot tell whether the CLI actually refused, or refused but sent
//! `quit` first anyway.

mod support;

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn scratch_home(tag: &str) -> PathBuf {
    let home =
        std::env::temp_dir().join(format!("ac-ensure-server-it-{}-{tag}", std::process::id()));
    fs::create_dir_all(home.join(".config").join("ac")).expect("create scratch HOME");
    home
}

struct ForeignDaemon {
    daemon: support::DaemonGuard,
    home: PathBuf,
    ctrl_port: u16,
    data_port: u16,
}

impl ForeignDaemon {
    fn spawn() -> Self {
        let home = scratch_home("foreign");
        let daemon = support::spawn_daemon(&home, true, "127.0.0.1", None);
        let (ctrl_port, data_port) = (daemon.ctrl, daemon.data);
        Self {
            daemon,
            home,
            ctrl_port,
            data_port,
        }
    }

    /// Still alive and answering `status` — the tell that `quit` never
    /// reached it.
    fn still_answers(&self) -> bool {
        let ctx = zmq::Context::new();
        let s = ctx.socket(zmq::REQ).unwrap();
        s.set_linger(0).ok();
        s.set_rcvtimeo(1_000).ok();
        s.set_sndtimeo(1_000).ok();
        if s.connect(&format!("tcp://127.0.0.1:{}", self.ctrl_port))
            .is_err()
        {
            return false;
        }
        if s.send(br#"{"cmd":"status"}"#.as_ref(), 0).is_err() {
            return false;
        }
        s.recv_bytes(0).is_ok()
    }
}

impl Drop for ForeignDaemon {
    fn drop(&mut self) {
        // Runs before the fields drop, so kill the daemon here first.
        self.daemon.kill();
        let _ = fs::remove_dir_all(&self.home);
    }
}

/// The architect's named failing case: an `ac` invocation whose own `HOME`
/// differs from the daemon it finds on the port must exit non-zero with the
/// mismatch warning, and must not send that daemon `quit` — not even via
/// the stale-`src_mtime` auto-respawn branch, which before #385 sent `quit`
/// to whatever answered on the port with no identity check at all.
#[test]
fn ac_refuses_and_never_quits_a_daemon_under_a_different_home() {
    let foreign = ForeignDaemon::spawn();
    let my_home = scratch_home("caller");

    let output = Command::new(env!("CARGO_BIN_EXE_ac"))
        .env("HOME", &my_home)
        .env("AC_CTRL_PORT", foreign.ctrl_port.to_string())
        .env("AC_DATA_PORT", foreign.data_port.to_string())
        .args(["server", "connections"])
        .output()
        .expect("run ac");

    assert!(
        !output.status.success(),
        "ac must exit non-zero against a mismatched-HOME daemon: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("different HOME"),
        "stderr should explain the mismatch: {stderr}"
    );
    assert!(
        stderr.contains(&my_home.display().to_string()),
        "stderr should show the caller's own HOME: {stderr}"
    );
    assert!(
        stderr.contains(&foreign.home.display().to_string()),
        "stderr should show the daemon's HOME: {stderr}"
    );

    assert!(
        foreign.still_answers(),
        "foreign daemon must still be alive — quit must never have reached it"
    );

    let _ = fs::remove_dir_all(&my_home);
}

/// The architect's revised scope (re-entry from PR #396 QA correctness #1):
/// a *remote* connection (`cfg.server_host` set, `is_local == false`) must
/// not be refused merely because the daemon it finds reports a different
/// `HOME` — that's expected for a different machine, not a squatting
/// signal. Regression guard for `ensure_server`'s original unconditional
/// mismatch check, which broke the shipped `ac server <remote-host>`
/// workflow before this fix scoped the check to `is_local`.
///
/// The daemon is switched to public mode (`bind_host = "*"`, i.e. it binds
/// every interface including loopback) with `server_enable` — a direct
/// daemon binds loopback only since #433 — and the client is pointed at `127.0.0.2` — a distinct address from the literal
/// strings `ensure_server`'s `is_local` match checks for
/// (`"localhost"`/`"127.0.0.1"`/`"::1"`) — so this exercises a genuinely
/// `is_local == false` code path while staying host-local and portable for
/// CI (the entire `127.0.0.0/8` block routes to loopback on Linux with no
/// interface aliasing required).
#[test]
fn ac_proceeds_against_a_remote_host_with_a_different_home() {
    let remote_home = scratch_home("remote");
    // A direct daemon binds loopback only (#433), so start it local and turn
    // public exposure on explicitly with `server_enable`; the client below
    // then reaches it on 127.0.0.2.
    let daemon = support::spawn_daemon(&remote_home, false, "127.0.0.1", None);
    let (ctrl_port, data_port) = (daemon.ctrl, daemon.data);
    go_public(ctrl_port);

    let caller_home = scratch_home("remote-caller");
    fs::write(
        caller_home.join(".config").join("ac").join("config.json"),
        r#"{"server_host": "127.0.0.2"}"#,
    )
    .expect("seed caller config.json with server_host");

    let output = Command::new(env!("CARGO_BIN_EXE_ac"))
        .env("HOME", &caller_home)
        .env("AC_CTRL_PORT", ctrl_port.to_string())
        .env("AC_DATA_PORT", data_port.to_string())
        .args(["server", "connections"])
        .output()
        .expect("run ac");

    assert!(
        output.status.success(),
        "ac must not refuse a remote host merely for having a different HOME: \
         stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("different HOME"),
        "must not warn on a remote HOME mismatch: {stderr}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&remote_home.display().to_string()),
        "`server connections` should still display the remote daemon's real Home: {stdout}"
    );

    drop(daemon);
    let _ = fs::remove_dir_all(&remote_home);
    let _ = fs::remove_dir_all(&caller_home);
}

/// Rebind a local daemon to every interface and wait until it answers on
/// 127.0.0.2.
fn go_public(ctrl_port: u16) {
    let call = |host: &str, cmd: &[u8]| -> bool {
        let ctx = zmq::Context::new();
        let s = ctx.socket(zmq::REQ).unwrap();
        s.set_linger(0).ok();
        s.set_rcvtimeo(500).ok();
        s.set_sndtimeo(500).ok();
        s.connect(&format!("tcp://{host}:{ctrl_port}")).is_ok()
            && s.send(cmd, 0).is_ok()
            && s.recv_bytes(0).is_ok()
    };
    assert!(
        call("127.0.0.1", br#"{"cmd":"server_enable"}"#),
        "server_enable"
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if call("127.0.0.2", br#"{"cmd":"status"}"#) {
            return;
        }
    }
    panic!("daemon never answered on 127.0.0.2 after server_enable");
}

/// Same-`HOME` case stays silent and proceeds — today's unchanged behaviour.
/// Regression guard for the "no output when HOME matches" half of the ux spec.
#[test]
fn ac_stays_silent_and_proceeds_when_home_matches() {
    let home = scratch_home("matching");
    let daemon = support::spawn_daemon(&home, true, "127.0.0.1", None);
    let (ctrl_port, data_port) = (daemon.ctrl, daemon.data);

    let output = Command::new(env!("CARGO_BIN_EXE_ac"))
        .env("HOME", &home)
        .env("AC_CTRL_PORT", ctrl_port.to_string())
        .env("AC_DATA_PORT", data_port.to_string())
        .args(["server", "connections"])
        .output()
        .expect("run ac");

    assert!(
        output.status.success(),
        "ac must succeed against its own-HOME daemon: stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("different HOME"),
        "must not warn when HOME matches: {stderr}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&home.display().to_string()),
        "`server connections` should print the matching Home: {stdout}"
    );

    drop(daemon);
    let _ = fs::remove_dir_all(&home);
}
