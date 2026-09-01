use serde_json::json;
use serde_json::Value;
use std::time::Duration;
use std::time::Instant;

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
        "level_dbfs": -6.0,
        "tail_s": 0.1,
        "window_len": 1024,
        "n_harmonics": 3,
    }));
    assert_eq!(r["ok"], json!(true));

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
                assert_eq!(v["report"]["schema_version"], json!(5));
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

// ---------------------------------------------------------------------
// Drive ceiling (#360) — plot_ir and calibrate previously emitted an
// unclamped level; both are commands whose whole point is to put a
// stimulus on a physical output, and `drive_max_dbfs` governed neither.
// ---------------------------------------------------------------------

/// `plot_ir` clamps its requested level to `drive_max_dbfs`.
///
/// The deconvolved IR itself cannot be used as the observable here: the
/// handler deliberately re-scales the recovered impulse response by
/// `1/amp` (`plot.rs`, "so the reported IR has unity peak for an identity
/// loopback regardless of `level_dbfs`"), and the fake backend's
/// `play_and_capture` is a noiseless echo of exactly what was played — so
/// on this backend the published IR is invariant to level by construction,
/// clamped or not, and asserting on it would prove nothing.
///
/// `report.stimulus.level_dbfs` is emitted from inside the worker, after
/// the capture, from the same binding that scaled the actually-played
/// sweep (`let amp = dbfs_to_amplitude(level_dbfs)`) — a different
/// computation from the synchronous CTRL reply, so this does not just
/// re-check the same echo twice under two names.
#[test]
fn plot_ir_clamps_level_to_drive_max_dbfs() {
    const CEILING_DBFS: f64 = -35.0;
    let d = Daemon::spawn_with_config(Some(json!({ "drive_max_dbfs": CEILING_DBFS })));
    let c = Client::new(&d);

    let r = c.call(json!({
        "cmd": "plot_ir",
        "f1_hz": 200.0,
        "f2_hz": 8_000.0,
        "duration": 0.5,
        "level_dbfs": 12.0,
        "tail_s": 0.1,
        "window_len": 1024,
        "n_harmonics": 3,
    }));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(
        r["level_dbfs"],
        json!(CEILING_DBFS),
        "sync reply must echo the applied (clamped) level, not the request: {r}"
    );

    let v = c
        .wait_for_topic("measurement/report", Duration::from_secs(15))
        .expect("measurement/report frame");
    let applied = v["report"]["stimulus"]["level_dbfs"]
        .as_f64()
        .expect("stimulus.level_dbfs");
    assert!(
        (applied - CEILING_DBFS).abs() < 1e-9,
        "report recorded level {applied}, requested 12.0 dBFS against a {CEILING_DBFS} \
         ceiling — plot_ir emitted the raw request instead of the clamped level"
    );
}

/// #283: `plot_ir` resolves τ by *exact* match on `TauConditions`, and
/// the entry it must hit was written by `calibrate`. Nothing but a test
/// couples those two condition tuples — they are built in different
/// handlers, from different locals — so a drift in either (a port
/// resolved differently, a device field read from elsewhere) would leave
/// every `plot_ir` reporting "distance unavailable" forever, with no
/// error anywhere. The failure is silent by construction, so it needs an
/// explicit check that the round trip lands.
#[test]
fn plot_ir_resolves_the_tau_that_calibrate_stored() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    // 1. Measure τ. Both voltage prompts skipped — τ is keyed on
    //    loopback detection, not on either reply (see
    //    `calibrate_cheap_refresh_still_measures_tau`).
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
    assert_eq!(done["tau_state"], json!("measured"), "frame: {done}");
    let stored_tau = done["tau_s"].as_f64().expect("tau_s");

    // 2. Run an IR capture under the same conditions.
    let r = c.call(json!({
        "cmd":"plot_ir",
        "f1_hz": 200.0,
        "f2_hz": 8_000.0,
        "duration": 0.5,
        "level_dbfs": -6.0,
        "tail_s": 0.1,
        "window_len": 1024,
        "n_harmonics": 3,
    }));
    assert_eq!(r["ok"], json!(true));
    let v = c
        .wait_for_topic("measurement/report", Duration::from_secs(15))
        .expect("measurement/report frame");

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

    // With a τ this close to the arrival (both are the same 32-sample
    // fake loopback), the τ-corrected flight time must land near zero —
    // the fake backend has no acoustic path. A τ that failed to subtract
    // would read ~0.67 ms instead (32 samples at 48 kHz).
    //
    // Stopgap tolerance (#346 → #351): `measure_tau` still locates τ via
    // `argmax|h|`, while `ir_stats().arrival_s` is onset-derived. Under
    // #378 the onset is an AIC change-point pick with no causal bound
    // available on this fixture, so it lands where the bandlimited
    // deconvolution's pre-ring leaves the numerical floor — 118 samples
    // (2.458 ms at 48 kHz) before the peak for this 200 Hz–8 kHz sweep.
    // The two estimators therefore no longer cancel. #351 tracks
    // reconciling them.
    //
    // Pinned tight around this fixture's known, computable answer (QA on
    // #352: a bare `< 0.15` gate over a fake-backend fixture with a known
    // exact value could hide unrelated regression) rather than left as an
    // open-ended bound — this fixture is deterministic (fake backend,
    // fixed 200 Hz–8 kHz / 1024-sample window), so its exact phantom
    // flight time is a known quantity, not measurement noise. Value moved
    // -0.310 → -2.458 ms under #378 for the reason above.
    const EXPECTED_PHANTOM_FLIGHT_MS: f64 = -2.4583;
    let report: ac_core::measurement::report::MeasurementReport =
        serde_json::from_value(v["report"].clone()).expect("decode report");
    let stats = report.ir_stats().expect("ir_stats");
    let flight_ms = (stats.arrival_s - used_tau) * 1000.0;
    assert!(
        (flight_ms - EXPECTED_PHANTOM_FLIGHT_MS).abs() < 0.03,
        "fake loopback has no acoustic path; expected the known #351 \
         phantom flight time ({EXPECTED_PHANTOM_FLIGHT_MS} ms ± 0.03), got \
         {flight_ms} ms"
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
        "level_dbfs": -6.0,
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
        "level_dbfs": -6.0,
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
        "level_dbfs": -6.0,
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
// Time-integration — set_time_integration / get_time_integration / reset_leq.
// See issue #62.
// ---------------------------------------------------------------------------
