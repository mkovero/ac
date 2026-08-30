use serde_json::json;
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;

use crate::common::{Client, Daemon};

#[test]
fn calibrate_prompt_reply_cycle() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let r = c.call(json!({"cmd":"calibrate"}));
    assert_eq!(r["ok"], json!(true));

    // The calibrate worker drives through several prompts; send "skip" (reply:null)
    // to each until we see a terminal frame.
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut saw_done = false;
    while Instant::now() < deadline {
        match c.recv_pub(2_000) {
            Some((topic, _payload)) if topic == "cal_prompt" => {
                let _ = c.call(json!({"cmd":"cal_reply", "vrms": null}));
            }
            Some((topic, _)) if topic == "done" || topic == "cal_done" => {
                saw_done = true;
                break;
            }
            Some(_) => continue,
            None => break,
        }
    }
    assert!(saw_done, "calibrate cycle never completed");
}

#[test]
fn calibrate_scales_user_reading_to_zero_dbfs() {
    // Reference tone plays at `ref_dbfs` (default -10 dBFS), so a Vrms
    // reading taken there is `1 / dbfs_to_amplitude(ref_dbfs)` smaller
    // than the Vrms at 0 dBFS. The handler MUST apply that scaling
    // before saving — otherwise a user who calibrates at -10 dBFS and
    // reads 2.095 V would get `0 dBu = 2.095 V` from `ac generate`,
    // ~10 dB hotter than what they asked for.
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let r = c.call(json!({"cmd": "calibrate", "ref_dbfs": -10.0,
                           "output_channel": 0, "input_channel": 0}));
    assert_eq!(r["ok"], json!(true));

    // Step 1 prompt → reply with a known DAC reading.
    let _ = c
        .wait_for_topic("cal_prompt", Duration::from_secs(3))
        .expect("step 1 prompt");
    let user_out_vrms = 2.095_f64;
    let _ = c.call(json!({"cmd": "cal_reply", "vrms": user_out_vrms}));

    // Step 2 prompt — fake backend loops the played tone back, so the
    // captured input level matches the played `ref_dbfs - 3.01` (RMS
    // vs peak), and the handler should flag `loopback: true`.
    let p2 = c
        .wait_for_topic("cal_prompt", Duration::from_secs(3))
        .expect("step 2 prompt");
    assert_eq!(
        p2["loopback"],
        json!(true),
        "expected loopback flag in step 2: {p2}"
    );
    let _ = c.call(json!({"cmd": "cal_reply", "vrms": user_out_vrms}));

    let done = c
        .wait_for_topic("cal_done", Duration::from_secs(5))
        .expect("cal_done frame");

    // ref_dbfs = -10 → out_scale = 10^(10/20) ≈ 3.16228.
    let expected_out = user_out_vrms * 10f64.powf(10.0 / 20.0);
    let saved_out = done["vrms_at_0dbfs_out"].as_f64().expect("out");
    assert!(
        (saved_out - expected_out).abs() < 1e-6,
        "vrms_at_0dbfs_out: got {saved_out}, want {expected_out}",
    );

    // Cross-check via get_calibration so we know it round-tripped to disk.
    let r = c.call(json!({"cmd": "get_calibration",
                           "output_channel": 0, "input_channel": 0}));
    assert_eq!(r["found"], json!(true));
    let stored_out = r["vrms_at_0dbfs_out"].as_f64().expect("stored out");
    assert!((stored_out - expected_out).abs() < 1e-6);
}

/// Drive a `calibrate` run to completion, skipping every voltage prompt
/// (`vrms: null`), and return the `cal_done` payload. `cmd` supplies any
/// extra fields (`output_channel` / `input_channel` / `ref_dbfs`) merged
/// into the `calibrate` request.
fn run_calibrate_skip_all(c: &Client, mut cmd: Value) -> Value {
    cmd["cmd"] = json!("calibrate");
    let r = c.call(cmd);
    assert_eq!(r["ok"], json!(true), "calibrate ack: {r}");
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((topic, _)) if topic == "cal_prompt" => {
                let _ = c.call(json!({"cmd":"cal_reply", "vrms": null}));
            }
            Some((topic, payload)) if topic == "cal_done" => return payload,
            Some(_) => continue,
            None => break,
        }
    }
    panic!("calibrate run never reached cal_done");
}

/// #370, acceptance criterion 1: `cal_done` carries the resolved input/output
/// port names actually used — server-side, not the client's copy of the
/// request — so a scan across channels stops reading as a flat, plausible
/// result when every run actually measured the same port.
#[test]
fn cal_done_reports_resolved_ports() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let done = run_calibrate_skip_all(&c, json!({"output_channel": 2, "input_channel": 3}));
    assert_eq!(
        done["output_port"],
        json!("fake:playback_2"),
        "cal_done: {done}"
    );
    assert_eq!(
        done["input_port"],
        json!("fake:capture_3"),
        "cal_done: {done}"
    );
}

/// #370, acceptance criterion 3 (the failing case named in the triage spec):
/// a config.json edit made between two measurements against one long-lived
/// daemon must reach the second one. Before the per-request reload in
/// `dispatch()`, this is exactly the reporter's repro — an auto-spawned
/// daemon outlives the `ac` command that spawned it, so a channel-scan
/// script editing `input_channel` between runs silently re-measured the
/// first channel every time.
#[test]
fn calibrate_picks_up_a_config_edit_made_between_two_runs() {
    let d = Daemon::spawn_with_config(Some(json!({"input_channel": 1})));
    let c = Client::new(&d);

    let done1 = run_calibrate_skip_all(&c, json!({}));
    assert_eq!(
        done1["input_port"],
        json!("fake:capture_1"),
        "first run: {done1}"
    );

    // Same daemon process, no restart — just the config file changing
    // underneath it, exactly as an operator's editor would.
    let cfg_path = d.home.join(".config").join("ac").join("config.json");
    fs::write(
        &cfg_path,
        serde_json::to_vec_pretty(&json!({"input_channel": 2})).unwrap(),
    )
    .expect("rewrite config.json");

    let done2 = run_calibrate_skip_all(&c, json!({}));
    assert_eq!(
        done2["input_port"],
        json!("fake:capture_2"),
        "second run: {done2}"
    );
    assert_ne!(
        done1["input_port"], done2["input_port"],
        "config edit between runs must change the resolved input port"
    );
}

/// #370, acceptance criterion 4: where the running daemon cannot serve the
/// current on-disk config (unparseable JSON, e.g. a file caught mid-write),
/// a routing command must say so and refuse rather than silently serving
/// against the last-known-good in-memory config. Non-routing commands
/// (`status`) stay reachable so the operator can tell what's wrong without
/// a restart.
#[test]
fn routing_command_refuses_when_config_json_is_unparseable() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let cfg_path = d.home.join(".config").join("ac").join("config.json");
    fs::write(&cfg_path, b"{ not json").expect("write malformed config.json");

    let r = c.call(json!({"cmd": "calibrate"}));
    assert_eq!(r["ok"], json!(false), "expected refusal: {r}");
    let err = r["error"].as_str().unwrap_or("");
    assert!(
        err.contains("config.json"),
        "error should name config.json: {r}"
    );
    // `{e:#}` (not `{e}`) on the reload's Err arm: the reply must carry the
    // actual parse failure, not just the file path — that's what makes the
    // refusal diagnosable rather than merely visible.
    assert!(
        err.contains("line") || err.contains("column") || err.to_lowercase().contains("expected"),
        "error should name *why* config.json failed to parse, not just that it did: {r}"
    );

    let s = c.call(json!({"cmd": "status"}));
    assert_eq!(s["ok"], json!(true), "status must still answer: {s}");
}

/// #281 QA correctness issue 1: `measure_tau`'s sweep→deconvolve→peak→seconds
/// path had zero test coverage — the only τ tests (`calibration.rs`)
/// construct `TauEntry`/`TauConditions` directly and never call
/// `measure_tau`. The fake backend's `play_and_capture` delays by a fixed
/// `FAKE_LOOPBACK_DELAY_SAMPLES = 32` (see `audio/fake.rs`), the same
/// deterministic delay `plot_ir_emits_impulse_response_with_expected_delay_peak`
/// already checks its IR peak against — this is that precedent applied to
/// `calibrate`'s τ measurement.
#[test]
fn calibrate_measures_tau_against_fake_loopback_delay() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let r = c.call(json!({"cmd": "calibrate", "ref_dbfs": -10.0,
                           "output_channel": 0, "input_channel": 0}));
    assert_eq!(r["ok"], json!(true));

    // Both prompts skipped — τ must still be measured (it keys only on
    // `is_loopback`, established at step 2, independent of the replies).
    for step in 1..=2 {
        c.wait_for_topic("cal_prompt", Duration::from_secs(5))
            .unwrap_or_else(|| panic!("step {step} prompt"));
        let _ = c.call(json!({"cmd": "cal_reply", "vrms": null}));
    }
    let done = c
        .wait_for_topic("cal_done", Duration::from_secs(5))
        .expect("cal_done frame");

    assert_eq!(done["tau_state"], json!("measured"), "frame: {done}");
    let tau_s = done["tau_s"].as_f64().expect("tau_s present when measured");
    let expected = 32.0 / 48_000.0; // FAKE_LOOPBACK_DELAY_SAMPLES / fake sample rate
    assert!(
        (tau_s - expected).abs() < 1e-4,
        "tau_s {tau_s} far from expected {expected} (32-sample fake loopback delay): {done}"
    );
    assert_eq!(done["tau_sample_rate"], json!(48_000), "frame: {done}");
    // #347: "measured" now means two independently-lifecycled readings
    // agreed — the fake backend's fixed loopback delay makes both
    // lifecycles land on the same 32-sample reading, so this must corroborate.
    assert_eq!(done["tau_agreement_count"], json!(2), "frame: {done}");
    assert!(done["tau_reading1_s"].as_f64().is_some(), "frame: {done}");
    assert!(done["tau_reading2_s"].as_f64().is_some(), "frame: {done}");
    // ZMQ.md: tau_delta_samples is present only on disagree_* — an Agree
    // outcome must not serialize a stray Some(0) (QA #348 correctness 1).
    assert!(done.get("tau_delta_samples").is_none(), "frame: {done}");
}

/// QA #348 test-coverage gap: every other disagreement test drives
/// `compare_tau_readings` or `tau_result` as a pure function, never
/// `measure_tau_twice` itself — the function that actually spins up two
/// engine lifecycles and feeds them into the comparison. A bug that mixed
/// up which lifecycle's `TauConditions` or reading fed the comparison
/// would pass every other test in this file. Drives it for real through
/// `calibrate`, using the fake backend's env-var delay/period-size test
/// hooks (`ac-daemon/src/audio/fake.rs`) to make the two lifecycles land
/// exactly one `period_size` apart.
#[test]
fn calibrate_reports_disagree_period_shift_end_to_end() {
    let d = Daemon::spawn_with_env(&[
        ("AC_FAKE_TAU_DELAY_SAMPLES_OVERRIDE", "32,1056"),
        ("AC_FAKE_PERIOD_SIZE_OVERRIDE", "1024"),
    ]);
    let cal_path = d.home.join(".config").join("ac").join("cal.json");
    let c = Client::new(&d);

    let r = c.call(json!({"cmd": "calibrate", "ref_dbfs": -10.0,
                           "output_channel": 0, "input_channel": 0}));
    assert_eq!(r["ok"], json!(true));

    for step in 1..=2 {
        c.wait_for_topic("cal_prompt", Duration::from_secs(5))
            .unwrap_or_else(|| panic!("step {step} prompt"));
        let _ = c.call(json!({"cmd": "cal_reply", "vrms": null}));
    }
    let done = c
        .wait_for_topic("cal_done", Duration::from_secs(5))
        .expect("cal_done frame");

    // 1056 - 32 = 1024 samples = exactly one period_size — the graph-
    // buffering shift the whole PR is about, not a generic fault.
    assert_eq!(
        done["tau_state"],
        json!("disagree_period_shift"),
        "frame: {done}"
    );
    assert_eq!(done["tau_periods"], json!(1), "frame: {done}");
    assert_eq!(done["tau_delta_samples"], json!(1024), "frame: {done}");
    assert_eq!(
        done["tau_s"],
        json!(null),
        "a disagreement must not report a τ: {done}"
    );
    assert_eq!(done["tau_agreement_count"], json!(0), "frame: {done}");
    assert!(done["tau_reading1_s"].as_f64().is_some(), "frame: {done}");
    assert!(done["tau_reading2_s"].as_f64().is_some(), "frame: {done}");
    assert!(done["tau_error"].as_str().is_some(), "frame: {done}");

    // Refused, not stored — no entry in tau_history at all.
    let after = read_cal_entry(&cal_path);
    assert!(
        after.get("tau_history").is_none()
            || after["tau_history"]
                .as_array()
                .is_some_and(|a| a.is_empty()),
        "a disagreement must not append to tau_history: {after}"
    );
}

/// #281 QA correctness issue 2: the cheap-refresh criterion (#279: both
/// voltage prompts skipped still refreshes stored state cheaply) is an
/// explicit issue acceptance criterion for τ too — a skipped-both-prompts
/// run must still append a fresh `tau_history` entry, not just leave the
/// voltage legs alone. Previously asserted only by reading the code (τ's
/// branch is keyed on `is_loopback`, not on either reply); this test pins
/// it down on the wire and on disk.
#[test]
fn calibrate_cheap_refresh_still_measures_tau() {
    let d = Daemon::spawn();
    let cal_path = seed_voltage_cal(&d, 2.345_67, 1.234_56, -20.0);
    let c = Client::new(&d);

    let r = c.call(json!({"cmd": "calibrate", "ref_dbfs": -10.0,
                          "output_channel": 0, "input_channel": 0}));
    assert_eq!(r["ok"], json!(true));

    for step in 1..=2 {
        c.wait_for_topic("cal_prompt", Duration::from_secs(5))
            .unwrap_or_else(|| panic!("step {step} prompt"));
        let _ = c.call(json!({"cmd": "cal_reply", "vrms": null}));
    }
    let done = c
        .wait_for_topic("cal_done", Duration::from_secs(5))
        .expect("cal_done frame");

    // Both voltage legs unchanged (the #279 path this test rides on)...
    assert_eq!(done["out_state"], json!("unchanged"), "frame: {done}");
    assert_eq!(done["in_state"], json!("unchanged"), "frame: {done}");
    // ...but τ was measured anyway.
    assert_eq!(done["tau_state"], json!("measured"), "frame: {done}");
    assert!(done["tau_s"].as_f64().is_some(), "frame: {done}");

    let after = read_cal_entry(&cal_path);
    let history = after["tau_history"]
        .as_array()
        .expect("tau_history present");
    assert_eq!(
        history.len(),
        1,
        "a cheap-refresh run must still append a tau_history entry: {after}"
    );
    // #347: a stored entry must record how many readings agreed — never
    // `1`, since a lone reading is no longer a storable outcome.
    assert_eq!(
        history[0]["agreement_count"],
        json!(2),
        "stored entry must record its corroboration count: {after}"
    );
}

/// Seed a `cal.json` entry with both voltage legs set, at a `ref_dbfs`
/// deliberately different from the one the test's `calibrate` run uses.
fn seed_voltage_cal(d: &Daemon, out_vrms: f64, in_vrms: f64, ref_dbfs: f64) -> PathBuf {
    let path = d.home.join(".config").join("ac").join("cal.json");
    let seeded = json!({
        "out0_in0": {
            "output_channel":                   0,
            "input_channel":                    0,
            "ref_freq":                         1000.0,
            "vrms_at_0dbfs_out":                out_vrms,
            "vrms_at_0dbfs_in":                 in_vrms,
            "ref_dbfs":                         ref_dbfs,
            "mic_sensitivity_dbfs_at_94db_spl": -32.5,
            "mic_response":                     null,
        }
    });
    fs::write(&path, serde_json::to_vec_pretty(&seeded).unwrap()).expect("seed cal.json");
    path
}

fn read_cal_entry(path: &PathBuf) -> Value {
    let raw = fs::read_to_string(path).expect("read cal.json");
    let all: Value = serde_json::from_str(&raw).expect("parse cal.json");
    all["out0_in0"].clone()
}

/// #279: a skipped prompt must preserve the stored reading, not erase it.
///
/// Mutation check — against the pre-fix handler, which assigned
/// `reading.map(..)` unconditionally, both `vrms_at_0dbfs_*` come back
/// `null` and every assertion below on a preserved value fails.
#[test]
fn calibrate_skipped_prompts_preserve_stored_voltage_cal() {
    let d = Daemon::spawn();
    let cal_path = seed_voltage_cal(&d, 2.345_67, 1.234_56, -20.0);
    let before = read_cal_entry(&cal_path);
    let c = Client::new(&d);

    // Run at a *different* ref_dbfs than the seeded entry records, so a
    // handler that rewrites `ref_dbfs` on a no-measurement run is caught
    // too — the stored level tag must keep describing the stored readings.
    let r = c.call(json!({"cmd": "calibrate", "ref_dbfs": -10.0,
                          "output_channel": 0, "input_channel": 0}));
    assert_eq!(r["ok"], json!(true));

    for step in 1..=2 {
        c.wait_for_topic("cal_prompt", Duration::from_secs(5))
            .unwrap_or_else(|| panic!("step {step} prompt"));
        let _ = c.call(json!({"cmd": "cal_reply", "vrms": null}));
    }
    let done = c
        .wait_for_topic("cal_done", Duration::from_secs(5))
        .expect("cal_done frame");

    // Three states, three words: nothing was measured, so both legs read
    // "unchanged" — not the null-valued frame that reads as "not measured".
    assert_eq!(done["out_state"], json!("unchanged"), "frame: {done}");
    assert_eq!(done["in_state"], json!("unchanged"), "frame: {done}");
    assert_eq!(done["vrms_at_0dbfs_out"], before["vrms_at_0dbfs_out"]);
    assert_eq!(done["vrms_at_0dbfs_in"], before["vrms_at_0dbfs_in"]);

    // On disk: both voltage fields survive bit-identical, and so does the
    // `ref_dbfs` that describes what level they were taken at.
    let after = read_cal_entry(&cal_path);
    assert_eq!(
        after["vrms_at_0dbfs_out"], before["vrms_at_0dbfs_out"],
        "skipping step 1 must not touch the stored output cal"
    );
    assert_eq!(
        after["vrms_at_0dbfs_in"], before["vrms_at_0dbfs_in"],
        "skipping step 2 must not touch the stored input cal"
    );
    assert_eq!(
        after["ref_dbfs"], before["ref_dbfs"],
        "a run that measured nothing must not relabel the stored readings"
    );
    // The other cal layers were already preserved via `load_or_new`;
    // assert it so this test fails if the preservation path is rewritten.
    assert_eq!(
        after["mic_sensitivity_dbfs_at_94db_spl"], before["mic_sensitivity_dbfs_at_94db_spl"],
        "SPL layer must be untouched by a voltage calibrate run"
    );
}

/// #279: erasing is still possible, but only when asked for by name —
/// `clear: true`, not the same reply that means "I did not measure this".
/// The two legs are independent: one cleared, one measured, in one run.
#[test]
fn calibrate_clear_erases_only_the_leg_it_names() {
    let d = Daemon::spawn();
    let cal_path = seed_voltage_cal(&d, 2.345_67, 1.234_56, -20.0);
    let c = Client::new(&d);

    let r = c.call(json!({"cmd": "calibrate", "ref_dbfs": -10.0,
                          "output_channel": 0, "input_channel": 0}));
    assert_eq!(r["ok"], json!(true));

    c.wait_for_topic("cal_prompt", Duration::from_secs(5))
        .expect("step 1 prompt");
    let _ = c.call(json!({"cmd": "cal_reply", "vrms": null, "clear": true}));

    c.wait_for_topic("cal_prompt", Duration::from_secs(5))
        .expect("step 2 prompt");
    let in_reading = 1.5_f64;
    let _ = c.call(json!({"cmd": "cal_reply", "vrms": in_reading}));

    let done = c
        .wait_for_topic("cal_done", Duration::from_secs(5))
        .expect("cal_done frame");
    assert_eq!(done["out_state"], json!("absent"), "frame: {done}");
    assert_eq!(done["in_state"], json!("measured"), "frame: {done}");
    assert_eq!(done["vrms_at_0dbfs_out"], json!(null), "frame: {done}");

    let after = read_cal_entry(&cal_path);
    assert_eq!(
        after["vrms_at_0dbfs_out"],
        json!(null),
        "an explicit clear must erase the output cal"
    );
    let stored_in = after["vrms_at_0dbfs_in"]
        .as_f64()
        .expect("input cal stored");
    assert!(
        stored_in > in_reading,
        "the measured leg must be rewritten (scaled up from the captured level), \
         got {stored_in} for a {in_reading} V reading"
    );
    // A measurement happened, so the level tag follows this run.
    assert_eq!(after["ref_dbfs"], json!(-10.0));
}

/// #279 criterion 3: `absent` has two origins, not one. A skip on a leg
/// that was never calibrated must report `absent`, not `unchanged` — the
/// no-prior-value branch of `apply_cal_reading` is otherwise untested.
/// Mutating that branch to `=> "unchanged"` passes every other calibrate
/// test in this file and fails only this one.
#[test]
fn calibrate_skip_on_uncalibrated_leg_reports_absent() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let r = c.call(json!({"cmd": "calibrate", "ref_dbfs": -10.0,
                          "output_channel": 0, "input_channel": 0}));
    assert_eq!(r["ok"], json!(true));

    for step in 1..=2 {
        c.wait_for_topic("cal_prompt", Duration::from_secs(5))
            .unwrap_or_else(|| panic!("step {step} prompt"));
        let _ = c.call(json!({"cmd": "cal_reply", "vrms": null}));
    }
    let done = c
        .wait_for_topic("cal_done", Duration::from_secs(5))
        .expect("cal_done frame");

    assert_eq!(done["out_state"], json!("absent"), "frame: {done}");
    assert_eq!(done["in_state"], json!("absent"), "frame: {done}");
    assert_eq!(done["vrms_at_0dbfs_out"], json!(null), "frame: {done}");
    assert_eq!(done["vrms_at_0dbfs_in"], json!(null), "frame: {done}");
}

/// #294 QA correctness issue 1: a cancel (`{"cmd": "stop"}`, what the
/// CLI's `q` sends) at the *second* prompt must not commit the first
/// prompt's reading. Pre-fix, the worker had no stop check after step 2
/// and fell through to `cal.save()` — the operator was told "Calibration
/// cancelled." while `vrms_at_0dbfs_out` and `ref_dbfs` were overwritten
/// anyway.
#[test]
fn calibrate_cancel_at_second_prompt_saves_nothing() {
    let d = Daemon::spawn();
    let cal_path = seed_voltage_cal(&d, 2.345_67, 1.234_56, -20.0);
    let before = read_cal_entry(&cal_path);
    let c = Client::new(&d);

    let r = c.call(json!({"cmd": "calibrate", "ref_dbfs": -10.0,
                          "output_channel": 0, "input_channel": 0}));
    assert_eq!(r["ok"], json!(true));

    c.wait_for_topic("cal_prompt", Duration::from_secs(5))
        .expect("step 1 prompt");
    let _ = c.call(json!({"cmd": "cal_reply", "vrms": 2.095}));

    c.wait_for_topic("cal_prompt", Duration::from_secs(5))
        .expect("step 2 prompt");
    let _ = c.call(json!({"cmd": "stop"}));

    // No cal_done at all — the run aborted, it did not complete with an
    // "unchanged"/"absent" verdict.
    assert!(
        c.wait_for_topic("cal_done", Duration::from_millis(500))
            .is_none(),
        "a cancelled run must not emit cal_done"
    );

    let after = read_cal_entry(&cal_path);
    assert_eq!(
        after, before,
        "a cancel at the second prompt must leave the stored entry \
         byte-identical, including the leg answered before the cancel"
    );
}

/// #295: symmetric with `calibrate_cancel_at_second_prompt_saves_nothing`,
/// but the cancel lands at the *first* prompt instead — the path that
/// worked all along, per the step-1 stop check at `handlers/calibrate.rs`
/// (checked immediately after `wait_cal_reply` for the output leg, before
/// `cal.save()`). No test pinned it, so a future edit to that check could
/// regress silently.
///
/// Mutation check — remove the step-1 stop check (or replace it with a
/// no-op) and this test must fail: `cal_done` would arrive instead of
/// timing out, and/or the stored entry would gain the seeded run's
/// `ref_dbfs`/output reading even though the operator cancelled before
/// answering it.
#[test]
fn calibrate_cancel_at_first_prompt_saves_nothing() {
    let d = Daemon::spawn();
    let cal_path = seed_voltage_cal(&d, 2.345_67, 1.234_56, -20.0);
    let before = read_cal_entry(&cal_path);
    let c = Client::new(&d);

    let r = c.call(json!({"cmd": "calibrate", "ref_dbfs": -10.0,
                          "output_channel": 0, "input_channel": 0}));
    assert_eq!(r["ok"], json!(true));

    c.wait_for_topic("cal_prompt", Duration::from_secs(5))
        .expect("step 1 prompt");
    let _ = c.call(json!({"cmd": "stop"}));

    // The step-1 check must return *before* step 2 is ever built — no
    // second cal_prompt. This is the assertion that actually pins the
    // step-1 check: the step-2 stop check (same `stop` flag, checked
    // again after step 2's wait_cal_reply) would independently catch a
    // cancel that fell through step 1 and still block cal_done/the save,
    // so only the absence of a second prompt distinguishes "step 1 caught
    // it" from "step 1 was bypassed and step 2 caught it instead."
    assert!(
        c.wait_for_topic("cal_prompt", Duration::from_millis(500))
            .is_none(),
        "a cancel at the first prompt must not advance to a second prompt"
    );

    // No cal_done at all — the run aborted before ever reaching step 2.
    assert!(
        c.wait_for_topic("cal_done", Duration::from_millis(500))
            .is_none(),
        "a cancelled run must not emit cal_done"
    );

    let after = read_cal_entry(&cal_path);
    assert_eq!(
        after, before,
        "a cancel at the first prompt must leave the stored entry \
         byte-identical — nothing was answered before the cancel"
    );
}

/// `calibrate` clamps its `ref_dbfs` to `drive_max_dbfs`, and an omitted
/// `ref_dbfs` defaults to the ceiling rather than a hardcoded -10.0.
///
/// `cal_prompt` step 2's `captured_dbfs` is a genuine round trip through
/// the fake engine — `capture_rms` reads back whatever `eng.set_tone` was
/// actually given, via the same capture path `analyze_mono` and `plot`
/// use — not a re-statement of the request. A sine's RMS sits ~3.01 dB
/// below its peak amplitude, so a tone actually played at the ceiling
/// reads back at `ceiling - 3.01`, not at the ~-3.0 dBFS a full-scale,
/// unclamped 0 dBFS request would produce — the two are far enough apart
/// that a clamp that silently didn't apply cannot pass this by accident.
#[test]
fn calibrate_default_and_explicit_ref_dbfs_are_clamped_to_the_ceiling() {
    const CEILING_DBFS: f64 = -25.0;
    const PEAK_TO_RMS_DB: f64 = 3.0103; // 20·log10(√2)
    let d = Daemon::spawn_with_config(Some(json!({ "drive_max_dbfs": CEILING_DBFS })));
    let c = Client::new(&d);

    // No `ref_dbfs` at all: must default to the session ceiling, not the
    // historical hardcoded -10.0 (#360 acceptance criterion 2).
    let r = c.call(json!({"cmd": "calibrate", "output_channel": 0, "input_channel": 0}));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(
        r["ref_dbfs"],
        json!(CEILING_DBFS),
        "an omitted ref_dbfs must default to drive_max_dbfs: {r}"
    );

    let step1 = c
        .wait_for_topic("cal_prompt", Duration::from_secs(5))
        .expect("step 1 prompt");
    assert_eq!(step1["ref_dbfs"], json!(CEILING_DBFS));
    let _ = c.call(json!({"cmd": "cal_reply", "vrms": null}));

    let step2 = c
        .wait_for_topic("cal_prompt", Duration::from_secs(5))
        .expect("step 2 prompt");
    let captured_dbfs = step2["captured_dbfs"].as_f64().expect("captured_dbfs");
    let expected = CEILING_DBFS - PEAK_TO_RMS_DB;
    assert!(
        (captured_dbfs - expected).abs() < 1.5,
        "captured {captured_dbfs} dBFS does not match a tone actually played at the \
         {CEILING_DBFS} dBFS ceiling (expected ~{expected}) — the default was not clamped \
         before the tone was set"
    );
    let _ = c.call(json!({"cmd": "cal_reply", "vrms": null}));
    let _ = c.wait_for_topic("cal_done", Duration::from_secs(5));

    // Explicit request above the ceiling: also clamped, defense in depth.
    let r = c.call(json!({
        "cmd": "calibrate", "ref_dbfs": 0.0, "output_channel": 0, "input_channel": 0,
    }));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(
        r["ref_dbfs"],
        json!(CEILING_DBFS),
        "an explicit ref_dbfs above the ceiling must be clamped: {r}"
    );
    let step1 = c
        .wait_for_topic("cal_prompt", Duration::from_secs(5))
        .expect("step 1 prompt");
    assert_eq!(step1["ref_dbfs"], json!(CEILING_DBFS));
    let _ = c.call(json!({"cmd": "cal_reply", "vrms": null}));
    let step2 = c
        .wait_for_topic("cal_prompt", Duration::from_secs(5))
        .expect("step 2 prompt");
    let captured_dbfs = step2["captured_dbfs"].as_f64().expect("captured_dbfs");
    assert!(
        (captured_dbfs - expected).abs() < 1.5,
        "captured {captured_dbfs} dBFS does not match a tone actually played at the \
         {CEILING_DBFS} dBFS ceiling (expected ~{expected}) — an explicit request above \
         the ceiling reached the engine unclamped"
    );
}

/// QA gap fill: the presence/rejection tests above only exercise the
/// uncalibrated (`cal_tags` all "none", `spl: null`) path. This drives
/// the calibrated branch — voltage cal + SPL cal + mic curve all loaded
/// on the meas channel, nothing on the ref channel — and checks every
/// new tag flips to "on" (meas) / stays "none" (ref) accordingly, and
/// `spl` becomes a finite, plausible number.
#[test]
fn transfer_stream_cal_tags_and_spl_reflect_loaded_calibration() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    // Voltage cal on channel 0 (both directions get saved by `calibrate`).
    let r = c.call(json!({"cmd": "calibrate", "ref_dbfs": -10.0,
                           "output_channel": 0, "input_channel": 0}));
    assert_eq!(r["ok"], json!(true));
    let _ = c
        .wait_for_topic("cal_prompt", Duration::from_secs(3))
        .expect("step 1 prompt");
    let _ = c.call(json!({"cmd": "cal_reply", "vrms": 2.0}));
    let _ = c
        .wait_for_topic("cal_prompt", Duration::from_secs(3))
        .expect("step 2 prompt");
    let _ = c.call(json!({"cmd": "cal_reply", "vrms": 2.0}));
    let _ = c
        .wait_for_topic("cal_done", Duration::from_secs(5))
        .expect("cal_done");

    // SPL cal on channel 0.
    let r = c.call(json!({"cmd": "calibrate_spl", "input_channel": 0, "capture_s": 0.05}));
    assert_eq!(r["ok"], json!(true));
    let _ = c
        .wait_for_topic("cal_prompt", Duration::from_secs(3))
        .expect("spl cal_prompt");
    let _ = c.call(json!({"cmd": "cal_reply", "vrms": Value::Null}));
    let _ = c
        .wait_for_topic("cal_done", Duration::from_secs(5))
        .expect("spl cal_done");

    // Mic curve on channel 0.
    let (freqs, gains) = {
        let mut f = Vec::with_capacity(24);
        let mut g = Vec::with_capacity(24);
        let log_min = 100.0_f64.ln();
        let log_max = 10_000.0_f64.ln();
        for i in 0..24 {
            let t = i as f64 / 23.0;
            f.push((log_min + t * (log_max - log_min)).exp());
            g.push(3.0);
        }
        (f, g)
    };
    let r = c.call(json!({
        "cmd": "calibrate_mic_curve", "op": "set", "input_channel": 0,
        "freqs_hz": freqs, "gain_db": gains,
    }));
    assert_eq!(r["ok"], json!(true));
    while c.recv_pub(50).is_some() {}

    let r = c.call(json!({
        "cmd": "transfer_stream", "meas_channel": 0, "ref_channel": 1,
    }));
    assert_eq!(r["ok"], json!(true), "REP: {r:?}");

    let mut frame: Option<Value> = None;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "data" && v["type"].as_str() == Some("transfer_stream") => {
                frame = Some(v);
                break;
            }
            Some(_) => continue,
            None => break,
        }
    }
    let _ = c.call(json!({"cmd": "stop"}));
    let frame = frame.expect("no transfer_stream frame within 10 s");

    let meas_tags = &frame["cal_tags"]["meas"];
    assert_eq!(
        meas_tags["voltage"],
        json!("on"),
        "meas voltage tag: {frame}"
    );
    assert_eq!(meas_tags["spl"], json!("on"), "meas spl tag: {frame}");
    assert_eq!(
        meas_tags["mic_curve"],
        json!("on"),
        "meas mic_curve tag: {frame}"
    );

    let ref_tags = &frame["cal_tags"]["ref"];
    assert_eq!(
        ref_tags["voltage"],
        json!("none"),
        "ref voltage tag: {frame}"
    );
    assert_eq!(ref_tags["spl"], json!("none"), "ref spl tag: {frame}");
    assert_eq!(
        ref_tags["mic_curve"],
        json!("none"),
        "ref mic_curve tag: {frame}"
    );

    let spl = frame["spl"].as_f64().unwrap_or_else(|| {
        panic!("spl must be a finite number when meas channel is SPL-calibrated: {frame}")
    });
    assert!(
        spl.is_finite() && (0.0..=200.0).contains(&spl),
        "spl={spl} outside a plausible dB SPL range"
    );
}

// ---------------------------------------------------------------------------
// server_enable / server_disable — toggle listen_mode between local and
// public and check the reported bind_addr. #52.
// ---------------------------------------------------------------------------

#[test]
fn calibrate_spl_records_capture_dbfs() {
    // End-to-end SPL cal flow:
    //   1. send `calibrate_spl`,
    //   2. respond to `cal_prompt` (any reply ⇒ proceed),
    //   3. wait for `cal_done` carrying `mic_sensitivity_dbfs_at_94db_spl`.
    //
    // The fake backend's `capture_block` returns a 0.1-amplitude sine, so
    // the captured RMS ≈ 0.0707 → ≈ -23 dBFS. Verify the cal_done payload
    // sits in that range (±2 dB headroom for the second-harmonic tracer
    // the fake adds and rounding).
    let d = Daemon::spawn();
    let c = Client::new(&d);

    // Tell the daemon which channel to probe; pick something non-zero so
    // a regression that drops the field would show up as wrong-key writes.
    let r = c.call(json!({
        "cmd":           "calibrate_spl",
        "input_channel": 2,
        "capture_s":     0.2,
    }));
    assert_eq!(r["ok"], json!(true), "calibrate_spl ack: {r}");

    // Wait for the prompt, then release the worker.
    let prompt = c
        .wait_for_topic("cal_prompt", Duration::from_secs(3))
        .expect("no cal_prompt within 3 s");
    assert_eq!(prompt["kind"], json!("spl"), "prompt kind: {prompt}");

    let r = c.call(json!({"cmd": "cal_reply", "vrms": Value::Null}));
    assert_eq!(r["ok"], json!(true));

    let done = c
        .wait_for_topic("cal_done", Duration::from_secs(5))
        .expect("no cal_done within 5 s");
    let dbfs = done["mic_sensitivity_dbfs_at_94db_spl"]
        .as_f64()
        .expect("dbfs field missing");
    assert!(
        (-26.0..-19.0).contains(&dbfs),
        "captured dBFS {dbfs} outside fake-backend window",
    );
    assert!(done["key"].as_str().unwrap_or("").contains("_in2"));
}

#[test]
fn get_and_list_calibrations_return_all_three_layers() {
    // After loading voltage cal (via the existing `calibrate` fake-mode
    // path is awkward — easier to just inject via `calibrate_spl` +
    // `calibrate_mic_curve` which write their own fields), `get_calibration`
    // and `list_calibrations` must return the SPL field and the
    // mic_response provenance, matching the schema documented in ZMQ.md.
    let d = Daemon::spawn();
    let c = Client::new(&d);

    // SPL cal — spawn worker, prompt arrives, we reply, daemon captures.
    let r = c.call(json!({
        "cmd": "calibrate_spl",
        "input_channel": 0,
        "capture_s": 0.1,
    }));
    assert_eq!(r["ok"], json!(true));
    let _ = c
        .wait_for_topic("cal_prompt", Duration::from_secs(3))
        .expect("cal_prompt");
    let _ = c.call(json!({"cmd": "cal_reply", "vrms": Value::Null}));
    let _ = c
        .wait_for_topic("cal_done", Duration::from_secs(5))
        .expect("cal_done");

    // Mic-curve — synthetic 24-point curve.
    let mut freqs = Vec::new();
    let mut gains = Vec::new();
    let log_min = 100.0_f64.ln();
    let log_max = 10_000.0_f64.ln();
    for i in 0..24 {
        let t = i as f64 / 23.0;
        freqs.push((log_min + t * (log_max - log_min)).exp());
        gains.push(2.5 * t);
    }
    let r = c.call(json!({
        "cmd":           "calibrate_mic_curve",
        "op":            "set",
        "input_channel": 0,
        "freqs_hz":      freqs,
        "gain_db":       gains,
        "source_path":   "/tmp/synthetic.frd",
    }));
    assert_eq!(r["ok"], json!(true));

    // get_calibration must surface both new fields.
    let r = c.call(json!({"cmd": "get_calibration", "input_channel": 0}));
    assert_eq!(r["ok"], json!(true));
    assert_eq!(r["found"], json!(true));
    assert!(
        r["mic_sensitivity_dbfs_at_94db_spl"].is_f64(),
        "missing or wrong-typed mic_sensitivity_dbfs_at_94db_spl in: {r}"
    );
    let mr = r["mic_response"].as_object().expect("mic_response object");
    assert_eq!(mr["freqs_hz"].as_array().unwrap().len(), 24);
    assert_eq!(mr["gain_db"].as_array().unwrap().len(), 24);
    assert_eq!(mr["source_path"], json!("/tmp/synthetic.frd"));
    assert!(mr["imported_at"].is_string());

    // list_calibrations must surface them too — find the entry we just wrote.
    let r = c.call(json!({"cmd": "list_calibrations"}));
    assert_eq!(r["ok"], json!(true));
    let cals = r["calibrations"].as_array().expect("calibrations array");
    let entry = cals
        .iter()
        .find(|e| e["key"].as_str() == Some("out0_in0"))
        .expect("out0_in0 entry not in list");
    assert!(
        entry["mic_sensitivity_dbfs_at_94db_spl"].is_f64(),
        "list_calibrations entry missing mic_sensitivity field: {entry}"
    );
    assert!(
        entry["mic_response"].is_object(),
        "list_calibrations entry missing mic_response: {entry}"
    );
}

/// #297: `get_calibration` / `list_calibrations` must surface `tau_history`
/// — before this issue neither reply carried the field at all, so no client
/// could show it. Seed a key with two τ entries and a second key with none,
/// and assert both the present-array and the absent-array (`[]`) shapes
/// round-trip over the wire per ZMQ.md.
#[test]
fn get_and_list_calibrations_carry_tau_history() {
    let d = Daemon::spawn();
    let cal_path = d.home.join(".config").join("ac").join("cal.json");
    let seeded = json!({
        "out0_in0": {
            "output_channel":                   0,
            "input_channel":                    0,
            "ref_freq":                         1000.0,
            "vrms_at_0dbfs_out":                null,
            "vrms_at_0dbfs_in":                 null,
            "ref_dbfs":                         -10.0,
            "mic_sensitivity_dbfs_at_94db_spl": null,
            "mic_response":                     null,
            "tau_history": [
                {
                    "conditions": {
                        "device":      0,
                        "backend":     "jack",
                        "sample_rate": 48000,
                        "period_size": 1024,
                        "output_port": "system:playback_1",
                        "input_port":  "system:capture_2"
                    },
                    "tau_s":       0.0011931,
                    "measured_at": "2026-08-01T00:00:00Z",
                    "method":      "farina_short_ess"
                },
                {
                    "conditions": {
                        "device":      0,
                        "backend":     "jack",
                        "sample_rate": 48000,
                        "period_size": 128,
                        "output_port": "system:playback_1",
                        "input_port":  "system:capture_2"
                    },
                    "tau_s":       0.0025,
                    "measured_at": "2026-08-15T09:12:03Z",
                    "method":      "farina_short_ess"
                }
            ]
        },
        "out1_in1": {
            "output_channel":                   1,
            "input_channel":                    1,
            "ref_freq":                         1000.0,
            "vrms_at_0dbfs_out":                null,
            "vrms_at_0dbfs_in":                 null,
            "ref_dbfs":                         -10.0,
            "mic_sensitivity_dbfs_at_94db_spl": null,
            "mic_response":                     null,
            "tau_history":                      []
        }
    });
    fs::write(&cal_path, serde_json::to_vec_pretty(&seeded).unwrap()).expect("seed cal.json");

    let c = Client::new(&d);

    // get_calibration on the key with history: both entries present.
    let r = c.call(json!({"cmd": "get_calibration", "output_channel": 0, "input_channel": 0}));
    assert_eq!(r["ok"], json!(true));
    let history = r["tau_history"].as_array().expect("tau_history array");
    assert_eq!(history.len(), 2, "wire reply: {r}");
    assert_eq!(history[1]["tau_s"], json!(0.0025));
    assert_eq!(
        history[1]["conditions"]["period_size"],
        json!(128),
        "conditions must round-trip: {r}"
    );

    // get_calibration on the key with no history: empty array, not absent.
    let r = c.call(json!({"cmd": "get_calibration", "output_channel": 1, "input_channel": 1}));
    assert_eq!(r["ok"], json!(true));
    assert_eq!(
        r["tau_history"],
        json!([]),
        "unmeasured key must reply with [], not omit the field: {r}"
    );

    // list_calibrations must carry the same shape for both keys.
    let r = c.call(json!({"cmd": "list_calibrations"}));
    assert_eq!(r["ok"], json!(true));
    let cals = r["calibrations"].as_array().expect("calibrations array");
    let with_history = cals
        .iter()
        .find(|e| e["key"].as_str() == Some("out0_in0"))
        .expect("out0_in0 entry not in list");
    assert_eq!(
        with_history["tau_history"].as_array().map(|a| a.len()),
        Some(2),
        "list_calibrations: {with_history}"
    );
    let without_history = cals
        .iter()
        .find(|e| e["key"].as_str() == Some("out1_in1"))
        .expect("out1_in1 entry not in list");
    assert_eq!(
        without_history["tau_history"],
        json!([]),
        "list_calibrations: {without_history}"
    );
}

#[test]
fn calibrate_mic_curve_set_then_clear() {
    // End-to-end: upload a synthetic curve, verify cal entry is written,
    // verify the `loaded` count comes back; then `op = clear` and verify
    // the count drops to zero.
    let d = Daemon::spawn();
    let c = Client::new(&d);

    // Synthetic 32-point curve, log-spaced 100..10k Hz, +0..+3 dB ramp.
    let mut freqs = Vec::with_capacity(32);
    let mut gains = Vec::with_capacity(32);
    let log_min = 100.0_f64.ln();
    let log_max = 10_000.0_f64.ln();
    for i in 0..32 {
        let t = i as f64 / 31.0;
        freqs.push((log_min + t * (log_max - log_min)).exp());
        gains.push(3.0 * t);
    }

    let r = c.call(json!({
        "cmd":           "calibrate_mic_curve",
        "op":            "set",
        "input_channel": 1,
        "freqs_hz":      freqs,
        "gain_db":       gains,
        "source_path":   "/tmp/synthetic.frd",
    }));
    assert_eq!(r["ok"], json!(true), "set failed: {r}");
    assert_eq!(r["loaded"], json!(32));
    assert!(r["key"].as_str().unwrap_or("").contains("_in1"));

    // Sparse curve: should be rejected (under MIN_POINTS).
    let r = c.call(json!({
        "cmd":           "calibrate_mic_curve",
        "op":            "set",
        "input_channel": 1,
        "freqs_hz":      [100.0, 200.0, 300.0],
        "gain_db":       [0.0, 0.5, 1.0],
    }));
    assert_eq!(r["ok"], json!(false));
    assert!(
        r["error"].as_str().unwrap_or("").contains("too sparse"),
        "{r}"
    );

    // Clear.
    let r = c.call(json!({
        "cmd":           "calibrate_mic_curve",
        "op":            "clear",
        "input_channel": 1,
    }));
    assert_eq!(r["ok"], json!(true));
    assert_eq!(r["loaded"], json!(0));
}
