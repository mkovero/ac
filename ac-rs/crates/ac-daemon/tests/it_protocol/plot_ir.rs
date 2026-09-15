use serde_json::json;
use serde_json::Value;
use std::time::Duration;
use std::time::Instant;

use ac_core::measurement::report::{MeasurementReport, ReferenceLatency};

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
                assert_eq!(v["report"]["schema_version"], json!(7));
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
    // `measure_tau` locates τ via `argmax|h|`, and under #378's
    // contingency (AC6 rig run, 2026-09-15) `ir_stats().arrival_s` is
    // peak-derived again, so both halves share one rule and cancel on
    // this fixture. The onset-vs-argmax phantom #351 tracked (-2.458 ms,
    // 118 samples of bandlimited pre-ring before the peak) no longer
    // reaches the arrival.
    //
    // Pinned tight around this fixture's known, computable answer (QA on
    // #352: a bare `< 0.15` gate over a fake-backend fixture with a known
    // exact value could hide unrelated regression) rather than left as an
    // open-ended bound — this fixture is deterministic (fake backend,
    // fixed 200 Hz–8 kHz / 1024-sample window), so its exact phantom
    // flight time is a known quantity, not measurement noise. Value moved
    // -0.310 → -2.458 ms under #378, and to 0 under its contingency.
    const EXPECTED_PHANTOM_FLIGHT_MS: f64 = 0.0;
    let report: ac_core::measurement::report::MeasurementReport =
        serde_json::from_value(v["report"].clone()).expect("decode report");
    let stats = report.ir_stats().expect("ir_stats");
    let flight_ms = (stats.arrival_s - used_tau) * 1000.0;
    assert!(
        (flight_ms - EXPECTED_PHANTOM_FLIGHT_MS).abs() < 0.03,
        "fake loopback has no acoustic path; τ and arrival are both \
         peak-derived, so the flight time must be {EXPECTED_PHANTOM_FLIGHT_MS} ms \
         ± 0.03, got {flight_ms} ms"
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
