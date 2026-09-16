//! End-to-end coverage for #459's fixed emission maximum and unified level
//! display. A successful run prints the emitted level, its provenance, and
//! the maximum reported by the daemon; an over-maximum request is refused.
//!
//! Its own small `Rig` rather than `it_plot_ir.rs`'s, which seeds a report
//! directory these tests do not need; the daemon plumbing is shared through
//! `support`.

mod support;

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

struct Rig {
    daemon: support::DaemonGuard,
    home: PathBuf,
    ctrl: u16,
    data: u16,
}

impl Rig {
    fn start() -> Self {
        let home = support::alloc_home("ac-cli-level-it");
        let cfg_dir = home.join(".config").join("ac");
        fs::write(cfg_dir.join("config.json"), b"{}\n").expect("seed config.json");

        let daemon = support::spawn_daemon(&home, true, "127.0.0.1", None);
        let (ctrl, data) = (daemon.ctrl, daemon.data);
        Self {
            daemon,
            home,
            ctrl,
            data,
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
        // Runs before the fields drop, so kill the daemon here first.
        self.daemon.kill();
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
