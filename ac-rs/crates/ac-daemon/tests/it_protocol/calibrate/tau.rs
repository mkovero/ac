//! Interface latency (τ) end to end — the sweep, the two lifecycles, and
//! what reaches `cal_done` and `cal.json` (#281, #347, #348).

use super::{expect_cal_done, expect_prompt, read_cal_entry, reply_vrms, seed_voltage_cal};
use crate::common::{Client, Daemon};
use serde_json::json;

/// #281 QA correctness issue 1: `measure_tau`'s sweep→deconvolve→peak→seconds
/// path had zero test coverage — the only τ tests (`shared/calibration/tau.rs`)
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

    // Both prompts skipped — τ must still be measured (#368: it is not
    // gated on the step-2 loopback flag at all, and never was gated on
    // either voltage reply).
    for step in 1..=2 {
        expect_prompt(&c, step);
        reply_vrms(&c, None);
    }
    let done = expect_cal_done(&c);

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
    // #368: present whenever a lifecycle reached deconvolution, including
    // a clean "measured" run — not only on the refusal leg.
    assert!(
        done["tau_pre_impulse_snr_db"].as_f64().is_some(),
        "frame: {done}"
    );
    assert!(
        done["tau_snr_threshold_db"].as_f64().is_some(),
        "frame: {done}"
    );
}

/// #368: the pre-attempt `is_loopback` level gate is gone — τ is refused
/// only when the deconvolved peak itself sits below
/// `tau_snr_threshold_db` pre-impulse SNR. This drives that refusal
/// end-to-end through `--fake-audio`'s low-SNR test hooks
/// (`AC_FAKE_TAU_GAIN_OVERRIDE` / `AC_FAKE_TAU_NOISE_AMPLITUDE_OVERRIDE`,
/// `audio/fake/hooks.rs`): a muted route (gain 0, dither only) must come
/// back `not_measured_low_snr`, not the plausible-looking `measured` a
/// deleted gate would still produce, since the fake backend's default
/// loopback shape has no other codepath capable of returning anything but
/// a clean peak. Pairs with `calibrate_measures_tau_against_fake_loopback_delay`
/// above (a passing, high-SNR loopback) to distinguish "measured because
/// SNR is genuinely adequate" from "measured because the gate was
/// deleted."
#[test]
fn calibrate_reports_not_measured_low_snr_on_muted_fake_loopback() {
    let d = Daemon::spawn_with_env(&[
        ("AC_FAKE_TAU_GAIN_OVERRIDE", "0.0"),
        ("AC_FAKE_TAU_NOISE_AMPLITUDE_OVERRIDE", "0.01"),
    ]);
    let cal_path = d.home.join(".config").join("ac").join("cal.json");
    let c = Client::new(&d);

    let r = c.call(json!({"cmd": "calibrate", "ref_dbfs": -10.0,
                           "output_channel": 0, "input_channel": 0}));
    assert_eq!(r["ok"], json!(true));

    for step in 1..=2 {
        expect_prompt(&c, step);
        reply_vrms(&c, None);
    }
    let done = expect_cal_done(&c);

    assert_eq!(
        done["tau_state"],
        json!("not_measured_low_snr"),
        "frame: {done}"
    );
    assert_eq!(
        done["tau_s"],
        json!(null),
        "a low-SNR refusal must not report a τ: {done}"
    );
    let snr = done["tau_pre_impulse_snr_db"]
        .as_f64()
        .expect("tau_pre_impulse_snr_db present on a refusal that reached deconvolution");
    let threshold = done["tau_snr_threshold_db"]
        .as_f64()
        .expect("tau_snr_threshold_db present alongside it");
    assert!(
        snr < threshold,
        "refused SNR {snr} should be below the {threshold} dB threshold: {done}"
    );

    // Refused, not stored — no entry in tau_history at all.
    let after = read_cal_entry(&cal_path);
    assert!(
        after.get("tau_history").is_none()
            || after["tau_history"]
                .as_array()
                .is_some_and(|a| a.is_empty()),
        "a low-SNR refusal must not append to tau_history: {after}"
    );
}

/// #368 AC8 (QA request-changes on PR #384): closes the gap the muted-route
/// test alone leaves. `calibrate_measures_tau_against_fake_loopback_delay`
/// above passes at the fake backend's default unity gain — exactly the one
/// case the old `is_loopback` ±2 dB gate already handled correctly, so it
/// cannot tell "measured because SNR is genuinely adequate" apart from
/// "measured because the gate was deleted" for any off-unity level. This
/// drives the +3.01 dB hot loopback from the issue's own rig case (drive
/// -30 dBFS, captured -30.0 dBFS) through `AC_FAKE_TAU_GAIN_OVERRIDE` and
/// asserts `measured` — a regression that reintroduced any captured-level
/// check keyed near unity would fail this without touching the muted-route
/// test.
#[test]
fn calibrate_measures_tau_on_hot_off_unity_fake_loopback() {
    let d = Daemon::spawn_with_env(&[
        ("AC_FAKE_TAU_GAIN_OVERRIDE", "1.4142135623730951"), // +3.01 dB
    ]);
    let c = Client::new(&d);

    let r = c.call(json!({"cmd": "calibrate", "ref_dbfs": -30.0,
                           "output_channel": 0, "input_channel": 0}));
    assert_eq!(r["ok"], json!(true));

    for step in 1..=2 {
        expect_prompt(&c, step);
        reply_vrms(&c, None);
    }
    let done = expect_cal_done(&c);

    assert_eq!(
        done["tau_state"],
        json!("measured"),
        "3.01 dB hot must not be refused (#368 AC1): {done}"
    );
    assert!(done["tau_s"].as_f64().is_some(), "frame: {done}");
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
        expect_prompt(&c, step);
        reply_vrms(&c, None);
    }
    let done = expect_cal_done(&c);

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
/// voltage legs alone. Previously asserted only by reading the code (τ is
/// never keyed on either voltage reply, and since #368 not on the step-2
/// loopback flag either); this test pins it down on the wire and on disk.
#[test]
fn calibrate_cheap_refresh_still_measures_tau() {
    let d = Daemon::spawn();
    let cal_path = seed_voltage_cal(&d, 2.345_67, 1.234_56, -20.0);
    let c = Client::new(&d);

    let r = c.call(json!({"cmd": "calibrate", "ref_dbfs": -10.0,
                          "output_channel": 0, "input_channel": 0}));
    assert_eq!(r["ok"], json!(true));

    for step in 1..=2 {
        expect_prompt(&c, step);
        reply_vrms(&c, None);
    }
    let done = expect_cal_done(&c);

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
