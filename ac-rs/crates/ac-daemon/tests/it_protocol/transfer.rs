use serde_json::json;
use serde_json::Value;
use std::time::Duration;
use std::time::Instant;

use crate::common::{Client, Daemon};

#[test]
fn transfer_stream_missing_reference_errors() {
    // Neither `ref_channel` nor a `pairs` array — the handler's pair
    // parser rejects this before any worker spawns.
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd":          "transfer_stream",
        "meas_channel": 0,
    }));
    assert_eq!(r["ok"], json!(false));
    let err = r["error"].as_str().unwrap_or("");
    assert!(
        err.contains("ref_channel") || err.contains("pairs"),
        "unexpected error message: {err:?}"
    );
}

#[test]
fn transfer_stream_emits_data_and_done() {
    // `drive=true` makes the daemon play pink noise on its own output
    // while capturing from two channels of the fake backend. Channel
    // pair (0, 1) should produce at least one `transfer_stream` data
    // frame carrying the expected fields.
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd":          "transfer_stream",
        "meas_channel": 0,
        "ref_channel":  1,
        "drive":        true,
        "level_dbfs":   -12.0,
    }));
    assert_eq!(r["ok"], json!(true), "unexpected REP: {r:?}");

    let mut got_frame = false;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "data" && v["type"].as_str() == Some("transfer_stream") => {
                for key in [
                    "freqs",
                    "magnitude_db",
                    "phase_deg",
                    "coherence",
                    "delay_samples",
                    "delay_ms",
                ] {
                    assert!(v.get(key).is_some(), "frame missing {key}: {v}");
                }
                // `re` / `im` were removed from this frame: they carried
                // H₁(ω) for Tier 2 views that were never built, nothing
                // in the tree read them, and they were 86 KB of a 222 KB
                // frame. Asserted absent rather than merely dropped from
                // the list above, so re-adding them has to be a decision
                // someone makes here and not a paste that goes unnoticed.
                // `ZMQ.md`'s "Complex H is not on this frame" paragraph
                // is the contract this guards.
                for key in ["re", "im"] {
                    assert!(
                        v.get(key).is_none(),
                        "{key} is back on the transfer_stream frame: {v}"
                    );
                }
                // What the re/im consistency check used to cover, in the
                // terms that survive: the display arrays are one
                // downsampling of one H₁, so they are the same length,
                // and every element is a number a plotter can use. A
                // frame whose arrays disagree in length is the failure
                // `ac-scene`'s `lengths_agree` filter silently swallows.
                let freqs = v["freqs"].as_array().unwrap();
                for key in ["magnitude_db", "phase_deg", "coherence"] {
                    let a = v[key].as_array().unwrap();
                    assert_eq!(a.len(), freqs.len(), "{key} must match freqs length");
                    assert!(
                        a.iter().all(|x| x.as_f64().is_some_and(f64::is_finite)),
                        "{key} carries a non-finite or non-numeric element"
                    );
                }
                // Coherence is a magnitude-squared coherence: outside
                // [0, 1] it is not one, whatever else is true.
                assert!(
                    v["coherence"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .all(|c| (0.0..=1.0).contains(&c.as_f64().unwrap())),
                    "coherence outside [0, 1]"
                );
                got_frame = true;
                break;
            }
            Some(_) => continue,
            None => break,
        }
    }
    assert!(got_frame, "never saw a transfer_stream data frame");

    let _ = c.call(json!({"cmd": "stop"}));
    let done = c
        .wait_for_topic("done", Duration::from_secs(5))
        .expect("no done frame after stop");
    assert_eq!(done["cmd"], json!("transfer_stream"));
}

#[test]
fn transfer_stream_default_level_ok() {
    // `level_dbfs` omitted — the handler's documented default (−10 dBFS
    // when `drive=true`) must be used without a REP error.
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd":          "transfer_stream",
        "meas_channel": 0,
        "ref_channel":  1,
        "drive":        true,
    }));
    assert_eq!(r["ok"], json!(true), "REP rejected default level: {r:?}");
    let _ = c.call(json!({"cmd": "stop"}));
    let _ = c.wait_for_topic("done", Duration::from_secs(5));
}

#[test]
fn transfer_stream_emits_ir_sidecar() {
    // unified.md Phase 4b: transfer_stream worker emits a
    // visualize/ir frame alongside the transfer_stream frame for
    // the same pair on the same tick. Daemon-side IFFT of H₁(ω)
    // into a centred time-domain h(t) downsampled to ≤2000 samples.
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd":          "transfer_stream",
        "meas_channel": 0,
        "ref_channel":  1,
        "drive":        true,
        "level_dbfs":   -12.0,
    }));
    assert_eq!(r["ok"], json!(true), "REP: {r:?}");

    let mut got_ir = false;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "data" && v["type"].as_str() == Some("visualize/ir") => {
                for key in [
                    "samples",
                    "sr",
                    "dt_ms",
                    "t_origin_ms",
                    "ref_channel",
                    "meas_channel",
                ] {
                    assert!(v.get(key).is_some(), "ir frame missing {key}: {v}");
                }
                let samples = v["samples"].as_array().unwrap();
                assert!(!samples.is_empty(), "ir samples must be non-empty");
                assert!(
                    samples.len() <= 2000,
                    "ir samples capped at 2000; got {}",
                    samples.len(),
                );
                let t_origin = v["t_origin_ms"].as_f64().unwrap();
                assert!(
                    t_origin <= 0.0,
                    "t_origin_ms should be ≤ 0 (centred IR); got {t_origin}",
                );
                got_ir = true;
                break;
            }
            Some(_) => continue,
            None => break,
        }
    }
    assert!(got_ir, "never saw a visualize/ir sidecar frame");

    let _ = c.call(json!({"cmd": "stop"}));
    let _ = c.wait_for_topic("done", Duration::from_secs(5));
}

/// handoff-transfer-frame-v2.md M0, AC #1 (presence) + AC #6 (wire economy,
/// measured/printed here for the PR description). Every `transfer_stream`
/// data frame must carry the new fields; `spec_freqs` is identical across
/// frames (deterministic function of a session-fixed sr/f_min/f_max/K).
#[test]
fn transfer_stream_frame_v2_fields_present_and_spec_freqs_stable() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd":          "transfer_stream",
        "meas_channel": 0,
        "ref_channel":  1,
    }));
    assert_eq!(r["ok"], json!(true), "REP: {r:?}");

    let mut frames: Vec<Value> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while frames.len() < 2 && Instant::now() < deadline {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "data" && v["type"].as_str() == Some("transfer_stream") => {
                frames.push(v);
            }
            Some(_) => continue,
            None => break,
        }
    }
    let _ = c.call(json!({"cmd": "stop"}));
    assert!(
        frames.len() >= 2,
        "need >=2 transfer_stream frames, got {}",
        frames.len()
    );

    for key in [
        "spec_freqs",
        "meas_spectrum",
        "ref_spectrum",
        "spl",
        "spl_weighting",
        "spl_integration",
        "cal_tags",
    ] {
        for f in &frames {
            assert!(f.get(key).is_some(), "frame missing {key}: {f}");
        }
    }
    let f0 = &frames[0];
    assert_eq!(f0["spl_weighting"], json!("Z"), "default weighting");
    assert_eq!(f0["spl_integration"], json!("fast"), "default integration");
    assert!(f0["spl"].is_null(), "no cal loaded, spl must be null: {f0}");
    for role in ["meas", "ref"] {
        let tags = &f0["cal_tags"][role];
        assert_eq!(tags["voltage"], json!("none"), "{role} voltage tag");
        assert_eq!(tags["spl"], json!("none"), "{role} spl tag");
        assert_eq!(tags["mic_curve"], json!("none"), "{role} mic_curve tag");
    }

    let sf0 = frames[0]["spec_freqs"].as_array().unwrap();
    let sf1 = frames[1]["spec_freqs"].as_array().unwrap();
    assert_eq!(sf0, sf1, "spec_freqs must be identical across frames");
    assert_eq!(
        sf0.len(),
        f0["meas_spectrum"].as_array().unwrap().len(),
        "spec_freqs / meas_spectrum length mismatch"
    );

    // AC #6: measure and print the actual per-frame wire size so the PR
    // description can state it — both the whole frame (existing + new
    // fields) and the new-fields-only delta (K × f64 × {spec_freqs,
    // meas_spectrum, ref_spectrum} dominate the addition).
    let bytes = serde_json::to_vec(f0).unwrap().len();
    let new_fields_only = json!({
        "spec_freqs":      f0["spec_freqs"],
        "meas_spectrum":   f0["meas_spectrum"],
        "ref_spectrum":    f0["ref_spectrum"],
        "spl":             f0["spl"],
        "spl_weighting":   f0["spl_weighting"],
        "spl_integration": f0["spl_integration"],
        "cal_tags":        f0["cal_tags"],
    });
    let new_bytes = serde_json::to_vec(&new_fields_only).unwrap().len();
    eprintln!(
        "transfer_stream frame v2: K={} columns, {} bytes/frame total (1 pair), \
         {} bytes/frame from the new M0 fields alone",
        sf0.len(),
        bytes,
        new_bytes
    );
}

/// #238: every published frame carries `delay_attempts`, and it is already
/// non-zero on the first one.
///
/// Both halves matter. The field is what lets a consumer separate a refusing
/// pair from a warming-up one — `delay_locked` is false for both — and the
/// fault indicator's two refusal states were unreachable without it. And
/// "non-zero on the first frame" is what makes the refusal clock honest: the
/// daemon publishes nothing until the rings hold a full Welch segment, which
/// is also when the first estimate runs, so a consumer counting from its first
/// frame counts from the first moment a lock was possible — not from session
/// start.
#[test]
fn transfer_stream_reports_delay_attempts_from_the_first_frame() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd":          "transfer_stream",
        "meas_channel": 0,
        "ref_channel":  1,
    }));
    assert_eq!(r["ok"], json!(true), "REP: {r:?}");

    let mut frames: Vec<Value> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while frames.len() < 2 && Instant::now() < deadline {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "data" && v["type"].as_str() == Some("transfer_stream") => {
                frames.push(v);
            }
            Some(_) => continue,
            None => break,
        }
    }
    let _ = c.call(json!({"cmd": "stop"}));
    assert!(
        frames.len() >= 2,
        "need >=2 transfer_stream frames, got {}",
        frames.len()
    );

    for f in &frames {
        let attempts = f["delay_attempts"]
            .as_u64()
            .unwrap_or_else(|| panic!("delay_attempts missing or not an integer: {f}"));
        assert!(
            attempts >= 1,
            "a frame was published before the estimator answered: {f}"
        );
        // The count says the estimator ran; `delay_locked` says what it
        // decided. Neither is inferred from the other.
        assert!(
            f["delay_locked"].is_boolean(),
            "delay_locked missing alongside delay_attempts: {f}"
        );
    }
}

/// AC #2 (amplitude truth): fake channel 0's default stimulus is a 1 kHz
/// sine at 0.1 peak amplitude (audio/fake.rs) = -20 dBFS, exactly bin-
/// aligned (nperseg=sr=48000 ⇒ Δf=1 Hz). `meas_spectrum`'s peak column
/// must land within tolerance of that, at the right frequency.
///
/// Tolerance derivation: at K≈491 (48 cols/octave, 20 Hz-24 kHz) a column
/// near 1 kHz spans ~14 Welch bins — wide enough to include the tone's
/// own Hann-window leakage into its ±1 Hz neighbour bins. A raised-cosine
/// window's DFT is the 3-tap kernel `[-0.25, 0.5, -0.25]`, so an exact-
/// bin tone of "ideal" amplitude X reads `0.5X` at the centre bin and
/// `0.25X` at each neighbour; band-power-summing those three,
/// `sqrt(0.5² + 0.25² + 0.25²) / 0.5 ≈ 1.2247` (+1.76 dB), is real signal
/// energy the aggregator is correctly summing (D18), not an artifact.
/// 2.2 dB clears that bound with margin while still catching a
/// several-dB normalization regression.
#[test]
fn transfer_stream_meas_spectrum_amplitude_truth() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd":          "transfer_stream",
        "meas_channel": 0,
        "ref_channel":  1,
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

    let freqs: Vec<f64> = frame["spec_freqs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect();
    let spectrum: Vec<f64> = frame["meas_spectrum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect();
    let (peak_i, &peak_amp) = spectrum
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
        .expect("non-empty meas_spectrum");
    let peak_hz = freqs[peak_i];
    let peak_dbfs = 20.0 * peak_amp.max(1e-12).log10();

    assert!(
        (peak_hz - 1000.0).abs() < 50.0,
        "peak at {peak_hz} Hz, expected ~1000 Hz"
    );
    assert!(
        (peak_dbfs - -20.0).abs() < 2.2,
        "peak {peak_dbfs} dBFS, expected ~-20 dBFS (fake ch0 default tone) within 2.2 dB"
    );
}

/// Invalid `weighting`/`integration` session params are rejected
/// synchronously before worker spawn, matching the handoff's validation
/// style — including the strict A/C/Z-only contract (no "off", unlike
/// `set_band_weighting`'s 4-way enum). Also checks no partial session is
/// left behind: `status` reports idle right after a rejection, not just
/// "the trailing valid call happened to succeed".
#[test]
fn transfer_stream_rejects_invalid_spl_session_params() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let r = c.call(json!({
        "cmd": "transfer_stream", "meas_channel": 0, "ref_channel": 1, "weighting": "off",
    }));
    assert_eq!(r["ok"], json!(false), "weighting=off must be rejected: {r}");
    let s = c.call(json!({"cmd": "status"}));
    assert_eq!(
        s["busy"],
        json!(false),
        "rejected weighting must leave no partial session running: {s}"
    );

    let r = c.call(json!({
        "cmd": "transfer_stream", "meas_channel": 0, "ref_channel": 1, "weighting": "q",
    }));
    assert_eq!(r["ok"], json!(false), "weighting=q must be rejected: {r}");

    let r = c.call(json!({
        "cmd": "transfer_stream", "meas_channel": 0, "ref_channel": 1, "integration": "leq",
    }));
    assert_eq!(
        r["ok"],
        json!(false),
        "integration=leq not implemented in M0, must be rejected: {r}"
    );

    let r = c.call(json!({
        "cmd": "transfer_stream", "meas_channel": 0, "ref_channel": 1,
        "weighting": "a", "integration": "SLOW",
    }));
    assert_eq!(r["ok"], json!(true), "valid params (case-insensitive): {r}");
    let _ = c.call(json!({"cmd": "stop"}));
    let _ = c.wait_for_topic("done", Duration::from_secs(5));
}

#[test]
fn transfer_stream_refuses_mic_curve_on_reference_channel() {
    // #101 (H): H1 is a ratio. Applying a mic-curve to the reference
    // leg cancels (or worse, biases) the measurement-leg correction.
    // The daemon refuses the request with a clear message instead of
    // silently producing a wrong transfer.
    let d = Daemon::spawn();
    let c = Client::new(&d);

    // Attach a synthetic curve to channel 1 (will be the reference).
    let mut freqs = Vec::new();
    let mut gains = Vec::new();
    let log_min = 100.0_f64.ln();
    let log_max = 10_000.0_f64.ln();
    for i in 0..24 {
        let t = i as f64 / 23.0;
        freqs.push((log_min + t * (log_max - log_min)).exp());
        gains.push(2.0);
    }
    let r = c.call(json!({
        "cmd":           "calibrate_mic_curve",
        "op":            "set",
        "input_channel": 1,
        "freqs_hz":      freqs,
        "gain_db":       gains,
    }));
    assert_eq!(r["ok"], json!(true));

    // Try to start transfer with channel 1 as the reference. Must refuse.
    let r = c.call(json!({
        "cmd":         "transfer_stream",
        "meas_channel": 0,
        "ref_channel":  1,
    }));
    assert_eq!(r["ok"], json!(false), "expected refusal: {r}");
    let err = r["error"].as_str().unwrap_or("");
    assert!(err.contains("ref channel 1"), "error message wrong: {err}");
    assert!(err.contains("mic-curve"), "error message wrong: {err}");
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
