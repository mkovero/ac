use serde_json::json;
use serde_json::Value;
use std::time::Duration;
use std::time::Instant;

use ac_core::measurement::report::{ArrivalCheck, MeasurementReport, ReferenceLatency};
use ac_core::shared::calibration::EnumerationCheck;

use crate::common::{Client, Daemon};

#[test]
fn plot_ir_emits_impulse_response_with_expected_delay_peak() {
    // Fake backend implements `play_and_capture` as a delayed loopback
    // (see audio/fake.rs). Running a Farina sweep through it and
    // deconvolving should produce a linear IR with its peak at the
    // window centre (the gate re-centres the peak on linear_ir.len()/2).
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd":"plot_ir",
        "f1_hz": 200.0,
        "f2_hz": 8_000.0,
        "duration": 0.5,
        "level_dbfs": -20.0,
        "tail_s": 0.1,
        "window_len": 1024,
        "n_harmonics": 3,
    }));
    assert_eq!(r["ok"], json!(true));
    // #501: a typed window is echoed in samples, and no default is claimed.
    assert_eq!(r["window_len"], json!(1024), "{r}");
    assert!(r.get("window_default_s").is_none(), "{r}");
    assert_eq!(r["duration"], json!(0.5), "{r}");

    let mut got_ir = false;
    let mut got_report = false;
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline && !(got_ir && got_report) {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "measurement/impulse_response" => {
                let ir = v["data"]["linear_ir"].as_array().expect("linear_ir array");
                assert_eq!(ir.len(), 1024, "window_len respected");
                // Find the max-absolute sample index.
                let (peak_idx, peak_val) =
                    ir.iter().enumerate().fold((0usize, 0.0f64), |acc, (i, x)| {
                        let mag = x.as_f64().unwrap_or(0.0).abs();
                        if mag > acc.1 {
                            (i, mag)
                        } else {
                            acc
                        }
                    });
                let centre = ir.len() / 2;
                // Fake backend delays by 32 samples; the linear-IR gate is
                // centred on the sweep endpoint, which after normalisation
                // places the peak near the window centre. Allow ±64 sample
                // tolerance for the finite-window deconvolution.
                assert!(
                    (peak_idx as i64 - centre as i64).abs() < 64,
                    "peak at {peak_idx}, expected near centre {centre}"
                );
                assert!(peak_val > 0.3, "peak magnitude too small: {peak_val}");
                got_ir = true;
            }
            Some((t, v)) if t == "measurement/report" => {
                assert_eq!(
                    v["report"]["data"][0]["data"]["kind"],
                    json!("impulse_response")
                );
                assert_eq!(v["report"]["schema_version"], json!(12));
                // #282 acceptance criterion 6: the ISO 18233 §6.3.2
                // tail-decay verdict rides in `notes`, not a silent default.
                let notes = v["report"]["notes"].as_str().expect("notes present");
                assert!(notes.contains("18233"), "notes: {notes:?}");
                got_report = true;
            }
            Some((t, _)) if t == "done" => break,
            Some(_) => continue,
            None => break,
        }
    }
    assert!(got_ir, "never saw measurement/impulse_response frame");
    assert!(got_report, "never saw measurement/report frame");
}

fn assert_plot_ir_stops_promptly(duration: f64, tail_s: f64, settle: Duration) {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let reply = c.call(json!({
        "cmd":"plot_ir",
        "f1_hz":200.0,
        "f2_hz":8000.0,
        "duration":duration,
        "tail_s":tail_s,
        "window_len":1024,
        "n_harmonics":3
    }));
    assert_eq!(reply["ok"], json!(true), "plot_ir rejected: {reply}");

    std::thread::sleep(settle);
    let started = Instant::now();
    let stopped = c.call(json!({"cmd":"stop", "name":"plot_ir"}));
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "stop blocked for {:?}: {stopped}",
        started.elapsed()
    );
    assert_eq!(stopped["stopped"], json!(["plot_ir"]), "{stopped}");
    assert_eq!(stopped["stimulus"], json!("silent"), "{stopped}");
    let status = c.call(json!({"cmd":"status"}));
    assert_eq!(status["busy"], json!(false), "{status}");

    // #437 codex-qa: a prompt `stop` reply plus `busy:false` also holds when
    // `plot_ir` simply ran to completion before `stop` was ever sent — a
    // finished worker's handle stays in the workers map until `stop` removes
    // it, so those two assertions alone cannot distinguish "cancelled
    // mid-run" from "already done, and `stop` just reaped it". Prove
    // cancellation directly: drain every PUB frame already queued and assert
    // none of them is this request's `measurement/impulse_response`,
    // `measurement/report`, or `done` frame. A regression that reverts the
    // handler to the non-cancellable `play_and_capture` would publish all
    // three almost immediately (that path has no pacing sleep at all on the
    // fake backend), so they would already be sitting on the SUB socket by
    // the time this drain runs, well before `stop` was sent.
    let mut drained = 0;
    while let Some((topic, payload)) = c.recv_pub(50) {
        drained += 1;
        assert!(
            drained <= 1000,
            "runaway PUB drain while checking for a post-cancel completion frame"
        );
        if payload["cmd"] != json!("plot_ir") {
            continue;
        }
        assert_ne!(
            topic, "measurement/impulse_response",
            "plot_ir published an impulse response after stop cancelled it: {payload}"
        );
        assert_ne!(
            topic, "measurement/report",
            "plot_ir published a report after stop cancelled it: {payload}"
        );
        assert_ne!(
            topic, "done",
            "plot_ir published a done frame after stop cancelled it: {payload}"
        );
    }
}

#[test]
fn plot_ir_stop_cancels_during_stimulus() {
    assert_plot_ir_stops_promptly(5.0, 0.1, Duration::from_millis(200));
}

#[test]
fn plot_ir_stop_cancels_during_tail() {
    assert_plot_ir_stops_promptly(0.1, 5.0, Duration::from_millis(300));
}

/// #283: `plot_ir` resolves τ by *exact* match on `TauConditions`, and
/// the entry it must hit was written by `calibrate`. Nothing but a test
/// couples those two condition tuples — they are built in different
/// handlers, from different locals — so a drift in either (a port
/// resolved differently, a device field read from elsewhere) would leave
/// every `plot_ir` reporting "distance unavailable" forever, with no
/// error anywhere. The failure is silent by construction, so it needs an
/// explicit check that the round trip lands.
///
/// #351: `calibrate`'s τ and the IR's own arrival are now both derived
/// from [`ac_core::measurement::sweep::ir_peak`] — one shared picker,
/// called from `analyse_tau_leg` and from `MeasurementReport::ir_stats`
/// — rather than two separate maxima that could drift onto different
/// tie-break/NaN rules and, worse, different estimator families (an
/// onset-derived arrival against a peak-derived τ) the way they briefly
/// could under #346/#378. The pairing is checked in whole samples, not a
/// millisecond or metre bound (#391): the fake loopback delay is an
/// integer, and a linear-phase peak lands on that integer sample in both
/// captures.
///
/// Two configurations run this same check ([`assert_calibrated_tau_pairs_with_plot_ir`]),
/// differing in both sweep band and window (#351 triage AC3), so the
/// pairing is shown to hold generally rather than only at the one band
/// this issue happened to be found with.
#[test]
fn plot_ir_resolves_the_tau_that_calibrate_stored() {
    assert_calibrated_tau_pairs_with_plot_ir(200.0, 8_000.0, 0.5, 1024, 3);
}

/// #351 triage AC3, second configuration: a different band and window
/// from the first. `n_harmonics: 1` means there is no neighbouring
/// harmonic order to clamp the linear IR's gate (see `analyse_tau_leg`'s
/// doc comment on why `n_harmonics == 1` matters the same way for τ's own
/// sweep), so `window_len_used` for the linear IR must equal the request
/// unclamped in this configuration too.
#[test]
fn plot_ir_resolves_the_tau_that_calibrate_stored_on_a_second_band_and_window() {
    assert_calibrated_tau_pairs_with_plot_ir(50.0, 20_000.0, 0.5, 4096, 1);
}

/// Shared body for the two tests above: measure τ via `calibrate`, run
/// `plot_ir` with the given sweep, and check that τ and the IR's own
/// arrival still pair off exactly (#351).
fn assert_calibrated_tau_pairs_with_plot_ir(
    f1_hz: f64,
    f2_hz: f64,
    duration_s: f64,
    window_len: u64,
    n_harmonics: u64,
) {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    // 1. Measure τ. Both voltage prompts skipped — τ is keyed on
    //    loopback detection, not on either reply (see
    //    `calibrate_cheap_refresh_still_measures_tau`).
    let r = c.call(json!({"cmd": "calibrate", "ref_dbfs": -20.0,
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
    assert_eq!(done["tau_state"], json!("measured"), "frame: {done}");
    let stored_tau = done["tau_s"].as_f64().expect("tau_s");

    // 2. Run an IR capture under the same conditions.
    let r = c.call(json!({
        "cmd":"plot_ir",
        "f1_hz": f1_hz,
        "f2_hz": f2_hz,
        "duration": duration_s,
        "level_dbfs": -20.0,
        "tail_s": 0.1,
        "window_len": window_len,
        "n_harmonics": n_harmonics,
    }));
    assert_eq!(r["ok"], json!(true));

    let mut ir_window_used: Option<u64> = None;
    let mut report_v: Option<Value> = None;
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline && (ir_window_used.is_none() || report_v.is_none()) {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "measurement/impulse_response" => {
                ir_window_used = v["window_len_used"][0].as_u64();
            }
            Some((t, v)) if t == "measurement/report" => report_v = Some(v),
            // A stray `done` frame from the earlier `calibrate` call can
            // still be sitting on the socket here — `wait_for_topic` above
            // only guarantees it consumed up to `cal_done`, not every frame
            // calibrate ever published. Discard anything else and keep
            // waiting for plot_ir's own two frames rather than treating an
            // unrelated `done` as this command's.
            Some(_) => continue,
            None => break,
        }
    }
    let ir_window_used = ir_window_used.expect("measurement/impulse_response frame");
    let v = report_v.expect("measurement/report frame");
    assert_eq!(
        ir_window_used, window_len,
        "linear IR gate must not be clamped in this fixture — n_harmonics \
         {n_harmonics} leaves no neighbouring order to clamp against"
    );

    let latency = &v["report"]["interface_latency"];
    assert_eq!(
        latency["state"],
        json!("measured"),
        "plot_ir did not match calibrate's stored τ — the two TauConditions \
         tuples have drifted apart: {latency}"
    );
    let used_tau = latency["tau_s"].as_f64().expect("tau_s in report");
    assert!(
        (used_tau - stored_tau).abs() < 1e-12,
        "plot_ir used τ {used_tau}, calibrate stored {stored_tau}"
    );
    // The τ provenance must be archived, not just the number.
    assert!(latency["measured_at"].is_string(), "{latency}");
    assert_eq!(latency["method"], json!("farina_short_ess_v2"), "{latency}");
    // #461: one daemon, one fake epoch — within an epoch τ resolves with no
    // new flag.
    assert_eq!(
        latency["enumeration"],
        json!({"state": "same"}),
        "{latency}"
    );

    let report: ac_core::measurement::report::MeasurementReport =
        serde_json::from_value(v["report"].clone()).expect("decode report");
    let stats = report.ir_stats().expect("ir_stats");

    // #351: τ and the IR's arrival both come from
    // `ac_core::measurement::sweep::ir_peak` now, so on the fake
    // backend's integer-sample loopback delay they must agree to the
    // exact sample.
    //
    // Budget: 0 samples on the fake — derived, not assumed. The fake
    // loopback delay is an integer (`DEFAULT_LOOPBACK_DELAY_SAMPLES`,
    // `ac-daemon/src/audio/fake/hooks.rs`) and a linear-phase peak lands
    // on that integer in both captures, whatever their sweep band or
    // window — see `sweep::peak`'s module doc and its band-invariance
    // test for why. On real hardware the budget is ±1 sample instead:
    // each half is rounded to the nearest sample independently, so a
    // fractional true delay can split by at most one.
    let tau_samples = (used_tau * stats.sample_rate_hz as f64).round() as i64;
    assert_eq!(
        stats.delay_samples, tau_samples,
        "peak-derived τ ({tau_samples} samples) and peak-derived arrival \
         ({} samples) must agree exactly on the fake loopback",
        stats.delay_samples
    );

    // #351 triage AC5, tested against the rejected pairing: if the
    // arrival were ever taken from the onset instead of the peak while τ
    // stayed peak-derived, this is the residual that would leak into
    // `ir_arrival_distance()`. Computed here, not asserted as a fixed
    // value (it moved -0.310 → -2.458 ms across #346/#378 and is a
    // diagnostic's bias, not a contract) — only that it clears even the
    // ±1-sample real-hardware budget above, so a future regression that
    // reinstates that mismatch fails here rather than only in a rig
    // session.
    let centre = (stats.window_len / 2) as i64;
    let rejected_residual = stats.onset_index as i64 - centre - tau_samples;
    assert!(
        rejected_residual.abs() >= 2,
        "rejected pairing (onset-derived arrival minus peak-derived τ) \
         residual is only {rejected_residual} samples on f1={f1_hz} \
         f2={f2_hz} window_len={window_len} — too small to demonstrate \
         #351's divergence on this fixture"
    );
}

/// Run `calibrate` on out0/in0 with both voltage prompts skipped and return
/// the stored τ.
fn calibrate_tau(c: &Client) -> f64 {
    let r = c.call(json!({"cmd": "calibrate", "ref_dbfs": -20.0,
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
    assert_eq!(done["tau_state"], json!("measured"), "frame: {done}");
    done["tau_s"].as_f64().expect("tau_s")
}

/// #461 AC6 end to end: a τ stored in one device-enumeration epoch and
/// resolved in another must not resolve as a valid match without a flag.
///
/// Daemon 1 (`AC_FAKE_DEVICE_EPOCH=T1`) calibrates. Daemon 2 (`T2`) reads the
/// same `cal.json` and runs `plot_ir`: the report's `interface_latency` is
/// still measured — the epoch flags, it never refuses — but its frozen
/// `enumeration` is `crossed`. A third daemon back in `T1` reads `same`, so
/// the flag is caused by the epoch and not by the daemon being a different
/// process.
///
/// #544 inverts the rest of this test: the stored τ is no longer subtracted,
/// so with no reference configured neither daemon produces a flight time,
/// crossed or not. Before #544 both did, from `arrival − stored τ`.
#[test]
fn plot_ir_flags_a_stored_tau_from_another_device_enumeration() {
    use ac_core::measurement::report::{LatencyBasis, WithheldBasis};
    const T1: &str = "2026-09-15T23:40:11Z";
    const T2: &str = "2026-09-16T00:08:31Z";

    let d1 = Daemon::spawn_with_env(&[("AC_FAKE_DEVICE_EPOCH", T1)]);
    let stored_tau = calibrate_tau(&Client::new(&d1));
    let cal_rel = std::path::Path::new(".config").join("ac").join("cal.json");
    let cal = std::fs::read(d1.home.join(&cal_rel)).expect("read cal.json");

    let resolve_in = |epoch: &str| {
        let d = Daemon::spawn_with_env(&[("AC_FAKE_DEVICE_EPOCH", epoch)]);
        std::fs::write(d.home.join(&cal_rel), &cal).expect("share cal.json");
        let c = Client::new(&d);
        let (_, report) = report_for(&c, plot_ir_request(json!({})));
        report
    };

    let crossed = resolve_in(T2);
    let latency = match &crossed.interface_latency {
        Some(ac_core::measurement::report::InterfaceLatency::Measured(m)) => m.clone(),
        other => panic!("an epoch change must flag, not refuse: {other:?}"),
    };
    assert!((latency.tau_s - stored_tau).abs() < 1e-12);
    assert_eq!(
        latency.enumeration,
        Some(EnumerationCheck::Crossed {
            boundary: "audio device re-enumerated; nodes: fake:device0 re-created".into(),
            since: Some(T2.into()),
        }),
        "a stored τ from another epoch must not resolve as same"
    );

    let same = resolve_in(T1);
    match &same.interface_latency {
        Some(ac_core::measurement::report::InterfaceLatency::Measured(m)) => {
            assert_eq!(m.enumeration, Some(EnumerationCheck::Same));
        }
        other => panic!("expected a measured τ, got {other:?}"),
    }
    for report in [&crossed, &same] {
        let stats = report.ir_stats().expect("ir_stats");
        assert_eq!(
            stats.latency_basis,
            LatencyBasis::Withheld(WithheldBasis::NoReference)
        );
        assert_eq!(stats.flight_time_s, None, "the stored τ is not subtracted");
    }
    drop(d1);
}

/// Run `calibrate` on out0/in0 with both prompts skipped and return its
/// `cal_done` frame.
fn calibrate_done(c: &Client) -> Value {
    let r = c.call(json!({"cmd": "calibrate", "ref_dbfs": -20.0,
                          "output_channel": 0, "input_channel": 0}));
    assert_eq!(r["ok"], json!(true), "{r}");
    for step in 1..=2 {
        c.wait_for_topic("cal_prompt", Duration::from_secs(5))
            .unwrap_or_else(|| panic!("step {step} prompt"));
        let _ = c.call(json!({"cmd": "cal_reply", "vrms": null}));
    }
    c.wait_for_topic("cal_done", Duration::from_secs(5))
        .expect("cal_done frame")
}

/// #544 end to end, tested against the rejected rule. Daemon 1 calibrates
/// out0/in0 with the reference loopback configured, storing the pair's τ
/// (32) and the reference's (20) from one capture: offset +12. Daemon 2
/// reads that `cal.json` in another device epoch, with the transport moved
/// by +16 samples on *both* legs — the common-mode shift a re-enumeration
/// produces. Its flight time is still 0 (arrival 48 − ref 36 − offset 12),
/// the offset's crossed epoch flags it without withholding it, and the
/// rejected `arrival − stored τ` is 16 samples off.
#[test]
fn plot_ir_compensates_a_moved_transport_from_the_live_reference() {
    use ac_core::measurement::report::{InterPairOffset, LatencyBasis, LiveOffset};
    const T1: &str = "2026-09-15T23:40:11Z";
    const T2: &str = "2026-09-16T00:08:31Z";
    const SHIFT: usize = 16;

    let d1 = Daemon::spawn_with(Some(reference_config()), &[("AC_FAKE_DEVICE_EPOCH", T1)]);
    let done = calibrate_done(&Client::new(&d1));
    assert_eq!(done["tau_state"], json!("measured"), "{done}");
    assert_eq!(done["tau_reference_state"], json!("measured"), "{done}");
    let offset = (FAKE_MEAS_DELAY_SAMPLES - FAKE_REF_DELAY_SAMPLES) as i64;
    assert_eq!(done["tau_offset_samples"], json!(offset), "{done}");
    let cal_rel = std::path::Path::new(".config").join("ac").join("cal.json");
    let cal = std::fs::read(d1.home.join(&cal_rel)).expect("read cal.json");

    let meas = (FAKE_MEAS_DELAY_SAMPLES + SHIFT).to_string();
    let reference = (FAKE_REF_DELAY_SAMPLES + SHIFT).to_string();
    let d2 = Daemon::spawn_with(
        Some(reference_config()),
        &[
            ("AC_FAKE_DEVICE_EPOCH", T2),
            ("AC_FAKE_TAU_DELAY_SAMPLES_OVERRIDE", &meas),
            ("AC_FAKE_REF_DELAY_SAMPLES", &reference),
        ],
    );
    std::fs::write(d2.home.join(&cal_rel), &cal).expect("share cal.json");
    let (_, report) = report_for(&Client::new(&d2), plot_ir_request(json!({})));

    match &report.inter_pair_offset {
        Some(InterPairOffset::Measured(m)) => {
            assert_eq!((m.offset_s * FAKE_SR).round() as i64, offset);
            assert!(
                matches!(m.enumeration, EnumerationCheck::Crossed { .. }),
                "{:?}",
                m.enumeration
            );
        }
        other => panic!("expected a measured offset, got {other:?}"),
    }
    let stats = report.ir_stats().expect("ir_stats");
    assert!(
        matches!(
            stats.latency_basis,
            LatencyBasis::Live {
                offset: LiveOffset::Measured { .. },
                ..
            }
        ),
        "{:?}",
        stats.latency_basis
    );
    let flight = stats
        .flight_time_s
        .expect("a crossed offset flags, never withholds");
    assert_eq!((flight * FAKE_SR).round() as i64, 0);

    let Some(ac_core::measurement::report::InterfaceLatency::Measured(stored)) =
        &report.interface_latency
    else {
        panic!("stored τ recorded: {:?}", report.interface_latency)
    };
    let rejected = stats.arrival_s - stored.tau_s;
    assert_eq!((rejected * FAKE_SR).round() as i64, SHIFT as i64);
}

/// #544 AC4 end to end: a τ stored with no reference configured carries no
/// reference leg, so once a loopback is configured `plot_ir` refuses the
/// offset, naming the pair — the stored τ is on file and is not used.
#[test]
fn plot_ir_refuses_an_offset_calibrate_never_measured() {
    use ac_core::measurement::report::{InterPairOffset, LatencyBasis, WithheldBasis};
    let d1 = Daemon::spawn();
    let done = calibrate_done(&Client::new(&d1));
    assert_eq!(
        done["tau_reference_state"],
        json!("not_configured"),
        "{done}"
    );
    let cal_rel = std::path::Path::new(".config").join("ac").join("cal.json");
    let cal = std::fs::read(d1.home.join(&cal_rel)).expect("read cal.json");

    let d2 = Daemon::spawn_with_config(Some(reference_config()));
    std::fs::write(d2.home.join(&cal_rel), &cal).expect("share cal.json");
    let (_, report) = report_for(&Client::new(&d2), plot_ir_request(json!({})));
    assert!(matches!(
        report.interface_latency,
        Some(ac_core::measurement::report::InterfaceLatency::Measured(_))
    ));
    let Some(InterPairOffset::Unavailable { reason }) = &report.inter_pair_offset else {
        panic!("{:?}", report.inter_pair_offset)
    };
    assert_eq!(
        reason,
        "[out0_in0] against ref [out1_in1]; \u{3c4} on file, reference leg not measured \
         with it; check: `ac calibrate` on this pair, loopback in place"
    );
    let stats = report.ir_stats().expect("ir_stats");
    assert!(matches!(
        stats.latency_basis,
        LatencyBasis::Withheld(WithheldBasis::OffsetNotMeasured { .. })
    ));
    assert_eq!(stats.flight_time_s, None);
}

/// #501: a bare request runs the `ac-core` default stimulus, the ack echoes
/// every value it accepted, and the defaulted window — echoed in seconds,
/// because the rate is unknown before the engine starts — arrives in the
/// IR frame as `round(0.4 s × rate)` samples, unclamped. On the fake's clean
/// loopback that default configuration clears the pre-impulse gate, which
/// the pre-#501 defaults (1 s, 4096 samples) could not.
#[test]
fn plot_ir_bare_request_runs_and_echoes_the_core_defaults() {
    use ac_core::measurement::sweep::{
        ir_default_window_len, IR_DEFAULT_DURATION_S, IR_DEFAULT_F1_HZ, IR_DEFAULT_F2_HZ,
        IR_DEFAULT_N_HARMONICS, IR_DEFAULT_TAIL_S, IR_DEFAULT_WINDOW_S,
    };
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({"cmd": "plot_ir"}));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(r["f1_hz"], json!(IR_DEFAULT_F1_HZ), "{r}");
    assert_eq!(r["f2_hz"], json!(IR_DEFAULT_F2_HZ), "{r}");
    assert_eq!(r["duration"], json!(IR_DEFAULT_DURATION_S), "{r}");
    assert_eq!(r["duration"], json!(4.0), "{r}");
    assert_eq!(r["n_harmonics"], json!(IR_DEFAULT_N_HARMONICS), "{r}");
    assert_eq!(r["tail_s"], json!(IR_DEFAULT_TAIL_S), "{r}");
    assert_eq!(r["window_default_s"], json!(IR_DEFAULT_WINDOW_S), "{r}");
    assert!(r.get("window_len").is_none(), "{r}");

    let expected = ir_default_window_len(FAKE_SR as u32);
    assert_eq!(expected, 19_200);
    let ir = c
        .wait_for_topic("measurement/impulse_response", Duration::from_secs(30))
        .expect("measurement/impulse_response frame");
    assert_eq!(ir["window_len_requested"], json!(expected), "{ir}");
    assert_eq!(
        ir["window_len_used"][0],
        json!(expected),
        "the default linear gate must not be clamped"
    );
    let v = c
        .wait_for_topic("measurement/report", Duration::from_secs(30))
        .expect("measurement/report frame");
    let report: MeasurementReport =
        serde_json::from_value(v["report"].clone()).expect("decode report");
    let stats = report.ir_stats().expect("ir_stats");
    assert_eq!(
        stats.verdict,
        ac_core::measurement::report::IrVerdict::Ok,
        "the default sweep must clear the gate on a clean loopback \
         (pre-imp SNR {:.2} dB)",
        stats.pre_impulse_snr_db
    );
}

#[test]
fn plot_ir_reports_the_gate_lengths_it_actually_used() {
    // #278: `window_len` is a request. Adjacent harmonic orders would
    // cross-contaminate if their gates overlapped, so each order is clamped
    // to the spacing of its nearest neighbour. At 200 Hz–8 kHz over 0.5 s at
    // 48 kHz, L = T/ln(f2/f1) = 135.5 ms, so Farina's Δt_k = L·ln(k) puts the
    // order centres at [0, 4510, 7148, 9019, 10471] samples and the gaps at
    // [4510, 2638, 1871, 1452].
    //
    // The linear IR must survive at the full 4096 — its only neighbour is
    // order 2, 4510 samples away — while orders 2..5 shrink. A global clamp
    // to the narrowest gap would cut the linear IR to 1452 and is what this
    // test is here to catch.
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd":"plot_ir",
        "f1_hz": 200.0,
        "f2_hz": 8_000.0,
        "duration": 0.5,
        "level_dbfs": -20.0,
        "tail_s": 0.1,
        "window_len": 4096,
        "n_harmonics": 5,
    }));
    assert_eq!(r["ok"], json!(true));

    let expected_used = json!([4096, 2638, 1871, 1452, 1452]);
    let mut got_ir = false;
    let mut got_report = false;
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline && !(got_ir && got_report) {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "measurement/impulse_response" => {
                assert_eq!(v["window_len_requested"], json!(4096));
                assert_eq!(
                    v["window_len_used"], expected_used,
                    "per-order gate lengths must be reported, not inferred"
                );
                let ir = v["data"]["linear_ir"].as_array().expect("linear_ir array");
                assert_eq!(
                    ir.len(),
                    4096,
                    "linear IR must keep the requested window; only the \
                     tight high orders are constrained"
                );
                let harmonics = v["data"]["harmonics"].as_array().expect("harmonics");
                for h in harmonics {
                    let order = h["order"].as_u64().expect("order") as usize;
                    let n = h["samples"].as_array().expect("samples").len();
                    assert_eq!(
                        json!(n),
                        expected_used[order - 1],
                        "order {order} gate length disagrees with window_len_used"
                    );
                }
                got_ir = true;
            }
            Some((t, v)) if t == "measurement/report" => {
                // A shortened gate changes what the harmonic IRs mean, so
                // it has to reach the operator rather than being applied
                // silently.
                let notes = v["report"]["notes"].as_str().expect("notes present");
                assert!(notes.contains("clamped"), "notes: {notes:?}");
                assert!(notes.contains("4096"), "notes: {notes:?}");
                assert!(notes.contains("order 2"), "notes: {notes:?}");
                assert!(
                    !notes.contains("order 1 \u{2192}"),
                    "the unclamped linear IR must not be listed: {notes:?}"
                );
                // The #282 tail-decay verdict shares the field and must
                // not have been displaced by the clamp note.
                assert!(notes.contains("18233"), "notes: {notes:?}");
                got_report = true;
            }
            Some((t, _)) if t == "done" => break,
            Some(_) => continue,
            None => break,
        }
    }
    assert!(got_ir, "never saw measurement/impulse_response frame");
    assert!(got_report, "never saw measurement/report frame");
}

/// #283 × #278: the `GateParams` archived on the IR payload must describe
/// the gate that actually ran, not the one that was asked for.
///
/// `window_len` is a request (#278) and the linear IR is clamped when
/// order 2 sits closer than the requested length. Recording the request
/// instead would archive a gate that never ran and an `f_low_hz` the
/// payload does not meet — and `f_low_hz` is the number #280 stores
/// precisely so a reader does not have to derive it.
///
/// The sweep is chosen so the two values differ: over 0.3 s from 200 Hz
/// to 8 kHz, L = T/ln(f2/f1) = 81.3 ms, so Farina's Δt_2 = L·ln 2 puts
/// order 2 about 2705 samples out — inside the requested 4096. An
/// implementation that recorded `window_len` would report a 4096-sample
/// gate and an f_low of 11.7 Hz here, both wrong.
#[test]
fn plot_ir_records_the_gate_it_used_not_the_one_requested() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd":"plot_ir",
        "f1_hz": 200.0,
        "f2_hz": 8_000.0,
        "duration": 0.3,
        "level_dbfs": -20.0,
        "tail_s": 0.1,
        "window_len": 4096,
        "n_harmonics": 3,
    }));
    assert_eq!(r["ok"], json!(true));

    let mut used: Option<u64> = None;
    let mut gate: Option<Value> = None;
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline && !(used.is_some() && gate.is_some()) {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "measurement/impulse_response" => {
                used = v["window_len_used"][0].as_u64();
            }
            Some((t, v)) if t == "measurement/report" => {
                gate = Some(v["report"]["data"][0]["gate"].clone());
            }
            Some((t, _)) if t == "done" => break,
            Some(_) => continue,
            None => break,
        }
    }
    let used = used.expect("window_len_used[0] on the IR frame");
    let gate = gate.expect("gate on the IR payload");

    // The premise: this sweep really does clamp. Without this the test
    // would pass against an implementation that records the request.
    assert!(
        used < 4096,
        "sweep did not clamp the linear IR ({used} samples) — the test no \
         longer distinguishes the recorded gate from the requested one"
    );

    let sr = 48_000.0;
    let gate_length_s = gate["gate_length_s"].as_f64().expect("gate_length_s");
    let f_low_hz = gate["f_low_hz"].as_f64().expect("f_low_hz");
    let gate_start_s = gate["gate_start_s"].as_f64().expect("gate_start_s");

    assert!(
        (gate_length_s - used as f64 / sr).abs() < 1e-9,
        "recorded gate_length_s {gate_length_s} does not match the {used} \
         samples actually used"
    );
    assert!(
        (f_low_hz - sr / used as f64).abs() < 1e-6,
        "recorded f_low_hz {f_low_hz} does not match the gate that ran"
    );
    // The gate is centred on the zero-delay reference, so it opens half a
    // window before it.
    assert!(
        (gate_start_s + (used / 2) as f64 / sr).abs() < 1e-9,
        "recorded gate_start_s {gate_start_s} is not half a window early"
    );
}

/// #284: `plot_ir`'s report carries a second payload — the gated
/// frequency response derived from the linear IR — alongside the
/// impulse-response payload. Its `f_low_hz` must match `1 / gate_length_s`
/// by hand arithmetic, and the points must carry both magnitude and phase.
#[test]
fn plot_ir_emits_a_gated_frequency_response_payload() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd":"plot_ir",
        "f1_hz": 200.0,
        "f2_hz": 8_000.0,
        "duration": 0.5,
        "level_dbfs": -20.0,
        "tail_s": 0.1,
        "window_len": 1024,
        "n_harmonics": 1,
    }));
    assert_eq!(r["ok"], json!(true));

    let v = c
        .wait_for_topic("measurement/report", Duration::from_secs(15))
        .expect("measurement/report frame");
    let data = v["report"]["data"].as_array().expect("data array");
    assert_eq!(data.len(), 2, "expected impulse response + gated response");
    assert_eq!(data[0]["data"]["kind"], json!("impulse_response"));
    assert_eq!(data[1]["data"]["kind"], json!("gated_frequency_response"));

    // Citations: the payload cites the Farina preprint (theoretical basis
    // only) and AES17-2020 Annex A.4.5 for the gating method itself — not
    // ISO 18233, which per the architect's #284 decision 4 only attaches
    // when a classical room standard also applies, which a quasi-anechoic
    // capture never has (PR #305 review, correctness issue 1).
    let standards = data[1]["standard"].as_array().expect("standard array");
    assert_eq!(standards.len(), 2, "{standards:?}");
    assert!(
        standards
            .iter()
            .any(|s| s["standard"].as_str().unwrap_or("").contains("AES17")),
        "{standards:?}"
    );
    assert!(
        !standards
            .iter()
            .any(|s| s["standard"].as_str().unwrap_or("").contains("ISO 18233")),
        "gated_frequency_response payload must not cite ISO 18233: {standards:?}"
    );

    // f_low_hz = 1 / gate_length_s, by hand arithmetic off the recorded
    // gate — not recomputed elsewhere.
    let gate = &data[1]["gate"];
    let gate_length_s = gate["gate_length_s"].as_f64().expect("gate_length_s");
    let f_low_hz = gate["f_low_hz"].as_f64().expect("f_low_hz");
    assert!(
        (f_low_hz - 1.0 / gate_length_s).abs() < 1e-9,
        "f_low_hz {f_low_hz} does not match 1/gate_length_s {}",
        1.0 / gate_length_s
    );
    assert_eq!(gate["window_kind"], json!("tukey0.25"));

    let points = data[1]["data"]["points"].as_array().expect("points array");
    assert!(points.len() > 4, "expected several frequency bins");
    for p in points {
        assert!(p["freq_hz"].is_number());
        assert!(p["magnitude_db"].is_number());
        assert!(p["phase_deg"].is_number());
    }

    // The impulse-response payload now also carries the noise-tail
    // boundary — derivable, not left to the reader (#284).
    let noise_tail = data[0]["data"]["noise_tail_start_s"]
        .as_f64()
        .expect("noise_tail_start_s present");
    assert!(
        (noise_tail - 0.5).abs() < 1e-9,
        "noise_tail_start_s should equal the sweep duration (0.5s): {noise_tail}"
    );
}

// ---------------------------------------------------------------------------
// Causal bound inputs (#460): a per-capture distance and a same-capture
// reference leg.
// ---------------------------------------------------------------------------

/// The fake reference leg's default delay (`DEFAULT_REF_DELAY_SAMPLES` in
/// `audio/fake/hooks.rs`) and the measurement leg's (32), at 48 kHz.
const FAKE_REF_DELAY_SAMPLES: usize = 20;
const FAKE_MEAS_DELAY_SAMPLES: usize = 32;
const FAKE_SR: f64 = 48_000.0;

/// A reference loopback pair on the fake backend: capture 1, playback 1.
fn reference_config() -> Value {
    json!({ "reference_channel": 1, "reference_output_channel": 1 })
}

/// `plot_ir` at a 4096-sample window (the IR's pre-impulse SNR clears #376's
/// floor on the fake) with a 0.2 s tail (holds the 0.1 s reference window),
/// plus `extra` fields.
fn plot_ir_request(extra: Value) -> Value {
    let mut req = json!({
        "cmd": "plot_ir",
        "f1_hz": 200.0,
        "f2_hz": 8_000.0,
        "duration": 0.5,
        "level_dbfs": -6.0,
        "tail_s": 0.2,
        "window_len": 4096,
        "n_harmonics": 3,
    });
    if let (Some(r), Some(e)) = (req.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            r.insert(k.clone(), v.clone());
        }
    }
    req
}

fn report_for(c: &Client, req: Value) -> (Value, MeasurementReport) {
    let reply = c.call(req);
    assert_eq!(reply["ok"], json!(true), "{reply}");
    let v = c
        .wait_for_topic("measurement/report", Duration::from_secs(15))
        .expect("measurement/report frame");
    let report = serde_json::from_value(v["report"].clone()).expect("decode report");
    (reply, report)
}

/// #460 AC1 + AC4, through the producer: a supplied distance reaches
/// `report.position.distance_m`, the same-capture reference is measured, and
/// `ir_stats()`'s causal bound is built from exactly those two. The expected
/// bound index is computed here from the fake's reference delay and `c`; so
/// is the index a bound built by mistake from the measurement leg would give,
/// which must differ. Two distances, one either side of the peak, so both
/// outcomes — bound enforced and the at-or-after-peak decline — are reached,
/// and which one each distance gives is computed rather than assumed.
#[test]
fn plot_ir_builds_the_causal_bound_from_distance_and_same_capture_reference() {
    let mut outcomes = Vec::new();
    for distance_m in [0.05_f64, 0.5] {
        let d = Daemon::spawn_with_config(Some(reference_config()));
        let c = Client::new(&d);
        let (reply, report) = report_for(&c, plot_ir_request(json!({ "distance_m": distance_m })));
        assert_eq!(reply["ref_in_port"], json!("fake:capture_1"), "{reply}");
        assert_eq!(reply["ref_out_port"], json!("fake:playback_1"), "{reply}");

        let position = report.position.as_ref().expect("position recorded");
        assert_eq!(position.distance_m, Some(distance_m));

        let reference = match report.reference_latency.as_ref() {
            Some(ReferenceLatency::Measured(m)) => m.clone(),
            other => panic!("reference not measured: {other:?}"),
        };
        let expected_tau_s = FAKE_REF_DELAY_SAMPLES as f64 / FAKE_SR;
        assert!(
            (reference.tau_s - expected_tau_s).abs() < 1e-12,
            "reference τ {} vs the fake reference leg's {expected_tau_s}",
            reference.tau_s
        );
        assert_eq!(reference.method, "farina_same_capture_reference_v1");
        assert_eq!(reference.input_port, "fake:capture_1");
        assert_eq!(reference.output_port, "fake:playback_1");

        let stats = report.ir_stats().expect("ir_stats");
        let centre = stats.window_len / 2;
        let c_m_s = ac_core::shared::conversions::speed_of_sound_from_config(None);
        let expected_bound =
            (centre as f64 + (expected_tau_s + distance_m / c_m_s) * FAKE_SR).round() as usize;
        let measurement_leg_bound = (centre as f64
            + (FAKE_MEAS_DELAY_SAMPLES as f64 / FAKE_SR + distance_m / c_m_s) * FAKE_SR)
            .round() as usize;
        assert_ne!(
            expected_bound, measurement_leg_bound,
            "test setup: the two legs must give different bounds"
        );
        assert_eq!(
            stats.causal_bound.min_admissible_index(),
            Some(expected_bound),
            "distance {distance_m}: {:?}",
            stats.causal_bound
        );
        if expected_bound < stats.peak_index {
            assert!(
                stats.onset_rule.contains("causal bound enforced"),
                "distance {distance_m}: {}",
                stats.onset_rule
            );
            assert!(stats.onset_index >= expected_bound);
            outcomes.push("enforced");
        } else {
            assert!(
                stats
                    .onset_rule
                    .contains("causal bound at or after the peak"),
                "distance {distance_m}: {}",
                stats.onset_rule
            );
            outcomes.push("declined");
        }
    }
    assert_eq!(
        outcomes,
        vec!["enforced", "declined"],
        "test setup: the two distances must reach both outcomes"
    );
}

/// #460 AC3 through the producer: a distance with no reference configured
/// records the reference as unavailable, with the reason the read-out prints,
/// and the bound names the reference latency as the missing input.
#[test]
fn plot_ir_with_a_distance_but_no_reference_names_the_reference_as_missing() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let (reply, report) = report_for(&c, plot_ir_request(json!({ "distance_m": 1.0 })));
    assert!(reply.get("ref_in_port").is_none(), "{reply}");
    assert_eq!(
        report.position.as_ref().and_then(|p| p.distance_m),
        Some(1.0)
    );
    match report.reference_latency.as_ref() {
        Some(ReferenceLatency::Unavailable { reason }) => {
            assert_eq!(reason, "no reference configured (ac setup reference)")
        }
        other => panic!("expected an unavailable reference, got {other:?}"),
    }
    let stats = report.ir_stats().expect("ir_stats");
    assert_eq!(stats.causal_bound.min_admissible_index(), None);
    assert!(
        stats
            .onset_rule
            .contains("no causal bound (reference latency unavailable)"),
        "{}",
        stats.onset_rule
    );
}

/// #460 AC3: a measured reference with no distance names the distance.
#[test]
fn plot_ir_with_a_reference_but_no_distance_names_the_distance_as_missing() {
    let d = Daemon::spawn_with_config(Some(reference_config()));
    let c = Client::new(&d);
    let (_, report) = report_for(&c, plot_ir_request(json!({})));
    assert_eq!(report.position.as_ref().and_then(|p| p.distance_m), None);
    assert!(
        matches!(
            report.reference_latency,
            Some(ReferenceLatency::Measured(_))
        ),
        "{:?}",
        report.reference_latency
    );
    let stats = report.ir_stats().expect("ir_stats");
    assert!(
        stats
            .onset_rule
            .contains("no causal bound (distance not given)"),
        "{}",
        stats.onset_rule
    );
}

/// A reference leg that fails `calibrate`'s SNR gate is recorded as
/// unavailable with an observation and a `check:` part, and no bound is built
/// from it. `AC_FAKE_REF_GAIN=0` leaves only the noise override's dither on
/// the reference leg.
#[test]
fn plot_ir_reports_a_failed_reference_reading_as_unavailable_with_its_check() {
    let d = Daemon::spawn_with(
        Some(reference_config()),
        &[
            ("AC_FAKE_REF_GAIN", "0"),
            ("AC_FAKE_TAU_NOISE_AMPLITUDE_OVERRIDE", "0.001"),
        ],
    );
    let c = Client::new(&d);
    let (_, report) = report_for(&c, plot_ir_request(json!({ "distance_m": 0.05 })));
    match report.reference_latency.as_ref() {
        Some(ReferenceLatency::Unavailable { reason }) => {
            assert!(reason.starts_with("peak SNR "), "{reason}");
            assert!(
                reason.contains("; check: reference loopback cable, ref input gain"),
                "{reason}"
            );
        }
        other => panic!("expected an SNR-refused reference, got {other:?}"),
    }
    let stats = report.ir_stats().expect("ir_stats");
    assert_eq!(stats.causal_bound.min_admissible_index(), None);
    assert!(
        stats.onset_rule.contains("reference latency unavailable"),
        "{}",
        stats.onset_rule
    );
}

/// #471: a good reference leg at `plot ir`'s **default** sweep must measure.
/// Before the derived floor it was refused — the default 20–20000 Hz band
/// floors at ~17 dB against a fixed 24 dB gate, so a mathematically perfect
/// loopback could not pass. Omits `f1_hz`/`f2_hz`/`duration` so the daemon's
/// own defaults apply; that is the whole point of the test.
#[test]
fn plot_ir_measures_the_reference_at_the_default_sweep() {
    let d = Daemon::spawn_with_config(Some(reference_config()));
    let c = Client::new(&d);
    let (_, report) = report_for(
        &c,
        json!({
            "cmd": "plot_ir",
            "level_dbfs": -6.0,
            "tail_s": 0.3,
            "window_len": 4096,
            "n_harmonics": 3,
            "distance_m": 0.05,
        }),
    );
    match report.reference_latency.as_ref() {
        Some(ReferenceLatency::Measured(m)) => {
            assert!(
                (m.tau_s - FAKE_REF_DELAY_SAMPLES as f64 / FAKE_SR).abs() < 1e-12,
                "reference τ {} is not the fake reference leg's delay",
                m.tau_s
            );
        }
        other => panic!("default sweep must measure the reference, got {other:?}"),
    }
    let stats = report.ir_stats().expect("ir_stats");
    assert!(
        stats.causal_bound.min_admissible_index().is_some(),
        "a measured reference and a distance must still build the bound: {:?}",
        stats.causal_bound
    );
}

/// A capture tail too short to hold the reference window is recorded as
/// unavailable with the tail reason, not silently retried and not folded
/// into "no reference configured" — the operator supplied one.
///
/// This branch was unreachable before #460: `calibrate` measures τ with a
/// fixed `TAU_TAIL_S` that `tau_tail_s_clears_tau_min_half_window_s_with_margin`
/// pins clear of the window. `plot_ir`'s `tail_s` is operator-controlled
/// (budget 0–60 s), so #460 makes it reachable and this is what it produces.
/// The reference window is `2 × TAU_MIN_HALF_WINDOW_S` = 0.10 s.
#[test]
fn plot_ir_reports_a_short_tail_reference_as_unavailable() {
    let d = Daemon::spawn_with_config(Some(reference_config()));
    let c = Client::new(&d);
    let mut req = plot_ir_request(json!({ "distance_m": 0.05 }));
    req["tail_s"] = json!(0.05);
    let (_, report) = report_for(&c, req);
    match report.reference_latency.as_ref() {
        Some(ReferenceLatency::Unavailable { reason }) => {
            assert_eq!(
                reason,
                "tail 0.05 s, reference window needs 0.10 s; \
                 check: lengthen the tail token (e.g. 0.8s)"
            );
        }
        other => panic!("expected a tail-refused reference, got {other:?}"),
    }
    let stats = report.ir_stats().expect("ir_stats");
    assert_eq!(stats.causal_bound.min_admissible_index(), None);
}

/// An xrun across the capture refuses the reference reading outright,
/// whatever its SNR — the precedence `analyse_tau_leg` inherits from
/// #368/#369. A reading taken across a discontinuity is a stable,
/// repeatable, wrong τ, which is exactly what the bound must not be built
/// from.
#[test]
fn plot_ir_reports_an_xrun_during_capture_as_unavailable() {
    let d = Daemon::spawn_with(Some(reference_config()), &[("AC_FAKE_XRUNS_OVERRIDE", "1")]);
    let c = Client::new(&d);
    let (_, report) = report_for(&c, plot_ir_request(json!({ "distance_m": 0.05 })));
    match report.reference_latency.as_ref() {
        Some(ReferenceLatency::Unavailable { reason }) => {
            assert_eq!(
                reason,
                "xrun during capture; check: JACK period size, system load"
            );
        }
        other => panic!("expected an xrun-refused reference, got {other:?}"),
    }
    let stats = report.ir_stats().expect("ir_stats");
    assert_eq!(stats.causal_bound.min_admissible_index(), None);
}

/// A reference peak inside the edge margin of its own window is refused,
/// not reported — a peak that close to the edge is indistinguishable from
/// one pinned by an arrival outside the window entirely (#340 AC4), and a
/// window edge imitates a latency.
///
/// The delay is derived, not guessed. At the fake's 48 kHz:
/// `half = ceil(0.05 × 48000) = 2400`, `window_len = 4800`, and
/// `check_peak_within_window` refuses when the peak sits within
/// `round(0.10 × half) = 240` of either edge. The reference peak lands at
/// `half + delay`, so refusal needs `half + delay ≥ 4800 - 1 - 240`, i.e.
/// `delay ≥ 2159`. 2200 clears that by 41 samples and still fits the
/// capture.
///
/// #494: the edge check now runs first and the reason names a failed SNR
/// gate beside it. This in-window peak clears its derived threshold, so the
/// exact edge-only string below is what holds; the combined string is
/// pinned by a unit test beside `reference_unavailable_reason`.
#[test]
fn plot_ir_reports_a_reference_peak_at_the_window_edge_as_unavailable() {
    let half = (0.05 * FAKE_SR).ceil() as usize;
    let window_len = 2 * half;
    let margin = (0.10 * half as f64).round() as usize;
    let delay: usize = 2200;
    assert!(
        half + delay >= window_len - 1 - margin,
        "test setup: delay {delay} does not reach the edge margin"
    );
    assert!(
        half + delay < window_len,
        "test setup: delay {delay} puts the peak outside the window"
    );

    let d = Daemon::spawn_with(
        Some(reference_config()),
        &[("AC_FAKE_REF_DELAY_SAMPLES", &delay.to_string())],
    );
    let c = Client::new(&d);
    let (_, report) = report_for(&c, plot_ir_request(json!({ "distance_m": 0.05 })));
    match report.reference_latency.as_ref() {
        Some(ReferenceLatency::Unavailable { reason }) => {
            assert_eq!(
                reason,
                "peak at reference window edge; \
                 check: reference loopback routing, capture tail"
            );
        }
        other => panic!("expected an edge-refused reference, got {other:?}"),
    }
    let stats = report.ir_stats().expect("ir_stats");
    assert_eq!(stats.causal_bound.min_admissible_index(), None);
}

/// #359: a same-capture reference reading exactly one JACK period apart
/// from the τ `calibrate` has on file for that pair is reported as a
/// graph-buffering shift, naming the period — the same fault #347 guards
/// for `calibrate`, now caught on `plot_ir`'s path too (#347 covers only
/// `calibrate`).
///
/// The measurement pair (`0`/`0`, the config default) and the reference
/// pair (`1`/`1`, `reference_config()`) are **distinct** — the ordinary
/// setup, and the one the QA correction on this issue's PR exists for:
/// `plot_ir`'s reference-pair lookup must land in the reference pair's own
/// `cal.json` entry, not the measurement pair's, because `Calibration::load`
/// keys strictly by channel pair and the two are routinely different.
///
/// Three separate engine lifecycles inside one daemon process: `calibrate`'s
/// own two (`measure_tau_twice`, forced to agree with each other via
/// `AC_FAKE_TAU_DELAY_SAMPLES_OVERRIDE`), then `plot_ir`'s. The reference
/// leg's own hook (`AC_FAKE_REF_DELAY_SAMPLES`) is independent of the
/// general loopback delay `calibrate` measured with, so it is set one
/// period later than what `calibrate` stored — the period-shift fault is a
/// property of separately-lifecycled engines, not of separate daemon
/// processes (#347's own "per client" framing), so one process suffices.
#[test]
fn plot_ir_detects_a_period_shift_on_the_reference_pair() {
    const PERIOD: u32 = 64;
    // `ref_delay_samples()`'s own default (`FAKE_REF_DELAY_SAMPLES` above) —
    // forcing `calibrate`'s stored τ to land here means "one period later"
    // is exactly `FAKE_REF_DELAY_SAMPLES + PERIOD` below.
    const STORED_DELAY: usize = FAKE_REF_DELAY_SAMPLES;
    let shifted_delay = STORED_DELAY + PERIOD as usize;

    let d = Daemon::spawn_with(
        Some(reference_config()),
        &[
            ("AC_FAKE_PERIOD_SIZE_OVERRIDE", &PERIOD.to_string()),
            (
                "AC_FAKE_TAU_DELAY_SAMPLES_OVERRIDE",
                &format!("{STORED_DELAY},{STORED_DELAY}"),
            ),
            ("AC_FAKE_REF_DELAY_SAMPLES", &shifted_delay.to_string()),
        ],
    );
    let c = Client::new(&d);

    // 1. Store a τ for the pair `cal_guard!` will load on step 2 —
    //    `calibrate`'s two independent engine lifecycles, forced to agree
    //    at `STORED_DELAY` samples by the override above.
    let r = c.call(json!({
        "cmd": "calibrate", "ref_dbfs": -20.0,
        "output_channel": 1, "input_channel": 1,
    }));
    assert_eq!(r["ok"], json!(true), "{r}");
    for step in 1..=2 {
        c.wait_for_topic("cal_prompt", Duration::from_secs(5))
            .unwrap_or_else(|| panic!("step {step} prompt"));
        let _ = c.call(json!({"cmd": "cal_reply", "vrms": null}));
    }
    let done = c
        .wait_for_topic("cal_done", Duration::from_secs(5))
        .expect("cal_done frame");
    assert_eq!(done["tau_state"], json!("measured"), "frame: {done}");

    // 2. A third, later engine lifecycle — `plot_ir`'s own capture — reads
    //    the reference leg `PERIOD` samples later than what step 1 stored.
    let (_, report) = report_for(&c, plot_ir_request(json!({})));

    let stats = report.ir_stats().expect("ir_stats");
    match stats.arrival_check {
        ArrivalCheck::PeriodShift(disagreement) => {
            assert_eq!(disagreement.period_size, Some(PERIOD));
            assert_eq!(disagreement.periods, Some(1));
        }
        other => panic!("expected a period shift, got {other:?}"),
    }
}

/// A reference that is configured but does not resolve is refused before
/// any audio, not silently run single-ended (#225).
#[test]
fn plot_ir_refuses_an_unresolvable_reference_before_any_audio() {
    let d = Daemon::spawn_with_config(Some(json!({ "reference_channel": 99 })));
    let c = Client::new(&d);
    let r = c.call(plot_ir_request(json!({})));
    assert_eq!(r["ok"], json!(false), "{r}");
    assert!(
        r["error"].as_str().unwrap_or_default().contains("99"),
        "{r}"
    );
    assert!(
        c.wait_for_topic("measurement/report", Duration::from_millis(1500))
            .is_none(),
        "a refused request must not produce a report"
    );
    let status = c.call(json!({"cmd": "status"}));
    assert_eq!(status["busy"], json!(false), "{status}");
}

/// Run one `plot_ir` and collect every PUB frame until `done` or `error`,
/// then keep listening `settle` longer so a late frame is seen too.
fn plot_ir_frames(c: &Client, req: Value, settle: Duration) -> Vec<(String, Value)> {
    let reply = c.call(req);
    assert_eq!(reply["ok"], json!(true), "{reply}");
    let mut frames = Vec::new();
    let mut deadline = Instant::now() + Duration::from_secs(20);
    let mut ended = false;
    while Instant::now() < deadline {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        let Some((t, v)) = c.recv_pub(remaining.max(1)) else {
            break;
        };
        if !ended && (t == "done" || t == "error") {
            ended = true;
            deadline = Instant::now() + settle;
        }
        frames.push((t, v));
    }
    assert!(ended, "plot_ir never finished: {frames:?}");
    frames
}

/// #576 AC3: the linear-IR gate reads `N − 1 + ceil(W₁/2)` capture samples.
/// With a 0.25 s tail (exactly 12000 samples at 48 kHz) and one harmonic
/// (no clamp), a 24002-sample window reads exactly the capture and runs;
/// 24003 reads one sample past it and is refused before any stimulus, with
/// no IR and no report. Before #576 both ran silently.
#[test]
fn plot_ir_refuses_a_tail_one_sample_short_of_the_ir_window() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let req = |window_len: usize| {
        json!({
            "cmd": "plot_ir",
            "f1_hz": 200.0,
            "f2_hz": 8_000.0,
            "duration": 0.5,
            "level_dbfs": -6.0,
            "tail_s": 0.25,
            "window_len": window_len,
            "n_harmonics": 1,
        })
    };

    let at = plot_ir_frames(&c, req(24_002), Duration::from_millis(200));
    assert!(
        !at.iter().any(|(t, _)| t == "error"),
        "a capture exactly at the minimum must run: {at:?}"
    );
    let ir = at
        .iter()
        .find(|(t, _)| t == "measurement/impulse_response")
        .map(|(_, v)| v)
        .expect("IR frame at the threshold");
    assert_eq!(ir["window_len_used"][0], json!(24_002), "{ir}");
    assert!(
        at.iter().any(|(t, _)| t == "measurement/report"),
        "report at the threshold"
    );

    let short = plot_ir_frames(&c, req(24_003), Duration::from_millis(1500));
    let err = short
        .iter()
        .find(|(t, _)| t == "error")
        .map(|(_, v)| v)
        .expect("error frame one sample short");
    assert_eq!(err["cmd"], json!("plot_ir"), "{err}");
    let msg = err["message"].as_str().unwrap_or_default();
    assert!(
        msg.starts_with("plot_ir not started \u{2014} tail too short for the IR window"),
        "{msg}"
    );
    for needle in [
        "12000 samples  (0.25 s typed, 48000 Hz)",
        "24003 samples  (0.5001 s, typed)",
        "12001 samples",
        "tail 0.26s or longer, or window 24002win or shorter",
        "stimulus  silent",
    ] {
        assert!(msg.contains(needle), "missing {needle:?}: {msg}");
    }
    for topic in ["measurement/impulse_response", "measurement/report", "done"] {
        assert!(
            !short.iter().any(|(t, _)| t == topic),
            "a refused run must not emit {topic}: {short:?}"
        );
    }
    let status = c.call(json!({"cmd": "status"}));
    assert_eq!(status["busy"], json!(false), "{status}");
}

/// `distance_m` budget: anything but a finite, positive number is refused
/// before port resolution, with the stimulus stated silent.
#[test]
fn plot_ir_refuses_an_unusable_distance_before_any_audio() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    for bad in [json!(0.0), json!(-1.0), json!("1m")] {
        let r = c.call(plot_ir_request(json!({ "distance_m": bad })));
        assert_eq!(r["ok"], json!(false), "{bad}: {r}");
        let err = r["error"].as_str().unwrap_or_default();
        assert!(
            err.contains("distance_m must be finite and > 0 m"),
            "{bad}: {err}"
        );
        assert!(err.contains("stimulus  silent"), "{bad}: {err}");
    }
    assert!(
        c.wait_for_topic("measurement/report", Duration::from_millis(1000))
            .is_none(),
        "a refused request must not produce a report"
    );
}

// ---------------------------------------------------------------------------
// Time-integration — set_time_integration / get_time_integration / reset_leq.
// See issue #62.
// ---------------------------------------------------------------------------

/// Run one `plot_ir` and return its `done` frame.
fn done_frame_for(c: &Client) -> Value {
    let reply = c.call(plot_ir_request(json!({})));
    assert_eq!(reply["ok"], json!(true), "{reply}");
    let done = c
        .wait_for_topic("done", Duration::from_secs(20))
        .expect("plot_ir done frame");
    assert_eq!(done["cmd"], json!("plot_ir"), "{done}");
    done
}

/// Point the daemon's report directory at `dir` through `setup`.
fn set_report_dir(c: &Client, dir: &std::path::Path) {
    let r = c.call(json!({"cmd": "setup", "update": {"report_dir": dir.to_str().unwrap()}}));
    assert_eq!(r["ok"], json!(true), "{r}");
}

/// #472 — with a report directory configured, `done.report_files` names the
/// two files, and both exist on disk.
#[test]
fn plot_ir_done_frame_names_the_files_it_wrote() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let dir = d.home.join("reports");
    std::fs::create_dir_all(&dir).unwrap();
    set_report_dir(&c, &dir);

    let done = done_frame_for(&c);
    let files = &done["report_files"];
    assert_eq!(files["dir"], json!(dir.to_str().unwrap()), "{done}");
    for (key, suffix) in [("json", "-plot_ir.json"), ("csv", "-plot_ir.csv")] {
        let path = files[key]["path"]
            .as_str()
            .unwrap_or_else(|| panic!("{key} has no path: {done}"));
        assert!(path.ends_with(suffix), "{path}");
        assert!(
            std::path::Path::new(path).starts_with(&dir),
            "{path} outside {}",
            dir.display()
        );
        assert!(std::path::Path::new(path).is_file(), "{path} not on disk");
        assert!(files[key].get("error").is_none(), "{done}");
    }
}

/// #472 — with no report directory, `done.report_files` says so explicitly.
#[test]
fn plot_ir_done_frame_says_when_no_report_dir_is_configured() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let done = done_frame_for(&c);
    assert_eq!(done["report_files"], json!({"dir": null}), "{done}");
}

/// #472 — a directory removed after `setup` accepted it: both writes report
/// the OS error, and the directory is *not* recreated (the old `write_to`
/// path ran `create_dir_all`, archiving into a place nobody chose).
#[test]
fn plot_ir_done_frame_reports_a_failed_write_and_does_not_recreate_the_dir() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let dir = d.home.join("reports");
    std::fs::create_dir_all(&dir).unwrap();
    set_report_dir(&c, &dir);
    std::fs::remove_dir(&dir).unwrap();

    let done = done_frame_for(&c);
    let files = &done["report_files"];
    assert_eq!(files["dir"], json!(dir.to_str().unwrap()), "{done}");
    for key in ["json", "csv"] {
        let err = files[key]["error"]
            .as_str()
            .unwrap_or_else(|| panic!("{key} carries no error: {done}"));
        assert!(err.contains("os error 2"), "{key}: {err:?}");
        assert!(files[key].get("path").is_none(), "{done}");
    }
    assert!(
        !dir.exists(),
        "plot_ir recreated the removed report directory"
    );
}

/// #669 acceptance on fake audio: `plot_ir` and a transfer session's
/// start-up Find read the same delay off the same 400-sample path — one
/// picker (`ir_peak`), two ways of measuring the IR. The fake paths are
/// exact, so the two agree to the sample here; the rig tolerance is the
/// PR's to state.
#[test]
fn plot_ir_and_transfer_find_the_same_delay() {
    const DELAY: i64 = 400;
    let d = Daemon::spawn_with(
        None,
        &[("AC_FAKE_TAU_DELAY_SAMPLES_OVERRIDE", &DELAY.to_string())],
    );
    let (_, report) = report_for(&Client::new(&d), plot_ir_request(json!({})));
    let stats = report.ir_stats().expect("ir_stats");
    assert_eq!(stats.delay_samples, DELAY, "plot_ir arrival");

    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd": "transfer_stream", "meas_channel": 0, "ref_channel": 1,
        "weighting": "Z", "integration": "fast",
        "fake_correlated_pair": {"gain": 0.6, "delay_samples": DELAY},
    }));
    assert_eq!(r["ok"], json!(true), "{r}");
    let f = c.frame_matching(Duration::from_secs(20), |f| {
        f["delay_locked"] == json!(true) && f["delay_residual"].is_i64()
    });
    assert_eq!(f["delay_samples"], json!(DELAY), "transfer Find: {f}");
    assert_eq!(f["delay_residual"], json!(0), "{f}");
    assert_eq!(f["delay_operator"], json!(false));
}
