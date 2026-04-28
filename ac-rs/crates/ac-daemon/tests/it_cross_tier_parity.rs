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

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU16, AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

static PORT_CURSOR: AtomicU16 = AtomicU16::new(26_400);
static HOME_CURSOR: AtomicU32 = AtomicU32::new(0);

fn alloc_ports() -> (u16, u16) {
    let base = PORT_CURSOR.fetch_add(2, Ordering::Relaxed);
    (base, base + 1)
}

fn alloc_home() -> PathBuf {
    let n = HOME_CURSOR.fetch_add(1, Ordering::Relaxed);
    let mut p = env::temp_dir();
    p.push(format!("ac-daemon-parity-{}-{n}", std::process::id()));
    let _ = fs::create_dir_all(p.join(".config").join("ac"));
    p
}

struct Daemon {
    child:     Child,
    ctrl_port: u16,
    data_port: u16,
    home:      PathBuf,
}

impl Daemon {
    fn spawn() -> Self {
        let (ctrl, data) = alloc_ports();
        let home = alloc_home();
        let bin = env!("CARGO_BIN_EXE_ac-daemon");
        let child = Command::new(bin)
            .env("HOME", &home)
            .args([
                "--fake-audio", "--local",
                "--ctrl-port", &ctrl.to_string(),
                "--data-port", &data.to_string(),
            ])
            .spawn()
            .expect("spawn ac-daemon");
        let deadline = Instant::now() + Duration::from_secs(3);
        let ctx = zmq::Context::new();
        loop {
            if Instant::now() > deadline { panic!("daemon never came up"); }
            thread::sleep(Duration::from_millis(50));
            let s = ctx.socket(zmq::REQ).unwrap();
            s.set_linger(0).ok();
            s.set_rcvtimeo(300).ok();
            s.set_sndtimeo(300).ok();
            if s.connect(&format!("tcp://127.0.0.1:{ctrl}")).is_err() { continue; }
            if s.send(br#"{"cmd":"status"}"#.as_ref(), 0).is_err() { continue; }
            if s.recv_bytes(0).is_ok() { break; }
        }
        Self { child, ctrl_port: ctrl, data_port: data, home }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.home);
    }
}

struct Client {
    _ctx: zmq::Context,
    req:  zmq::Socket,
    sub:  zmq::Socket,
}

impl Client {
    fn new(d: &Daemon) -> Self {
        let ctx = zmq::Context::new();
        let req = ctx.socket(zmq::REQ).unwrap();
        req.set_linger(0).unwrap();
        req.set_rcvtimeo(5_000).unwrap();
        req.set_sndtimeo(5_000).unwrap();
        req.connect(&format!("tcp://127.0.0.1:{}", d.ctrl_port)).unwrap();
        let sub = ctx.socket(zmq::SUB).unwrap();
        sub.set_linger(0).unwrap();
        sub.set_rcvtimeo(8_000).unwrap();
        sub.set_subscribe(b"").unwrap();
        sub.connect(&format!("tcp://127.0.0.1:{}", d.data_port)).unwrap();
        thread::sleep(Duration::from_millis(100));
        Self { _ctx: ctx, req, sub }
    }

    fn call(&self, cmd: Value) -> Value {
        self.req.send(serde_json::to_vec(&cmd).unwrap(), 0).unwrap();
        let bytes = self.req.recv_bytes(0).expect("CTRL recv");
        serde_json::from_slice(&bytes).expect("CTRL decode")
    }

    fn recv_pub(&self, timeout_ms: i32) -> Option<(String, Value)> {
        self.sub.set_rcvtimeo(timeout_ms).ok();
        let bytes = self.sub.recv_bytes(0).ok()?;
        let split = bytes.iter().position(|&b| b == b' ')?;
        let topic = String::from_utf8(bytes[..split].to_vec()).ok()?;
        let payload = &bytes[split + 1..];
        Some((topic, serde_json::from_slice(payload).unwrap_or(Value::Null)))
    }

    fn wait_for_topic(&self, want: &str, timeout: Duration) -> Option<Value> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now()).as_millis() as i32;
            match self.recv_pub(remaining.max(1)) {
                Some((t, v)) if t == want => return Some(v),
                Some(_) => continue,
                None    => return None,
            }
        }
        None
    }
}

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
    c:             &Client,
    mode:          &str,
    expected_type: &str,
    timeout_secs:  u64,
) -> Value {
    let r = c.call(json!({"cmd": "set_analysis_mode", "mode": mode}));
    assert_eq!(r["ok"], json!(true), "set_analysis_mode {mode}: {r}");
    let r = c.call(json!({"cmd": "monitor_spectrum", "freq_hz": 1000.0}));
    assert_eq!(r["ok"], json!(true), "monitor_spectrum start: {r}");

    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let mut frame: Option<Value> = None;
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now()).as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "data" && v["type"] == json!(expected_type) => {
                frame = Some(v);
                break;
            }
            Some(_) => continue,
            None    => break,
        }
    }
    let _ = c.call(json!({"cmd": "stop"}));
    // Drain the inevitable trailing frames so the next caller starts clean.
    let drain_deadline = Instant::now() + Duration::from_millis(300);
    while Instant::now() < drain_deadline {
        if c.recv_pub(50).is_none() { break; }
    }
    frame.unwrap_or_else(|| panic!("no {expected_type} frame within {timeout_secs} s"))
}

fn assert_envelope(frame: &Value, expected_mc: &str, expect_spl: bool, label: &str) {
    assert_eq!(
        frame["mic_correction"], json!(expected_mc),
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
        let remaining = deadline.saturating_duration_since(Instant::now()).as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v))
                if t == "data" && v["type"] == json!("measurement/frequency_response/point") =>
            {
                frame = Some(v);
                break;
            }
            Some(_) => continue,
            None    => break,
        }
    }
    // Drain to `done`.
    let drain_deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < drain_deadline {
        match c.recv_pub(50) {
            Some((t, _)) if t == "done" => break,
            Some(_) => continue,
            None    => break,
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
        ("fft",        "visualize/spectrum",   3),
        ("cwt",        "visualize/cwt",        3),
        ("cqt",        "visualize/cqt",        5),                  // 1 s ring
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
    let _ = c.wait_for_topic("cal_prompt", Duration::from_secs(3)).expect("cal_prompt");
    let _ = c.call(json!({"cmd": "cal_reply", "vrms": Value::Null}));
    let _ = c.wait_for_topic("cal_done", Duration::from_secs(5)).expect("cal_done");

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
    let plot_offset = pf["spl_offset_db"].as_f64().expect("plot spl_offset_db f64");
    assert!(
        (plot_offset - 114.0).abs() < 5.0,
        "plot spl_offset_db = {plot_offset}, expected ≈ 114"
    );

    // Tier 2.
    let cases = [
        ("fft",        "visualize/spectrum",   3),
        ("cwt",        "visualize/cwt",        3),
        ("cqt",        "visualize/cqt",        5),
        ("reassigned", "visualize/reassigned", 3),
    ];
    for (mode, ty, secs) in cases {
        let f = capture_one_monitor_frame(&c, mode, ty, secs);
        assert_envelope(&f, "on", true, mode);
        let off = f["spl_offset_db"].as_f64()
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
        ("fft",        "visualize/spectrum",   3),
        ("cwt",        "visualize/cwt",        3),
        ("cqt",        "visualize/cqt",        5),
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
