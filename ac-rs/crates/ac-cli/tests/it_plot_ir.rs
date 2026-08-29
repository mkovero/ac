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
    // `window_len` is 4096, not the smaller windows this fixture used
    // before #376: a short gate window leaves too few pre-impulse
    // samples for a clean floor estimate even on this noise-free fake
    // loopback (measured ~17.5 dB with a 1024-sample window, under the
    // 18.0 dB threshold #376 added) — 4096 samples clears it with
    // margin (~27 dB) so this fixture still exercises the passing path.
    let stdout = rig.run_ac(&[
        "plot", "ir", "200hz", "8000hz", "0.5s", "-6dbfs", "3harm", "4096win", "0.1s",
    ]);

    // ── printed arrival is onset-derived, not peak-derived (#346) ─────
    // The gate re-centres the linear IR on the sweep endpoint, so the
    // fake backend's 32-sample loopback still shows up as the *peak's*
    // offset from centre — but `arrival` now comes from
    // `estimate_onset`, which under #378 picks the change point in the
    // IR's own variance over a window ending at the peak. No geometry is
    // recorded for this run, so no causal bound applies and the pick can
    // legitimately land before the peak — and on this fixture it lands
    // well before it, inside the bandlimited deconvolution's pre-ring
    // (a 200 Hz lower sweep limit puts the main lobe's leading skirt
    // ~240 samples wide at 48 kHz). That is the case the causal bound
    // exists to reject, and there is none here.
    let arrival = printed_arrival_samples(&stdout);
    // Pinned exact, not just bounded (QA on #352): this fixture's IR
    // shape is deterministic (fake backend, fixed 200hz/8000hz/4096win
    // sweep), so the estimator's answer for it is a known, computable
    // value — pinning it catches a regression in the estimator's exact
    // behaviour, not only a sign or bound violation. Recompute if the
    // fixture's stimulus parameters above ever change. Value moved
    // 19 → 17 under #353 (median floor) and 17 → -86 under #378: the
    // level crossing stopped where the skirt fell below floor+12 dB,
    // while the change-point pick keys on where the skirt's variance
    // leaves the numerical floor, which on a 200 Hz-limited sweep is
    // much earlier. Expected per the #378 design decision — the pick is
    // deliberately not amplitude-referenced — not a regression.
    const EXPECTED_ONSET_SAMPLES: i64 = -86;
    assert_eq!(
        arrival, EXPECTED_ONSET_SAMPLES,
        "printed arrival should be the fake backend's exact onset sample \
         for this fixture, not just somewhere in (0, {FAKE_LOOPBACK_DELAY_SAMPLES}]:\n{stdout}"
    );

    // ── the rest of the printed summary ───────────────────────────────
    for want in ["peak", "pre-imp SNR", "gate", "f_low"] {
        assert!(
            stdout.contains(want),
            "printed summary missing {want:?}:\n{stdout}"
        );
    }
    // #346 AC4 / #378: the onset rule must reach the terminal, and the
    // peak line must carry the onset-to-peak distance now that the two
    // can legitimately disagree — a silent divergence between them would
    // read as a bug in the tool rather than the fix working.
    assert!(
        stdout.contains("onset: AIC change-point pick, 10.0 ms window"),
        "printed summary missing the onset rule line (AC4):\n{stdout}"
    );
    assert!(
        stdout.contains("(search span, no geometry known)"),
        "printed summary missing the window-start clause (AC4):\n{stdout}"
    );
    assert!(
        stdout.contains("diagnostic \u{2014} arrival is onset-derived, 118 samples earlier"),
        "peak line must carry the onset-to-peak distance (#378):\n{stdout}"
    );
    assert!(
        !stdout.contains("median floor"),
        "the pre-#378 rule's text must not survive anywhere:\n{stdout}"
    );
    // #391: no distance figure prints at all — the ms → m conversion it
    // came from is gone, and milliseconds are what's asserted above.
    assert!(
        !stdout.contains("distance "),
        "distance must not print — the ms \u{2192} m conversion was removed:\n{stdout}"
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
    assert!((gate.f_low_hz - 48_000.0 / 4096.0).abs() < 1e-6);

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

/// #376: a capture whose pre-impulse SNR does not clear the threshold is
/// reported as a failed deconvolution, not as a result with a number in
/// it — a short (1024-sample) gate window leaves too few pre-impulse
/// samples for a clean floor estimate even on this noise-free fake
/// loopback (measured ~17.5 dB, under the 18.0 dB threshold), so it
/// reliably exercises the failure path without hardware.
#[test]
fn plot_ir_reports_low_pre_impulse_snr_as_a_failed_deconvolution() {
    let rig = Rig::start();
    let stdout = rig.run_ac(&[
        "plot", "ir", "200hz", "8000hz", "0.5s", "-6dbfs", "3harm", "1024win", "0.1s",
    ]);

    assert!(
        stdout.contains("DECONVOLUTION FAILED"),
        "expected a failed-deconvolution banner:\n{stdout}"
    );
    assert!(
        stdout.contains("check: drive level, mic gain, distance, room noise"),
        "banner must name what to check:\n{stdout}"
    );
    // The exact plausible-looking-wrong-number shape #376 exists to
    // close: neither line may print on a failed verdict.
    assert!(
        !stdout.contains("arrival "),
        "arrival must not print on a failed verdict:\n{stdout}"
    );
    assert!(
        !stdout.contains("distance "),
        "distance must not print on a failed verdict:\n{stdout}"
    );
    assert!(
        stdout.contains("diagnostic only"),
        "the one remaining peak number must be labelled diagnostic:\n{stdout}"
    );
    assert!(
        stdout.contains("required \u{2265} 18.0 dB"),
        "pre-imp SNR line must state the threshold it failed against:\n{stdout}"
    );

    let dir = rig.report_dir();
    let jsons = files_with_extension(&dir, "json");
    assert_eq!(jsons.len(), 1, "expected one report JSON, got {jsons:?}");
    let json = fs::read_to_string(&jsons[0]).expect("read report json");
    let report: ac_core::measurement::report::MeasurementReport =
        serde_json::from_str(&json).expect("persisted report must decode");
    let stats = report.ir_stats().expect("persisted IR payload");
    assert!(
        matches!(
            stats.verdict,
            ac_core::measurement::report::IrVerdict::Failed { .. }
        ),
        "persisted report's own ir_stats() must agree with the printed verdict: {:?}",
        stats.verdict
    );
}
