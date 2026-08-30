use serde_json::json;
use serde_json::Value;
use std::time::Duration;
use std::time::Instant;

use crate::common::{Client, Daemon};

#[test]
fn set_monitor_params_rejects_when_idle() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({"cmd":"set_monitor_params","interval":0.1,"fft_n":4096}));
    assert_eq!(r["ok"], json!(false));
    assert_eq!(r["error"], json!("no active monitor"));
}

#[test]
fn set_monitor_params_validates_ranges() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({"cmd":"monitor_spectrum","interval":0.2,"fft_n":8192}));
    assert_eq!(r["ok"], json!(true));

    let r = c.call(json!({"cmd":"set_monitor_params","fft_n":3000}));
    assert_eq!(r["ok"], json!(false));
    assert!(r["error"].as_str().unwrap().contains("power of 2"));

    let r = c.call(json!({"cmd":"set_monitor_params","interval":-1.0}));
    assert_eq!(r["ok"], json!(false));
    assert!(r["error"].as_str().unwrap().contains("interval"));

    let _ = c.call(json!({"cmd":"stop"}));
}

#[test]
fn set_monitor_params_live_updates_running_worker() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({"cmd":"monitor_spectrum","interval":0.2,"fft_n":8192}));
    assert_eq!(r["ok"], json!(true));

    let r = c.call(json!({"cmd":"set_monitor_params","interval":0.1,"fft_n":4096}));
    assert_eq!(r["ok"], json!(true));
    assert_eq!(r["interval"], json!(0.1));
    assert_eq!(r["fft_n"], json!(4096));

    // A partial update leaves the other field unchanged.
    let r = c.call(json!({"cmd":"set_monitor_params","fft_n":16384}));
    assert_eq!(r["ok"], json!(true));
    assert_eq!(r["interval"], json!(0.1));
    assert_eq!(r["fft_n"], json!(16384));

    let _ = c.call(json!({"cmd":"stop"}));
    let done = c
        .wait_for_topic("done", Duration::from_secs(3))
        .expect("no done frame after stop");
    assert_eq!(done["cmd"], json!("monitor_spectrum"));
}

#[test]
fn monitor_spectrum_wire_values_match_fake_tone() {
    // End-to-end value-correctness test: spin up the daemon with the
    // fake-audio backend (deterministic 1 kHz sine + 1% 2nd-harmonic at
    // 0.1 peak on channel 0; see audio/fake.rs), open monitor_spectrum,
    // and assert every numeric field on the wire matches the known
    // signal within published tolerances. Catches regressions in:
    //   - FFT magnitude normalisation (`fundamental_dbfs` ≈ -20 dBFS),
    //   - parabolic peak interpolation (`peaks[0]` within ≤0.4 dB and
    //     ≤1 Hz of (1000.0, -20.0)),
    //   - 2nd-harmonic detection (`peaks` contains 2000 Hz @ ~-60 dBFS),
    //   - cal-offset wiring (`dbu_offset_db`/`spl_offset_db`/`in_dbu`
    //     all null when no cal is loaded for the test channel).
    //
    // If you change the wire schema, the FFT path, or the peak
    // detector, this test is your first line of defence — failing it
    // means the cursor footer can't be trusted.
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let r = c.call(json!({
        "cmd": "monitor_spectrum",
        "channels": [0],
        "interval_ms": 100,
        "fft_n": 8192,
    }));
    assert_eq!(r["ok"], json!(true), "monitor_spectrum ack: {r}");

    // Skip the first frame or two — the FFT ring is still filling and
    // the first analyze() may include partial-window edge artefacts.
    let mut frame: Option<Value> = None;
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut accepted = 0;
    while Instant::now() < deadline {
        let Some((topic, payload)) = c.recv_pub(2_000) else {
            break;
        };
        if topic != "data" {
            continue;
        }
        if payload.get("type").and_then(Value::as_str) != Some("visualize/spectrum") {
            continue;
        }
        if payload
            .get("spectrum")
            .and_then(Value::as_array)
            .is_none_or(|a| a.is_empty())
        {
            continue;
        }
        accepted += 1;
        if accepted >= 2 {
            frame = Some(payload);
            break;
        }
    }
    let _ = c.call(json!({"cmd": "stop"}));
    let frame = frame.expect("no usable spectrum frame within 5 s");

    // ── 1. Wire schema: cal offsets are null when no cal is loaded ──
    assert!(
        frame.get("dbu_offset_db").is_none_or(|v| v.is_null()),
        "dbu_offset_db must be null without cal: {frame}"
    );
    assert!(
        frame.get("spl_offset_db").is_none_or(|v| v.is_null()),
        "spl_offset_db must be null without cal: {frame}"
    );
    assert!(
        frame.get("in_dbu").is_none_or(|v| v.is_null()),
        "in_dbu must be null without cal: {frame}"
    );

    // ── 2. fundamental_dbfs ≈ -20 dBFS (with up to ~1.5 dB Hann scallop) ──
    let fund_dbfs = frame["fundamental_dbfs"]
        .as_f64()
        .expect("fundamental_dbfs");
    assert!(
        (fund_dbfs - (-20.0)).abs() < 1.5,
        "fundamental_dbfs = {fund_dbfs:.3} dBFS, want ~-20.0 (raw bin, scallop ≤1.42 dB)",
    );
    // fundamental_hz must lock onto the actual fake-tone freq within
    // ±20 Hz (the same find-peak window the daemon uses).
    let fund_hz = frame["freq_hz"].as_f64().expect("freq_hz");
    assert!(
        (fund_hz - 1000.0).abs() < 20.0,
        "fundamental_hz = {fund_hz:.2} Hz, want ~1000 Hz",
    );

    // ── 3. peaks[]: parabolic interp recovers the tone within 0.4 dB ──
    let peaks = frame["peaks"].as_array().expect("peaks array");
    assert!(!peaks.is_empty(), "expected at least one detected peak");
    let p0 = peaks[0].as_array().expect("peak entry [freq, db]");
    let p0_hz = p0[0].as_f64().expect("peak freq");
    let p0_dbfs = p0[1].as_f64().expect("peak dbfs");
    assert!(
        (p0_hz - 1000.0).abs() < 1.0,
        "peaks[0] freq = {p0_hz:.3} Hz, want 1000.0 ±1.0",
    );
    assert!(
        (p0_dbfs - (-20.0)).abs() < 0.4,
        "peaks[0] dbfs = {p0_dbfs:.3} dBFS, want -20.0 ±0.4 (parabolic interp)",
    );

    // ── 4. 2nd harmonic at 2000 Hz, ~-60 dBFS (1% of fundamental amp) ──
    let h2 = peaks
        .iter()
        .filter_map(|v| v.as_array())
        .find(|p| {
            let f = p[0].as_f64().unwrap_or(0.0);
            (f - 2000.0).abs() < 2.0
        })
        .expect("2nd harmonic peak at ~2000 Hz");
    let h2_dbfs = h2[1].as_f64().unwrap();
    assert!(
        (h2_dbfs - (-60.0)).abs() < 1.0,
        "2nd-harmonic dbfs = {h2_dbfs:.3} dBFS, want ~-60 ±1.0",
    );
}

#[test]
fn monitor_spectrum_fake_tones_produce_two_distinct_peaks() {
    // #170 display-truth harness: `fake_tones` must actually reach the
    // fake engine (via `dbfs_to_amplitude` + `set_tone_pair` in
    // handlers/audio/monitor.rs) and produce two independently-detectable
    // spectral peaks at their requested levels — the I1/I3 stimulus this
    // harness needs. Frequencies chosen well clear of each other and of
    // the LF/HF crossover so both land cleanly in one FFT.
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let r = c.call(json!({
        "cmd": "monitor_spectrum",
        "channels": [0],
        "interval_ms": 100,
        "fft_n": 8192,
        "fake_tones": [
            {"freq_hz": 2000.0, "level_dbfs": -6.0},
            {"freq_hz": 9000.0, "level_dbfs": -24.0},
        ],
    }));
    assert_eq!(r["ok"], json!(true), "monitor_spectrum ack: {r}");

    let mut frame: Option<Value> = None;
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut accepted = 0;
    while Instant::now() < deadline {
        let Some((topic, payload)) = c.recv_pub(2_000) else {
            break;
        };
        if topic != "data"
            || payload.get("type").and_then(Value::as_str) != Some("visualize/spectrum")
        {
            continue;
        }
        if payload
            .get("spectrum")
            .and_then(Value::as_array)
            .is_none_or(|a| a.is_empty())
        {
            continue;
        }
        accepted += 1;
        if accepted >= 2 {
            frame = Some(payload);
            break;
        }
    }
    let _ = c.call(json!({"cmd": "stop"}));
    let frame = frame.expect("no usable spectrum frame within 5 s");

    let peaks = frame["peaks"].as_array().expect("peaks array");
    let find = |target_hz: f64| {
        peaks
            .iter()
            .filter_map(|v| v.as_array())
            .find(|p| (p[0].as_f64().unwrap_or(0.0) - target_hz).abs() < 5.0)
            .map(|p| p[1].as_f64().unwrap())
    };
    let p1 = find(2000.0).expect("peak near 2000 Hz");
    let p2 = find(9000.0).expect("peak near 9000 Hz");
    assert!(
        (p1 - (-6.0)).abs() < 1.5,
        "2000 Hz peak = {p1:.2} dBFS, want ~-6.0"
    );
    assert!(
        (p2 - (-24.0)).abs() < 1.5,
        "9000 Hz peak = {p2:.2} dBFS, want ~-24.0"
    );
    assert!(
        p1 > p2,
        "louder tone (-6 dBFS) must measure above quieter tone (-24 dBFS)"
    );
}

#[test]
fn monitor_spectrum_fake_noise_stays_bounded() {
    // #170 I4 (bounded output): calibrated broadband noise stimulus must
    // never produce a post-receiver value above 0 dBFS.
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let r = c.call(json!({
        "cmd": "monitor_spectrum",
        "channels": [0],
        "interval_ms": 100,
        "fft_n": 8192,
        "fake_noise_dbfs": -20.0,
    }));
    assert_eq!(r["ok"], json!(true), "monitor_spectrum ack: {r}");

    let mut frame: Option<Value> = None;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let Some((topic, payload)) = c.recv_pub(2_000) else {
            break;
        };
        if topic != "data"
            || payload.get("type").and_then(Value::as_str) != Some("visualize/spectrum")
        {
            continue;
        }
        if let Some(spec) = payload.get("spectrum").and_then(Value::as_array) {
            if !spec.is_empty() {
                frame = Some(payload);
                break;
            }
        }
    }
    let _ = c.call(json!({"cmd": "stop"}));
    let frame = frame.expect("no usable spectrum frame within 5 s");
    let spec = frame["spectrum"].as_array().expect("spectrum array");
    let max = spec
        .iter()
        .filter_map(Value::as_f64)
        .fold(f64::MIN, f64::max);
    // Tolerance rationale: -20 dBFS peak-amplitude noise is 20 dB clear of
    // 0 dBFS; a single FFT bin can still read a few hundredths of a dB
    // above nominal from window-leakage constructive summation of random
    // phase, so 1.0 dB catches a real gain/clamping bug (which produces
    // multi-dB or +19 dB-class violations, see fixtures-spectrum-hf-garbage)
    // without flagging that benign noise floor.
    assert!(
        max <= 1.0,
        "noise stimulus produced a value above 0 dBFS + tolerance: max={max}"
    );
}

#[test]
fn monitor_spectrum_emits_scope_frames() {
    // unified.md Phase 0b: the daemon must emit a `visualize/scope`
    // sidecar frame per channel per tick alongside the spectrum frame.
    // Both channels of one tick share the same `frame_idx` so the UI
    // can pair L+R for the Goniometer view. Asserting on:
    //   - frames arrive at all (regression catch if the emit is removed)
    //   - non-empty f32 samples in [-1, 1]
    //   - capped at SCOPE_MAX_SAMPLES = 2048
    //   - both channels of a tick share frame_idx within 0 (strict)
    //   - successive tick frame_idx values are monotonic (allowing for
    //     channel interleaving so a single channel sees +1 / +2 jumps).
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let r = c.call(json!({
        "cmd":         "monitor_spectrum",
        "channels":    [0, 1],
        "interval_ms": 100,
        "fft_n":       8192,
    }));
    assert_eq!(r["ok"], json!(true), "monitor_spectrum ack: {r}");

    // Collect scope frames for ~3 s — that's ~30 ticks at 100 ms, more
    // than enough to see several L+R pairs and detect missing emits.
    let mut frames_by_idx: std::collections::HashMap<u64, Vec<Value>> =
        std::collections::HashMap::new();
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        if remaining <= 0 {
            break;
        }
        let Some((topic, payload)) = c.recv_pub(remaining.max(1)) else {
            break;
        };
        if topic != "data" {
            continue;
        }
        if payload.get("type").and_then(Value::as_str) != Some("visualize/scope") {
            continue;
        }
        let frame_idx = payload["frame_idx"].as_u64().expect("frame_idx u64");
        frames_by_idx.entry(frame_idx).or_default().push(payload);
    }
    let _ = c.call(json!({"cmd": "stop"}));

    assert!(
        !frames_by_idx.is_empty(),
        "expected visualize/scope frames; got none in 3 s",
    );

    // Every observed frame must carry samples in [-1, 1] and ≤2048 long.
    for frames in frames_by_idx.values() {
        for f in frames {
            let samples = f["samples"].as_array().expect("samples array");
            assert!(!samples.is_empty(), "samples must be non-empty: {f}");
            assert!(
                samples.len() <= 2048,
                "samples capped at 2048; got {} (frame: {f})",
                samples.len(),
            );
            for s in samples {
                let v = s.as_f64().expect("f64 sample");
                assert!(
                    (-1.000_001..=1.000_001).contains(&v),
                    "sample out of [-1,1]: {v} (frame: {f})",
                );
            }
        }
    }

    // At least one tick must contain both channel 0 AND channel 1 with
    // the SAME frame_idx — that's the L+R pairing the UI relies on.
    let mut paired_ticks = 0;
    for frames in frames_by_idx.values() {
        let mut chans: Vec<u64> = frames
            .iter()
            .filter_map(|f| f["channel"].as_u64())
            .collect();
        chans.sort();
        chans.dedup();
        if chans.len() >= 2 && chans.contains(&0) && chans.contains(&1) {
            paired_ticks += 1;
        }
    }
    assert!(
        paired_ticks >= 3,
        "expected ≥3 ticks with both ch 0 and ch 1 sharing frame_idx; got {paired_ticks}",
    );

    // Tick counter must be monotonic (per-tick increment, not per-channel).
    let mut idxs: Vec<u64> = frames_by_idx.keys().copied().collect();
    idxs.sort();
    let mut prev = idxs[0];
    for &i in &idxs[1..] {
        assert!(
            i >= prev,
            "frame_idx must be monotonic: saw {prev} then {i}",
        );
        prev = i;
    }
}

#[test]
fn monitor_cqt_emits_visualize_cqt_frame() {
    // End-to-end smoke: set analysis mode → cqt, fire monitor_spectrum, and
    // confirm the daemon publishes `visualize/cqt` frames with the expected
    // payload shape (log-spaced freqs, magnitudes one-per-bin).
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let r = c.call(json!({"cmd": "set_analysis_mode", "mode": "cqt"}));
    assert_eq!(r["ok"], json!(true), "set_analysis_mode cqt: {r}");

    let r = c.call(json!({"cmd": "monitor_spectrum", "freq_hz": 1000.0}));
    assert_eq!(r["ok"], json!(true));

    // The CQT branch waits for the ring to fill (1 s @ 48 kHz), then emits
    // ~50 frames per second. Give it up to 5 s to produce one.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut frame: Option<Value> = None;
    while Instant::now() < deadline {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "data" && v["type"] == json!("visualize/cqt") => {
                frame = Some(v);
                break;
            }
            Some(_) => continue,
            None => break,
        }
    }
    let _ = c.call(json!({"cmd": "stop"}));
    let frame = frame.expect("no visualize/cqt frame within 5 s");

    let mags = frame["magnitudes"].as_array().expect("magnitudes array");
    let freqs = frame["frequencies"].as_array().expect("frequencies array");
    assert_eq!(
        mags.len(),
        freqs.len(),
        "magnitudes/frequencies length mismatch"
    );
    assert!(!mags.is_empty(), "empty cqt column");
    // Geometric spacing: f[k+1] / f[k] should be constant (= 2^(1/bpo)).
    let f0 = freqs[0].as_f64().unwrap();
    let f1 = freqs[1].as_f64().unwrap();
    let f_last = freqs[freqs.len() - 1].as_f64().unwrap();
    let ratio = f1 / f0;
    let bpo = frame["bpo"].as_u64().unwrap() as f64;
    let expected_ratio = 2.0_f64.powf(1.0 / bpo);
    assert!(
        (ratio - expected_ratio).abs() < 1e-3,
        "freq ratio {ratio} (bpo={bpo}, expected {expected_ratio})"
    );
    assert!(f_last > f0, "freqs not monotonically increasing");
}

#[test]
fn monitor_reassigned_emits_visualize_reassigned_frame() {
    // Symmetric to the cqt smoke test: switch to reassigned mode, drive
    // monitor_spectrum, confirm frame shape on the wire.
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let r = c.call(json!({"cmd": "set_analysis_mode", "mode": "reassigned"}));
    assert_eq!(r["ok"], json!(true), "set_analysis_mode reassigned: {r}");

    let r = c.call(json!({"cmd": "monitor_spectrum", "freq_hz": 1000.0}));
    assert_eq!(r["ok"], json!(true));

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut frame: Option<Value> = None;
    while Instant::now() < deadline {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "data" && v["type"] == json!("visualize/reassigned") => {
                frame = Some(v);
                break;
            }
            Some(_) => continue,
            None => break,
        }
    }
    let _ = c.call(json!({"cmd": "stop"}));
    let frame = frame.expect("no visualize/reassigned frame within 5 s");

    let mags = frame["magnitudes"].as_array().expect("magnitudes array");
    let freqs = frame["frequencies"].as_array().expect("frequencies array");
    assert_eq!(
        mags.len(),
        freqs.len(),
        "magnitudes/frequencies length mismatch"
    );
    assert!(
        mags.len() >= 256,
        "reassigned column suspiciously short: {}",
        mags.len()
    );
    let f0 = freqs[0].as_f64().unwrap();
    let f_last = freqs[freqs.len() - 1].as_f64().unwrap();
    assert!(
        f_last > f0 * 100.0,
        "freqs span less than 2 decades: {f0}..{f_last}"
    );
}
