//! Shared test-only daemon-spawning harness for `ac-view`'s
//! integration tests. `CARGO_BIN_EXE_ac-daemon` isn't available here
//! (that env var only resolves for binaries of the package under test,
//! not a sibling workspace crate — confirmed empirically, not
//! assumed), so the binary is located via the workspace's shared
//! `target/` directory instead, matching whichever profile this test
//! binary itself was built with.

#![allow(dead_code)]

use std::env;
use std::fs;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

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
        let (ctrl, data) = alloc_ports();
        let child = Command::new(ac_daemon_bin())
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

        let deadline = Instant::now() + Duration::from_secs(3);
        let ctx = zmq::Context::new();
        loop {
            if Instant::now() > deadline {
                panic!("daemon never came up");
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
            if s.recv_bytes(0).is_ok() {
                break;
            }
        }

        Self {
            child,
            ctrl_port: ctrl,
            data_port: data,
            home,
        }
    }
}

impl Drop for DaemonProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.home);
    }
}
