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

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Fake backend's fixed loopback delay — `FAKE_LOOPBACK_DELAY_SAMPLES` in
/// `ac-daemon/src/audio/fake.rs`, at the fake backend's 48 kHz.
const FAKE_LOOPBACK_DELAY_SAMPLES: i64 = 32;

struct Rig {
    daemon: support::DaemonGuard,
    home: PathBuf,
    ctrl: u16,
    data: u16,
}

impl Rig {
    fn start() -> Self {
        Self::start_with(serde_json::json!({}))
    }

    /// [`Self::start`] with extra config keys merged in (#460: a reference
    /// pair for the same-capture reference leg).
    fn start_with(extra_config: serde_json::Value) -> Self {
        let home = support::alloc_home("ac-cli-it");
        let cfg_dir = home.join(".config").join("ac");
        let report_dir = home.join("reports");
        fs::create_dir_all(&report_dir).expect("create report dir");

        // `report_dir` is the whole point of two acceptance criteria, so
        // it is configured rather than left to the default.
        let mut config = serde_json::json!({
            "report_dir": report_dir.to_str().unwrap(),
        });
        if let (Some(c), Some(e)) = (config.as_object_mut(), extra_config.as_object()) {
            for (k, v) in e {
                c.insert(k.clone(), v.clone());
            }
        }
        fs::write(
            cfg_dir.join("config.json"),
            serde_json::to_vec_pretty(&config).unwrap(),
        )
        .expect("seed config.json");

        let daemon = support::spawn_daemon(&home, true, "127.0.0.1", None);
        let (ctrl, data) = (daemon.ctrl, daemon.data);
        Self {
            daemon,
            home,
            ctrl,
            data,
        }
    }

    fn report_dir(&self) -> PathBuf {
        self.home.join("reports")
    }

    /// Run the real `ac` binary against this rig in `cwd`, whatever its
    /// exit status.
    fn ac_output_in(&self, cwd: &Path, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_ac"))
            .env("HOME", &self.home)
            .env("AC_CTRL_PORT", self.ctrl.to_string())
            .env("AC_DATA_PORT", self.data.to_string())
            .current_dir(cwd)
            .args(args)
            .output()
            .expect("run ac")
    }

    /// Run the real `ac` binary against this rig, returning its stdout.
    fn run_ac(&self, args: &[&str]) -> String {
        self.run_ac_in(&self.home, args)
    }

    /// [`Self::run_ac`] from a chosen working directory.
    fn run_ac_in(&self, cwd: &Path, args: &[&str]) -> String {
        let out = self.ac_output_in(cwd, args);
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

/// Metre figures printed in `stdout`: a number immediately followed by the
/// word `m` (`m`, `m,` or `m)`), so `c 343.0 m/s` is not one. #391 removed
/// every distance read-out derived from a time; since #460 the only metre
/// figure allowed is the distance the operator typed, echoed back.
fn metre_figures(stdout: &str) -> Vec<String> {
    let mut found = Vec::new();
    for line in stdout.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        for (i, w) in words.iter().enumerate() {
            let number = w.trim_start_matches('(');
            let unit = words.get(i + 1).copied().unwrap_or_default();
            if number.parse::<f64>().is_ok() && matches!(unit, "m" | "m," | "m)") {
                found.push(number.to_string());
            }
        }
    }
    found
}

/// The #391 guard must still go red if a distance read-out derived from a
/// time comes back, whatever it is phrased as: tested against the rejected
/// pre-#391 read-out, and against the #460 lines it has to admit.
#[test]
fn metre_figure_guard_catches_a_derived_distance_and_admits_the_typed_one() {
    assert_eq!(
        metre_figures("  distance      1.199 m  (c = 343.2 m/s)"),
        vec!["1.199".to_string()],
        "the pre-#391 distance read-out must be caught"
    );
    assert!(metre_figures("  distance      not given").is_empty());
    assert!(metre_figures(
        "                       no causal bound — distance not given (token: 1m)"
    )
    .is_empty());
    assert_eq!(
        metre_figures(
            "                       bound from ref latency + 0.05 m, c 343.0 m/s assumed"
        ),
        vec!["0.05".to_string()]
    );
}

/// #424: backend provenance must survive in the archived files themselves,
/// independently of the transient ZMQ envelope that announced each report.
#[test]
fn persisted_plot_artifacts_identify_the_live_backend() {
    let rig = Rig::start();

    let _ = rig.run_ac(&["plot", "200hz", "400hz", "-20dbfs", "1ppd", "3bpo"]);
    let _ = rig.run_ac(&[
        "plot", "ir", "200hz", "8000hz", "0.5s", "-20dbfs", "3harm", "4096win", "0.1s",
    ]);

    let dir = rig.report_dir();
    for suffix in ["-plot.json", "-plot-bands.json", "-plot_ir.json"] {
        let path = fs::read_dir(&dir)
            .expect("read report dir")
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .find(|path| path.to_string_lossy().ends_with(suffix))
            .unwrap_or_else(|| panic!("missing {suffix} in {}", dir.display()));
        let report: ac_core::measurement::report::MeasurementReport = serde_json::from_str(
            &fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display())),
        )
        .unwrap_or_else(|e| panic!("decode {}: {e}", path.display()));
        assert_eq!(
            report.backend.as_deref(),
            Some("fake"),
            "{}",
            path.display()
        );
    }

    let csv_path = fs::read_dir(&dir)
        .expect("read report dir")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .find(|path| path.to_string_lossy().ends_with("-plot_ir.csv"))
        .unwrap_or_else(|| panic!("missing plot_ir CSV in {}", dir.display()));
    let csv = fs::read_to_string(&csv_path).expect("read plot_ir CSV");
    assert!(
        csv.starts_with("# backend: fake\n"),
        "{} lacks fake backend provenance:\n{csv}",
        csv_path.display()
    );
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
        "plot", "ir", "200hz", "8000hz", "0.5s", "-20dbfs", "3harm", "4096win", "0.1s",
    ]);

    // ── printed arrival is the peak's offset (#378 contingency) ───────
    // The gate re-centres the linear IR on the sweep endpoint, so the
    // fake backend's 32-sample loopback shows up as the peak's offset
    // from centre, and that is what `arrival` prints. #378's AC6 rig run
    // (2026-09-15) triggered the contingency fixed in its design: the
    // onset estimate's 1 m → 2 m increment missed `transfer_stream`'s by
    // 143.75 samples, the peak's by 8.62, so the arrival reverted to the
    // peak and the onset is printed as a diagnostic below it.
    let arrival = printed_arrival_samples(&stdout);
    assert_eq!(
        arrival, FAKE_LOOPBACK_DELAY_SAMPLES,
        "printed arrival should be the fake loopback's peak offset:\n{stdout}"
    );
    // The rejected implementation, computed rather than assumed: on this
    // fixture the onset picker lands 118 samples before the peak (inside
    // the 200 Hz-limited deconvolution's leading skirt, no causal bound),
    // so an arrival still wired to the onset would print -86, not 32.
    assert_ne!(
        arrival,
        FAKE_LOOPBACK_DELAY_SAMPLES - 118,
        "arrival must not be the onset-derived value:\n{stdout}"
    );

    // ── the rest of the printed summary ───────────────────────────────
    // #501 UX: a pass states the threshold and its basis too — the margin
    // is the reading — and the stimulus rows carry the typed values.
    for want in [
        "  IR sweep\n",
        "  band       200 Hz \u{2192} 8000 Hz  (typed)",
        "  length        0.50 s  (typed)",
        "  window     4096 samples  (typed)",
        "  harmonics     3 orders  (typed)",
        "  tail          0.10 s  (typed)",
        "  captured      0.60 s  (0.50 s sweep + 0.10 s tail)",
        "(required \u{2265} 18.0 dB)",
        "                fixed threshold, scored for the default sweep only",
    ] {
        assert!(
            stdout.contains(want),
            "passing run missing {want:?}:\n{stdout}"
        );
    }
    for gone in ["  IR: ", "-sample window, ", "threshold set from rig data"] {
        assert!(
            !stdout.contains(gone),
            "pre-#501 line {gone:?} must not print:\n{stdout}"
        );
    }
    for want in ["peak", "pre-imp SNR", "gate", "f_low"] {
        assert!(
            stdout.contains(want),
            "printed summary missing {want:?}:\n{stdout}"
        );
    }
    // #346 AC4 / #378: the onset rule must still reach the terminal in
    // its own labelled block, and the onset-to-peak distance must not read
    // as the arrival. Row 2 under `arrival` names the peak rule; this run
    // has no distance, and row 3 of the onset block says so.
    assert!(
        stdout.contains("onset         at sample ")
            && stdout.contains("(AIC change-point pick, 10.0 ms window)"),
        "printed summary missing the onset rule line (AC4):\n{stdout}"
    );
    assert!(
        stdout.contains("from peak (largest magnitude sample)"),
        "arrival row 2 must name the peak rule (#346 AC4):\n{stdout}"
    );
    assert!(
        !stdout.contains("5 cm earlier"),
        "no edge-guard row, pass or fail, may print without a causal bound:\n{stdout}"
    );
    assert!(
        stdout.contains("(search span), pick"),
        "printed summary missing the window-start clause (AC4):\n{stdout}"
    );
    // #460 AC3: no bound applies on this rig (no reference, no distance),
    // and row 3 names both missing inputs. The pre-#460 clause named neither.
    assert!(
        stdout.contains("no causal bound \u{2014} no distance, ref latency unavailable"),
        "onset row 3 must name the missing inputs (#460 AC3):\n{stdout}"
    );
    assert!(
        !stdout.contains("no geometry known"),
        "the pre-#460 clause must not survive:\n{stdout}"
    );
    assert!(
        stdout.contains("  distance   not given"),
        "the header must state that no distance was given (#460):\n{stdout}"
    );
    assert!(
        stdout.contains(
            "  ref latency   unavailable \u{2014} no reference configured (ac setup reference)"
        ),
        "the ref latency line is always printed (#460):\n{stdout}"
    );
    assert!(
        stdout.contains("118 samples before peak, not the arrival"),
        "onset-to-peak distance must print as not the arrival (#378):\n{stdout}"
    );
    assert!(
        !stdout.contains("onset-derived"),
        "no line may still claim the arrival is onset-derived:\n{stdout}"
    );
    assert!(
        !stdout.contains("median floor"),
        "the pre-#378 rule's text must not survive anywhere:\n{stdout}"
    );
    // #391: no metre figure prints at all when no distance was typed — the
    // ms → m conversion it came from is gone. See `metre_figures`.
    assert!(
        metre_figures(&stdout).is_empty(),
        "no metre figure may print — the ms \u{2192} m conversion was removed:\n{stdout}"
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

/// #501 through the real binary: `ac plot ir` with no arguments runs the
/// daemon's default stimulus, prints it from the ack with `(default)` tags,
/// and passes on a clean loopback — the first-use case that used to print
/// `DECONVOLUTION FAILED`. The window row is in seconds before emission and
/// becomes 19200 samples (0.4 s at the fake's 48 kHz) in the result `gate`.
#[test]
fn plot_ir_with_no_arguments_runs_and_passes_the_default_sweep() {
    let rig = Rig::start();
    let stdout = rig.run_ac(&["plot", "ir"]);
    for want in [
        "  band       20 Hz \u{2192} 20000 Hz  (default)",
        "  length        4.00 s  (default)",
        "  window        0.40 s  (default)",
        "  harmonics     5 orders  (default)",
        "  tail          0.50 s  (default)",
        "  level       -40.0 dBFS  (default)",
        "  captured      4.50 s  (4.00 s sweep + 0.50 s tail)",
        "(required \u{2265} 18.0 dB)",
        "fixed threshold, scored for the default sweep only",
        "rectangular window, 19200 samples (400.00 ms)",
    ] {
        assert!(stdout.contains(want), "missing {want:?}:\n{stdout}");
    }
    assert!(
        !stdout.contains("DECONVOLUTION FAILED"),
        "the default sweep must pass on a clean loopback:\n{stdout}"
    );
    assert!(
        stdout.contains("  arrival       "),
        "a passing default run prints its arrival:\n{stdout}"
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
        "plot", "ir", "200hz", "8000hz", "0.5s", "-20dbfs", "3harm", "1024win", "0.1s",
    ]);

    assert!(
        stdout.contains("DECONVOLUTION FAILED"),
        "expected a failed-deconvolution banner:\n{stdout}"
    );
    for want in [
        "check: sweep length, band, window (above)",
        "check: drive level, input gain, distance, room noise",
    ] {
        assert!(
            stdout.contains(want),
            "banner must name what to check ({want:?}):\n{stdout}"
        );
    }
    assert!(
        !stdout.contains("mic gain"),
        "a cable has no mic; the check list says input gain:\n{stdout}"
    );
    // The exact plausible-looking-wrong-number shape #376 exists to
    // close: neither line may print on a failed verdict.
    assert!(
        !stdout.contains("arrival "),
        "arrival must not print on a failed verdict:\n{stdout}"
    );
    assert!(
        metre_figures(&stdout).is_empty(),
        "no metre figure may print on a failed verdict:\n{stdout}"
    );
    assert!(
        stdout.contains("diagnostic only"),
        "the one remaining peak number must be labelled diagnostic:\n{stdout}"
    );
    assert!(
        stdout.contains("required \u{2265} 18.0 dB"),
        "pre-imp SNR line must state the threshold it failed against:\n{stdout}"
    );
    assert!(
        stdout.contains("fixed threshold, scored for the default sweep only"),
        "the threshold's basis must print under it:\n{stdout}"
    );
    assert!(
        stdout.contains("  window     1024 samples  (typed)"),
        "the typed window the check list points at must be above it:\n{stdout}"
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

/// #460 UX frame 1 through the real binary: a typed distance and a configured
/// reference pair print the header lines, onset row 3 built from the bound's
/// own inputs, and the `ref latency` line. The #391 guard admits exactly the
/// typed distance and nothing derived.
#[test]
fn plot_ir_prints_the_bound_inputs_the_reference_and_its_ports() {
    let rig = Rig::start_with(serde_json::json!({
        "reference_channel": 1,
        "reference_output_channel": 2,
    }));
    let stdout = rig.run_ac(&[
        "plot", "ir", "200hz", "8000hz", "0.5s", "-6dbfs", "3harm", "4096win", "0.2s", "0.05m",
    ]);
    for want in [
        "  distance   0.05 m",
        "  output     fake:playback_0",
        "  ref out    fake:playback_2",
        "  ref in     fake:capture_1",
        "(causal bound), pick",
        "bound from ref latency + 0.05 m, c 343.0 m/s assumed",
        "  ref latency    0.4167 ms  (20 samples, SNR",
        "same capture)",
    ] {
        assert!(stdout.contains(want), "missing {want:?}:\n{stdout}");
    }
    let figures = metre_figures(&stdout);
    assert!(
        !figures.is_empty() && figures.iter().all(|f| f == "0.05"),
        "only the typed distance may print as a metre figure, got {figures:?}:\n{stdout}"
    );
}

/// The short `plot ir` run every #472 test uses.
const QUICK_IR: &[&str] = &[
    "plot", "ir", "200hz", "8000hz", "0.5s", "-20dbfs", "3harm", "4096win", "0.1s",
];

/// The value printed on the `  report        ` line, if any.
fn report_line(stdout: &str) -> Option<&str> {
    stdout
        .lines()
        .find_map(|l| l.strip_prefix("  report        "))
}

/// #472: `ac setup report-dir <dir>` (relative, resolved against the CLI's
/// cwd for a local daemon) is shown in the read-out, and the next `plot ir`
/// prints the report file the daemon wrote there.
#[test]
fn setup_report_dir_then_plot_ir_prints_the_written_report() {
    let rig = Rig::start_with(serde_json::json!({ "report_dir": null }));
    let dir = rig.home.join("archive");
    fs::create_dir_all(&dir).unwrap();

    let before = rig.run_ac(&["setup"]);
    assert!(
        before.contains("  Report dir:    (not set \u{2014} plot results are not saved)"),
        "{before}"
    );

    let setup = rig.run_ac_in(&rig.home, &["setup", "report-dir", "archive"]);
    assert!(
        setup.contains(&format!("  Report dir:    {}\n", dir.display())),
        "the read-out must show the resolved absolute path:\n{setup}"
    );
    assert!(!setup.contains("cannot write"), "{setup}");
    assert!(setup.contains("  Saved."), "{setup}");

    let stdout = rig.run_ac(QUICK_IR);
    let report = report_line(&stdout).unwrap_or_else(|| panic!("no report line:\n{stdout}"));
    assert!(
        report.starts_with(dir.to_str().unwrap()) && report.ends_with("-plot_ir.json"),
        "{report:?}"
    );
    assert!(Path::new(report).is_file(), "{report} not on disk");
    assert!(!stdout.contains("not saved"), "{stdout}");
    assert_eq!(files_with_extension(&dir, "json").len(), 1);
    assert_eq!(files_with_extension(&dir, "csv").len(), 1);
}

/// #472: `ac setup report-dir none` clears a hand-seeded directory; `plot ir`
/// then prints the not-saved line on stdout, naming the setup command, and
/// writes nothing.
#[test]
fn setup_report_dir_none_then_plot_ir_prints_not_saved() {
    let rig = Rig::start();
    let setup = rig.run_ac(&["setup", "report-dir", "none"]);
    assert!(
        setup.contains("  Report dir:    (not set \u{2014} plot results are not saved)"),
        "{setup}"
    );

    let stdout = rig.run_ac(QUICK_IR);
    assert_eq!(
        report_line(&stdout),
        Some("not saved \u{2014} no report directory  (ac setup report-dir <dir>)"),
        "{stdout}"
    );
    assert!(!stdout.contains("  csv "), "{stdout}");
    let dir = rig.report_dir();
    assert!(files_with_extension(&dir, "json").is_empty());
    assert!(files_with_extension(&dir, "csv").is_empty());
}

/// #472: a directory removed after it was configured is flagged by the
/// read-out before a capture, and `plot ir` prints the write failure on the
/// `report` line instead of a path to a file that does not exist.
#[test]
fn removed_report_dir_is_flagged_by_setup_and_by_plot_ir() {
    let rig = Rig::start();
    let dir = rig.report_dir();
    fs::remove_dir(&dir).unwrap();

    let setup = rig.run_ac(&["setup"]);
    assert!(
        setup.contains(&format!(
            "  Report dir:    {}\n                 cannot write: No such file or directory (os error 2)",
            dir.display()
        )),
        "{setup}"
    );

    let stdout = rig.run_ac(QUICK_IR);
    let want = format!(
        "  report        not saved \u{2014} write failed in {}\n                No such file or directory (os error 2)",
        dir.display()
    );
    assert!(stdout.contains(&want), "missing {want:?}:\n{stdout}");
    assert!(!dir.exists(), "plot ir recreated the removed directory");
}

/// #472: a directory that does not exist is refused when typed, in the
/// two-line form, and the old value stays in force.
#[test]
fn setup_refuses_a_missing_report_dir_and_keeps_the_old_one() {
    let rig = Rig::start();
    let typo = rig.home.join("reprots");
    let out = rig.ac_output_in(&rig.home, &["setup", "report-dir", typo.to_str().unwrap()]);
    assert!(!out.status.success(), "a missing directory must be refused");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let want = format!(
        "  error: report-dir {}\n         No such file or directory (os error 2) \u{2014} setting not changed\n",
        typo.display()
    );
    assert!(stderr.contains(&want), "missing {want:?}:\n{stderr}");
    assert!(!typo.exists(), "a refused directory must not be created");

    let setup = rig.run_ac(&["setup"]);
    assert!(
        setup.contains(&format!(
            "  Report dir:    {}\n",
            rig.report_dir().display()
        )),
        "the previous directory must still be in force:\n{setup}"
    );
}
