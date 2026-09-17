//! The per-session calibration check (#466), over the wire.
//!
//! A "session" is a daemon process: every fake-backend fault hook is read
//! once per process, so each shifted or unshifted session below is its own
//! `ac-daemon`. The sessions share their stored state the way two daemon
//! runs on one `HOME` do — `cal.json` and `session_refusals.json` are
//! copied from one daemon's `HOME` into the next before it is asked
//! anything; the daemon re-reads both files on every access.
//!
//! The reference loopback is `out1_in1` (reference input 1, reference
//! output 1) in every configuration that has one.

use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::expect_prompt;
use crate::common::{Client, Daemon};

const LOOPBACK: &str = "out1_in1";
/// −3.10 dB: far outside any tolerance the check can carry.
const GAIN_SHIFT: &str = "0.7";

/// The measurement pair *is* the loopback: `plot` consumes its scale.
fn loop_pair_config() -> Value {
    json!({
        "output_channel": 1, "input_channel": 1,
        "reference_channel": 1, "reference_output_channel": 1,
    })
}

/// Measurement pair `out0_in0`, reference loopback `out1_in1`.
fn split_config() -> Value {
    json!({"reference_channel": 1, "reference_output_channel": 1})
}

fn ac_dir(home: &Path) -> PathBuf {
    home.join(".config").join("ac")
}

fn cal_path(d: &Daemon) -> PathBuf {
    ac_dir(&d.home).join("cal.json")
}

fn refusals_path(d: &Daemon) -> PathBuf {
    ac_dir(&d.home).join("session_refusals.json")
}

/// Carry the stored state of `from` into `to`, as a second daemon run on the
/// same `HOME` would find it.
fn share_state(from: &Daemon, to: &Daemon) {
    for name in ["cal.json", "session_refusals.json"] {
        let src = ac_dir(&from.home).join(name);
        let dst = ac_dir(&to.home).join(name);
        match fs::read(&src) {
            Ok(bytes) => fs::write(&dst, bytes).expect("share state"),
            Err(_) => {
                let _ = fs::remove_file(&dst);
            }
        }
    }
}

/// `calibrate` on the loopback with both voltage legs measured at the
/// default drive, so a loop-gain baseline is stored.
fn calibrate_loopback(c: &Client) -> Value {
    let r = c.call(json!({"cmd": "calibrate", "output_channel": 1, "input_channel": 1}));
    assert_eq!(r["ok"], json!(true), "calibrate ack: {r}");
    let _ = expect_prompt(c, 1);
    let _ = c.call(json!({"cmd": "cal_reply", "vrms": 0.034641}));
    let _ = expect_prompt(c, 2);
    let _ = c.call(json!({"cmd": "cal_reply", "vrms": 0.034995}));
    let done = c
        .wait_for_topic("cal_done", Duration::from_secs(15))
        .expect("cal_done");
    assert_eq!(done["loop_gain_state"], json!("measured"), "{done}");
    assert_eq!(done["tau_state"], json!("measured"), "{done}");
    done
}

/// Add voltage-only entries to `cal.json` (other pairs a refusal can reach).
fn seed_voltage_entries(d: &Daemon, keys: &[(u32, u32)]) {
    let path = cal_path(d);
    let mut all: Value =
        serde_json::from_slice(&fs::read(&path).expect("read cal.json")).expect("parse cal.json");
    for (o, i) in keys {
        all[format!("out{o}_in{i}")] = json!({
            "output_channel": o, "input_channel": i, "ref_freq": 1000.0,
            "vrms_at_0dbfs_out": 1.0, "vrms_at_0dbfs_in": 1.0, "ref_dbfs": -40.0,
            "mic_sensitivity_dbfs_at_94db_spl": null, "mic_response": null,
        });
    }
    fs::write(&path, serde_json::to_vec_pretty(&all).unwrap()).expect("seed cal.json");
}

fn get_cal(c: &Client, o: u32, i: u32) -> Value {
    let r = c.call(json!({"cmd": "get_calibration", "output_channel": o, "input_channel": i}));
    assert_eq!(r["ok"], json!(true), "{r}");
    r
}

fn state(v: &Value) -> &str {
    v["state"].as_str().unwrap_or("<none>")
}

fn cause(v: &Value) -> &str {
    v["cause"].as_str().unwrap_or("<none>")
}

/// Run `session_check` and return its PUB frame.
fn run_check(c: &Client) -> Value {
    let r = c.call(json!({"cmd": "session_check"}));
    assert_eq!(r["ok"], json!(true), "session_check ack: {r}");
    let frame = frame_on(c, "session_check", "session_check", Duration::from_secs(15))
        .unwrap_or_else(|| panic!("no session_check frame\n{}", c.daemon().log_tail()));
    let _ = frame_on(c, "done", "session_check", Duration::from_secs(5));
    frame
}

/// The next frame on `topic` whose `cmd` is `cmd`.
fn frame_on(c: &Client, topic: &str, cmd: &str, timeout: Duration) -> Option<Value> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let left = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(left.max(1)) {
            Some((t, v)) if t == topic && v["cmd"] == json!(cmd) => return Some(v),
            Some(_) => continue,
            None => return None,
        }
    }
    None
}

/// Every frame of one `plot` run, until `done` or `error`.
fn run_plot(c: &Client, extra: Value) -> Vec<(String, Value)> {
    let mut req = json!({
        "cmd": "plot", "start_hz": 1000.0, "stop_hz": 1000.0,
        "level_dbfs": -40.0, "ppd": 1, "duration": 0.1,
    });
    for (k, v) in extra.as_object().into_iter().flatten() {
        req[k] = v.clone();
    }
    let ack = c.call(req);
    assert_eq!(ack["ok"], json!(true), "plot ack: {ack}");
    let mut frames = vec![("ack".to_string(), ack)];
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        let left = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        let Some((t, v)) = c.recv_pub(left.max(1)) else {
            break;
        };
        let end = (t == "done" || t == "error") && v["cmd"] == json!("plot");
        frames.push((t, v));
        if end {
            break;
        }
    }
    frames
}

fn points(frames: &[(String, Value)]) -> Vec<&Value> {
    frames
        .iter()
        .filter(|(t, v)| t == "data" && v["type"] == json!("measurement/frequency_response/point"))
        .map(|(_, v)| v)
        .collect()
}

fn topic<'a>(frames: &'a [(String, Value)], want: &str) -> Option<&'a Value> {
    frames.iter().find(|(t, _)| t == want).map(|(_, v)| v)
}

/// Triage AC8/AC9, voltage: a gain shift between two sessions refuses the
/// stale scale and names it; with the check bypassed (no loopback) the same
/// fixture consumes the stale value; an unshifted session verifies; a
/// persisted refusal outlives its daemon until a passing check clears it.
#[test]
fn a_gain_shift_refuses_the_stored_scale_across_sessions() {
    // Leg 1: session 1 calibrates the loopback.
    let d1 = Daemon::spawn_with_config(Some(loop_pair_config()));
    let c1 = Client::new(&d1);
    let done = calibrate_loopback(&c1);
    let stored_in = done["vrms_at_0dbfs_in"].as_f64().expect("stored in scale");

    // Coupling: the baseline and an immediate check read the loop the same
    // way — a systematic offset between the two would refuse every check.
    let check = run_check(&c1);
    assert_eq!(state(&check["voltage"]), "verified", "{check}");
    let calibrated = done["loop_gain_db"].as_f64().unwrap();
    let probed = check["probe"]["loop_gain_db"].as_f64().unwrap();
    assert!(
        (calibrated - probed).abs() < 1e-6,
        "calibrate {calibrated} vs session_check {probed}"
    );

    // Leg 2 (rejected behaviour): the same gain shift with the check
    // bypassed — no reference configured, nothing refused on disk yet. The
    // stale scale is consumed, which is what the check exists to stop.
    let bypass = Daemon::spawn_with(
        Some(json!({"output_channel": 1, "input_channel": 1})),
        &[("AC_FAKE_TAU_GAIN_OVERRIDE", GAIN_SHIFT)],
    );
    share_state(&d1, &bypass);
    let cb = Client::new(&bypass);
    let frames = run_plot(&cb, json!({}));
    assert_eq!(
        frames[0].1["session_check"],
        json!("not_run"),
        "{:?}",
        frames[0]
    );
    let p = points(&frames);
    assert!(!p.is_empty(), "{frames:?}");
    assert_eq!(state(&p[0]["voltage_check"]), "unverified");
    assert_eq!(cause(&p[0]["voltage_check"]), "no_loopback");
    assert_eq!(
        p[0]["vrms_at_0dbfs_in"].as_f64(),
        Some(stored_in),
        "without the check the stale scale is applied"
    );
    drop(bypass);

    // Leg 3: session 2, gain shifted, check configured — refused and named.
    let d2 = Daemon::spawn_with(
        Some(loop_pair_config()),
        &[("AC_FAKE_TAU_GAIN_OVERRIDE", GAIN_SHIFT)],
    );
    share_state(&d1, &d2);
    let c2 = Client::new(&d2);
    let frames = run_plot(&c2, json!({}));
    assert_eq!(frames[0].1["session_check"], json!("pending"));
    let sc = topic(&frames, "session_check").expect("session_check frame");
    assert_eq!(state(&sc["pair"]["voltage"]), "refused", "{sc}");
    assert_eq!(sc["pair"]["key"], json!(LOOPBACK));
    assert!(sc.get("latency").is_none(), "plot uses no τ: {sc}");
    assert!(sc["persisted"] == json!(true), "{sc}");
    let first_point = frames
        .iter()
        .position(|(t, v)| {
            t == "data" && v["type"] == json!("measurement/frequency_response/point")
        })
        .expect("points still measured in dBFS");
    let sc_at = frames
        .iter()
        .position(|(t, _)| t == "session_check")
        .unwrap();
    assert!(
        sc_at < first_point,
        "the check reports before any measurement"
    );
    for p in points(&frames) {
        assert_eq!(state(&p["voltage_check"]), "refused");
        for k in ["vrms_at_0dbfs_in", "vrms_at_0dbfs_out", "gain_db"] {
            assert!(p[k].is_null(), "{k} must be withheld: {p}");
        }
    }

    // Leg 6: a dBu level under the same refusal is refused outright.
    let frames = run_plot(&c2, json!({"level_unit": "dbu"}));
    let err = topic(&frames, "error").expect("command-level refusal");
    assert_eq!(state(&err["voltage_check"]), "refused", "{err}");
    assert!(
        err["message"]
            .as_str()
            .unwrap()
            .contains("nothing was emitted"),
        "{err}"
    );
    assert!(
        !frames.iter().any(|(t, v)| t.starts_with("measurement/")
            || v["type"]
                .as_str()
                .is_some_and(|s| s.starts_with("measurement/"))),
        "nothing may be measured after a command-level refusal: {frames:?}"
    );
    // Session 2 wrote its refusal; session 1 wrote none.
    let refusals = fs::read(refusals_path(&d2)).expect("session 2 persisted its refusal");
    assert!(!refusals_path(&d1).exists());
    drop(d2);

    // Leg 8: session 3, unshifted. The persisted refusal stands before any
    // check, a passing check clears it, and the stored layer then reads
    // verified (leg 4).
    let d3 = Daemon::spawn_with_config(Some(loop_pair_config()));
    share_state(&d1, &d3);
    fs::write(refusals_path(&d3), &refusals).unwrap();
    drop(d1);
    let c3 = Client::new(&d3);
    let before = get_cal(&c3, 1, 1);
    assert_eq!(
        state(&before["session_check"]["voltage"]),
        "refused",
        "{before}"
    );
    let check = run_check(&c3);
    assert_eq!(state(&check["voltage"]), "verified", "{check}");
    let after = get_cal(&c3, 1, 1);
    assert_eq!(
        state(&after["session_check"]["voltage"]),
        "verified",
        "{after}"
    );
    let file: Value = serde_json::from_slice(&fs::read(refusals_path(&d3)).unwrap()).unwrap();
    assert!(file["voltage"].get(LOOPBACK).is_none(), "cleared: {file}");
}

/// Triage AC9, τ only: a delay shift refuses latency and leaves voltage
/// verified, through both τ sources.
#[test]
fn a_delay_shift_refuses_latency_and_leaves_voltage_verified() {
    let d1 = Daemon::spawn_with_config(Some(split_config()));
    let c1 = Client::new(&d1);
    calibrate_loopback(&c1);
    seed_voltage_entries(&d1, &[(0, 0)]);

    // `session_check`: both τ lifecycles read 16 samples late.
    let d2 = Daemon::spawn_with(
        Some(split_config()),
        &[("AC_FAKE_TAU_DELAY_SAMPLES_OVERRIDE", "48,48")],
    );
    share_state(&d1, &d2);
    let check = run_check(&Client::new(&d2));
    assert_eq!(state(&check["latency"]), "refused", "{check}");
    assert_eq!(check["latency"]["delta"], json!(16.0));
    assert_eq!(state(&check["voltage"]), "verified", "{check}");
    drop(d2);

    // `plot_ir`: the same-capture reference leg. Unshifted, the reference
    // arrives where calibrate stored it (32 samples) and τ verifies.
    let plot_ir = |ref_delay: &str| -> Value {
        let d = Daemon::spawn_with(
            Some(split_config()),
            &[("AC_FAKE_REF_DELAY_SAMPLES", ref_delay)],
        );
        share_state(&d1, &d);
        let c = Client::new(&d);
        let ack = c.call(json!({
            "cmd": "plot_ir", "f1_hz": 200.0, "f2_hz": 8000.0, "duration": 0.5,
            "level_dbfs": -20.0, "tail_s": 0.1, "window_len": 4096, "n_harmonics": 3,
        }));
        assert_eq!(ack["ok"], json!(true), "{ack}");
        assert_eq!(ack["session_check"], json!("pending"), "{ack}");
        let sc = frame_on(&c, "session_check", "plot_ir", Duration::from_secs(30))
            .unwrap_or_else(|| panic!("no session_check frame\n{}", d.log_tail()));
        let _ = frame_on(&c, "done", "plot_ir", Duration::from_secs(30));
        sc
    };
    let same = plot_ir("32");
    assert_eq!(state(&same["latency"]), "verified", "{same}");
    assert_eq!(same["latency"]["source"], json!("same_capture"));
    assert_eq!(state(&same["voltage"]), "verified", "{same}");

    let shifted = plot_ir("48");
    assert_eq!(state(&shifted["latency"]), "refused", "{shifted}");
    assert_eq!(state(&shifted["voltage"]), "verified", "{shifted}");
    assert_eq!(
        state(&shifted["pair"]["voltage"]),
        "unverified",
        "verification never propagates: {shifted}"
    );
    assert_eq!(cause(&shifted["pair"]["voltage"]), "not_covered");
}

/// `calibrate` on an arbitrary pair, τ measured.
fn calibrate_pair(c: &Client, o: u32, i: u32) -> Value {
    let r = c.call(json!({"cmd": "calibrate", "output_channel": o, "input_channel": i}));
    assert_eq!(r["ok"], json!(true), "calibrate ack: {r}");
    let _ = expect_prompt(c, 1);
    let _ = c.call(json!({"cmd": "cal_reply", "vrms": 0.034641}));
    let _ = expect_prompt(c, 2);
    let _ = c.call(json!({"cmd": "cal_reply", "vrms": 0.034995}));
    let done = c
        .wait_for_topic("cal_done", Duration::from_secs(15))
        .expect("cal_done");
    assert_eq!(done["tau_state"], json!("measured"), "{done}");
    done
}

/// The next frame on `topic`, whatever its `cmd`.
fn frame_on_topic(c: &Client, topic: &str, timeout: Duration) -> Option<Value> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let left = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(left.max(1)) {
            Some((t, v)) if t == topic => return Some(v),
            Some(_) => continue,
            None => return None,
        }
    }
    None
}

/// One `plot_ir` run's archived report, as JSON and decoded.
fn plot_ir_report(d: &Daemon) -> (Value, ac_core::measurement::report::MeasurementReport) {
    let c = Client::new(d);
    let ack = c.call(json!({
        "cmd": "plot_ir", "f1_hz": 200.0, "f2_hz": 8000.0, "duration": 0.5,
        "level_dbfs": -40.0, "tail_s": 0.1, "window_len": 4096, "n_harmonics": 3,
    }));
    assert_eq!(ack["ok"], json!(true), "{ack}");
    let frame = frame_on_topic(&c, "measurement/report", Duration::from_secs(30))
        .unwrap_or_else(|| panic!("no measurement/report\n{}", d.log_tail()));
    let json = frame["report"].clone();
    let report = serde_json::from_value(json.clone()).expect("decode MeasurementReport");
    (json, report)
}

/// Triage AC8(b) for delay, and R3-1 / R6-2 / R6-7: a persisted τ refusal
/// that reaches the measurement pair is frozen into `plot_ir`'s report and
/// withholds the flight time, even with no reference configured. The bypass
/// leg (no refusal on disk) shows the same stored τ consumed, so the test
/// can fail.
#[test]
fn a_persisted_latency_refusal_withholds_the_pair_tau_in_plot_ir() {
    let d1 = Daemon::spawn_with_config(Some(split_config()));
    let c1 = Client::new(&d1);
    calibrate_pair(&c1, 0, 0);
    calibrate_loopback(&c1);

    // Session 2: τ shifted; the check refuses and reaches out0_in0.
    let d2 = Daemon::spawn_with(
        Some(split_config()),
        &[("AC_FAKE_TAU_DELAY_SAMPLES_OVERRIDE", "48,48")],
    );
    share_state(&d1, &d2);
    let check = run_check(&Client::new(&d2));
    assert_eq!(state(&check["latency"]), "refused", "{check}");
    assert!(
        check["reach"]["latency"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["key"] == json!("out0_in0")),
        "{check}"
    );

    // Rejected behaviour: no refusal on disk, no reference → τ applied.
    let bypass = Daemon::spawn_with_config(Some(json!({})));
    share_state(&d1, &bypass);
    let (r, report) = plot_ir_report(&bypass);
    let latency = &r["interface_latency"];
    assert_eq!(latency["state"], json!("measured"), "{r}");
    assert_eq!(state(&latency["session_check"]), "unverified", "{latency}");
    assert_eq!(cause(&latency["session_check"]), "no_loopback", "{latency}");
    let stats = report.ir_stats().expect("ir stats");
    assert!(stats.flight_time_s.is_some(), "stale τ consumed: {latency}");
    drop(bypass);

    // The refusal on disk, reference removed → the stored τ is kept in the
    // report but not applied.
    let d3 = Daemon::spawn_with_config(Some(json!({})));
    share_state(&d2, &d3);
    let (r, report) = plot_ir_report(&d3);
    let latency = &r["interface_latency"];
    assert_eq!(latency["state"], json!("measured"), "{r}");
    assert!(latency["tau_s"].is_f64(), "{latency}");
    assert_eq!(state(&latency["session_check"]), "refused", "{latency}");
    assert_eq!(
        latency["session_check"]["via"],
        json!(LOOPBACK),
        "{latency}"
    );
    assert!(latency.get("session_check_loopback").is_none(), "{latency}");
    let stats = report.ir_stats().expect("ir stats");
    assert_eq!(
        stats.flight_time_s, None,
        "a refused τ was applied: {latency}"
    );
}

/// Leg 7: a silent return is refused as a bound, and the refusal reaches
/// every other stored voltage entry.
#[test]
fn a_silent_return_is_refused_as_a_bound_and_reaches_every_voltage_entry() {
    let d1 = Daemon::spawn_with_config(Some(split_config()));
    calibrate_loopback(&Client::new(&d1));
    seed_voltage_entries(&d1, &[(0, 0), (3, 3)]);

    let d2 = Daemon::spawn_with(
        Some(split_config()),
        &[("AC_FAKE_TAU_GAIN_OVERRIDE", "0.0")],
    );
    share_state(&d1, &d2);
    let c2 = Client::new(&d2);
    let check = run_check(&c2);
    assert_eq!(state(&check["voltage"]), "refused", "{check}");
    assert_eq!(check["voltage"]["delta_bound"], json!("at_most"));
    assert_eq!(check["reach"]["voltage"], json!(["out0_in0", "out3_in3"]));
    // A silent loop does not measure τ; it is never read as a τ refusal.
    assert_ne!(state(&check["latency"]), "refused", "{check}");

    let other = get_cal(&c2, 0, 0);
    let v = &other["session_check"]["voltage"];
    assert_eq!(state(v), "refused", "{other}");
    assert_eq!(v["via"], json!(LOOPBACK));
}

/// Leg 9: `session_refusals.json` unreadable (UX tests a–c, e, g).
#[test]
fn an_unreadable_refusal_record_is_never_read_as_clear() {
    const GARBAGE: &[u8] = b"{ not json";
    let d1 = Daemon::spawn_with_config(Some(split_config()));
    calibrate_loopback(&Client::new(&d1));
    seed_voltage_entries(&d1, &[(0, 0)]);

    // (c) no check: every stored layer reads `refusals_unreadable`.
    let d2 = Daemon::spawn_with_config(Some(split_config()));
    share_state(&d1, &d2);
    fs::write(refusals_path(&d2), GARBAGE).unwrap();
    let c2 = Client::new(&d2);
    let lb = get_cal(&c2, 1, 1);
    assert!(lb["refusal_record"]["unreadable"].is_string(), "{lb}");
    for layer in ["voltage", "latency"] {
        assert_eq!(
            cause(&lb["session_check"][layer]),
            "refusals_unreadable",
            "{lb}"
        );
    }
    let other = get_cal(&c2, 0, 0);
    assert_eq!(
        cause(&other["session_check"]["voltage"]),
        "refusals_unreadable"
    );
    // Rejected behaviour: the file treated as missing reads `not_checked`.
    let missing = Daemon::spawn_with_config(Some(split_config()));
    share_state(&d1, &missing);
    let lm = get_cal(&Client::new(&missing), 1, 1);
    assert_eq!(lm["refusal_record"], json!("ok"));
    assert_eq!(
        cause(&lm["session_check"]["voltage"]),
        "not_checked",
        "{lm}"
    );
    drop(missing);

    // (a) a passing check decides for its own pair only.
    let check = run_check(&c2);
    assert_eq!(state(&check["voltage"]), "verified", "{check}");
    let lb = get_cal(&c2, 1, 1);
    assert_eq!(state(&lb["session_check"]["voltage"]), "verified", "{lb}");
    let other = get_cal(&c2, 0, 0);
    assert_eq!(
        cause(&other["session_check"]["voltage"]),
        "refusals_unreadable"
    );
    assert_eq!(fs::read(refusals_path(&d2)).unwrap(), GARBAGE);
    drop(d2);

    // (b) a refusing check is held, never written over the file.
    let d3 = Daemon::spawn_with(
        Some(split_config()),
        &[("AC_FAKE_TAU_GAIN_OVERRIDE", GAIN_SHIFT)],
    );
    share_state(&d1, &d3);
    fs::write(refusals_path(&d3), GARBAGE).unwrap();
    let c3 = Client::new(&d3);
    let check = run_check(&c3);
    assert_eq!(state(&check["voltage"]), "refused", "{check}");
    assert_eq!(check["persisted"], json!(false), "{check}");
    assert_eq!(check["persist_error"]["kind"], json!("unreadable"));
    assert_eq!(
        fs::read(refusals_path(&d3)).unwrap(),
        GARBAGE,
        "file untouched"
    );
    let lb = get_cal(&c3, 1, 1);
    assert_eq!(state(&lb["session_check"]["voltage"]), "refused", "{lb}");

    // (e) repair the file: the next access writes the held refusal, and a
    // later daemon reads it.
    fs::write(refusals_path(&d3), b"{}").unwrap();
    let lb = get_cal(&c3, 1, 1);
    assert_eq!(lb["refusal_record"], json!("ok"), "{lb}");
    assert_eq!(
        lb["session_check"]["recorded"]["voltage"]["persisted"],
        json!(true)
    );
    let file: Value = serde_json::from_slice(&fs::read(refusals_path(&d3)).unwrap()).unwrap();
    assert_eq!(
        state(&file["voltage"][LOOPBACK]["voltage"]),
        "refused",
        "{file}"
    );
    let d4 = Daemon::spawn_with_config(Some(split_config()));
    share_state(&d3, &d4);
    let lb = get_cal(&Client::new(&d4), 1, 1);
    assert_eq!(state(&lb["session_check"]["voltage"]), "refused", "{lb}");
}

/// Leg 9 (g), UX R4-3 and R4-5: a readable file whose write fails is held
/// with `write_failed`, the detail is the root io error with no path, and
/// the hold ends at the next access that can write.
#[cfg(unix)]
#[test]
fn a_failed_write_is_held_with_its_root_cause() {
    use std::os::unix::fs::PermissionsExt;
    let d1 = Daemon::spawn_with_config(Some(split_config()));
    calibrate_loopback(&Client::new(&d1));

    let d2 = Daemon::spawn_with(
        Some(split_config()),
        &[("AC_FAKE_TAU_GAIN_OVERRIDE", GAIN_SHIFT)],
    );
    share_state(&d1, &d2);
    fs::write(refusals_path(&d2), b"{}").unwrap();
    let dir = ac_dir(&d2.home);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();
    if fs::write(dir.join("probe-writable"), b"x").is_ok() {
        let _ = fs::remove_file(dir.join("probe-writable"));
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        eprintln!("skipped: running as root, a read-only directory is still writable");
        return;
    }

    let c2 = Client::new(&d2);
    let check = run_check(&c2);
    assert_eq!(state(&check["voltage"]), "refused", "{check}");
    assert_eq!(check["persisted"], json!(false), "{check}");
    assert_eq!(
        check["persist_error"]["kind"],
        json!("write_failed"),
        "{check}"
    );
    let detail = check["persist_error"]["detail"].as_str().unwrap();
    assert!(detail.contains("os error"), "{detail}");
    assert!(!detail.contains('/'), "no path in the detail: {detail}");

    // The rejected form: the wrapped error's top line is a path, not a cause.
    let wrapped = ac_core::shared::atomic_write::write_atomic(&dir.join("x.json"), b"{}")
        .expect_err("the directory is read-only");
    assert!(!format!("{wrapped}").contains("os error"), "{wrapped}");

    // R4-5: writable again, the next access writes the held refusal.
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    let lb = get_cal(&c2, 1, 1);
    assert_eq!(
        lb["session_check"]["recorded"]["voltage"]["persisted"],
        json!(true),
        "{lb}"
    );
    assert!(lb["session_check"]["recorded"]["voltage"]
        .get("persist_error")
        .is_none());
}

/// Leg 10: `session_check` is exclusive with every other worker.
#[test]
fn session_check_is_refused_while_a_worker_runs() {
    let d = Daemon::spawn_with_config(Some(split_config()));
    let c = Client::new(&d);
    let g = c.call(json!({"cmd": "generate", "freq_hz": 1000.0, "level_dbfs": -40.0}));
    assert_eq!(g["ok"], json!(true), "{g}");
    let r = c.call(json!({"cmd": "session_check"}));
    assert_eq!(r["ok"], json!(false), "{r}");
    assert!(r["error"].as_str().unwrap().starts_with("busy:"), "{r}");
    assert!(
        frame_on(
            &c,
            "session_check",
            "session_check",
            Duration::from_millis(800)
        )
        .is_none(),
        "a refused check publishes nothing"
    );
    let _ = c.call(json!({"cmd": "stop", "name": "generate"}));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let r = c.call(json!({"cmd": "session_check"}));
        if r["ok"] == json!(true) {
            assert!(frame_on(
                &c,
                "session_check",
                "session_check",
                Duration::from_secs(15)
            )
            .is_some());
            break;
        }
        assert!(Instant::now() < deadline, "still refused after stop: {r}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// No reference loopback: the command does not run, and says why.
#[test]
fn session_check_without_a_loopback_does_not_run() {
    let d = Daemon::spawn();
    let r = Client::new(&d).call(json!({"cmd": "session_check"}));
    assert_eq!(r["ok"], json!(false), "{r}");
    assert!(r["error"]
        .as_str()
        .unwrap()
        .starts_with("no reference loopback configured"));
    assert_eq!(r["refusal_record"], json!("ok"));
}
