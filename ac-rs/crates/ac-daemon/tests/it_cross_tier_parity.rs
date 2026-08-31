//! Cross-tier numeric parity tests — the systemic gate that protects
//! the ARCHITECTURE.md promise that calibration applies *identically*
//! across Tier 1 and Tier 2 paths. See [#99](
//! https://github.com/mkovero/ac/issues/99).
//!
//! Coverage:
//!
//! - **Envelope parity** — every analysis path (`plot`, `monitor` ×
//!   `{fft, cwt, cqt, reassigned}`) emits the same processing-context
//!   envelope (`mic_correction`, `spl_offset_db`, …) for the same
//!   channel + cal state. If a future change forgets to stamp the
//!   envelope on a new frame, this test catches it.
//!
//! - **Plot numeric parity** — `plot`'s `fundamental_dbfs` reflects
//!   the mic-curve correction by exactly the curve's value at the
//!   measurement frequency. With a +3 dB curve at 1 kHz, the
//!   reported fundamental is 3 dB lower than the uncorrected case
//!   (within the analysis window's bin-leakage tolerance).
//!
//! Tier 2 absolute-amplitude parity across techniques is intentionally
//! *not* asserted: Morlet wavelets, Q-invariant kernels, and
//! reassigned STFT each have different bin-leakage envelopes by
//! design. Verifying technique-internal calibration plumbing is what
//! makes the cross-tier promise real.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

#[path = "common/mod.rs"]
mod common;

use common::{Client, Daemon};

/// Build a synthetic mic-curve with a constant `peak_db` gain across
/// 24 log-spaced points from 100 Hz to 10 kHz. Constant gain (rather
/// than a peak-at-frequency Gaussian) keeps the log-linear interp
/// inside `Calibration::correction_at` exact at every test frequency
/// — useful for numeric parity assertions where 0.1 dB matters.
fn synthetic_curve_flat(peak_db: f64) -> (Vec<f64>, Vec<f64>) {
    let mut freqs = Vec::with_capacity(24);
    let mut gains = Vec::with_capacity(24);
    let log_min = 100.0_f64.ln();
    let log_max = 10_000.0_f64.ln();
    for i in 0..24 {
        let t = i as f64 / 23.0;
        let log_f = log_min + t * (log_max - log_min);
        freqs.push(log_f.exp());
        gains.push(peak_db);
    }
    (freqs, gains)
}

/// Fire `set_analysis_mode` and `monitor_spectrum`, wait for the first
/// frame whose `type` matches `expected_type`, then `stop`. Returns
/// the frame payload — caller asserts envelope / amplitude.
fn capture_one_monitor_frame(
    c: &Client,
    mode: &str,
    expected_type: &str,
    timeout_secs: u64,
) -> Value {
    let r = c.call(json!({"cmd": "set_analysis_mode", "mode": mode}));
    assert_eq!(r["ok"], json!(true), "set_analysis_mode {mode}: {r}");
    let r = c.call(json!({"cmd": "monitor_spectrum", "freq_hz": 1000.0}));
    assert_eq!(r["ok"], json!(true), "monitor_spectrum start: {r}");

    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let mut frame: Option<Value> = None;
    while Instant::now() < deadline {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "data" && v["type"] == json!(expected_type) => {
                frame = Some(v);
                break;
            }
            Some(_) => continue,
            None => break,
        }
    }
    let _ = c.call(json!({"cmd": "stop"}));
    // Drain the inevitable trailing frames so the next caller starts clean.
    let drain_deadline = Instant::now() + Duration::from_millis(300);
    while Instant::now() < drain_deadline {
        if c.recv_pub(50).is_none() {
            break;
        }
    }
    frame.unwrap_or_else(|| panic!("no {expected_type} frame within {timeout_secs} s"))
}

fn assert_envelope(frame: &Value, expected_mc: &str, expect_spl: bool, label: &str) {
    assert_eq!(
        frame["mic_correction"],
        json!(expected_mc),
        "[{label}] wrong mic_correction tag: {frame}"
    );
    if expect_spl {
        assert!(
            frame["spl_offset_db"].is_f64(),
            "[{label}] spl_offset_db missing or wrong type: {frame}"
        );
    } else {
        assert!(
            frame["spl_offset_db"].is_null() || frame["spl_offset_db"].is_f64(),
            "[{label}] spl_offset_db has unexpected type: {frame}"
        );
    }
}

/// Drive a tiny `plot` and return the first per-point frame.
fn capture_plot_point(c: &Client, freq_hz: f64) -> Value {
    let r = c.call(json!({
        "cmd":        "plot",
        "start_hz":   freq_hz,
        "stop_hz":    freq_hz,
        "level_dbfs": -20.0,
        "ppd":        1,
        "duration":   0.1,
    }));
    assert_eq!(r["ok"], json!(true));
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut frame: Option<Value> = None;
    while Instant::now() < deadline {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v))
                if t == "data" && v["type"] == json!("measurement/frequency_response/point") =>
            {
                frame = Some(v);
                break;
            }
            Some(_) => continue,
            None => break,
        }
    }
    // Drain to `done`.
    let drain_deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < drain_deadline {
        match c.recv_pub(50) {
            Some((t, _)) if t == "done" => break,
            Some(_) => continue,
            None => break,
        }
    }
    frame.expect("no frequency_response/point frame")
}

#[test]
fn parity_envelope_present_on_all_paths_uncalibrated() {
    // Baseline: no calibration loaded. Every analysis path must emit
    // `mic_correction: "none"` and a null `spl_offset_db`.
    let d = Daemon::spawn();
    let c = Client::new(&d);

    // Tier 1: plot.
    let pf = capture_plot_point(&c, 1000.0);
    assert_envelope(&pf, "none", false, "plot");
    assert!(pf["spl_offset_db"].is_null(), "plot spl: {pf}");

    // Tier 2: each mode.
    let cases = [
        ("fft", "visualize/spectrum", 3),
        ("cwt", "visualize/cwt", 3),
        ("cqt", "visualize/cqt", 5), // 1 s ring
        ("reassigned", "visualize/reassigned", 3),
    ];
    for (mode, ty, secs) in cases {
        let f = capture_one_monitor_frame(&c, mode, ty, secs);
        assert_envelope(&f, "none", false, mode);
        assert!(f["spl_offset_db"].is_null(), "{mode} spl: {f}");
    }
}

#[test]
fn parity_envelope_consistent_with_spl_and_mic_cal() {
    // SPL + mic-curve loaded: every path must report `mic_correction:
    // "on"` and a non-null numeric `spl_offset_db`.
    let d = Daemon::spawn();
    let c = Client::new(&d);

    // SPL cal — fake captures −20 dBFS, so spl_offset_db = 94 - (−20) = 114.
    let r = c.call(json!({"cmd": "calibrate_spl", "input_channel": 0, "capture_s": 0.05}));
    assert_eq!(r["ok"], json!(true));
    let _ = c
        .wait_for_topic("cal_prompt", Duration::from_secs(3))
        .expect("cal_prompt");
    let _ = c.call(json!({"cmd": "cal_reply", "vrms": Value::Null}));
    let _ = c
        .wait_for_topic("cal_done", Duration::from_secs(5))
        .expect("cal_done");

    let (freqs, gains) = synthetic_curve_flat(3.0);
    let r = c.call(json!({
        "cmd":           "calibrate_mic_curve",
        "op":            "set",
        "input_channel": 0,
        "freqs_hz":      freqs,
        "gain_db":       gains,
    }));
    assert_eq!(r["ok"], json!(true));

    // Drain anything that snuck in.
    while c.recv_pub(50).is_some() {}

    // Tier 1.
    let pf = capture_plot_point(&c, 1000.0);
    assert_envelope(&pf, "on", true, "plot");
    let plot_offset = pf["spl_offset_db"]
        .as_f64()
        .expect("plot spl_offset_db f64");
    assert!(
        (plot_offset - 114.0).abs() < 5.0,
        "plot spl_offset_db = {plot_offset}, expected ≈ 114"
    );

    // Tier 2.
    let cases = [
        ("fft", "visualize/spectrum", 3),
        ("cwt", "visualize/cwt", 3),
        ("cqt", "visualize/cqt", 5),
        ("reassigned", "visualize/reassigned", 3),
    ];
    for (mode, ty, secs) in cases {
        let f = capture_one_monitor_frame(&c, mode, ty, secs);
        assert_envelope(&f, "on", true, mode);
        let off = f["spl_offset_db"]
            .as_f64()
            .unwrap_or_else(|| panic!("{mode} spl_offset_db not f64: {f}"));
        // All paths read the same cal entry → same offset.
        assert!(
            (off - plot_offset).abs() < 0.01,
            "{mode} spl_offset_db = {off} doesn't match plot's {plot_offset}"
        );
    }
}

#[test]
fn parity_envelope_off_when_mic_correction_disabled() {
    // Curve loaded, but `set_mic_correction_enabled false` → tag must
    // read "off" (curve loaded but not applied) on every path. SPL is
    // independent and stays present.
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let (freqs, gains) = synthetic_curve_flat(3.0);
    let r = c.call(json!({
        "cmd":           "calibrate_mic_curve",
        "op":            "set",
        "input_channel": 0,
        "freqs_hz":      freqs,
        "gain_db":       gains,
    }));
    assert_eq!(r["ok"], json!(true));
    let r = c.call(json!({"cmd": "set_mic_correction_enabled", "enabled": false}));
    assert_eq!(r["ok"], json!(true));

    while c.recv_pub(50).is_some() {}

    let pf = capture_plot_point(&c, 1000.0);
    assert_envelope(&pf, "off", false, "plot");

    let cases = [
        ("fft", "visualize/spectrum", 3),
        ("cwt", "visualize/cwt", 3),
        ("cqt", "visualize/cqt", 5),
        ("reassigned", "visualize/reassigned", 3),
    ];
    for (mode, ty, secs) in cases {
        let f = capture_one_monitor_frame(&c, mode, ty, secs);
        assert_envelope(&f, "off", false, mode);
    }
}

#[test]
fn plot_fundamental_dbfs_reflects_mic_curve_at_test_freq() {
    // Numeric parity assertion (#97): with a curve that has +3 dB at
    // 1 kHz, plot's fundamental_dbfs is 3 dB lower than the
    // uncorrected reading on the same fake-audio signal.
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let pf_no_curve = capture_plot_point(&c, 1000.0);
    let fund_uncorrected = pf_no_curve["fundamental_dbfs"]
        .as_f64()
        .expect("uncorrected fundamental_dbfs f64");
    // Drain.
    while c.recv_pub(50).is_some() {}

    let (freqs, gains) = synthetic_curve_flat(3.0);
    let r = c.call(json!({
        "cmd":           "calibrate_mic_curve",
        "op":            "set",
        "input_channel": 0,
        "freqs_hz":      freqs,
        "gain_db":       gains,
    }));
    assert_eq!(r["ok"], json!(true));
    while c.recv_pub(50).is_some() {}

    let pf_corrected = capture_plot_point(&c, 1000.0);
    let fund_corrected = pf_corrected["fundamental_dbfs"]
        .as_f64()
        .expect("corrected fundamental_dbfs f64");
    let delta = fund_uncorrected - fund_corrected;
    assert!(
        (delta - 3.0).abs() < 0.5,
        "expected ≈ 3 dB drop, got Δ={delta:.2} dB \
         (uncorrected={fund_uncorrected:.2}, corrected={fund_corrected:.2})"
    );
}

/// #99 extension (handoff: transfer-frame-v2 M0, AC #4): the same fake
/// channel-0 stimulus (1 kHz @ 0.1 peak amplitude, `audio/fake.rs`'s
/// fallback default — both `monitor_spectrum`'s implicit `set_tone(1000,
/// 0.0)` and `transfer_stream`'s untouched-stimulus passive path hit the
/// same "no amplitude set" branch) must read the same physical level
/// whether observed through `monitor_spectrum`'s `visualize/spectrum`
/// frame or `transfer_stream`'s new `meas_spectrum` field — two
/// independent aggregation code paths (`DEFAULT_WIRE_COLUMNS`=4096 vs the
/// coarser 48-cols/octave transfer grid) over the same underlying signal.
///
/// Tolerance derivation: the two paths use different FFT lengths
/// (monitor's default `fft_n` vs transfer's fixed `nperseg=sr`), so 1 kHz
/// isn't bin-aligned on both — monitor pays a Hann scalloping loss
/// (measured ≈0.6 dB) on its non-bin-aligned nearest bin, while
/// transfer's exact bin-aligned tone instead sums its own Hann 3-tap
/// leakage into ±1 Hz neighbours at transfer's coarser column width
/// (measured ≈1.76 dB, see `transfer_stream_meas_spectrum_amplitude_truth`
/// in `it_protocol.rs` for the derivation). These are different, real
/// artifacts of each path's own geometry, not a shared physical
/// discrepancy — 3.0 dB clears their combined ≈2.4 dB with margin while
/// still catching a several-dB normalization or calibration regression.
#[test]
fn parity_transfer_meas_spectrum_matches_monitor_spectrum_level() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let mf = capture_one_monitor_frame(&c, "fft", "visualize/spectrum", 3);
    let m_freqs: Vec<f64> = mf["freqs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect();
    let m_spec: Vec<f64> = mf["spectrum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect();
    let (m_peak_i, &m_peak_amp) = m_spec
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
        .expect("non-empty monitor spectrum");
    let m_peak_hz = m_freqs[m_peak_i];
    let m_peak_dbfs = 20.0 * m_peak_amp.max(1e-12).log10();
    assert!(
        (m_peak_hz - 1000.0).abs() < 20.0,
        "monitor peak at {m_peak_hz} Hz, expected ~1000 Hz"
    );

    while c.recv_pub(50).is_some() {}

    let r = c.call(json!({
        "cmd": "transfer_stream", "meas_channel": 0, "ref_channel": 1,
    }));
    assert_eq!(r["ok"], json!(true), "transfer_stream start: {r}");
    let mut tframe: Option<Value> = None;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "data" && v["type"] == json!("transfer_stream") => {
                tframe = Some(v);
                break;
            }
            Some(_) => continue,
            None => break,
        }
    }
    let _ = c.call(json!({"cmd": "stop"}));
    let tframe = tframe.expect("no transfer_stream frame within 10 s");

    let t_freqs: Vec<f64> = tframe["spec_freqs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect();
    let t_spec: Vec<f64> = tframe["meas_spectrum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect();
    let (t_peak_i, &t_peak_amp) = t_spec
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
        .expect("non-empty meas_spectrum");
    let t_peak_hz = t_freqs[t_peak_i];
    let t_peak_dbfs = 20.0 * t_peak_amp.max(1e-12).log10();
    assert!(
        (t_peak_hz - 1000.0).abs() < 50.0,
        "transfer peak at {t_peak_hz} Hz, expected ~1000 Hz"
    );

    let delta = (m_peak_dbfs - t_peak_dbfs).abs();
    assert!(
        delta < 3.0,
        "cross-path level mismatch: monitor={m_peak_dbfs:.2} dBFS \
         transfer={t_peak_dbfs:.2} dBFS (Δ={delta:.2})"
    );
}

/// qa-signoff.md item 4, bullet 2-3 (identical cal chain, voltage layer):
/// with voltage cal loaded, `meas_spectrum` is voltage-scaled (linear
/// Vrms-domain) while monitor's plain `spectrum` field is never
/// voltage-scaled (it stays dBFS-domain; voltage info ships separately as
/// `in_dbu`) — this is by design (ZMQ.md's `visualize/spectrum` contract
/// predates this PR and isn't changed by it). So "matches" here means:
/// monitor's dBFS-domain peak, scaled by the *same* `vrms_at_0dbfs_in`
/// the daemon applied to `meas_spectrum`, reproduces transfer's peak —
/// not bit-identical numbers, but the same physical quantity read
/// through both paths' own unit contracts. `get_calibration` is queried
/// for the scale factor rather than re-deriving it, so this test can't
/// silently drift from whatever the daemon actually calibrated.
///
/// No mic curve is loaded for this test — see
/// `cal_tags_mic_curve_matches_monitor_mic_correction_tag` below for why.
#[test]
fn parity_transfer_meas_spectrum_matches_monitor_after_voltage_cal_scale() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

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

    let cal = c.call(json!({"cmd": "get_calibration", "output_channel": 0, "input_channel": 0}));
    assert_eq!(cal["ok"], json!(true));
    assert_eq!(cal["found"], json!(true));
    let vrms_scale = cal["vrms_at_0dbfs_in"]
        .as_f64()
        .expect("vrms_at_0dbfs_in must be set after calibrate");

    while c.recv_pub(50).is_some() {}

    let mf = capture_one_monitor_frame(&c, "fft", "visualize/spectrum", 3);
    let m_freqs: Vec<f64> = mf["freqs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect();
    let m_spec: Vec<f64> = mf["spectrum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect();
    let (m_peak_i, &m_peak_amp) = m_spec
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
        .expect("non-empty monitor spectrum");
    assert!(
        (m_freqs[m_peak_i] - 1000.0).abs() < 20.0,
        "monitor peak at {} Hz",
        m_freqs[m_peak_i]
    );
    // monitor's `spectrum` is dBFS-domain amplitude, never voltage-scaled
    // (unlike transfer's `meas_spectrum`) — predict what transfer *should*
    // read by applying the same scale the daemon fetched above.
    let predicted_transfer_dbfs = 20.0 * (m_peak_amp * vrms_scale).max(1e-12).log10();

    while c.recv_pub(50).is_some() {}

    let r = c.call(json!({
        "cmd": "transfer_stream", "meas_channel": 0, "ref_channel": 1,
    }));
    assert_eq!(r["ok"], json!(true), "transfer_stream start: {r}");
    let mut tframe: Option<Value> = None;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "data" && v["type"] == json!("transfer_stream") => {
                tframe = Some(v);
                break;
            }
            Some(_) => continue,
            None => break,
        }
    }
    let _ = c.call(json!({"cmd": "stop"}));
    let tframe = tframe.expect("no transfer_stream frame within 10 s");

    let t_freqs: Vec<f64> = tframe["spec_freqs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect();
    let t_spec: Vec<f64> = tframe["meas_spectrum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect();
    let (t_peak_i, &t_peak_amp) = t_spec
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
        .expect("non-empty meas_spectrum");
    assert!(
        (t_freqs[t_peak_i] - 1000.0).abs() < 50.0,
        "transfer peak at {} Hz",
        t_freqs[t_peak_i]
    );
    let t_peak_dbfs = 20.0 * t_peak_amp.max(1e-12).log10();

    // Same 3.0 dB combined-artifact bound as the uncalibrated cross-path
    // test above (Hann scallop vs Hann leak, different column geometry)
    // — the voltage-cal scale factor is exact (queried, not re-derived),
    // so it doesn't add its own error term.
    let delta = (predicted_transfer_dbfs - t_peak_dbfs).abs();
    assert!(
        delta < 3.0,
        "voltage-cal cross-path mismatch: predicted={predicted_transfer_dbfs:.2} dBFS \
         (monitor {m_peak_amp:.4} × scale {vrms_scale:.4}) actual transfer={t_peak_dbfs:.2} dBFS \
         (Δ={delta:.2})"
    );
}

/// qa-signoff.md item 4, bullet 3 (spl vs monitor-derived SPL). Monitor
/// has no broadband weighted+integrated SPL scalar to compare against
/// directly (by design — M0 adds no field to `monitor_spectrum`), so
/// this reconstructs the equivalent quantity from monitor's own
/// calibrated `fundamental_dbfs` (the parabolic-interpolated, window-
/// corrected peak reading — *not* re-derived from the aggregated
/// `spectrum` display array; see the note below on why) and compares to
/// transfer's actual `spl` on its first published frame — the one frame
/// where `EmaIntegrator` hasn't smoothed anything yet (`primed == false`
/// on the first `update()` call returns the input unchanged; see
/// `time_integration.rs`), so it's directly comparable to a single-frame
/// reconstruction rather than a running average.
///
/// **Why not sum monitor's `spectrum` array directly** (tried first):
/// `spectrum` is already column-aggregated for display
/// (`DEFAULT_WIRE_COLUMNS`=4096). At low frequencies many consecutive
/// columns are narrower than the FFT's bin spacing and get the *same*
/// interpolated noise-floor value repeated across each empty column
/// (`spectrum_to_columns`'s documented empty-column fallback) — summing
/// power over all 4096 columns therefore counts that repeated floor
/// value many times over, inflating a "total power" reconstruction by
/// several dB. That's a real property of a *display* aggregate, not a
/// bug — but it makes the aggregate array the wrong basis for a power-
/// summing reconstruction. `fundamental_dbfs` sidesteps it entirely
/// (single windowed-and-normalized peak reading, not touched by column
/// aggregation) and is the correct native quantity to compare against
/// for a stimulus that's a near-pure tone (harmonics ≳40 dB down,
/// contributing <0.01 dB to a linear power sum — negligible).
///
/// No mic curve loaded (SPL cal only) — deliberately avoids exercising
/// `monitor.rs`'s mic-curve application to the `spectrum` array, which a
/// close read during this review suggests may apply a dB-domain
/// correction (`apply_mic_curve_inplace_f64`) to an array documented as
/// linear amplitude (`AnalysisResult.spectrum`). That's a pre-existing
/// question unrelated to this PR (monitor.rs is untouched) — flagged in
/// qa-signoff.md as an out-of-scope finding, not asserted against here
/// either way.
#[test]
fn parity_transfer_spl_matches_monitor_derived_spl_on_first_frame() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let r = c.call(json!({"cmd": "calibrate_spl", "input_channel": 0, "capture_s": 0.05}));
    assert_eq!(r["ok"], json!(true));
    let _ = c
        .wait_for_topic("cal_prompt", Duration::from_secs(3))
        .expect("spl cal_prompt");
    let _ = c.call(json!({"cmd": "cal_reply", "vrms": Value::Null}));
    let _ = c
        .wait_for_topic("cal_done", Duration::from_secs(5))
        .expect("spl cal_done");
    while c.recv_pub(50).is_some() {}

    let mf = capture_one_monitor_frame(&c, "fft", "visualize/spectrum", 3);
    let m_spl_offset = mf["spl_offset_db"]
        .as_f64()
        .expect("spl_offset_db must be set after calibrate_spl");
    let m_fundamental_dbfs = mf["fundamental_dbfs"]
        .as_f64()
        .expect("fundamental_dbfs must be present");

    while c.recv_pub(50).is_some() {}

    let r = c.call(json!({
        "cmd": "transfer_stream", "meas_channel": 0, "ref_channel": 1,
        "weighting": "Z", "integration": "fast",
    }));
    assert_eq!(r["ok"], json!(true), "transfer_stream start: {r}");
    let mut tframe: Option<Value> = None;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "data" && v["type"] == json!("transfer_stream") => {
                tframe = Some(v);
                break;
            }
            Some(_) => continue,
            None => break,
        }
    }
    let _ = c.call(json!({"cmd": "stop"}));
    let tframe = tframe.expect("no transfer_stream frame within 10 s");
    let t_spl = tframe["spl"]
        .as_f64()
        .expect("spl must be a finite number, meas channel is SPL-calibrated");

    // Z weighting (identity) matches the transfer session's own
    // `weighting: "Z"` param above — no curve to import.
    let m_derived_spl = m_fundamental_dbfs + m_spl_offset;

    // Tolerance derivation: transfer's `spl` sums power over *all*
    // ~24 001 raw 1 Hz bins (no display aggregation), so it includes the
    // tone's own Hann 3-tap leakage into its ±1 Hz neighbours — the same
    // effect derived and measured in
    // `transfer_stream_meas_spectrum_amplitude_truth` (it_protocol.rs):
    // +1.76 dB in theory. Monitor's `fundamental_dbfs` is a peak reading
    // and does *not* carry that same-signal leakage inflation. Plus a
    // smaller cross-FFT-length scalloping/quantization difference
    // (≲1 dB, same character as the uncalibrated cross-path test above).
    // Combined expected delta ≈ 2-2.5 dB; 4.0 dB clears it with margin
    // while still catching a several-dB regression.
    let delta = (t_spl - m_derived_spl).abs();
    assert!(
        delta < 4.0,
        "spl cross-path mismatch: transfer spl={t_spl:.2} monitor-derived={m_derived_spl:.2} \
         (Δ={delta:.2}, monitor spl_offset={m_spl_offset:.2}, fundamental_dbfs={m_fundamental_dbfs:.2})"
    );
}

/// handoff: ac-scene M2 (QA follow-up item 1) — the SPL-parity test
/// above never loads a voltage calibration, so it can't distinguish
/// "voltage cal and SPL cal are parallel layers off raw digital
/// amplitude" (the actual, deliberate topology — see
/// `Calibration::spl_offset_db`'s doc comment) from "voltage cal gets
/// composed into SPL" (a plausible-looking but wrong alternative that
/// would double-count the calibration tone's own already-known SPL).
/// This test loads a **non-trivial** voltage cal (`vrms=5.0`, chosen
/// far from the 1.0/unset trivial case so a composed-topology bug would
/// produce an obvious ~14 dB error, not something tolerance could hide)
/// alongside SPL cal, and asserts `spl` is unchanged from the no-
/// voltage-cal baseline within a tolerance that only needs to absorb
/// capture-timing jitter between two independent daemon runs of the
/// same deterministic fake stimulus — not a cross-path methodology
/// difference like the test above, so the bound is much tighter.
#[test]
fn parity_transfer_spl_is_independent_of_voltage_cal_scale() {
    fn spl_with_optional_voltage_cal(vrms: Option<f64>) -> f64 {
        let d = Daemon::spawn();
        let c = Client::new(&d);

        if let Some(vrms) = vrms {
            let r = c.call(json!({"cmd": "calibrate", "ref_dbfs": -10.0,
                                   "output_channel": 0, "input_channel": 0}));
            assert_eq!(r["ok"], json!(true));
            let _ = c
                .wait_for_topic("cal_prompt", Duration::from_secs(3))
                .expect("voltage cal step 1 prompt");
            let _ = c.call(json!({"cmd": "cal_reply", "vrms": vrms}));
            let _ = c
                .wait_for_topic("cal_prompt", Duration::from_secs(3))
                .expect("voltage cal step 2 prompt");
            let _ = c.call(json!({"cmd": "cal_reply", "vrms": vrms}));
            let _ = c
                .wait_for_topic("cal_done", Duration::from_secs(5))
                .expect("voltage cal_done");
        }

        let r = c.call(json!({"cmd": "calibrate_spl", "input_channel": 0, "capture_s": 0.05}));
        assert_eq!(r["ok"], json!(true));
        let _ = c
            .wait_for_topic("cal_prompt", Duration::from_secs(3))
            .expect("spl cal_prompt");
        let _ = c.call(json!({"cmd": "cal_reply", "vrms": Value::Null}));
        let _ = c
            .wait_for_topic("cal_done", Duration::from_secs(5))
            .expect("spl cal_done");
        while c.recv_pub(50).is_some() {}

        let r = c.call(json!({
            "cmd": "transfer_stream", "meas_channel": 0, "ref_channel": 1,
            "weighting": "Z", "integration": "fast",
        }));
        assert_eq!(r["ok"], json!(true), "transfer_stream start: {r}");
        let tframe = wait_for_transfer_frame(&c).expect("no transfer_stream frame within 10 s");
        let _ = c.call(json!({"cmd": "stop"}));
        tframe["spl"]
            .as_f64()
            .expect("spl must be finite, meas channel is SPL-calibrated")
    }

    let spl_no_voltage_cal = spl_with_optional_voltage_cal(None);
    let spl_with_voltage_cal = spl_with_optional_voltage_cal(Some(5.0));

    // A composed topology shows up as **~27 dB**, not the ~14 dB this comment
    // claimed until #261 measured it. `5.0` is the DMM reading; what gets
    // applied is `vrms_at_0dbfs_in = reading / amplitude(measured dBFS)` —
    // extrapolated to full scale from a cal capture at −13.01 dBFS, so 22.36,
    // and 20·log10(22.36) = 26.99. The bound was never wrong, only the
    // prediction behind it: 2.0 dB separates cross-run capture jitter from an
    // error that is larger still than the one it was sized against. See
    // `parity_monitor_spl_is_independent_of_voltage_cal_scale` below for the
    // falsification run the figure comes from.
    let delta = (spl_with_voltage_cal - spl_no_voltage_cal).abs();
    assert!(
        delta < 2.0,
        "spl must not depend on voltage-cal scale (parallel-layer topology): \
         no-voltage-cal spl={spl_no_voltage_cal:.2} vrms=5.0 spl={spl_with_voltage_cal:.2} \
         (Δ={delta:.2} — a composed topology would show ~27 dB here)"
    );
}

/// qa-signoff.md item 4, bullet 4: `cal_tags` vocabulary must be
/// string-identical to the existing `mic_correction` tag vocabulary
/// monitor-path frames already use — asserted as literal string
/// equality, not "semantically equivalent". Mic curve loaded here (this
/// test only compares tag *strings*, not the `spectrum` array's
/// numbers, so it doesn't touch the numeric question flagged in the
/// `spl` parity test above).
#[test]
fn cal_tags_mic_curve_matches_monitor_mic_correction_tag() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let (freqs, gains) = synthetic_curve_flat(3.0);
    let r = c.call(json!({
        "cmd": "calibrate_mic_curve", "op": "set", "input_channel": 0,
        "freqs_hz": freqs, "gain_db": gains,
    }));
    assert_eq!(r["ok"], json!(true));
    while c.recv_pub(50).is_some() {}

    let mf = capture_one_monitor_frame(&c, "fft", "visualize/spectrum", 3);
    let monitor_tag = mf["mic_correction"]
        .as_str()
        .expect("mic_correction must be a string")
        .to_string();
    assert_eq!(monitor_tag, "on", "sanity: curve should be applied");

    while c.recv_pub(50).is_some() {}

    let r = c.call(json!({
        "cmd": "transfer_stream", "meas_channel": 0, "ref_channel": 1,
    }));
    assert_eq!(r["ok"], json!(true));
    let tframe = wait_for_transfer_frame(&c);
    let _ = c.call(json!({"cmd": "stop"}));
    let tframe = tframe.expect("no transfer_stream frame within 10 s");

    assert_eq!(
        tframe["cal_tags"]["meas"]["mic_curve"].as_str(),
        Some(monitor_tag.as_str()),
        "cal_tags.meas.mic_curve must be string-identical to monitor's mic_correction tag"
    );
    // mic_correction (top-level, existing field) must also agree —
    // same channel, same curve, same daemon-global enable flag.
    assert_eq!(
        tframe["mic_correction"].as_str(),
        Some(monitor_tag.as_str()),
        "transfer's own mic_correction tag must match monitor's"
    );
}

fn wait_for_transfer_frame(c: &Client) -> Option<Value> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "data" && v["type"] == json!("transfer_stream") => return Some(v),
            Some(_) => continue,
            None => return None,
        }
    }
    None
}

/// #261 — the third of the three call sites `shared/calibration/`'s layer
/// topology names, and the last one that held by human reading alone.
///
/// `derive_pair` and the `transfer_stream` handler are covered by
/// `parity_transfer_spl_is_independent_of_voltage_cal_scale` above.
/// `monitor.rs`'s clause is the same invariant seen from its own side: it must
/// never scale `spectrum` / `cwt_mags` by `vrms_at_0dbfs_in`, and voltage
/// information must ship only in the separate `dbu_offset_db` field. If it
/// composed the layers instead, every absolute SPL a consumer derives from a
/// monitor frame — `20·log10(bin) + spl_offset_db` — would move the moment the
/// channel picked up an electrical calibration, with nothing on screen to say
/// so.
///
/// What existed nearby is not this test.
/// `parity_transfer_meas_spectrum_matches_monitor_after_voltage_cal_scale`
/// scales a monitor peak *by* the same vrms and compares paths — it asserts the
/// two agree once converted, which a composed topology could still satisfy.
/// `it_protocol.rs`'s `dbu_offset_db` check covers the unset case, where the
/// scale factor is 1.0 and composition is unobservable by construction. The
/// distinction this test dissolves is "verified by test" versus "verified by
/// reading".
///
/// Two runs of the same deterministic fake stimulus, the second with a
/// non-trivial voltage cal, asserting the derived SPL does not move.
#[test]
fn parity_monitor_spl_is_independent_of_voltage_cal_scale() {
    /// Derived SPL from one monitor `visualize/spectrum` frame: the peak bin in
    /// dBFS plus the frame's own `spl_offset_db`. This is the number a consumer
    /// displays, which is what the invariant is about — not an internal.
    fn monitor_spl_with_optional_voltage_cal(vrms: Option<f64>) -> f64 {
        let d = Daemon::spawn();
        let c = Client::new(&d);

        if let Some(vrms) = vrms {
            let r = c.call(json!({"cmd": "calibrate", "ref_dbfs": -10.0,
                                   "output_channel": 0, "input_channel": 0}));
            assert_eq!(r["ok"], json!(true));
            let _ = c
                .wait_for_topic("cal_prompt", Duration::from_secs(3))
                .expect("voltage cal step 1 prompt");
            let _ = c.call(json!({"cmd": "cal_reply", "vrms": vrms}));
            let _ = c
                .wait_for_topic("cal_prompt", Duration::from_secs(3))
                .expect("voltage cal step 2 prompt");
            let _ = c.call(json!({"cmd": "cal_reply", "vrms": vrms}));
            let _ = c
                .wait_for_topic("cal_done", Duration::from_secs(5))
                .expect("voltage cal_done");

            // **This test's discriminating power, asserted rather than
            // assumed.** Composition scales by the *stored*
            // `vrms_at_0dbfs_in`, so the symptom is 20·log10(that) — which is
            // bounded below by nothing. A rig whose full scale is 1 Vrms
            // stores 1.0 and composes with an error of exactly **0 dB**:
            // ordinary hardware, not a contrived case, on which the composed
            // and correct topologies are indistinguishable by the reading.
            //
            // So the stored constant is read back from the daemon rather than
            // re-derived, and the test refuses to run on a value that cannot
            // separate the two. Without this, tidying `5.0` to a rounder-
            // looking `1.0` would leave a test that passes with the layers
            // composed — and it would look like a simplification.
            let cal = c.call(json!({"cmd": "get_calibration",
                                    "output_channel": 0, "input_channel": 0}));
            assert_eq!(cal["ok"], json!(true));
            let stored = cal["vrms_at_0dbfs_in"]
                .as_f64()
                .expect("vrms_at_0dbfs_in must be set after calibrate");
            let composed_error_db = 20.0 * stored.log10();
            assert!(
                composed_error_db.abs() > 10.0,
                "this test cannot discriminate: stored vrms_at_0dbfs_in={stored:.3} makes                  composition a {composed_error_db:.2} dB error, which the 2.0 dB tolerance                  below would swallow. The `vrms` handed to `calibrate` must land the stored                  constant far from 1.0 — at 1.0 exactly, a composed topology is invisible                  here."
            );
        }

        let r = c.call(json!({"cmd": "calibrate_spl", "input_channel": 0, "capture_s": 0.05}));
        assert_eq!(r["ok"], json!(true));
        let _ = c
            .wait_for_topic("cal_prompt", Duration::from_secs(3))
            .expect("spl cal_prompt");
        let _ = c.call(json!({"cmd": "cal_reply", "vrms": Value::Null}));
        let _ = c
            .wait_for_topic("cal_done", Duration::from_secs(5))
            .expect("spl cal_done");
        while c.recv_pub(50).is_some() {}

        let mf = capture_one_monitor_frame(&c, "fft", "visualize/spectrum", 5);
        let _ = c.call(json!({"cmd": "stop"}));

        // Present-and-finite, not just present: a null here would make the
        // comparison below trivially true on both runs and the test vacuous.
        let spl_offset = mf["spl_offset_db"]
            .as_f64()
            .expect("spl_offset_db must be finite — the channel is SPL-calibrated");
        let spec: Vec<f64> = mf["spectrum"]
            .as_array()
            .expect("spectrum array")
            .iter()
            .map(|v| v.as_f64().unwrap())
            .collect();
        let peak = spec
            .iter()
            .copied()
            .fold(f64::NEG_INFINITY, f64::max)
            .max(1e-12);
        20.0 * peak.log10() + spl_offset
    }

    let spl_no_voltage_cal = monitor_spl_with_optional_voltage_cal(None);
    let spl_with_voltage_cal = monitor_spl_with_optional_voltage_cal(Some(5.0));

    // `vrms = 5.0` is the operator's DMM reading, chosen far from the trivial
    // 1.0/unset case. **The scale a composed topology would apply is not 5.0**,
    // and that is worth being precise about, because the obvious prediction —
    // 20·log10(5.0) = 13.98 dB — is wrong.
    //
    // `calibrate` stores `vrms_at_0dbfs_in = reading / amplitude(measured
    // dBFS)`: Vrms extrapolated to full scale, from a reading taken well below
    // it. Here the cal capture sits at −13.01 dBFS, so the stored constant is
    // 5.0 / 0.2236 = **22.36**, and composition scales by that.
    //
    // Falsified, and the arithmetic closes exactly: composing the layers in
    // `monitor.rs` moves the peak from −20.63 to +6.36 dBFS, a delta of
    // **26.99 dB**, against 20·log10(22.36) = 26.989 predicted. `spl_offset_db`
    // is **117.010 in both runs** — the SPL layer does not move at all.
    //
    // That last measurement is the operationally important one. `calibrate_spl`
    // derives `mic_sensitivity_dbfs_at_94db_spl` from `capture_rms`, raw, and
    // never touches `vrms_at_0dbfs_in`. So **re-running the SPL calibration
    // after a composition bug appeared would not mask it**: the stored
    // sensitivity comes back identical and the live reading stays wrong by the
    // full 26.99 dB. An operator debugging a bad SPL number at the rig would
    // re-calibrate first, and would learn nothing from it.
    //
    // **The tolerance absorbs capture jitter only.** Two independent daemon
    // runs of the same deterministic fake stimulus land on slightly different
    // FFT windows, which moves the peak bin by a fraction of a dB of Hann
    // scalloping. 2.0 dB covers that with room; it is not a margin fitted to an
    // observed discrepancy, and it is not a number to tighten later. If a
    // future change makes this fail by a few tenths, the question is what moved
    // the capture — not whether the bound is too generous.
    let delta = (spl_with_voltage_cal - spl_no_voltage_cal).abs();
    assert!(
        delta < 2.0,
        "monitor SPL moved with voltage cal: no-voltage-cal={spl_no_voltage_cal:.2} dB SPL, \
         vrms=5.0 gives {spl_with_voltage_cal:.2} dB SPL (Δ={delta:.2}). ~27 dB means \
         `monitor.rs` is scaling `spectrum` by `vrms_at_0dbfs_in` — that is \
         20·log10(22.36), the stored full-scale constant, not 20·log10(5.0) of the DMM \
         reading. Voltage cal and SPL cal are parallel layers off raw digital amplitude, \
         not composed (`shared/calibration/`, layer topology)."
    );
}
