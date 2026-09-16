//! Shared test-only daemon-spawning harness for `ac-view`'s
//! integration tests. `CARGO_BIN_EXE_ac-daemon` isn't available here
//! (that env var only resolves for binaries of the package under test,
//! not a sibling workspace crate — confirmed empirically, not
//! assumed), so the binary is located via the workspace's shared
//! `target/` directory instead, matching whichever profile this test
//! binary itself was built with.
//!
//! Spawn identity rules (#486) match `ac-cli/tests/support/mod.rs` and
//! `ac-daemon/tests/common/mod.rs`, which cannot share this file: OS ports,
//! a pid check, a HOME check, a bounded retry that kills the loser. Keep the
//! three in step.

#![allow(dead_code)]

use std::env;
use std::fs;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

static HOME_CURSOR: AtomicU32 = AtomicU32::new(0);

/// Two **OS-assigned** free ports (#195). A shared/derived port base
/// collided across the three daemon-spawning ac-view binaries under
/// parallel `cargo test` — statics are per-process, and any deterministic
/// base (a literal, or a `pid % N` seed) can hand two concurrent binaries
/// the same range. Binding `:0` lets the OS pick a currently-free port,
/// with no modulo to alias on. The listeners are dropped before the
/// daemon rebinds, leaving a small TOCTOU window; ephemeral ports are
/// assigned round-robin over a large range, so immediate reuse by another
/// process is vanishingly unlikely — strictly better than a base that can
/// alias deterministically.
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
    // Guard the (rare) case the OS handed the same port twice across the
    // two independent binds.
    while data == ctrl {
        data = port();
    }
    (ctrl, data)
}

pub fn alloc_home() -> PathBuf {
    let n = HOME_CURSOR.fetch_add(1, Ordering::Relaxed);
    let mut p = env::temp_dir();
    p.push(format!("ac-view-it-{}-{n}", std::process::id()));
    let _ = fs::create_dir_all(p.join(".config").join("ac"));
    p
}

fn ac_daemon_bin() -> PathBuf {
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    // Honour CARGO_TARGET_DIR when the caller set it (#314) — `bin/*.sh`
    // agent scripts export it (bin/common.sh) to keep worktrees isolated,
    // which redirects build output away from the hardcoded
    // `<manifest_dir>/../../target` guess below.
    let target_root = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../target")));
    let candidate = target_root.join(profile).join("ac-daemon");
    assert!(
        candidate.exists(),
        "ac-daemon binary not found at {} — resolved from {} ({}); build it with `cargo build -p ac-daemon`",
        candidate.display(),
        if env::var_os("CARGO_TARGET_DIR").is_some() {
            "CARGO_TARGET_DIR"
        } else {
            "default workspace target dir"
        },
        target_root.display()
    );
    candidate
}

/// How many port pairs [`DaemonProcess::spawn_at_home`] tries before it
/// panics. Provenance: assumed — the same value as `ac-daemon`'s and
/// `ac-cli`'s `SPAWN_ATTEMPTS`.
const SPAWN_ATTEMPTS: usize = 5;

/// How long one attempt waits for a `status` reply. Provenance: assumed —
/// the value `ac-cli`'s and `ac-daemon`'s harnesses use. A deadline, not a
/// delay: it only lengthens the failure path.
const READY_TIMEOUT: Duration = Duration::from_secs(10);

/// Wait until `ctrl` is answered by `child` itself, running under `home`.
///
/// Retryable (`Err`): the child exited first, the reply's `pid` is not the
/// child's, the reply's `home` is not `home` (plain string comparison, as in
/// `ac-cli/src/spawn.rs`), or nothing answered before [`READY_TIMEOUT`]. A
/// reply with no `pid` or `home` panics.
fn await_own_daemon(child: &mut Child, ctrl: u16, home: &str) -> Result<(), String> {
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
        let s = ctx.socket(zmq::REQ).unwrap();
        s.set_linger(0).ok();
        s.set_rcvtimeo(300).ok();
        s.set_sndtimeo(300).ok();
        if s.connect(&format!("tcp://127.0.0.1:{ctrl}")).is_err() {
            continue;
        }
        if s.send(br#"{"cmd":"status"}"#.as_ref(), 0).is_err() {
            continue;
        }
        let Ok(bytes) = s.recv_bytes(0) else { continue };
        let reply: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
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

pub struct DaemonProcess {
    child: Child,
    pub ctrl_port: u16,
    pub data_port: u16,
    home: PathBuf,
}

impl DaemonProcess {
    pub fn spawn() -> Self {
        Self::spawn_at_home(alloc_home())
    }

    pub fn spawn_at_home(home: PathBuf) -> Self {
        let home_str = home.to_str().expect("scratch HOME is UTF-8").to_string();
        let mut last = String::new();
        for attempt in 0..SPAWN_ATTEMPTS {
            let (ctrl, data) = alloc_ports();
            let mut child = Command::new(ac_daemon_bin())
                .env("HOME", &home)
                .args([
                    "--fake-audio",
                    "--local",
                    "--ctrl-port",
                    &ctrl.to_string(),
                    "--data-port",
                    &data.to_string(),
                ])
                .spawn()
                .expect("spawn ac-daemon");

            match await_own_daemon(&mut child, ctrl, &home_str) {
                Ok(()) => {
                    return Self {
                        child,
                        ctrl_port: ctrl,
                        data_port: data,
                        home,
                    }
                }
                Err(why) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    eprintln!(
                        "spawn_at_home: dropped ports ctrl {ctrl} / data {data} \
                         (attempt {}/{SPAWN_ATTEMPTS}): {why}",
                        attempt + 1
                    );
                    last = why;
                }
            }
        }
        panic!("daemon never came up after {SPAWN_ATTEMPTS} attempts: {last}");
    }
}

impl Drop for DaemonProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.home);
    }
}
