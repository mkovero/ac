//! End-to-end coverage for #380 (PR #395's QA finding): the CLI must read
//! the *applied* level back off a clamped sync ack and print the
//! reconciling line, per #360's UX mockups. `it_plot_ir.rs`'s own tests
//! only ever run against the default `drive_max_dbfs` (-10 dBFS) with
//! requests already under it, so none of them exercise the clamp path this
//! issue is about — this file seeds a lower ceiling instead so a clamp
//! actually fires.
//!
//! Self-contained `Rig` rather than importing `it_plot_ir.rs`'s: there is
//! no shared test-support module in this crate to put one in without
//! restructuring a file this PR does not otherwise touch, and duplicating
//! ~60 lines of scratch-daemon plumbing is cheaper than that restructure.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
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
    /// Spawn `ac-daemon --fake-audio` with `drive_max_dbfs` set to
    /// `ceiling_dbfs` in its scratch `config.json`, so a request above the
    /// ceiling actually gets clamped.
    fn start_with_ceiling(ceiling_dbfs: f64) -> Self {
        let base = PORT_CURSOR.fetch_add(2, Ordering::Relaxed);
        let (ctrl, data) = (base, base + 1);
        let home =
            std::env::temp_dir().join(format!("ac-cli-clamp-it-{}-{base}", std::process::id()));
        let cfg_dir = home.join(".config").join("ac");
        fs::create_dir_all(&cfg_dir).expect("create scratch config dir");

        fs::write(
            cfg_dir.join("config.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "drive_max_dbfs": ceiling_dbfs,
            }))
            .unwrap(),
        )
        .expect("seed config.json");

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
    fn run_ac(&self, args: &[&str]) -> String {
        let out = Command::new(env!("CARGO_BIN_EXE_ac"))
            .env("HOME", &self.home)
            .env("AC_CTRL_PORT", self.ctrl.to_string())
            .env("AC_DATA_PORT", self.data.to_string())
            .current_dir(&self.home)
            .args(args)
            .output()
            .expect("run ac");
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

/// Scalar clamp, `plot ir` leg: requested -6 dBFS against a -30 dBFS
/// ceiling must print the reconciling line before the IR run starts.
#[test]
fn plot_ir_clamped_run_prints_the_reconciling_line() {
    let rig = Rig::start_with_ceiling(-30.0);
    let stdout = rig.run_ac(&[
        "plot", "ir", "200hz", "8000hz", "0.5s", "-6dbfs", "3harm", "4096win", "0.1s",
    ]);
    assert!(
        stdout
            .contains("level clamped to ceiling  -6.0 dBFS \u{2192} -30.0 dBFS  (drive_max_dbfs)"),
        "clamped plot_ir must print the reconciling line:\n{stdout}"
    );
}

/// Scalar clamp, unclamped path: ceiling above the requested level must
/// add no line at all — the byte-for-byte-identical acceptance criterion,
/// checked here against the real ack round trip rather than just the pure
/// renderer (see `commands/mod.rs`'s unit tests for that half).
#[test]
fn plot_ir_unclamped_run_prints_no_clamp_line() {
    let rig = Rig::start_with_ceiling(0.0); // ceiling above the requested -6.0 dBFS
    let stdout = rig.run_ac(&[
        "plot", "ir", "200hz", "8000hz", "0.5s", "-6dbfs", "3harm", "4096win", "0.1s",
    ]);
    assert!(
        !stdout.contains("level clamped"),
        "unclamped run must add no line:\n{stdout}"
    );
}

/// Range clamp, degenerate case: the whole requested range sits above the
/// ceiling and must collapse to the explicit flat annotation, not print as
/// an ordinary sweep.
#[test]
fn plot_level_degenerate_range_prints_the_flat_annotation() {
    let rig = Rig::start_with_ceiling(-30.0);
    let stdout = rig.run_ac(&["plot", "level", "-10dbfs", "6dbfs", "1000hz", "3steps"]);
    assert!(
        stdout.contains(
            "applied    -30.0 dBFS  (flat \u{2014} entire requested range exceeds ceiling)"
        ),
        "degenerate range must print the flat-collapse annotation:\n{stdout}"
    );
}

/// Range clamp, partial case: only the top of the range exceeds the
/// ceiling — the unmoved bound (start) must not be reported as the
/// ceiling value.
#[test]
fn plot_level_partial_clamp_reports_the_moved_bound_as_ceiling() {
    let rig = Rig::start_with_ceiling(-30.0);
    let stdout = rig.run_ac(&["plot", "level", "-40dbfs", "-20dbfs", "1000hz", "3steps"]);
    assert!(
        stdout.contains("level clamped to ceiling  (drive_max_dbfs -30.0 dBFS)"),
        "partial clamp must report the ceiling value, not the unmoved bound:\n{stdout}"
    );
    assert!(
        stdout.contains("applied    -40.0 \u{2192} -30.0 dBFS"),
        "partial clamp must leave the unmoved bound alone:\n{stdout}"
    );
}
