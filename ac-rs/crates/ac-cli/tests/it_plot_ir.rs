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
use std::process::{Child, Command};
use std::sync::atomic::AtomicU16;
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
    /// Held until the daemon is gone: `Drop for Rig` runs before fields drop.
    _ports: support::PortLease,
}

impl Rig {
    fn start() -> Self {
        Self::start_with(serde_json::json!({}))
    }

    /// [`Self::start`] with extra config keys merged in (#460: a reference
    /// pair for the same-capture reference leg).
    fn start_with(extra_config: serde_json::Value) -> Self {
        let ports = support::lease(&PORT_CURSOR);
        let (ctrl, data, base) = (ports.ctrl, ports.data, ports.ctrl);
        let home = std::env::temp_dir().join(format!("ac-cli-it-{}-{base}", std::process::id()));
        let cfg_dir = home.join(".config").join("ac");
        fs::create_dir_all(&cfg_dir).expect("create scratch config dir");
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
            _ports: ports,
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
    for want in ["peak", "pre-imp SNR", "gate", "f_low"] {
        assert!(
            stdout.contains(want),
            "printed summary missing {want:?}:\n{stdout}"
        );
    }
    // #346 AC4 / #378: the onset rule must still reach the terminal in
    // its own labelled block, and the onset-to-peak distance must not read
    // as the arrival: this run has no distance, so the arrival stays on
    // the peak and row 2 under `arrival` says why.
    assert!(
        stdout.contains("onset         at sample ")
            && stdout.contains("(AIC change-point pick, 10.0 ms window)"),
        "printed summary missing the onset rule line (AC4):\n{stdout}"
    );
    assert!(
        stdout.contains("from peak \u{2014} onset search had no causal bound (below)"),
        "arrival row 2 must name the peak and why (#346 AC4):\n{stdout}"
    );
    assert!(
        !stdout.contains("window start 5 cm earlier"),
        "no edge-guard row may print without a causal bound:\n{stdout}"
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
