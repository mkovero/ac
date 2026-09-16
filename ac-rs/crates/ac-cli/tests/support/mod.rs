//! Daemon spawning for the ac-cli integration tests (#486).
//!
//! Ports come from the OS ([`alloc_ports`]), and a spawn is accepted only once
//! its own child answers `status` ([`await_own_daemon`]). A fixed per-binary
//! port range is unique only inside one process: two processes running the
//! same binary — nextest's process-per-test, or two worktrees testing at once —
//! collided, the loser's daemon died on bind, and its client talked to the
//! winner's daemon ("belongs to a different HOME", "daemon never came up").
//!
//! The same rules live in two other copies that cannot share this file:
//! `ac-daemon/tests/common/mod.rs` (it resolves the daemon through
//! `env!("CARGO_BIN_EXE_ac-daemon")`, which only compiles inside that package)
//! and `ac-view/tests/support.rs`. The three must stay in step: OS ports, a
//! pid check, a HOME check, a bounded retry that kills the loser.

#![allow(dead_code)]

use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

static HOME_CURSOR: AtomicU32 = AtomicU32::new(0);

/// How many port pairs [`spawn_daemon`] tries before it panics.
///
/// Provenance: assumed. Same value as `ac-daemon`'s `SPAWN_ATTEMPTS`, which
/// covers well over a hundred daemons per run; this crate starts about ten.
/// A collision needs the OS to hand an ephemeral port that another process
/// takes before our daemon binds it, so five independent draws in a row all
/// losing points at something other than a race.
pub const SPAWN_ATTEMPTS: usize = 5;

/// How long one attempt waits for a `status` reply.
///
/// Provenance: assumed. The value every per-binary `wait_until_up` in this
/// crate already used. It is a deadline, not a delay: the loop returns on the
/// first reply, so it only lengthens the failure path.
pub const READY_TIMEOUT: Duration = Duration::from_secs(10);

/// Path to a sibling binary in the same target dir as this test's own
/// executable. `CARGO_BIN_EXE_ac` covers `ac` (same package), but
/// `ac-daemon` lives in another package and gets no such variable.
pub fn sibling_binary(name: &str) -> PathBuf {
    let exe = std::env::current_exe().expect("test exe path");
    // target/debug/deps/<test>-<hash> → target/debug/<name>
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

/// A scratch HOME unique within this process, named `{prefix}-{pid}-{n}`.
/// Not derived from the port: two concurrent spawns can be handed the same
/// port, and they must still not share a HOME.
pub fn alloc_home(prefix: &str) -> PathBuf {
    let n = HOME_CURSOR.fetch_add(1, Ordering::Relaxed);
    let home = std::env::temp_dir().join(format!("{prefix}-{}-{n}", std::process::id()));
    fs::create_dir_all(home.join(".config").join("ac")).expect("create scratch HOME");
    home
}

/// Two OS-assigned free ports. The listeners are dropped before the daemon
/// binds, so another process can take either port in between; that race is
/// what [`await_own_daemon`] detects and [`spawn_daemon`] retries.
pub fn alloc_ports() -> (u16, u16) {
    let port = || {
        TcpListener::bind("127.0.0.1:0")
            .expect("bind ephemeral port")
            .local_addr()
            .expect("local_addr")
            .port()
    };
    let ctrl = port();
    let mut data = port();
    while data == ctrl {
        data = port();
    }
    (ctrl, data)
}

/// One `status` request to `host:ctrl`; `None` when nothing answered.
fn probe_status(ctx: &zmq::Context, host: &str, ctrl: u16, timeout_ms: i32) -> Option<Value> {
    let s = ctx.socket(zmq::REQ).unwrap();
    s.set_linger(0).ok();
    s.set_rcvtimeo(timeout_ms).ok();
    s.set_sndtimeo(timeout_ms).ok();
    s.connect(&format!("tcp://{host}:{ctrl}")).ok()?;
    s.send(br#"{"cmd":"status"}"#.as_ref(), 0).ok()?;
    let bytes = s.recv_bytes(0).ok()?;
    Some(serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// Wait until `host:ctrl` is answered by `child` itself, running under `home`.
///
/// Retryable (`Err`): the child exited first, the reply's `pid` is not the
/// child's, the reply's `home` is not `home`, or nothing answered before
/// [`READY_TIMEOUT`]. `home` is compared as a string with no normalisation —
/// the same comparison `ac-cli/src/spawn.rs` makes. A reply with no `pid` or
/// `home` panics: without them no spawn can tell its own daemon apart.
fn await_own_daemon(child: &mut Child, host: &str, ctrl: u16, home: &str) -> Result<(), String> {
    let deadline = Instant::now() + READY_TIMEOUT;
    let ctx = zmq::Context::new();
    loop {
        if Instant::now() > deadline {
            return Err(format!(
                "no status reply on ctrl {ctrl} within {READY_TIMEOUT:?}"
            ));
        }
        if let Ok(Some(st)) = child.try_wait() {
            return Err(format!("daemon exited before serving ({st})"));
        }
        thread::sleep(Duration::from_millis(50));

        let Some(reply) = probe_status(&ctx, host, ctrl, 300) else {
            continue;
        };
        let pid = reply["pid"]
            .as_u64()
            .unwrap_or_else(|| panic!("status carried no pid: {reply}"));
        let their_home = reply["home"]
            .as_str()
            .unwrap_or_else(|| panic!("status carried no home: {reply}"));
        if pid != u64::from(child.id()) {
            return Err(format!(
                "ctrl {ctrl} answered by pid {pid}, not our {}",
                child.id()
            ));
        }
        if their_home != home {
            return Err(format!(
                "ctrl {ctrl} answered with HOME {their_home}, not our {home}"
            ));
        }
        return Ok(());
    }
}

/// A daemon this test started. Dropping it kills and reaps the daemon,
/// including during a panic unwind. Removing the scratch HOME stays with the
/// caller.
pub struct DaemonGuard {
    child: Child,
    probe_host: String,
    pub ctrl: u16,
    pub data: u16,
    /// Port pairs dropped before this one was accepted.
    pub retries: usize,
}

impl DaemonGuard {
    /// Kill and reap the daemon now. Idempotent; `Drop` calls it too. For
    /// owners whose own `Drop` must stop the daemon before removing its HOME.
    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// The daemon process's own pid.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// A fresh `status` reply from this guard's ctrl port, if anything answers.
    pub fn status(&self) -> Option<Value> {
        probe_status(&zmq::Context::new(), &self.probe_host, self.ctrl, 1_000)
    }
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Start `ac-daemon --fake-audio` under `home` and return once it is the one
/// answering. `local` adds `--local`; `probe_host` is where readiness is
/// probed. `first_ports`, when set, is used for attempt 0; every retry draws
/// from [`alloc_ports`]. Each dropped pair is killed, reaped and reported on
/// stderr. Panics after [`SPAWN_ATTEMPTS`] failed pairs.
pub fn spawn_daemon(
    home: &Path,
    local: bool,
    probe_host: &str,
    first_ports: Option<(u16, u16)>,
) -> DaemonGuard {
    let home_str = home.to_str().expect("scratch HOME is UTF-8");
    let mut last = String::new();
    for attempt in 0..SPAWN_ATTEMPTS {
        let (ctrl, data) = match first_ports {
            Some(p) if attempt == 0 => p,
            _ => alloc_ports(),
        };
        let mut cmd = Command::new(sibling_binary("ac-daemon"));
        cmd.env("HOME", home).arg("--fake-audio");
        if local {
            cmd.arg("--local");
        }
        let mut child = cmd
            .args([
                "--ctrl-port",
                &ctrl.to_string(),
                "--data-port",
                &data.to_string(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn ac-daemon");

        match await_own_daemon(&mut child, probe_host, ctrl, home_str) {
            Ok(()) => {
                return DaemonGuard {
                    child,
                    probe_host: probe_host.to_string(),
                    ctrl,
                    data,
                    retries: attempt,
                }
            }
            Err(why) => {
                let _ = child.kill();
                let _ = child.wait();
                eprintln!(
                    "spawn_daemon: dropped ports ctrl {ctrl} / data {data} \
                     (attempt {}/{SPAWN_ATTEMPTS}): {why}",
                    attempt + 1
                );
                last = why;
            }
        }
    }
    panic!("daemon never came up after {SPAWN_ATTEMPTS} attempts: {last}");
}
