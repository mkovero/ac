use serde_json::json;
use serde_json::Value;

use crate::common::{Client, Daemon};

/// #459 — `setup` carries the fixed emission maximum at the top level of
/// its ack, beside `config` rather than inside it: `config` is the config
/// file, and the maximum is a build constant, not a setting. This is the
/// one place the maximum is visible without starting an emission.
#[test]
fn setup_ack_carries_the_fixed_emission_maximum() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let r = c.call(json!({"cmd": "setup", "update": {}}));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(
        r["max_dbfs"],
        json!(ac_core::shared::emission_level::MAX_EMISSION_DBFS)
    );
}

/// `setup` is never refused — not by the retired `drive_max_dbfs` key, and
/// not by the emission guard (it does not emit anything). The retired key
/// still round-trips through `config` so `ac setup` can show it, and
/// `max_dbfs` is unaffected by its presence.
#[test]
fn setup_ack_is_never_refused_by_the_retired_key_and_still_reports_it() {
    let d = Daemon::spawn_with_config(Some(json!({"drive_max_dbfs": -10.0})));
    let c = Client::new(&d);

    let r = c.call(json!({"cmd": "setup", "update": {}}));
    assert_eq!(r["ok"], json!(true), "setup must never refuse: {r}");
    assert_eq!(r["config"]["drive_max_dbfs"], json!(-10.0));
    assert_eq!(
        r["max_dbfs"],
        json!(ac_core::shared::emission_level::MAX_EMISSION_DBFS)
    );
}

#[test]
fn setup_channel_clears_sticky_port() {
    // A stale sticky port in config.json — left over from a prior session,
    // a manual edit, or the older Python era — used to silently override
    // any subsequent `ac setup output|input|reference N`. `resolve_output`
    // and friends prefer the sticky string over the channel-index lookup,
    // so the configured channel got effectively muted (audio routed to
    // whatever the stale name pointed at, often nothing). Setting a new
    // channel must invalidate the stale override.
    let d = Daemon::spawn_with_config(Some(json!({
        "output_channel":    7,
        "output_port":       "system:playback_99",
        "input_channel":     7,
        "input_port":        "system:capture_99",
        "reference_channel": 7,
        "reference_port":    "system:capture_99",
    })));
    let c = Client::new(&d);

    let r = c.call(json!({"cmd":"setup","update":{
        "output_channel":    0,
        "input_channel":     0,
        "reference_channel": 0,
    }}));
    assert_eq!(r["ok"], json!(true));
    let cfg = &r["config"];
    assert!(
        cfg["output_port"].is_null(),
        "setup output_channel must clear sticky output_port (got {:?})",
        cfg["output_port"]
    );
    assert!(
        cfg["input_port"].is_null(),
        "setup input_channel must clear sticky input_port (got {:?})",
        cfg["input_port"]
    );
    assert!(
        cfg["reference_port"].is_null(),
        "setup reference_channel must clear sticky reference_port (got {:?})",
        cfg["reference_port"]
    );
}

/// handoff: snapshot-backend M1 — `snapshot_ring_s`/`snapshot_spool_dir`
/// round-trip through `setup` like every other config field, including
/// persistence (a second `setup` read reflects the earlier write).
#[test]
fn setup_updates_snapshot_ring_and_spool_dir() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let r = c.call(json!({"cmd":"setup","update":{
        "snapshot_ring_s": 60.0,
        "snapshot_spool_dir": "/tmp/custom-acsnap-spool",
    }}));
    assert_eq!(r["ok"], json!(true), "setup: {r}");
    assert_eq!(r["config"]["snapshot_ring_s"], json!(60.0));
    assert_eq!(
        r["config"]["snapshot_spool_dir"],
        json!("/tmp/custom-acsnap-spool")
    );

    // Persisted, not just echoed back.
    let r2 = c.call(json!({"cmd": "setup", "update": {}}));
    assert_eq!(r2["config"]["snapshot_ring_s"], json!(60.0));
    assert_eq!(
        r2["config"]["snapshot_spool_dir"],
        json!("/tmp/custom-acsnap-spool")
    );

    // snapshot_ring_s <= 0 is ignored (invalid), not silently accepted.
    let r3 = c.call(json!({"cmd":"setup","update":{"snapshot_ring_s": -5.0}}));
    assert_eq!(r3["ok"], json!(true));
    assert_eq!(
        r3["config"]["snapshot_ring_s"],
        json!(60.0),
        "non-positive snapshot_ring_s must be rejected, not applied"
    );

    // null clears the spool dir override back to the default.
    let r4 = c.call(json!({"cmd":"setup","update":{"snapshot_spool_dir": Value::Null}}));
    assert_eq!(r4["ok"], json!(true));
    assert!(r4["config"]["snapshot_spool_dir"].is_null());
}

#[test]
fn setup_clearing_reference_channel_clears_reference_port() {
    // The `reference_channel: null` branch (the way the user disables
    // the H1 reference channel) must also clear the sticky.
    let d = Daemon::spawn_with_config(Some(json!({
        "reference_channel": 3,
        "reference_port":    "system:capture_99",
    })));
    let c = Client::new(&d);

    let r = c.call(json!({"cmd":"setup","update":{ "reference_channel": null }}));
    assert_eq!(r["ok"], json!(true));
    assert!(r["config"]["reference_channel"].is_null());
    assert!(r["config"]["reference_port"].is_null());
}

/// #225 — the reference *output* leg is configured on its own playback index.
/// Moving the capture-side `reference_channel` must not move it, and each leg
/// clears only its own sticky port.
#[test]
fn setup_reference_output_channel_is_independent_of_reference_channel() {
    let d = Daemon::spawn_with_config(Some(json!({
        "reference_channel":        2,
        "reference_port":           "system:capture_9",
        "reference_output_channel": 1,
        "reference_output_port":    "system:playback_9",
    })));
    let c = Client::new(&d);

    let r = c.call(json!({"cmd":"setup","update":{ "reference_channel": 3 }}));
    assert_eq!(r["ok"], json!(true), "setup: {r}");
    assert_eq!(r["config"]["reference_channel"], json!(3));
    assert!(r["config"]["reference_port"].is_null());
    assert_eq!(
        r["config"]["reference_output_channel"],
        json!(1),
        "reference_channel must not move the reference output leg"
    );
    assert_eq!(
        r["config"]["reference_output_port"],
        json!("system:playback_9")
    );

    let r2 = c.call(json!({"cmd":"setup","update":{ "reference_output_channel": 5 }}));
    assert_eq!(r2["config"]["reference_output_channel"], json!(5));
    assert!(
        r2["config"]["reference_output_port"].is_null(),
        "reference_output_channel must clear its own sticky port"
    );
    assert_eq!(
        r2["config"]["reference_channel"],
        json!(3),
        "reference_output_channel must not move the capture leg"
    );

    let r3 = c.call(json!({"cmd":"setup","update":{ "reference_output_channel": null }}));
    assert!(r3["config"]["reference_output_channel"].is_null());
    assert!(r3["config"]["reference_output_port"].is_null());
}

/// #225 — the regression itself: the resolved reference output port comes from
/// `reference_output_channel`. It used to come from `reference_channel`, a
/// *capture* index, so on a rig where the two differ the daemon drove a
/// playback port nothing was patched to and the reference leg stayed at
/// digital silence while the session believed it had a reference.
///
/// Asserted through `test_hardware` rather than `transfer_stream` because
/// `transfer_stream`'s start reply does not carry `ref_out_port` on `main` —
/// that field arrives with #205 (PR #214). Both commands resolve the leg
/// through the same `resolve_ref_output`, so this covers the fix without
/// taking a dependency on an unmerged branch.
#[test]
fn ref_out_port_resolves_from_reference_output_channel() {
    let d = Daemon::spawn_with_config(Some(json!({
        "output_channel":           4,
        "reference_channel":        2,
        "reference_output_channel": 1,
    })));
    let c = Client::with_ctrl_timeout(&d, 15_000);

    let r = c.call(json!({"cmd":"test_hardware"}));
    assert_eq!(r["ok"], json!(true), "test_hardware start: {r}");
    assert_eq!(r["out_port"], json!("fake:playback_4"));
    assert_eq!(
        r["ref_out_port"],
        json!("fake:playback_1"),
        "reference output must resolve from reference_output_channel; \
         resolving from reference_channel would give fake:playback_2"
    );
    c.call(json!({"cmd":"stop"}));
}

/// With no reference output configured the leg falls back to the main output,
/// as it always did — and a configured `reference_channel` alone does not
/// change that.
#[test]
fn ref_out_port_falls_back_to_main_output() {
    let d = Daemon::spawn_with_config(Some(json!({
        "output_channel":    4,
        "reference_channel": 2,
    })));
    let c = Client::with_ctrl_timeout(&d, 15_000);

    let r = c.call(json!({"cmd":"test_hardware"}));
    assert_eq!(r["ok"], json!(true), "test_hardware start: {r}");
    assert_eq!(
        r["ref_out_port"],
        json!("fake:playback_4"),
        "unconfigured reference output must follow the main output"
    );
    c.call(json!({"cmd":"stop"}));
}

/// #225 changed what an existing config *means*: `reference_channel: N` alone
/// used to drive the reference out `playback[N]` and now leaves it on the main
/// output. A rig where the loopback happened to sit at that index worked before
/// and silently does not now, so the reply says so.
///
/// The warning is a stopgap for that migration, not a fault detector — #228's
/// `NO REFERENCE` observes the symptom instead of predicting it from config.
#[test]
fn a_config_whose_meaning_changed_carries_a_migration_warning() {
    let d = Daemon::spawn_with_config(Some(json!({
        "output_channel":    4,
        "reference_channel": 2,
    })));
    let c = Client::with_ctrl_timeout(&d, 15_000);

    let r = c.call(json!({"cmd":"test_hardware"}));
    assert_eq!(r["ok"], json!(true), "test_hardware: {r}");
    let warnings = r["warnings"]
        .as_array()
        .expect("warnings on a migrated config");
    let text = warnings[0].as_str().unwrap_or_default();
    assert!(
        text.contains("playback[2]") && text.contains("ac setup refout 2"),
        "warning must name the old port and the exact fix, got {text:?}"
    );
    c.call(json!({"cmd":"stop"}));
}

/// ...and is absent once the leg is configured either way, so it cannot become
/// background noise the operator learns to skip.
#[test]
fn a_configured_reference_output_carries_no_migration_warning() {
    let d = Daemon::spawn_with_config(Some(json!({
        "output_channel":           4,
        "reference_channel":        2,
        "reference_output_channel": 1,
    })));
    let c = Client::with_ctrl_timeout(&d, 15_000);

    let r = c.call(json!({"cmd":"test_hardware"}));
    assert_eq!(r["ok"], json!(true), "test_hardware: {r}");
    assert!(
        r.get("warnings").is_none(),
        "an explicitly configured reference output must not warn: {r}"
    );
    c.call(json!({"cmd":"stop"}));
}

/// A sticky port whose gating channel is unset is an explicitly configured
/// value with no effect — the same class of silent misconfiguration as #225
/// itself. Both gated legs refuse it instead of resolving past it.
#[test]
fn sticky_reference_ports_without_their_channel_are_refused() {
    let d = Daemon::spawn_with_config(Some(json!({
        "reference_output_port": "fake:playback_1",
    })));
    let c = Client::new(&d);

    let r = c.call(json!({"cmd":"transfer_stream","meas_channel":0,"ref_channel":1}));
    assert_eq!(
        r["ok"],
        json!(false),
        "reference_output_port without reference_output_channel must not resolve \
         silently to the main output: {r}"
    );
    let err = r["error"].as_str().unwrap_or_default();
    assert!(
        err.contains("reference_output_port") && err.contains("reference_output_channel"),
        "error must name both keys, got {err:?}"
    );

    // Same rule on the capture leg. `test_hardware`'s own guard passes on
    // either field, so before this it fell through to `Ok(None)` and measured
    // single-ended against the measurement input.
    let d2 = Daemon::spawn_with_config(Some(json!({
        "reference_port": "fake:capture_3",
    })));
    let c2 = Client::new(&d2);

    let r2 = c2.call(json!({"cmd":"test_hardware"}));
    assert_eq!(
        r2["ok"],
        json!(false),
        "reference_port without reference_channel must not downgrade to \
         single-ended: {r2}"
    );
    let err2 = r2["error"].as_str().unwrap_or_default();
    assert!(
        err2.contains("reference_port") && err2.contains("reference_channel"),
        "error must name both keys, got {err2:?}"
    );
}

/// The on-disk config the daemon persists under `home`.
fn config_on_disk(home: &std::path::Path) -> Value {
    let path = home.join(".config").join("ac").join("config.json");
    serde_json::from_slice(&std::fs::read(&path).expect("read config.json"))
        .expect("config.json decodes")
}

/// #472 — `report_dir` is set, read back with a writable status on every
/// call (including the read-only one), and cleared, after which the status
/// is absent rather than reporting on nothing.
#[test]
fn setup_report_dir_round_trips_with_a_writable_status() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let dir = d.home.join("reports");
    std::fs::create_dir_all(&dir).unwrap();
    let dir_s = dir.to_str().unwrap();

    let r = c.call(json!({"cmd": "setup", "update": {"report_dir": dir_s}}));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(r["config"]["report_dir"], json!(dir_s));
    assert_eq!(r["report_dir_status"], json!({"writable": true}), "{r}");

    let r2 = c.call(json!({"cmd": "setup", "update": {}}));
    assert_eq!(r2["config"]["report_dir"], json!(dir_s));
    assert_eq!(r2["report_dir_status"], json!({"writable": true}), "{r2}");
    assert_eq!(config_on_disk(&d.home)["report_dir"], json!(dir_s));

    // The probe leaves nothing behind.
    let leftovers: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
    assert!(leftovers.is_empty(), "probe left files: {leftovers:?}");

    let r3 = c.call(json!({"cmd": "setup", "update": {"report_dir": Value::Null}}));
    assert_eq!(r3["ok"], json!(true), "{r3}");
    assert!(r3["config"]["report_dir"].is_null(), "{r3}");
    assert!(
        r3.get("report_dir_status").is_none(),
        "no status without a directory: {r3}"
    );
    assert!(config_on_disk(&d.home)["report_dir"].is_null());
}

/// #472 — a directory that is missing, relative, or a plain file is refused
/// at setup time with a reason, and the refusal leaves the *whole* update
/// unapplied: an `output_channel` in the same command does not land either.
/// A missing directory is not created.
#[test]
fn setup_refuses_an_unusable_report_dir_and_changes_nothing() {
    let d = Daemon::spawn_with_config(Some(json!({"output_channel": 3})));
    let c = Client::new(&d);
    let missing = d.home.join("ac-reprots");
    let plain_file = d.home.join("not-a-dir");
    std::fs::write(&plain_file, b"x").unwrap();

    let cases = [
        (missing.to_str().unwrap().to_string(), "os error 2"),
        (
            "reports".to_string(),
            "must be an absolute path on the daemon host",
        ),
        (
            "~/reports".to_string(),
            "must be an absolute path on the daemon host",
        ),
        (String::new(), "must be an absolute path on the daemon host"),
        (plain_file.to_str().unwrap().to_string(), "Not a directory"),
    ];
    for (bad, want_reason) in cases {
        let r = c.call(json!({"cmd": "setup", "update": {
            "output_channel": 5,
            "report_dir": bad,
        }}));
        assert_eq!(r["ok"], json!(false), "{bad:?} must be refused: {r}");
        assert_eq!(r["refused"]["key"], json!("report_dir"), "{r}");
        assert_eq!(r["refused"]["path"], json!(bad), "{r}");
        let reason = r["refused"]["reason"].as_str().unwrap_or_default();
        assert!(
            reason.contains(want_reason),
            "{bad:?}: reason {reason:?} lacks {want_reason:?}"
        );
        let err = r["error"].as_str().unwrap_or_default();
        assert!(
            err.starts_with(&format!("report-dir {bad}: ")) && err.ends_with("setting not changed"),
            "generic error line: {err:?}"
        );

        let on_disk = config_on_disk(&d.home);
        assert_eq!(on_disk["output_channel"], json!(3), "{bad:?}: {on_disk}");
        assert!(on_disk["report_dir"].is_null(), "{bad:?}: {on_disk}");
        let read = c.call(json!({"cmd": "setup", "update": {}}));
        assert_eq!(read["config"]["output_channel"], json!(3), "{read}");
    }
    assert!(!missing.exists(), "a refused directory must not be created");
}

/// #472 — a directory removed after it was set is reported as not writable
/// on the next read, before any capture runs, and is not recreated.
#[test]
fn setup_reports_a_removed_report_dir_as_not_writable() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let dir = d.home.join("reports");
    std::fs::create_dir_all(&dir).unwrap();
    let dir_s = dir.to_str().unwrap();

    let r = c.call(json!({"cmd": "setup", "update": {"report_dir": dir_s}}));
    assert_eq!(r["report_dir_status"]["writable"], json!(true), "{r}");

    std::fs::remove_dir(&dir).unwrap();
    let r2 = c.call(json!({"cmd": "setup", "update": {}}));
    assert_eq!(r2["ok"], json!(true), "{r2}");
    assert_eq!(r2["config"]["report_dir"], json!(dir_s));
    assert_eq!(r2["report_dir_status"]["writable"], json!(false), "{r2}");
    let err = r2["report_dir_status"]["error"]
        .as_str()
        .unwrap_or_default();
    assert!(
        err.contains("os error 2"),
        "OS error text expected: {err:?}"
    );
    assert!(
        !dir.exists(),
        "the read-out must not recreate the directory"
    );
}

/// #472 criterion 4 — a value set through `setup` is the one a restarted
/// daemon on the same HOME holds.
#[test]
fn setup_report_dir_survives_a_daemon_restart() {
    let mut d = Daemon::spawn();
    let dir = d.home.join("reports");
    std::fs::create_dir_all(&dir).unwrap();
    let dir_s = dir.to_str().unwrap().to_string();
    {
        let c = Client::new(&d);
        let r = c.call(json!({"cmd": "setup", "update": {"report_dir": dir_s}}));
        assert_eq!(r["ok"], json!(true), "{r}");
    }
    let home = d.home.clone();
    d.kill_without_cleanup();
    std::mem::forget(d);

    let d2 = Daemon::spawn_at_home(home);
    let c2 = Client::new(&d2);
    let r = c2.call(json!({"cmd": "setup", "update": {}}));
    assert_eq!(r["config"]["report_dir"], json!(dir_s), "{r}");
    assert_eq!(r["report_dir_status"]["writable"], json!(true), "{r}");
}

/// #472 — an update whose config save fails is refused, since the daemon
/// reloads config from disk before the next request and the value would
/// silently revert. A read-only call still answers.
#[test]
fn setup_refuses_an_update_it_could_not_save() {
    use std::os::unix::fs::PermissionsExt;

    let d = Daemon::spawn();
    let c = Client::new(&d);
    // Save something first so config.json exists, then make both the file
    // and its directory read-only.
    let r = c.call(json!({"cmd": "setup", "update": {"output_channel": 1}}));
    assert_eq!(r["ok"], json!(true), "{r}");
    let cfg_dir = d.home.join(".config").join("ac");
    let cfg_file = cfg_dir.join("config.json");
    let set_mode = |p: &std::path::Path, mode: u32| {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap()
    };
    set_mode(&cfg_file, 0o400);
    set_mode(&cfg_dir, 0o500);

    // A process that ignores permissions (root) cannot exercise this path;
    // say so rather than pass without checking anything.
    let probe_writable = std::fs::OpenOptions::new()
        .append(true)
        .open(&cfg_file)
        .is_ok();

    let refused = c.call(json!({"cmd": "setup", "update": {"output_channel": 2}}));
    let read = c.call(json!({"cmd": "setup", "update": {}}));

    set_mode(&cfg_dir, 0o700);
    set_mode(&cfg_file, 0o600);

    if probe_writable {
        eprintln!("skipped: permissions are not enforced for this user");
        return;
    }
    assert_eq!(refused["ok"], json!(false), "{refused}");
    let err = refused["error"].as_str().unwrap_or_default();
    assert!(
        err.starts_with("config not saved (") && err.contains("config.json"),
        "{err:?}"
    );
    assert_eq!(
        read["ok"],
        json!(true),
        "read-only setup must still answer: {read}"
    );
    assert_eq!(read["config"]["output_channel"], json!(1), "{read}");
}
