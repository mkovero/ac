//! #513 — `ac new|use|rm` must not report success when the config
//! could not be read or written. A corrupt `config.json` is refused with the
//! `session not saved` block on stderr, exit 1, and the file and the session
//! directories are left exactly as they were.
//!
//! Drives the real `ac` binary under a scratch `HOME`; no daemon is needed.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const CORRUPT: &[u8] = b"{not json";
const REFUSAL: &str = "session not saved \u{2014} existing configuration is unreadable";

fn scratch_home(tag: &str) -> PathBuf {
    let home =
        std::env::temp_dir().join(format!("ac-session-save-it-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(home.join(".config").join("ac")).expect("create scratch HOME");
    home
}

fn config_path(home: &Path) -> PathBuf {
    home.join(".config").join("ac").join("config.json")
}

fn session_dir(home: &Path, name: &str) -> PathBuf {
    home.join(".local/share/ac/sessions").join(name)
}

fn ac(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ac"))
        .args(args)
        .env("HOME", home)
        .output()
        .expect("run ac")
}

fn active_session(home: &Path) -> Option<String> {
    let raw = fs::read_to_string(config_path(home)).expect("read config");
    let v: serde_json::Value = serde_json::from_str(&raw).expect("config is JSON");
    v["session"].as_str().map(str::to_string)
}

/// Exit ≠ 0, the refusal and the config path on stderr, nothing on stdout,
/// and `config.json` byte-identical.
fn assert_refused(home: &Path, out: &Output) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !out.status.success(),
        "must exit non-zero; stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stderr.contains(&format!("  error: {REFUSAL}")),
        "stderr={stderr:?}"
    );
    assert!(
        stderr.contains(&config_path(home).display().to_string()),
        "stderr must name the config file: {stderr:?}"
    );
    assert!(stderr.contains("existing file preserved"), "{stderr:?}");
    assert!(stdout.trim().is_empty(), "no success line: {stdout:?}");
    assert_eq!(fs::read(config_path(home)).unwrap(), CORRUPT);
}

#[test]
fn new_with_corrupt_config_refuses_and_creates_nothing() {
    let home = scratch_home("new-corrupt");
    fs::write(config_path(&home), CORRUPT).unwrap();

    let out = ac(&home, &["new", "s1"]);
    assert_refused(&home, &out);
    assert!(
        !session_dir(&home, "s1").exists(),
        "a refused `new` must leave no directory behind"
    );
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn use_with_corrupt_config_refuses() {
    let home = scratch_home("use-corrupt");
    fs::create_dir_all(session_dir(&home, "s1")).unwrap();
    fs::write(config_path(&home), CORRUPT).unwrap();

    let out = ac(&home, &["use", "s1"]);
    assert_refused(&home, &out);
    assert!(session_dir(&home, "s1").exists());
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn rm_with_corrupt_config_refuses_and_keeps_the_directory() {
    let home = scratch_home("rm-corrupt");
    fs::create_dir_all(session_dir(&home, "s1")).unwrap();
    fs::write(session_dir(&home, "s1").join("data.csv"), b"x").unwrap();
    fs::write(config_path(&home), CORRUPT).unwrap();

    let out = ac(&home, &["rm", "s1"]);
    assert_refused(&home, &out);
    assert!(
        session_dir(&home, "s1").join("data.csv").exists(),
        "a refused `rm` must delete nothing"
    );
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn new_then_retry_after_repair_succeeds() {
    // A refused `new` must not block the same name later with
    // "already exists".
    let home = scratch_home("new-retry");
    fs::write(config_path(&home), CORRUPT).unwrap();
    assert!(!ac(&home, &["new", "s1"]).status.success());

    fs::write(config_path(&home), b"{}").unwrap();
    let out = ac(&home, &["new", "s1"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(active_session(&home).as_deref(), Some("s1"));
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn new_use_rm_succeed_against_a_valid_config() {
    let home = scratch_home("ok");
    fs::write(config_path(&home), b"{}").unwrap();

    let out = ac(&home, &["new", "s1"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "  Created and switched to session: s1\n"
    );
    assert!(session_dir(&home, "s1").is_dir());
    assert_eq!(active_session(&home).as_deref(), Some("s1"));

    fs::create_dir_all(session_dir(&home, "s2")).unwrap();
    let out = ac(&home, &["use", "s2"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "  Switched to session: s2\n"
    );
    assert_eq!(active_session(&home).as_deref(), Some("s2"));

    // Removing an inactive session leaves the active one alone.
    let out = ac(&home, &["rm", "s1"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "  Removed session: s1\n"
    );
    assert!(!session_dir(&home, "s1").exists());
    assert_eq!(active_session(&home).as_deref(), Some("s2"));

    // Removing the active session clears it.
    let out = ac(&home, &["rm", "s2"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "  Removed session: s2\n"
    );
    assert!(!session_dir(&home, "s2").exists());
    assert_eq!(active_session(&home), None);
    let _ = fs::remove_dir_all(&home);
}
