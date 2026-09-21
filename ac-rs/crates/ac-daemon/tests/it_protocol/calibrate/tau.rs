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

    let r = c.call(json!({"cmd": "calibrate", "ref_dbfs": -20.0,
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
    // #369 clean-path regression: a run with no xruns still carries the
    // fields, present-as-zero rather than absent (per ZMQ.md's presence
    // rule), and does not take the refused_xrun path.
    assert_eq!(done["tau_reading1_xruns"], json!(0), "frame: {done}");
    assert_eq!(done["tau_reading2_xruns"], json!(0), "frame: {done}");
    // #461: a measured τ names the device-enumeration epoch it belongs to.
    assert_eq!(
        done["tau_enumeration"]["kind"],
        json!("observed"),
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
        // Deterministic dither seeded from the loopback delay (see
        // `audio/fake/hooks.rs`'s doc comment); the default 32-sample
        // delay happens to land this noise-only IR's peak within the
        // edge margin, which since #494 reports `not_measured_window_edge`
        // (the edge check is judged first) instead of the SNR-only state
        // this test means to exercise — 800 lands well clear of either
        // edge (empirically probed, not derived; re-checked under the
        // #494 order).
        ("AC_FAKE_TAU_DELAY_SAMPLES_OVERRIDE", "800,800"),
        ("AC_FAKE_TAU_GAIN_OVERRIDE", "0.0"),
        ("AC_FAKE_TAU_NOISE_AMPLITUDE_OVERRIDE", "0.01"),
    ]);
    let cal_path = d.home.join(".config").join("ac").join("cal.json");
    let c = Client::new(&d);

    let r = c.call(json!({"cmd": "calibrate", "ref_dbfs": -20.0,
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
    // #494: the first lifecycle refuses, and the run short-circuits there.
    assert_eq!(done["tau_refused_reading"], json!(1), "frame: {done}");
    assert!(
        done.get("tau_peak_offset_samples").is_none(),
        "a low-SNR refusal carries no peak position: {done}"
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

/// #494: an arrival past the far edge of the τ window is refused as
/// `not_measured_window_edge`, with typed numeric fields, not as `error`
/// prose and not as `not_measured_low_snr`. The fake runs at 48 kHz, so
/// `half = ceil(0.05 × 48000) = 2400` and the window is offsets −2400 to
/// +2399 with a `round(0.10 × 2400) = 240`-sample margin. A 2410-sample
/// delay lands 11 samples past the last one; 2500 lands 101 past, where a
/// skirt peak rather than a pinned one is expected. Both must name the edge.
#[test]
fn calibrate_reports_not_measured_window_edge_for_an_arrival_past_the_window() {
    for delay in [2410, 2500] {
        let d = Daemon::spawn_with_env(&[(
            "AC_FAKE_TAU_DELAY_SAMPLES_OVERRIDE",
            &format!("{delay},{delay}"),
        )]);
        let cal_path = d.home.join(".config").join("ac").join("cal.json");
        let c = Client::new(&d);

        let r = c.call(json!({"cmd": "calibrate", "ref_dbfs": -20.0,
                               "output_channel": 0, "input_channel": 0}));
        assert_eq!(r["ok"], json!(true));
        for step in 1..=2 {
            expect_prompt(&c, step);
            reply_vrms(&c, None);
        }
        let done = expect_cal_done(&c);

        assert_eq!(
            done["tau_state"],
            json!("not_measured_window_edge"),
            "delay {delay}: frame: {done}"
        );
        assert_eq!(done["tau_s"], json!(null), "delay {delay}: {done}");
        assert_eq!(
            done["tau_agreement_count"],
            json!(0),
            "delay {delay}: {done}"
        );
        assert_eq!(
            done["tau_refused_reading"],
            json!(1),
            "delay {delay}: {done}"
        );
        assert_eq!(
            done["tau_window_first_offset_samples"],
            json!(-2400),
            "delay {delay}: {done}"
        );
        assert_eq!(
            done["tau_window_last_offset_samples"],
            json!(2399),
            "delay {delay}: {done}"
        );
        assert_eq!(
            done["tau_edge_margin_samples"],
            json!(240),
            "delay {delay}: {done}"
        );
        let offset = done["tau_peak_offset_samples"]
            .as_i64()
            .unwrap_or_else(|| panic!("delay {delay}: peak offset missing: {done}"));
        assert!(
            (2399 - 240..=2399).contains(&offset),
            "delay {delay}: peak offset {offset} is not inside the far edge margin: {done}"
        );
        let snr = done["tau_pre_impulse_snr_db"]
            .as_f64()
            .unwrap_or_else(|| panic!("delay {delay}: SNR missing without an xrun: {done}"));
        let threshold = done["tau_snr_threshold_db"]
            .as_f64()
            .unwrap_or_else(|| panic!("delay {delay}: threshold missing: {done}"));
        assert_eq!(
            done["tau_snr_below_threshold"],
            json!(snr < threshold),
            "delay {delay}: the flag must agree with the pair it travels with: {done}"
        );
        assert!(
            done.get("tau_error").is_none(),
            "delay {delay}: an edge refusal is not an error: {done}"
        );
        assert!(
            done.get("tau_reading1_s").is_none(),
            "delay {delay}: {done}"
        );

        let after = read_cal_entry(&cal_path);
        assert!(
            after.get("tau_history").is_none()
                || after["tau_history"]
                    .as_array()
                    .is_some_and(|a| a.is_empty()),
            "delay {delay}: an edge refusal must not append to tau_history: {after}"
        );
    }
}

/// #369: a lifecycle that crosses an xrun refuses the reading end-to-end,
/// through the real `measure_tau_twice` → `tau_result` → `cal_done` path —
/// not just the hand-constructed `TauAttempt::Compared` unit tests. Uses
/// the fake backend's `AC_FAKE_XRUNS_OVERRIDE` hook
/// (`audio/fake/hooks.rs`) to make reading 2's lifecycle report one xrun
/// while reading 1 stays clean; both lifecycles' delays are left at the
/// default so the two readings would otherwise agree — the exact
/// doubly-corrupted-but-agreeing shape the xrun-first dispatch order
/// exists to catch (a disagreement would have been refused anyway, and
/// would not have exercised this).
#[test]
fn calibrate_reports_refused_xrun_end_to_end() {
    let d = Daemon::spawn_with_env(&[("AC_FAKE_XRUNS_OVERRIDE", "0,1")]);
    let cal_path = d.home.join(".config").join("ac").join("cal.json");
    let c = Client::new(&d);

    let r = c.call(json!({"cmd": "calibrate", "ref_dbfs": -20.0,
                           "output_channel": 0, "input_channel": 0}));
    assert_eq!(r["ok"], json!(true));

    for step in 1..=2 {
        expect_prompt(&c, step);
        reply_vrms(&c, None);
    }
    let done = expect_cal_done(&c);

    assert_eq!(done["tau_state"], json!("refused_xrun"), "frame: {done}");
    assert_eq!(
        done["tau_s"],
        json!(null),
        "a refused reading must not report a τ: {done}"
    );
    assert_eq!(done["tau_agreement_count"], json!(0), "frame: {done}");
    assert!(done["tau_reading1_s"].as_f64().is_some(), "frame: {done}");
    assert!(done["tau_reading2_s"].as_f64().is_some(), "frame: {done}");
    // Per-reading attribution, not summed — reading 1 stayed clean.
    assert_eq!(done["tau_reading1_xruns"], json!(0), "frame: {done}");
    assert_eq!(done["tau_reading2_xruns"], json!(1), "frame: {done}");

    // Refused, not stored — no entry in tau_history at all.
    let after = read_cal_entry(&cal_path);
    assert!(
        after.get("tau_history").is_none()
            || after["tau_history"]
                .as_array()
                .is_some_and(|a| a.is_empty()),
        "a refused-xrun reading must not append to tau_history: {after}"
    );
}

/// #368/#369 merge precedence: when a lifecycle both crosses an xrun and
/// would independently have failed the SNR gate (a muted/noise-only
/// route — `AC_FAKE_TAU_GAIN_OVERRIDE`/`AC_FAKE_TAU_NOISE_AMPLITUDE_
/// OVERRIDE` apply to both lifecycles here, since those two hooks are not
/// call-indexed), the run is reported `refused_xrun`, not
/// `not_measured_low_snr` — a contaminated capture's SNR figure is
/// meaningless, so the xrun is what gets named, not the noise floor it
/// produced. Both lifecycles carry an xrun (`AC_FAKE_XRUNS_OVERRIDE`
/// `"1,1"`) so that both reach `TauAttempt::Compared` at all: if only one
/// did, the *other* (clean-of-xruns, still muted) lifecycle would fail its
/// own SNR gate on its own account and short-circuit into
/// `not_measured_low_snr` before the dirty lifecycle's xrun ever entered
/// the picture — a different mechanism than the one this test exists to
/// pin down.
///
/// Swap the precedence (restore the SNR gate to run unconditionally
/// inside `measure_tau`, i.e. revert `calibrate/tau/mod.rs`'s `run_once`
/// to call `check_peak_snr` regardless of `xruns`) and this goes red: the
/// first lifecycle would refuse via `not_measured_low_snr` before its own
/// xrun count is ever consulted, and the run never reaches
/// `refused_xrun` at all.
#[test]
fn calibrate_reports_refused_xrun_over_low_snr_when_both_conditions_hold() {
    let d = Daemon::spawn_with_env(&[
        ("AC_FAKE_XRUNS_OVERRIDE", "1,1"),
        // See the sibling low-SNR test above for why 800 (not the
        // default 32): it keeps this noise-only peak clear of the
        // edge-margin refusal so the SNR-vs-xrun precedence is what
        // this test actually exercises.
        ("AC_FAKE_TAU_DELAY_SAMPLES_OVERRIDE", "800,800"),
        ("AC_FAKE_TAU_GAIN_OVERRIDE", "0.0"),
        ("AC_FAKE_TAU_NOISE_AMPLITUDE_OVERRIDE", "0.01"),
    ]);
    let cal_path = d.home.join(".config").join("ac").join("cal.json");
    let c = Client::new(&d);

    let r = c.call(json!({"cmd": "calibrate", "ref_dbfs": -20.0,
                           "output_channel": 0, "input_channel": 0}));
    assert_eq!(r["ok"], json!(true));

    for step in 1..=2 {
        expect_prompt(&c, step);
        reply_vrms(&c, None);
    }
    let done = expect_cal_done(&c);

    assert_eq!(
        done["tau_state"],
        json!("refused_xrun"),
        "both lifecycles crossed an xrun *and* are muted (low SNR) — xrun \
         must be the reported cause: {done}"
    );
    assert_eq!(
        done["tau_s"],
        json!(null),
        "a refused reading must not report a τ: {done}"
    );
    assert_eq!(done["tau_reading1_xruns"], json!(1), "frame: {done}");
    assert_eq!(done["tau_reading2_xruns"], json!(1), "frame: {done}");
    // Not the low-SNR fields' job to report on this path — the state name
    // itself is the assertion that xrun, not SNR, was named as the cause.
    assert!(done["tau_reading1_s"].as_f64().is_some(), "frame: {done}");
    assert!(done["tau_reading2_s"].as_f64().is_some(), "frame: {done}");

    // Refused, not stored — no entry in tau_history at all.
    let after = read_cal_entry(&cal_path);
    assert!(
        after.get("tau_history").is_none()
            || after["tau_history"]
                .as_array()
                .is_some_and(|a| a.is_empty()),
        "a refused-xrun reading must not append to tau_history: {after}"
    );
}

/// #368 AC8 (QA request-changes on PR #384; codex-qa finding on the first
/// attempt at this test — see below). `calibrate_measures_tau_against_
/// fake_loopback_delay` above passes at the fake backend's default unity
/// gain — exactly the one case the old `is_loopback` ±2 dB gate already
/// handled correctly, so it cannot tell "measured because SNR is genuinely
/// adequate" apart from "measured because the gate was deleted" for any
/// off-unity level. This drives the +3.01 dB hot loopback from the issue's
/// own rig case (drive -30 dBFS, captured -30.0 dBFS) through
/// `AC_FAKE_TAU_GAIN_OVERRIDE` and asserts `measured` — a regression that
/// reintroduced any captured-level check keyed near unity would fail this
/// without touching the muted-route test.
///
/// codex-qa on PR #384 caught that the first version of this test asserted
/// only the final `tau_state`, never the off-unity level it claimed to
/// drive: `AC_FAKE_TAU_GAIN_OVERRIDE` at the time scaled only
/// `play_and_capture` (the τ ESS), not the step-2 tone capture
/// `capture_rms` reads — so step 2 still saw the unity-loopback level and
/// `measured` proved nothing about the off-unity path. Fixed at the
/// source (`audio/fake/mod.rs::capture_block` now applies the same
/// override) and pinned here: step 2's own `captured_dbfs`/`loopback`
/// fields are asserted before the final `tau_state` check, so a regression
/// in either the fake model or a reintroduced level gate fails this test.
#[test]
fn calibrate_measures_tau_on_hot_off_unity_fake_loopback() {
    let d = Daemon::spawn_with_env(&[
        ("AC_FAKE_TAU_GAIN_OVERRIDE", "1.4142135623730951"), // +3.01 dB
    ]);
    let c = Client::new(&d);

    let r = c.call(json!({"cmd": "calibrate", "ref_dbfs": -30.0,
                           "output_channel": 0, "input_channel": 0}));
    assert_eq!(r["ok"], json!(true));

    expect_prompt(&c, 1);
    reply_vrms(&c, None);
    let step2 = expect_prompt(&c, 2);
    // Unity loopback at ref_dbfs -30.0 would capture at -33.01 dBFS
    // (the sine peak/RMS factor); the +3.01 dB override must land step 2
    // at -30.0, matching the issue's own hot-loopback rig case, and take
    // it outside the old ±2 dB `is_loopback` window.
    let captured_dbfs = step2["captured_dbfs"]
        .as_f64()
        .expect("captured_dbfs present on step 2");
    assert!(
        (captured_dbfs - (-30.0)).abs() < 0.1,
        "step 2 must see the +3.01 dB hot level (#368 AC1), not unity loopback: {step2}"
    );
    assert_eq!(
        step2["loopback"],
        json!(false),
        "3.01 dB off unity must fall outside the ±2 dB is_loopback window: {step2}"
    );
    reply_vrms(&c, None);
    let done = expect_cal_done(&c);

    assert_eq!(
        done["tau_state"],
        json!("measured"),
        "3.01 dB hot must not be refused (#368 AC1): {done}"
    );
    assert!(done["tau_s"].as_f64().is_some(), "frame: {done}");
}

/// #363: the graph's own declared path latency moved between the two
/// lifecycles, so the run is refused even though the two readings agree to
/// the sample. That combination — agreeing readings, moved declaration — is
/// the whole reason the state exists: #363 measured 42 of 97 rig runs storing
/// a value one period short while reporting that two readings agreed.
///
/// Drives it through the real `measure_tau_twice`, with equal delays so the
/// comparison would have said `Agree`, and a two-value declaration hook so
/// the two lifecycles declare different frame counts.
#[test]
fn calibrate_refuses_when_the_declared_latency_moves_between_lifecycles() {
    let d = Daemon::spawn_with_env(&[
        ("AC_FAKE_TAU_DELAY_SAMPLES_OVERRIDE", "32,32"),
        ("AC_FAKE_DECLARED_LATENCY_FRAMES_OVERRIDE", "244,1268"),
        ("AC_FAKE_PERIOD_SIZE_OVERRIDE", "1024"),
    ]);
    let cal_path = d.home.join(".config").join("ac").join("cal.json");
    let c = Client::new(&d);

    let r = c.call(json!({"cmd": "calibrate", "ref_dbfs": -20.0,
                           "output_channel": 0, "input_channel": 0}));
    assert_eq!(r["ok"], json!(true));
    for step in 1..=2 {
        expect_prompt(&c, step);
        reply_vrms(&c, None);
    }
    let done = expect_cal_done(&c);

    assert_eq!(
        done["tau_state"],
        json!("disagree_declared_latency"),
        "frame: {done}"
    );
    assert_eq!(
        done["tau_reading1_declared_frames"],
        json!(244),
        "frame: {done}"
    );
    assert_eq!(
        done["tau_reading2_declared_frames"],
        json!(1268),
        "frame: {done}"
    );
    assert_eq!(
        done["tau_s"],
        json!(null),
        "a moved declaration must not report a τ: {done}"
    );
    assert_eq!(done["tau_agreement_count"], json!(0), "frame: {done}");
    // The readings are reported even though they agree — that is the point.
    assert!(done["tau_reading1_s"].as_f64().is_some(), "frame: {done}");
    assert!(done["tau_reading2_s"].as_f64().is_some(), "frame: {done}");
    assert_eq!(
        done["tau_reading1_s"], done["tau_reading2_s"],
        "test setup: the readings must agree, or this proves nothing: {done}"
    );
    // ZMQ.md's presence rules for a `disagree_*` state that reached
    // deconvolution (QA, PR #476): the xrun pair is a concrete integer
    // including 0, never an absent key a client reads as a pre-#369 daemon,
    // and the SNR pair says whether the refused sweep was otherwise clean.
    assert_eq!(done["tau_reading1_xruns"], json!(0), "frame: {done}");
    assert_eq!(done["tau_reading2_xruns"], json!(0), "frame: {done}");
    assert!(
        done["tau_pre_impulse_snr_db"].as_f64().is_some(),
        "frame: {done}"
    );
    assert!(
        done["tau_snr_threshold_db"].as_f64().is_some(),
        "frame: {done}"
    );
    assert!(
        done["tau_reading_separation_s"]
            .as_f64()
            .is_some_and(|s| s > 0.0),
        "frame: {done}"
    );
    assert!(
        done["tau_error"]
            .as_str()
            .is_some_and(|m| m.contains("244") && m.contains("1268")),
        "frame: {done}"
    );

    let after = read_cal_entry(&cal_path);
    assert!(
        after.get("tau_history").is_none()
            || after["tau_history"]
                .as_array()
                .is_some_and(|a| a.is_empty()),
        "a moved declaration must not append to tau_history: {after}"
    );
}

/// #363: the fake declares nothing by default, so a healthy two-lifecycle run
/// must report the declaration as `null` — *not applicable* — and still
/// measure. Guards the null-versus-absent rule from the daemon side: a
/// backend that declares nothing must never look like two backends declaring
/// the same thing.
#[test]
fn calibrate_measures_with_null_declared_latency_when_the_backend_declares_nothing() {
    let d = Daemon::spawn_with_env(&[("AC_FAKE_TAU_DELAY_SAMPLES_OVERRIDE", "32,32")]);
    let c = Client::new(&d);
    let r = c.call(json!({"cmd": "calibrate", "ref_dbfs": -20.0,
                           "output_channel": 0, "input_channel": 0}));
    assert_eq!(r["ok"], json!(true));
    for step in 1..=2 {
        expect_prompt(&c, step);
        reply_vrms(&c, None);
    }
    let done = expect_cal_done(&c);
    assert_eq!(done["tau_state"], json!("measured"), "frame: {done}");
    assert_eq!(
        done["tau_reading1_declared_frames"],
        json!(null),
        "frame: {done}"
    );
    assert_eq!(
        done["tau_reading2_declared_frames"],
        json!(null),
        "frame: {done}"
    );
    assert!(
        done["tau_reading_separation_s"]
            .as_f64()
            .is_some_and(|s| s > 0.0),
        "a measured run records how far apart its lifecycles were: {done}"
    );
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

    let r = c.call(json!({"cmd": "calibrate", "ref_dbfs": -20.0,
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

    let r = c.call(json!({"cmd": "calibrate", "ref_dbfs": -20.0,
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
    // #461: the entry records the epoch and session it was measured in, and
    // the epoch is the one `cal_done` reported.
    assert_eq!(
        history[0]["enumeration"], done["tau_enumeration"],
        "stored epoch must match the reported one: {after} / {done}"
    );
    assert_eq!(
        history[0]["enumeration"]["kind"],
        json!("observed"),
        "{after}"
    );
    let session = history[0]["session"].as_str().expect("session stamped");
    assert!(
        session.contains('@'),
        "session is <pid>@<started_at>: {session}"
    );
}

/// Run `calibrate` on `out_ch`/`in_ch` with both prompts skipped.
fn calibrate_pair(c: &Client, out_ch: u32, in_ch: u32) -> serde_json::Value {
    let r = c.call(json!({"cmd": "calibrate", "ref_dbfs": -20.0,
                          "output_channel": out_ch, "input_channel": in_ch}));
    assert_eq!(r["ok"], json!(true), "{r}");
    for step in 1..=2 {
        expect_prompt(c, step);
        reply_vrms(c, None);
    }
    expect_cal_done(c)
}

/// #544 AC5: with a reference loopback configured, `calibrate` reads its
/// leg in the same two captures as the pair's τ and stores it with the
/// entry, ports and all. On the fake the pair leg is 32 samples and the
/// reference leg 20 (`audio/fake/hooks.rs`), so the offset is +12.
#[test]
fn calibrate_stores_the_reference_leg_of_the_same_captures() {
    let d = Daemon::spawn_with_config(Some(
        json!({ "reference_channel": 1, "reference_output_channel": 1 }),
    ));
    let c = Client::new(&d);
    let done = calibrate_pair(&c, 0, 0);
    assert_eq!(done["tau_state"], json!("measured"), "{done}");
    assert_eq!(done["tau_reference_state"], json!("measured"), "{done}");
    assert_eq!(done["tau_offset_samples"], json!(12), "{done}");
    assert_eq!(
        done["tau_reference_output_port"],
        json!("fake:playback_1"),
        "{done}"
    );
    assert_eq!(
        done["tau_reference_input_port"],
        json!("fake:capture_1"),
        "{done}"
    );
    let ref_tau_s = done["tau_reference_s"].as_f64().expect("tau_reference_s");
    assert!((ref_tau_s * 48_000.0 - 20.0).abs() < 1e-6, "{done}");
    assert!(
        done["tau_reference_pre_impulse_snr_db"].as_f64().is_some(),
        "{done}"
    );

    let cal_path = d.home.join(".config").join("ac").join("cal.json");
    let entry = read_cal_entry(&cal_path);
    let stored = &entry["tau_history"][0]["reference"];
    assert_eq!(stored["output_port"], json!("fake:playback_1"), "{entry}");
    assert_eq!(stored["input_port"], json!("fake:capture_1"), "{entry}");
    assert_eq!(stored["tau_s"], done["tau_reference_s"], "{entry}");
}

/// #544: calibrating the reference loopback itself reads no second leg,
/// and a run with no reference configured says so; neither stores one.
#[test]
fn calibrate_names_why_no_reference_leg_was_read() {
    let d = Daemon::spawn_with_config(Some(
        json!({ "reference_channel": 0, "reference_output_channel": 0 }),
    ));
    let done = calibrate_pair(&Client::new(&d), 0, 0);
    assert_eq!(done["tau_reference_state"], json!("same_pair"), "{done}");
    assert!(done.get("tau_offset_samples").is_none(), "{done}");

    let d = Daemon::spawn();
    let done = calibrate_pair(&Client::new(&d), 0, 0);
    assert_eq!(done["tau_state"], json!("measured"), "{done}");
    assert_eq!(done["tau_reference_state"], json!("not_configured"), "{done}");
    let cal_path = d.home.join(".config").join("ac").join("cal.json");
    let entry = read_cal_entry(&cal_path);
    assert_eq!(entry["tau_history"][0]["reference"], json!(null), "{entry}");
}

/// #544: a refused reference leg never blocks τ. With the reference cable
/// muted (`AC_FAKE_REF_GAIN=0`, dither only) τ is still measured and stored,
/// and the reference state is `refused` with its reason and SNR.
#[test]
fn calibrate_stores_tau_when_the_reference_leg_is_refused() {
    let d = Daemon::spawn_with(
        Some(json!({ "reference_channel": 1, "reference_output_channel": 1 })),
        &[
            ("AC_FAKE_REF_GAIN", "0"),
            ("AC_FAKE_TAU_NOISE_AMPLITUDE_OVERRIDE", "0.00001"),
        ],
    );
    let done = calibrate_pair(&Client::new(&d), 0, 0);
    assert_eq!(done["tau_state"], json!("measured"), "{done}");
    assert_eq!(done["tau_reference_state"], json!("refused"), "{done}");
    assert!(done["tau_reference_reason"].is_string(), "{done}");
    assert!(done.get("tau_offset_samples").is_none(), "{done}");
    let cal_path = d.home.join(".config").join("ac").join("cal.json");
    let entry = read_cal_entry(&cal_path);
    assert_eq!(entry["tau_history"].as_array().map(Vec::len), Some(1));
    assert_eq!(entry["tau_history"][0]["reference"], json!(null), "{entry}");
}
