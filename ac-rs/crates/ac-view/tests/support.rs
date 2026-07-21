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
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU16, AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

static PORT_CURSOR: AtomicU16 = AtomicU16::new(29_400);
static HOME_CURSOR: AtomicU32 = AtomicU32::new(0);

pub fn alloc_ports() -> (u16, u16) {
    let base = PORT_CURSOR.fetch_add(2, Ordering::Relaxed);
    (base, base + 1)
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
    let candidate = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../target"))
        .join(profile)
        .join("ac-daemon");
    assert!(
        candidate.exists(),
        "ac-daemon binary not found at {} — run `cargo build -p ac-daemon` first",
        candidate.display()
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
