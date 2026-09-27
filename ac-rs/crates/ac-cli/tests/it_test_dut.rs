//! End-to-end `ac test dut compare` against a real `ac-daemon --fake-audio`
//! (issue #619).
//!
//! Reads the real binary's stdout, like `it_plot_ir.rs`: #619 was the CLI
//! dropping every `test_result` frame, which no wire-level test can see.
//! One run covers both halves of the fix — the DUT rows print, and the
//! bypass prompt with nobody to answer it (stdin closed) stops the run and
//! fails it, instead of the daemon's 300 s wait followed by "bypass" rows
//! that bypassed nothing. The fake backend's DUT pass takes about 40 s.

mod support;

use std::fs;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn test_dut_compare_prints_rows_and_stops_when_nobody_answers_the_prompt() {
    let home = support::alloc_home("ac-cli-it-dut");
    // No session is configured, so the CSV lands in the working directory
    // (`io::output_dir`); that is `run_dir`, empty until `ac` writes to it.
    let run_dir = home.join("run");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(
        home.join(".config").join("ac").join("config.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "reference_channel": 1,
        }))
        .unwrap(),
    )
    .unwrap();
    let mut daemon = support::spawn_daemon(&home, true, "127.0.0.1", None);

    let started = Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_ac"))
        .env("HOME", &home)
        .env("AC_CTRL_PORT", daemon.ctrl.to_string())
        .env("AC_DATA_PORT", daemon.data.to_string())
        .current_dir(&run_dir)
        .args(["test", "dut", "compare"])
        .stdin(Stdio::null())
        .output()
        .expect("run ac");
    let elapsed = started.elapsed();
    daemon.kill();

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let csvs: Vec<_> = fs::read_dir(&run_dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("csv"))
        .collect();
    let csv = csvs.first().map(|p| fs::read_to_string(p).unwrap());
    let _ = fs::remove_dir_all(&home);
    let ctx = format!("stdout:\n{stdout}\nstderr:\n{stderr}");

    for name in [
        "Noise floor",
        "Gain",
        "THD vs level",
        "Frequency response",
        "Clipping point",
    ] {
        assert!(
            stdout.contains(&format!("  pass  {name} (")),
            "no row for {name}\n{ctx}"
        );
    }
    assert!(stdout.contains("  With DUT"), "{ctx}");
    assert!(stdout.contains("Bypass DUT and press Enter"), "{ctx}");
    assert!(
        stderr.contains("no answer on stdin, stopping before the bypass pass"),
        "{ctx}"
    );
    assert!(!stdout.contains("  Bypass\n"), "bypass pass ran\n{ctx}");
    assert!(stdout.contains("  5 of 5 pass"), "{ctx}");
    assert_eq!(
        out.status.code(),
        Some(1),
        "a stopped compare must fail\n{ctx}"
    );
    assert!(
        elapsed < Duration::from_secs(240),
        "took {elapsed:?}: the stop did not cut the 300 s prompt wait\n{ctx}"
    );
    assert_eq!(csvs.len(), 1, "{ctx}");
    assert_eq!(csv.unwrap().lines().count(), 1 + 5, "{ctx}");
}
