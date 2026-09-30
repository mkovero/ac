//! Exit status of `ac plot frequency` / `ac plot level` when the sweep ends
//! on the daemon's `error` frame (#596). It used to print the error and
//! exit 0, so a script could not tell a failed sweep from a good one.
//!
//! The failure is reached through the fake backend's opt-in fault hook
//! `AC_FAKE_START_FAIL_CALLS` (see `it_generate_exit.rs` for the hook): in
//! a fresh daemon with no loopback configured, the sweep worker's engine
//! start is call 0. The test checks the hook's name reached the output, so
//! a failure consumed before the sweep (which would exit 1 for another
//! reason) cannot pass it.

mod support;

use std::fs;
use std::process::Command;

fn run(args: &[&str], env: &[(&str, &str)]) -> (Option<i32>, String) {
    let home = support::alloc_home("ac-cli-plotexit");
    fs::write(home.join(".config").join("ac").join("config.json"), b"{}")
        .expect("seed config.json");
    let daemon = support::spawn_daemon_with_env(&home, true, "127.0.0.1", None, env);
    let out = Command::new(env!("CARGO_BIN_EXE_ac"))
        .env("HOME", &home)
        .env("AC_CTRL_PORT", daemon.ctrl.to_string())
        .env("AC_DATA_PORT", daemon.data.to_string())
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .current_dir(&home)
        .args(args)
        .output()
        .expect("run ac");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.code(), text)
}

#[test]
fn a_sweep_that_ends_on_a_daemon_error_exits_1() {
    for args in [
        &["plot", "1khz", "2khz", "-40dbfs", "2ppd"][..],
        &["plot", "level", "-50dbfs", "-40dbfs", "1khz", "2steps"][..],
    ] {
        let (code, text) = run(args, &[("AC_FAKE_START_FAIL_CALLS", "0")]);
        assert!(
            text.contains("AC_FAKE_START_FAIL_CALLS"),
            "`ac {}` must fail on the hooked start:\n{text}",
            args.join(" ")
        );
        assert_eq!(code, Some(1), "`ac {}`:\n{text}", args.join(" "));
    }
}
