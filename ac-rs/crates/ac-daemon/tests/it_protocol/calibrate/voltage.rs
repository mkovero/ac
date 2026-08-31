//! The two DMM voltage prompts: what a reading, a skip, a clear and a
//! cancel each do to the stored entry (#279, #294), and the level the
//! reading is scaled from (#360).

use super::{
    expect_cal_done, expect_prompt, read_cal_entry, reply_cancel, reply_clear, reply_vrms,
    run_calibrate_skip_all, seed_voltage_cal,
};
use crate::common::{Client, Daemon};
use serde_json::json;
use std::time::Duration;
use std::time::Instant;

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
                reply_vrms(&c, None);
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
    let _ = expect_prompt(&c, 1);
    let user_out_vrms = 2.095_f64;
    reply_vrms(&c, Some(user_out_vrms));

    // Step 2 prompt — fake backend loops the played tone back, so the
    // captured input level matches the played `ref_dbfs - 3.01` (RMS
    // vs peak), and the handler should flag `loopback: true`.
    let p2 = expect_prompt(&c, 2);
    assert_eq!(
        p2["loopback"],
        json!(true),
        "expected loopback flag in step 2: {p2}"
    );
    reply_vrms(&c, Some(user_out_vrms));

    let done = expect_cal_done(&c);

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
        expect_prompt(&c, step);
        reply_vrms(&c, None);
    }
    let done = expect_cal_done(&c);

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

    expect_prompt(&c, 1);
    reply_clear(&c);

    expect_prompt(&c, 2);
    let in_reading = 1.5_f64;
    reply_vrms(&c, Some(in_reading));

    let done = expect_cal_done(&c);
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
        expect_prompt(&c, step);
        reply_vrms(&c, None);
    }
    let done = expect_cal_done(&c);

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

    expect_prompt(&c, 1);
    reply_vrms(&c, Some(2.095));

    expect_prompt(&c, 2);
    reply_cancel(&c);

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
/// worked all along, per the step-1 stop check at `handlers/calibrate/mod.rs`
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

    expect_prompt(&c, 1);
    reply_cancel(&c);

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

    let step1 = expect_prompt(&c, 1);
    assert_eq!(step1["ref_dbfs"], json!(CEILING_DBFS));
    reply_vrms(&c, None);

    let step2 = expect_prompt(&c, 2);
    let captured_dbfs = step2["captured_dbfs"].as_f64().expect("captured_dbfs");
    let expected = CEILING_DBFS - PEAK_TO_RMS_DB;
    assert!(
        (captured_dbfs - expected).abs() < 1.5,
        "captured {captured_dbfs} dBFS does not match a tone actually played at the \
         {CEILING_DBFS} dBFS ceiling (expected ~{expected}) — the default was not clamped \
         before the tone was set"
    );
    reply_vrms(&c, None);
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
    let step1 = expect_prompt(&c, 1);
    assert_eq!(step1["ref_dbfs"], json!(CEILING_DBFS));
    reply_vrms(&c, None);
    let step2 = expect_prompt(&c, 2);
    let captured_dbfs = step2["captured_dbfs"].as_f64().expect("captured_dbfs");
    assert!(
        (captured_dbfs - expected).abs() < 1.5,
        "captured {captured_dbfs} dBFS does not match a tone actually played at the \
         {CEILING_DBFS} dBFS ceiling (expected ~{expected}) — an explicit request above \
         the ceiling reached the engine unclamped"
    );
}
