//! End-to-end coverage for #459's fixed emission maximum and unified level
//! display. A successful run prints the emitted level, its provenance, and
//! the maximum reported by the daemon; an over-maximum request is refused.
//!
//! Self-contained `Rig` rather than importing `it_plot_ir.rs`'s: there is
//! no shared test-support module in this crate to put one in without
//! restructuring a file this PR does not otherwise touch, and duplicating
//! ~60 lines of scratch-daemon plumbing is cheaper than that restructure.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};
use std::sync::atomic::{AtomicU16, Ordering};
use std::thread;
use std::time::{Duration, Instant};

static PORT_CURSOR: AtomicU16 = AtomicU16::new(26_500);

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

struct Rig {
    daemon: Child,
    home: PathBuf,
    ctrl: u16,
    data: u16,
}

impl Rig {
    fn start() -> Self {
        let base = PORT_CURSOR.fetch_add(2, Ordering::Relaxed);
        let (ctrl, data) = (base, base + 1);
        let home =
            std::env::temp_dir().join(format!("ac-cli-level-it-{}-{base}", std::process::id()));
        let cfg_dir = home.join(".config").join("ac");
        fs::create_dir_all(&cfg_dir).expect("create scratch config dir");

        fs::write(cfg_dir.join("config.json"), b"{}\n").expect("seed config.json");

        let daemon = Command::new(sibling_binary("ac-daemon"))
            .env("HOME", &home)
            .args([
                "--fake-audio",
                "--local",
                "--ctrl-port",
                &ctrl.to_string(),
                "--data-port",
                &data.to_string(),
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn ac-daemon");

        let rig = Self {
            daemon,
            home,
            ctrl,
            data,
        };
        rig.wait_until_up();
        rig
    }

    fn wait_until_up(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        let ctx = zmq::Context::new();
        loop {
            assert!(Instant::now() < deadline, "daemon never came up");
            thread::sleep(Duration::from_millis(50));
            let s = ctx.socket(zmq::REQ).unwrap();
            s.set_linger(0).ok();
            s.set_rcvtimeo(300).ok();
            s.set_sndtimeo(300).ok();
            if s.connect(&format!("tcp://127.0.0.1:{}", self.ctrl))
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

    /// Run the real `ac` binary against this rig, returning its stdout.
    /// `current_dir` pinned to the scratch home so `plot level`'s CSV
    /// export (no `session` configured → cwd, see `io::output_dir`) lands
    /// somewhere `Drop` cleans up instead of the crate's own directory.
    fn run_ac_raw(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_ac"))
            .env("HOME", &self.home)
            .env("AC_CTRL_PORT", self.ctrl.to_string())
            .env("AC_DATA_PORT", self.data.to_string())
            .current_dir(&self.home)
            .args(args)
            .output()
            .expect("run ac")
    }

    fn run_ac(&self, args: &[&str]) -> String {
        let out = self.run_ac_raw(args);
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        assert!(
            out.status.success(),
            "`ac {}` failed ({}):\nstdout:\n{stdout}\nstderr:\n{}",
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr),
        );
        stdout
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
        let _ = fs::remove_dir_all(&self.home);
    }
}

#[test]
fn plot_ir_typed_run_prints_level_origin_and_maximum() {
    let rig = Rig::start();
    let stdout = rig.run_ac(&[
        "plot", "ir", "200hz", "8000hz", "0.5s", "-20dbfs", "3harm", "4096win", "0.1s",
    ]);
    assert!(
        stdout.contains("level       -20.0 dBFS  (typed)"),
        "typed plot_ir must identify its level:\n{stdout}"
    );
    assert!(
        stdout.contains("maximum       0.0 dBFS  (full scale)"),
        "plot_ir must print the daemon-reported maximum:\n{stdout}"
    );
}

#[test]
fn plot_level_default_run_prints_named_range_and_maximum() {
    let rig = Rig::start();
    let stdout = rig.run_ac(&["plot", "level", "1000hz", "3steps"]);
    assert!(
        stdout.contains("level       -40.0 \u{2192} -30.0 dBFS  (default)"),
        "default ramp must print its named range:\n{stdout}"
    );
    assert!(
        stdout.contains("maximum       0.0 dBFS  (full scale)"),
        "{stdout}"
    );
}

#[test]
fn over_maximum_run_is_refused_without_a_success_level_block() {
    let rig = Rig::start();
    let out = rig.run_ac_raw(&["generate", "sine", "1000hz", "1dbfs"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(!out.status.success(), "over-maximum request succeeded");
    assert!(
        stderr.contains("level +1.0 dBFS is above full scale (0.0 dBFS)"),
        "refusal must name the ceiling:\n{stderr}"
    );
    assert!(
        stderr.contains("nothing was emitted"),
        "refusal must state its side effect:\n{stderr}"
    );
    assert!(
        !stdout.contains("level      "),
        "a refusal must not print a successful level block:\n{stdout}"
    );
}
