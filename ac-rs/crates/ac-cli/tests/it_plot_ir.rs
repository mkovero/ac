//! End-to-end `ac plot ir` against a real `ac-daemon --fake-audio`
//! (issue #283).
//!
//! Unlike the daemon's own `it_protocol.rs` tests, this drives the actual
//! `ac` binary and reads its **stdout** — the acceptance criterion is
//! about what an operator sees printed, and a wire-level assertion cannot
//! tell whether the CLI dropped the frame on the floor (which is exactly
//! the bug #283 was filed for).
//!
//! The daemon is spawned here rather than auto-spawned by `ac`, because
//! auto-spawn never passes `--fake-audio`. `AC_CTRL_PORT`/`AC_DATA_PORT`
//! point both processes at the same private port pair, so the test cannot
//! silently pass by talking to a developer's real daemon on 5556.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU16, Ordering};
use std::thread;
use std::time::{Duration, Instant};

/// Fake backend's fixed loopback delay — `FAKE_LOOPBACK_DELAY_SAMPLES` in
/// `ac-daemon/src/audio/fake.rs`, at the fake backend's 48 kHz.
const FAKE_LOOPBACK_DELAY_SAMPLES: i64 = 32;

static PORT_CURSOR: AtomicU16 = AtomicU16::new(26_400);

/// Path to a sibling binary in the same target dir as this test's own
/// executable. `CARGO_BIN_EXE_ac` covers `ac` (same package), but
/// `ac-daemon` lives in another package and gets no such variable.
fn sibling_binary(name: &str) -> PathBuf {
    let exe = std::env::current_exe().expect("test exe path");
    // target/debug/deps/it_plot_ir-<hash> → target/debug/<name>
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
        let home = std::env::temp_dir().join(format!("ac-cli-it-{}-{base}", std::process::id()));
        let cfg_dir = home.join(".config").join("ac");
        fs::create_dir_all(&cfg_dir).expect("create scratch config dir");
        let report_dir = home.join("reports");
        fs::create_dir_all(&report_dir).expect("create report dir");

        // `report_dir` is the whole point of two acceptance criteria, so
        // it is configured rather than left to the default.
        fs::write(
            cfg_dir.join("config.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "report_dir": report_dir.to_str().unwrap(),
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

    fn report_dir(&self) -> PathBuf {
        self.home.join("reports")
    }

    /// Run the real `ac` binary against this rig, returning its stdout.
    fn run_ac(&self, args: &[&str]) -> String {
        let out = Command::new(env!("CARGO_BIN_EXE_ac"))
            .env("HOME", &self.home)
            .env("AC_CTRL_PORT", self.ctrl.to_string())
            .env("AC_DATA_PORT", self.data.to_string())
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

/// Files written under `dir` whose name ends in `ext`.
fn files_with_extension(dir: &Path, ext: &str) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .expect("read report dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some(ext))
        .collect()
}

/// Pull the signed sample count out of the printed arrival line, e.g.
/// `  arrival       +32 samples  (+0.667 ms re gate centre @ 48000 Hz)`.
fn printed_arrival_samples(stdout: &str) -> i64 {
    let line = stdout
        .lines()
        .find(|l| l.trim_start().starts_with("arrival"))
        .unwrap_or_else(|| panic!("no arrival line printed:\n{stdout}"));
    let field = line
        .split_whitespace()
        .nth(1)
        .unwrap_or_else(|| panic!("arrival line has no value: {line:?}"));
    field
        .trim_start_matches('+')
        .parse::<i64>()
        .unwrap_or_else(|_| panic!("arrival value not an integer: {field:?}"))
}

/// The whole of #283 in one run: `ac plot ir` must print its result and
/// persist it. Before the fix the command printed nothing past "Running
/// IR measurement..." and wrote no report at all.
#[test]
fn plot_ir_prints_the_arrival_and_persists_json_and_csv() {
    let rig = Rig::start();
    // Unit-suffixed positional form (see `parse/plot.rs`): f1, f2,
    // duration, level, n_harmonics, window_len, then tail as the second
    // `Time` token.
    let stdout = rig.run_ac(&[
        "plot", "ir", "200hz", "8000hz", "0.5s", "-6dbfs", "3harm", "1024win", "0.1s",
    ]);

    // ── printed arrival matches the fake backend's known delay ────────
    // The gate re-centres the linear IR on the sweep endpoint, so a
    // 32-sample loopback shows up as a +32-sample offset from centre.
    // Tolerance is for finite-window deconvolution, not for the constant:
    // a regression that lost the delay entirely would read 0.
    let arrival = printed_arrival_samples(&stdout);
    assert!(
        (arrival - FAKE_LOOPBACK_DELAY_SAMPLES).abs() <= 8,
        "printed arrival {arrival} samples, expected ~{FAKE_LOOPBACK_DELAY_SAMPLES} \
         (fake loopback delay):\n{stdout}"
    );

    // ── the rest of the printed summary ───────────────────────────────
    for want in ["peak", "pre-imp SNR", "gate", "f_low"] {
        assert!(
            stdout.contains(want),
            "printed summary missing {want:?}:\n{stdout}"
        );
    }
    // No τ is calibrated in this rig, so distance must be refused by
    // name — never derived from the uncorrected arrival above.
    assert!(
        stdout.contains("distance      unavailable"),
        "distance must be stated unavailable without \u{3c4}:\n{stdout}"
    );
    // ISO 18233 §B.5: the reader must be told the tail is an artefact.
    assert!(
        stdout.contains("linear-deconvolution artefact"),
        "printed summary missing the \u{a7}B.5 tail-artefact statement:\n{stdout}"
    );

    // ── files on disk ─────────────────────────────────────────────────
    let dir = rig.report_dir();
    let jsons = files_with_extension(&dir, "json");
    let csvs = files_with_extension(&dir, "csv");
    assert_eq!(jsons.len(), 1, "expected one report JSON, got {jsons:?}");
    assert_eq!(csvs.len(), 1, "expected one report CSV, got {csvs:?}");

    let json = fs::read_to_string(&jsons[0]).expect("read report json");
    let report: ac_core::measurement::report::MeasurementReport =
        serde_json::from_str(&json).expect("persisted report must decode");
    assert_eq!(
        report.schema_version,
        ac_core::measurement::report::SCHEMA_VERSION
    );
    // The persisted report must carry the same artefact statement the
    // summary printed — a reader of the archive alone gets it too.
    assert!(
        report
            .notes
            .as_deref()
            .unwrap_or_default()
            .contains("linear-deconvolution artefact"),
        "persisted notes missing the \u{a7}B.5 statement: {:?}",
        report.notes
    );
    // And the same arrival, from the same accessor the CLI printed from.
    let stats = report.ir_stats().expect("persisted IR payload");
    assert_eq!(
        stats.delay_samples, arrival,
        "printed arrival and archived arrival disagree"
    );
    // The gate is recorded, not left for a reader to infer (#280).
    let gate = report.data[0].gate.as_ref().expect("gate recorded");
    assert_eq!(gate.window_kind, "rectangular");
    assert!((gate.f_low_hz - 48_000.0 / 1024.0).abs() < 1e-6);

    let csv = fs::read_to_string(&csvs[0]).expect("read report csv");
    assert!(
        csv.contains("sample_idx,time_s,order,amplitude"),
        "CSV is not the IR branch's output:\n{csv}"
    );

    // ── `ac report <path.json>` renders it with no further work ───────
    let rendered = rig.run_ac(&["report", jsons[0].to_str().unwrap()]);
    assert!(
        !rendered.trim().is_empty(),
        "ac report produced nothing for {}",
        jsons[0].display()
    );
}
