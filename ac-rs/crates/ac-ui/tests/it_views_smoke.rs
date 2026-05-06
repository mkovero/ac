//! Pre-tag smoke matrix — RC-11. Spawns the `ac-ui` binary in synthetic
//! benchmark mode for each (view × channel-count) combination and
//! asserts a clean exit plus a non-empty benchmark report.
//!
//! `#[ignore]`'d by default so `cargo test` stays cheap; flip on per
//! the JACK-loopback runbook pattern:
//!
//!   cargo test -p ac-ui --test it_views_smoke -- --include-ignored
//!
//! `scripts/rc-smoke.sh` runs the same matrix outside cargo for human-
//! in-the-loop verification.
//!
//! Requires a wgpu-capable adapter — on a host that exits 71 (RC-6),
//! the test panics, surfacing the missing-GPU condition explicitly.

use std::process::{Command, Stdio};
use std::time::Duration;

const AC_UI: &str = env!("CARGO_BIN_EXE_ac-ui");

/// Views in the canonical W-cycle (RC-4 plan §1) plus the two hidden
/// views the parser still accepts. Run them all so `--view <name>`
/// remains exercised.
const VIEWS: &[&str] = &[
    "spectrum_ember",
    "goniometer",
    "iotransfer",
    "bode_mag",
    "coherence",
    "bode_phase",
    "group_delay",
    "nyquist",
    "ir",
    "spectrum",
    "waterfall",
    "scope",
];

/// How many synthetic input channels to drive. Three shapes per view:
/// 1 (active-channel only), 2 (stereo pair lit), 8 (matrix layout).
const CHANNEL_COUNTS: &[u32] = &[1, 2, 8];

fn spawn_smoke(view: &str, channels: u32) -> std::process::Output {
    Command::new(AC_UI)
        .args([
            "--synthetic",
            "--no-persist",
            "--benchmark",
            "1.5",
            "--view",
            view,
            "--channels",
            &channels.to_string(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ac-ui")
        .wait_with_output()
        .expect("wait")
}

fn assert_clean_run(view: &str, channels: u32, out: &std::process::Output) {
    assert!(
        out.status.success(),
        "view={view} channels={channels}: ac-ui exited with {:?}\nstdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("ac-ui benchmark:"),
        "view={view} channels={channels}: missing benchmark report.\nstdout: {stdout}",
    );
    // Sanity: the benchmark printed at least 5 frames over 1.5 s.
    // Below this we're either not painting or hitting a render stall.
    let frames = parse_benchmark_frames(&stdout).unwrap_or(0);
    assert!(
        frames >= 5,
        "view={view} channels={channels}: only {frames} frames in 1.5 s",
    );
}

fn parse_benchmark_frames(stdout: &str) -> Option<usize> {
    // Format: "ac-ui benchmark: 1.5 s, NNNN frames\n  fps mean ..."
    let line = stdout
        .lines()
        .find(|l| l.contains("ac-ui benchmark:"))?;
    let frames = line.split_whitespace().rev().nth(1)?;
    frames.parse().ok()
}

#[test]
#[ignore = "spawns ac-ui per (view × channels); needs a wgpu adapter — runbook only"]
fn smoke_default_views() {
    for &view in VIEWS {
        for &ch in CHANNEL_COUNTS {
            let out = spawn_smoke(view, ch);
            assert_clean_run(view, ch, &out);
        }
    }
}

#[test]
#[ignore = "spawns ac-ui — runbook only"]
fn smoke_short_run_completes_in_time() {
    // Loose time bound — the benchmark hard-exits at 1.5 s plus a
    // generous shutdown budget. Anything past 6 s is a hang.
    let start = std::time::Instant::now();
    let out = spawn_smoke("spectrum_ember", 2);
    assert_clean_run("spectrum_ember", 2, &out);
    assert!(
        start.elapsed() < Duration::from_secs(6),
        "ac-ui took {:?} for a 1.5 s benchmark — likely a hang",
        start.elapsed(),
    );
}
