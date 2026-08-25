//! #385 — `ensure_server` must refuse (and never send `quit` to) a daemon
//! whose `HOME` differs from the caller's own, even though the daemon
//! answers `status` fine and looks, on the wire, exactly like a normal one.
//!
//! Drives the real `ac` binary against a real `ac-daemon --fake-audio`, the
//! same pattern `it_plot_ir.rs` uses — a wire-level assertion on `spawn.rs`
//! alone cannot tell whether the CLI actually refused, or refused but sent
//! `quit` first anyway.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU16, Ordering};
use std::thread;
use std::time::{Duration, Instant};

static PORT_CURSOR: AtomicU16 = AtomicU16::new(26_800);

/// Path to a sibling binary in the same target dir as this test's own
/// executable — `ac-daemon` lives in another package, so it gets no
/// `CARGO_BIN_EXE_ac-daemon` (see `it_plot_ir.rs`).
fn sibling_binary(name: &str) -> PathBuf {
    let exe = std::env::current_exe().expect("test exe path");
    let dir = exe
        .parent()
        .and_then(Path::parent)
        .expect("target dir above deps/");
    let p = dir.join(name);
    assert!(
        p.exists(),
        "{} not built at {} — this test needs the whole workspace built \
         (`cargo test --workspace`), not just `-p ac-cli`",
        name,
        p.display()
    );
    p
}

fn scratch_home(tag: &str) -> PathBuf {
    let home =
        std::env::temp_dir().join(format!("ac-ensure-server-it-{}-{tag}", std::process::id()));
    fs::create_dir_all(home.join(".config").join("ac")).expect("create scratch HOME");
    home
}

struct ForeignDaemon {
    child: Child,
    home: PathBuf,
    ctrl_port: u16,
    data_port: u16,
}

impl ForeignDaemon {
    fn spawn() -> Self {
        let base = PORT_CURSOR.fetch_add(2, Ordering::Relaxed);
        let (ctrl_port, data_port) = (base, base + 1);
        let home = scratch_home("foreign");

        let child = Command::new(sibling_binary("ac-daemon"))
            .env("HOME", &home)
            .args([
                "--fake-audio",
                "--local",
                "--ctrl-port",
                &ctrl_port.to_string(),
                "--data-port",
                &data_port.to_string(),
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn foreign ac-daemon");

        let d = Self {
            child,
            home,
            ctrl_port,
            data_port,
        };
        d.wait_until_up();
        d
    }

    fn wait_until_up(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        let ctx = zmq::Context::new();
        loop {
            assert!(Instant::now() < deadline, "foreign daemon never came up");
            thread::sleep(Duration::from_millis(50));
            let s = ctx.socket(zmq::REQ).unwrap();
            s.set_linger(0).ok();
            s.set_rcvtimeo(300).ok();
            s.set_sndtimeo(300).ok();
            if s.connect(&format!("tcp://127.0.0.1:{}", self.ctrl_port))
                .is_err()
            {
                continue;
            }
            if s.send(br#"{"cmd":"status"}"#.as_ref(), 0).is_err() {
                continue;
            }
            if s.recv_bytes(0).is_ok() {
                return;
            }
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
        let _ = self.child.kill();
        let _ = self.child.wait();
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
/// The daemon is spawned in public mode (`bind_host = "*"`, i.e. it binds
/// every interface including loopback) rather than `--local`, and the
/// client is pointed at `127.0.0.2` — a distinct address from the literal
/// strings `ensure_server`'s `is_local` match checks for
/// (`"localhost"`/`"127.0.0.1"`/`"::1"`) — so this exercises a genuinely
/// `is_local == false` code path while staying host-local and portable for
/// CI (the entire `127.0.0.0/8` block routes to loopback on Linux with no
/// interface aliasing required).
#[test]
fn ac_proceeds_against_a_remote_host_with_a_different_home() {
    let base = PORT_CURSOR.fetch_add(2, Ordering::Relaxed);
    let (ctrl_port, data_port) = (base, base + 1);
    let remote_home = scratch_home("remote");

    let mut child = Command::new(sibling_binary("ac-daemon"))
        .env("HOME", &remote_home)
        .args([
            "--fake-audio",
            "--ctrl-port",
            &ctrl_port.to_string(),
            "--data-port",
            &data_port.to_string(),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn ac-daemon");

    {
        let deadline = Instant::now() + Duration::from_secs(10);
        let ctx = zmq::Context::new();
        loop {
            assert!(Instant::now() < deadline, "daemon never came up");
            thread::sleep(Duration::from_millis(50));
            let s = ctx.socket(zmq::REQ).unwrap();
            s.set_linger(0).ok();
            s.set_rcvtimeo(300).ok();
            s.set_sndtimeo(300).ok();
            if s.connect(&format!("tcp://127.0.0.2:{ctrl_port}")).is_err() {
                continue;
            }
            if s.send(br#"{"cmd":"status"}"#.as_ref(), 0).is_err() {
                continue;
            }
            if s.recv_bytes(0).is_ok() {
                break;
            }
        }
    }

    let caller_home = scratch_home("remote-caller");
    fs::write(
        caller_home.join(".config").join("ac").join("config.json"),
        format!(r#"{{"server_host": "127.0.0.2"}}"#),
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

    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_dir_all(&remote_home);
    let _ = fs::remove_dir_all(&caller_home);
}

/// Same-`HOME` case stays silent and proceeds — today's unchanged behaviour.
/// Regression guard for the "no output when HOME matches" half of the ux spec.
#[test]
fn ac_stays_silent_and_proceeds_when_home_matches() {
    let base = PORT_CURSOR.fetch_add(2, Ordering::Relaxed);
    let (ctrl_port, data_port) = (base, base + 1);
    let home = scratch_home("matching");

    let child = Command::new(sibling_binary("ac-daemon"))
        .env("HOME", &home)
        .args([
            "--fake-audio",
            "--local",
            "--ctrl-port",
            &ctrl_port.to_string(),
            "--data-port",
            &data_port.to_string(),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn ac-daemon");

    // Wait for it to come up before pointing `ac` at it.
    {
        let deadline = Instant::now() + Duration::from_secs(10);
        let ctx = zmq::Context::new();
        loop {
            assert!(Instant::now() < deadline, "daemon never came up");
            thread::sleep(Duration::from_millis(50));
            let s = ctx.socket(zmq::REQ).unwrap();
            s.set_linger(0).ok();
            s.set_rcvtimeo(300).ok();
            s.set_sndtimeo(300).ok();
            if s.connect(&format!("tcp://127.0.0.1:{ctrl_port}")).is_err() {
                continue;
            }
            if s.send(br#"{"cmd":"status"}"#.as_ref(), 0).is_err() {
                continue;
            }
            if s.recv_bytes(0).is_ok() {
                break;
            }
        }
    }

    let mut child = child;
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

    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_dir_all(&home);
}
