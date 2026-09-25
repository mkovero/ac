use serde_json::json;
use serde_json::Value;
use std::time::Duration;
use std::time::Instant;

use crate::common::{Client, Daemon};

/// Cadence (seconds) the value-correctness monitor tests request. The
/// send and the readback in [`assert_monitor_interval`] share this so
/// they cannot drift apart.
const MONITOR_TEST_INTERVAL_S: f64 = 0.1;

/// Read back the running monitor's stored parameters and assert the
/// `interval` the test sent was honoured. The request deliberately
/// carries no fields: with none present `set_monitor_params` changes
/// nothing and echoes what `monitor_spectrum` parsed, so a dropped or
/// renamed request field shows up here as the 0.2 s default. Sending
/// `interval` here would store the value itself and make the check
/// unable to fail.
fn assert_monitor_interval(c: &Client) {
    let r = c.call(json!({"cmd": "set_monitor_params"}));
    assert_eq!(r["ok"], json!(true), "set_monitor_params readback: {r}");
    assert_eq!(
        r["interval"],
        json!(MONITOR_TEST_INTERVAL_S),
        "monitor_spectrum did not honour the requested interval: {r}"
    );
}

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
        "interval": MONITOR_TEST_INTERVAL_S,
        "fft_n": 8192,
    }));
    assert_eq!(r["ok"], json!(true), "monitor_spectrum ack: {r}");
    assert_monitor_interval(&c);

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
    assert_eq!(frame["backend"], json!("fake"));

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
    // fake engine (via `dbfs_to_amplitude` + `set_external_tones` in
    // handlers/audio/monitor/mod.rs) and produce two independently-detectable
    // spectral peaks at their requested levels — the I1/I3 stimulus this
    // harness needs. Frequencies chosen well clear of each other and of
    // the LF/HF crossover so both land cleanly in one FFT.
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let r = c.call(json!({
        "cmd": "monitor_spectrum",
        "channels": [0],
        "interval": MONITOR_TEST_INTERVAL_S,
        "fft_n": 8192,
        "fake_tones": [
            {"freq_hz": 2000.0, "level_dbfs": -6.0},
            {"freq_hz": 9000.0, "level_dbfs": -24.0},
        ],
    }));
    assert_eq!(r["ok"], json!(true), "monitor_spectrum ack: {r}");
    assert_monitor_interval(&c);

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
        "interval": MONITOR_TEST_INTERVAL_S,
        "fft_n": 8192,
        "fake_noise_dbfs": -20.0,
    }));
    assert_eq!(r["ok"], json!(true), "monitor_spectrum ack: {r}");
    assert_monitor_interval(&c);

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
    // The daemon emits a `visualize/scope` sidecar frame per channel
    // capture. Multi-channel monitor captures channels one after another
    // (reconnect, flush, block capture), so scope frames must NOT carry a
    // shared identity or timestamp that would let a consumer pair them as
    // simultaneous (#434). Asserting on:
    //   - frames arrive at all (regression catch if the emit is removed)
    //   - non-empty f32 samples in [-1, 1], capped at SCOPE_MAX_SAMPLES
    //   - every frame declares `capture_mode: "sequential"`
    //   - `frame_idx` strictly increases in emission order, so no two
    //     frames (in particular ch 0 and ch 1 of one round) share it
    //   - when the channel changes between consecutive frames, the
    //     timestamps are at least half a per-channel capture apart. The
    //     fake engine's block capture sleeps for its full duration
    //     (interval / n_channels = 50 ms here), so a sequential capture
    //     puts ≥ 50 ms between the two channels' completion times, while
    //     a simultaneous (or tick-wide) timestamp puts 0 there.
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let r = c.call(json!({
        "cmd":         "monitor_spectrum",
        "channels":    [0, 1],
        "interval":    MONITOR_TEST_INTERVAL_S,
        "fft_n":       8192,
    }));
    assert_eq!(r["ok"], json!(true), "monitor_spectrum ack: {r}");
    assert_monitor_interval(&c);

    // Collect scope frames for ~3 s, in the order the daemon's single
    // PUB socket delivered them — that is the emission order.
    let mut frames: Vec<Value> = Vec::new();
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
        frames.push(payload);
    }
    let _ = c.call(json!({"cmd": "stop"}));

    assert!(
        frames.len() >= 6,
        "expected ≥6 visualize/scope frames in 3 s; got {}",
        frames.len(),
    );

    for f in &frames {
        assert_eq!(
            f["capture_mode"],
            json!("sequential"),
            "scope frame must declare sequential capture: {f}",
        );
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

    let mut chans: Vec<u64> = frames
        .iter()
        .map(|f| f["channel"].as_u64().expect("channel u64"))
        .collect();
    chans.sort();
    chans.dedup();
    assert_eq!(chans, vec![0, 1], "expected frames from both channels");

    const MIN_CHANNEL_GAP_NS: u64 = 25_000_000;
    let mut channel_switches = 0;
    for pair in frames.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        let (ia, ib) = (
            a["frame_idx"].as_u64().expect("frame_idx u64"),
            b["frame_idx"].as_u64().expect("frame_idx u64"),
        );
        assert!(
            ib > ia,
            "frame_idx must be unique and increasing per emitted frame: \
             saw {ia} (ch {}) then {ib} (ch {})",
            a["channel"],
            b["channel"],
        );
        let (ta, tb) = (
            a["timestamp"].as_u64().expect("timestamp u64"),
            b["timestamp"].as_u64().expect("timestamp u64"),
        );
        if a["channel"] != b["channel"] {
            channel_switches += 1;
            assert!(
                tb >= ta + MIN_CHANNEL_GAP_NS,
                "sequential captures must carry their own completion times: \
                 ch {} at {ta} then ch {} at {tb} (gap {} ns < {MIN_CHANNEL_GAP_NS})",
                a["channel"],
                b["channel"],
                tb.saturating_sub(ta),
            );
        }
    }
    assert!(
        channel_switches >= 3,
        "expected ≥3 channel switches in emission order; got {channel_switches}",
    );
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

fn assert_monitor_refused_idle(c: &Client, channels: Value, want: &str) {
    let r = c.call(json!({"cmd": "monitor_spectrum", "channels": channels}));
    assert_eq!(r["ok"], json!(false), "must be refused: {r}");
    assert_eq!(r["error"].as_str().unwrap_or_default(), want);
    let s = c.call(json!({"cmd": "status"}));
    assert_eq!(s["ok"], json!(true), "daemon must still answer: {s}");
    assert_eq!(s["busy"], json!(false), "no worker may start: {s}");
}

/// #635: `channels` holds at most 64 entries, none repeated. Checked on
/// the list as sent, before any port lookup, so length beats repeats.
#[test]
fn monitor_spectrum_bounds_channel_list() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    // The ceiling itself is not refused by the bound: 64 distinct entries
    // pass it and fail later, on the fake backend's 20 capture ports.
    let at: Vec<u32> = (0..64).collect();
    let r = c.call(json!({"cmd": "monitor_spectrum", "channels": at}));
    assert_eq!(r["ok"], json!(false), "{r}");
    let err = r["error"].as_str().unwrap_or_default();
    assert!(
        !err.contains("at most") && err.contains("channel 20 is out of range"),
        "64 entries must pass the bound and reach port resolution: {err:?}"
    );

    // #640: the echo gets the 61 columns left after the indent and the
    // 8-wide label column, `…` included, and is cut after the last
    // complete element: `[0,1,…,22,` is 60 characters.
    let over: Vec<u32> = (0..65).collect();
    let echo = format!("[{}", (0..=22).map(|i| format!("{i},")).collect::<String>());
    assert_eq!(echo.chars().count(), 60);
    assert_monitor_refused_idle(
        &c,
        json!(over),
        &format!(
            "monitor not started \u{2014} channels must list at most 64 entries\n\
             \x20        received  {echo}\u{2026}\n\
             \x20        entries   65"
        ),
    );

    // Overflow scale: 100 000 zeros are refused for length, not repeats.
    assert_monitor_refused_idle(
        &c,
        json!(vec![0u32; 100_000]),
        &format!(
            "monitor not started \u{2014} channels must list at most 64 entries\n\
             \x20        received  [{}\u{2026}\n\
             \x20        entries   100000",
            "0,".repeat(29)
        ),
    );

    // Duplicate-only.
    assert_monitor_refused_idle(
        &c,
        json!([0, 0]),
        "monitor not started \u{2014} channels[1] repeats channels[0]\n\
         \x20        received  [0,0]\n\
         \x20        channel   0",
    );
    assert_monitor_refused_idle(
        &c,
        json!([0, 1, 0]),
        "monitor not started \u{2014} channels[2] repeats channels[0]\n\
         \x20        received  [0,1,0]\n\
         \x20        channel   0",
    );

    // Distinct channels in range still start.
    let r = c.call(json!({"cmd": "monitor_spectrum", "channels": [0, 1]}));
    assert_eq!(r["ok"], json!(true), "{r}");
    let _ = c.call(json!({"cmd": "stop"}));
}

// ---- key-set characterisation (#112 D4.1) ----
//
// The exact key set of each monitor frame `ac_core::wire` types, as the
// daemon publishes it — `wire_version` included, the one key the move onto
// those types added, stamped at the publish seam. The spectrum frame has two shapes: the THD branch
// carries the tone readouts, and the branch with no resolvable fundamental
// omits them entirely — absent, not null. The two are characterised apart
// because turning an absent key into a present `null` is a wire change a
// lenient consumer never notices.

/// Every key path in `v`: `a`, `a.b`, and `a[].b` for objects inside arrays.
pub(crate) fn key_paths(v: &Value) -> std::collections::BTreeSet<String> {
    fn walk(v: &Value, prefix: &str, out: &mut std::collections::BTreeSet<String>) {
        match v {
            Value::Object(m) => {
                for (k, child) in m {
                    let path = if prefix.is_empty() {
                        k.clone()
                    } else {
                        format!("{prefix}.{k}")
                    };
                    out.insert(path.clone());
                    walk(child, &path, out);
                }
            }
            Value::Array(a) => {
                for child in a {
                    walk(child, &format!("{prefix}[]"), out);
                }
            }
            _ => {}
        }
    }
    let mut out = std::collections::BTreeSet::new();
    walk(v, "", &mut out);
    out
}

fn key_set(list: &[&str]) -> std::collections::BTreeSet<String> {
    list.iter().map(|s| s.to_string()).collect()
}

/// Keys on every `visualize/spectrum` frame, whichever branch built it.
const SPECTRUM_ENVELOPE_KEYS: &[&str] = &[
    "backend",
    "channel",
    "cmd",
    "dbu_offset_db",
    "freqs",
    "mic_correction",
    "n_channels",
    "spectrum",
    "spl_offset_db",
    "sr",
    "type",
    "voltage_check",
    "wire_version",
    "xruns",
];

/// Keys only the THD branch adds.
const SPECTRUM_THD_KEYS: &[&str] = &[
    "clipping",
    "freq_hz",
    "fundamental_dbfs",
    "in_dbu",
    "peaks",
    "thd_pct",
    "thdn_pct",
];

const LOUDNESS_KEYS: &[&str] = &[
    "backend",
    "channel",
    "cmd",
    "gated_duration_s",
    "integrated_lkfs",
    "lra_lu",
    "mic_correction",
    "momentary_lkfs",
    "n_channels",
    "short_term_lkfs",
    "spl_offset_db",
    "sr",
    "timestamp",
    "true_peak_dbtp",
    "type",
    "wire_version",
    "xruns",
];

/// Start a one-channel monitor with `extra` request fields and return the
/// second `visualize/spectrum` frame and a `measurement/loudness` frame.
pub(crate) fn capture_monitor_frames(extra: Value) -> (Value, Value) {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let mut req = json!({
        "cmd": "monitor_spectrum",
        "channels": [0],
        "interval": MONITOR_TEST_INTERVAL_S,
        "fft_n": 8192,
    });
    if let (Some(obj), Some(more)) = (req.as_object_mut(), extra.as_object()) {
        for (k, v) in more {
            obj.insert(k.clone(), v.clone());
        }
    }
    let r = c.call(req);
    assert_eq!(r["ok"], json!(true), "monitor_spectrum ack: {r}");

    let mut spectrum: Option<Value> = None;
    let mut loudness: Option<Value> = None;
    let mut spectra_seen = 0;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && (spectrum.is_none() || loudness.is_none()) {
        let Some((topic, payload)) = c.recv_pub(2_000) else {
            break;
        };
        if topic != "data" {
            continue;
        }
        match payload.get("type").and_then(Value::as_str) {
            Some("visualize/spectrum") => {
                spectra_seen += 1;
                if spectra_seen >= 2 {
                    spectrum = Some(payload);
                }
            }
            Some("measurement/loudness") => loudness = Some(payload),
            _ => {}
        }
    }
    let _ = c.call(json!({"cmd": "stop"}));
    (
        spectrum.expect("no spectrum frame within 5 s"),
        loudness.expect("no loudness frame within 5 s"),
    )
}

#[test]
fn monitor_frame_key_sets_are_characterised() {
    // The fake engine's default 1 kHz tone: THD analysis succeeds.
    let (spectrum, loudness) = capture_monitor_frames(json!({}));
    let mut want = key_set(SPECTRUM_ENVELOPE_KEYS);
    want.extend(key_set(SPECTRUM_THD_KEYS));
    assert_eq!(key_paths(&spectrum), want, "THD branch: {spectrum}");
    assert_eq!(key_paths(&loudness), key_set(LOUDNESS_KEYS), "{loudness}");

    // A tone far below the analyser's "No signal" floor: no fundamental,
    // so the tone readouts must be absent, not null.
    let (spectrum, _) = capture_monitor_frames(json!({
        "fake_tones": [{"freq_hz": 1000.0, "level_dbfs": -300.0}],
    }));
    assert_eq!(
        key_paths(&spectrum),
        key_set(SPECTRUM_ENVELOPE_KEYS),
        "no-THD branch: {spectrum}"
    );
}

/// `to_value(from_value::<T>(v)) == v`: the shared type names every key the
/// daemon published and changes no value's type or formatting on the way
/// through (#112 D4.2/D4.4). A key the type forgot is dropped, so it shows
/// here as a difference.
///
/// `f32_keys` names top-level arrays the type holds as `f32`. Those compare
/// at `f32` precision: serde_json's default float parser is not correctly
/// rounded (no `float_roundtrip` feature), so the `f64` it reads for an
/// `f32`-origin number can sit an ULP off the exact widening the type
/// re-serialises. The daemon's bytes are unchanged; only this side's parse
/// of them is inexact. Integer-versus-float formatting is still compared
/// exactly.
pub(crate) fn assert_lossless<T>(v: &Value, f32_keys: &[&str])
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let typed: T = serde_json::from_value(v.clone())
        .unwrap_or_else(|e| panic!("does not parse as the shared type: {e}\n{v}"));
    let back = serde_json::to_value(&typed).expect("serialise");
    let (Some(want), Some(got)) = (v.as_object(), back.as_object()) else {
        panic!("frame is not an object: {v}");
    };
    let same = |k: &str, a: Option<&Value>, b: Option<&Value>| -> bool {
        match (a, b) {
            (Some(Value::Array(a)), Some(Value::Array(b))) if f32_keys.contains(&k) => {
                a.len() == b.len()
                    && a.iter().zip(b).all(|(x, y)| {
                        x.is_f64() == y.is_f64()
                            && match (x.as_f64(), y.as_f64()) {
                                (Some(p), Some(q)) => p as f32 == q as f32,
                                _ => x == y,
                            }
                    })
            }
            _ => a == b,
        }
    };
    let mut changed: Vec<&String> = want
        .keys()
        .chain(got.keys())
        .filter(|k| !same(k, want.get(*k), got.get(*k)))
        .collect();
    changed.dedup();
    assert!(
        changed.is_empty(),
        "round trip through the shared type changed keys {changed:?} of a `{}` frame",
        v["type"]
    );
}

#[test]
fn live_monitor_frames_round_trip_through_the_shared_types() {
    use ac_core::wire::{LoudnessFrame, SpectrumFrame, WIRE_VERSION};

    for extra in [
        json!({}),
        json!({"fake_tones": [{"freq_hz": 1000.0, "level_dbfs": -300.0}]}),
    ] {
        let (spectrum, loudness) = capture_monitor_frames(extra);
        assert_eq!(spectrum["wire_version"], json!(WIRE_VERSION), "{spectrum}");
        assert_eq!(loudness["wire_version"], json!(WIRE_VERSION), "{loudness}");
        assert_lossless::<SpectrumFrame>(&spectrum, &[]);
        assert_lossless::<LoudnessFrame>(&loudness, &[]);
    }
}
