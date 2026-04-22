//! ZMQ integration tests against a real `ac-daemon` binary in `--fake-audio` mode.
//!
//! Each test spawns its own daemon on a random port pair, drives the CTRL/DATA
//! sockets, and kills the process on drop. No shared state, no hardware needed.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU16, AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

static PORT_CURSOR: AtomicU16 = AtomicU16::new(25_600);
static HOME_CURSOR: AtomicU32 = AtomicU32::new(0);

fn alloc_ports() -> (u16, u16) {
    let base = PORT_CURSOR.fetch_add(2, Ordering::Relaxed);
    (base, base + 1)
}

/// Unique scratch HOME per daemon so tests don't write to the real config.
fn alloc_home() -> PathBuf {
    let n = HOME_CURSOR.fetch_add(1, Ordering::Relaxed);
    let mut p = env::temp_dir();
    p.push(format!("ac-daemon-it-{}-{n}", std::process::id()));
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
                "--fake-audio",
                "--local",
                "--ctrl-port", &ctrl.to_string(),
                "--data-port", &data.to_string(),
            ])
            .spawn()
            .expect("spawn ac-daemon");
        // Wait for the CTRL socket to accept a probe.
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
            if let Ok(_msg) = s.recv_bytes(0) { break; }
        }
        Self { child, ctrl_port: ctrl, data_port: data, home }
    }

    fn ctrl_endpoint(&self) -> String { format!("tcp://127.0.0.1:{}", self.ctrl_port) }
    fn data_endpoint(&self) -> String { format!("tcp://127.0.0.1:{}", self.data_port) }
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
        req.set_rcvtimeo(3_000).unwrap();
        req.set_sndtimeo(3_000).unwrap();
        req.connect(&d.ctrl_endpoint()).unwrap();

        let sub = ctx.socket(zmq::SUB).unwrap();
        sub.set_linger(0).unwrap();
        sub.set_rcvtimeo(3_000).unwrap();
        sub.set_subscribe(b"").unwrap();
        sub.connect(&d.data_endpoint()).unwrap();

        // Allow a tick for the SUB to latch before returning.
        thread::sleep(Duration::from_millis(100));
        Self { _ctx: ctx, req, sub }
    }

    fn call(&self, cmd: Value) -> Value {
        let raw = serde_json::to_vec(&cmd).unwrap();
        self.req.send(raw, 0).unwrap();
        let bytes = self.req.recv_bytes(0).expect("CTRL recv");
        serde_json::from_slice(&bytes).expect("CTRL decode")
    }

    /// Pop one PUB frame (topic + JSON payload); returns None on timeout.
    /// Wire format: single frame `<topic> <json>\n` (see ZMQ.md §DATA).
    fn recv_pub(&self, timeout_ms: i32) -> Option<(String, Value)> {
        self.sub.set_rcvtimeo(timeout_ms).ok();
        let bytes = match self.sub.recv_bytes(0) {
            Ok(b)  => b,
            Err(_) => return None,
        };
        let split = bytes.iter().position(|&b| b == b' ')?;
        let topic = String::from_utf8(bytes[..split].to_vec()).ok()?;
        let payload = &bytes[split + 1..];
        let v: Value = serde_json::from_slice(payload).unwrap_or(Value::Null);
        Some((topic, v))
    }

    /// Wait for a frame on `topic`, discarding others, until `timeout` elapses.
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

// ---------------------------------------------------------------------------

#[test]
fn status_replies_ok() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({"cmd":"status"}));
    assert_eq!(r["ok"], json!(true));
    assert_eq!(r["busy"], json!(false));
    assert_eq!(r["listen_mode"], json!("local"));
}

#[test]
fn unknown_command_rejected() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({"cmd":"nope"}));
    assert_eq!(r["ok"], json!(false));
    assert!(r["error"].as_str().unwrap().contains("unknown command"));
}

#[test]
fn devices_lists_ports() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({"cmd":"devices"}));
    assert_eq!(r["ok"], json!(true));
    assert!(r["playback"].as_array().unwrap().len() > 0);
    assert!(r["capture"].as_array().unwrap().len() > 0);
}

#[test]
fn generate_stop_emits_done_frame() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({"cmd":"generate","freq_hz":1000.0,"level_dbfs":-12.0}));
    assert_eq!(r["ok"], json!(true));

    // Should now be busy.
    let s = c.call(json!({"cmd":"status"}));
    assert_eq!(s["busy"], json!(true));
    assert_eq!(s["running_cmd"], json!("generate"));

    // Stop should emit a "done" frame on the PUB channel.
    let _ = c.call(json!({"cmd":"stop"}));
    let done = c.wait_for_topic("done", Duration::from_secs(3))
        .expect("no done frame after stop");
    assert_eq!(done["cmd"], json!("generate"));
}

#[test]
fn busy_guard_blocks_duplicate() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    c.call(json!({"cmd":"generate","freq_hz":1000.0,"level_dbfs":-20.0}));
    let dup = c.call(json!({"cmd":"generate","freq_hz":2000.0,"level_dbfs":-20.0}));
    assert_eq!(dup["ok"], json!(false));
    assert!(dup["error"].as_str().unwrap().contains("busy"));
    let _ = c.call(json!({"cmd":"stop"}));
}

#[test]
fn sweep_frequency_publishes_done() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd":"sweep_frequency",
        "start_hz": 100.0,
        "stop_hz":  200.0,
        "level_dbfs": -20.0,
        "duration": 0.3,
    }));
    assert_eq!(r["ok"], json!(true));
    let done = c.wait_for_topic("done", Duration::from_secs(5))
        .expect("sweep_frequency never finished");
    assert_eq!(done["cmd"], json!("sweep_frequency"));
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
    let done = c.wait_for_topic("done", Duration::from_secs(3))
        .expect("no done frame after stop");
    assert_eq!(done["cmd"], json!("monitor_spectrum"));
}

#[test]
fn calibrate_prompt_reply_cycle() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let r = c.call(json!({"cmd":"calibrate"}));
    assert_eq!(r["ok"], json!(true));

    // The calibrate worker drives through several prompts; send "skip" (reply:null)
    // to each until we see a terminal frame.
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut saw_done = false;
    while Instant::now() < deadline {
        match c.recv_pub(2_000) {
            Some((topic, _payload)) if topic == "cal_prompt" => {
                let _ = c.call(json!({"cmd":"cal_reply", "vrms": null}));
            }
            Some((topic, _)) if topic == "done" || topic == "cal_done" => {
                saw_done = true;
                break;
            }
            Some(_) => continue,
            None    => break,
        }
    }
    assert!(saw_done, "calibrate cycle never completed");
}

#[test]
fn sweep_ir_emits_impulse_response_with_expected_delay_peak() {
    // Fake backend implements `play_and_capture` as a delayed loopback
    // (see audio/fake.rs). Running a Farina sweep through it and
    // deconvolving should produce a linear IR with its peak at the
    // window centre (the gate re-centres the peak on linear_ir.len()/2).
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd":"sweep_ir",
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
        let remaining = deadline.saturating_duration_since(Instant::now()).as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "measurement/impulse_response" => {
                let ir = v["data"]["linear_ir"].as_array().expect("linear_ir array");
                assert_eq!(ir.len(), 1024, "window_len respected");
                // Find the max-absolute sample index.
                let (peak_idx, peak_val) = ir.iter().enumerate().fold((0usize, 0.0f64), |acc, (i, x)| {
                    let mag = x.as_f64().unwrap_or(0.0).abs();
                    if mag > acc.1 { (i, mag) } else { acc }
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
                assert_eq!(v["report"]["data"]["kind"], json!("impulse_response"));
                assert_eq!(v["report"]["schema_version"], json!(1));
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

// ---------------------------------------------------------------------------
// Time-integration — set_time_integration / get_time_integration / reset_leq.
// See issue #62.
// ---------------------------------------------------------------------------

#[test]
fn time_integration_default_is_off() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({"cmd": "get_time_integration"}));
    assert_eq!(r["ok"], json!(true));
    assert_eq!(r["mode"], json!("off"));
}

#[test]
fn time_integration_accepts_valid_modes() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    for mode in ["off", "fast", "slow", "leq"] {
        let r = c.call(json!({"cmd": "set_time_integration", "mode": mode}));
        assert_eq!(r["ok"], json!(true), "set {mode} failed: {r}");
        assert_eq!(r["mode"], json!(mode));
        let g = c.call(json!({"cmd": "get_time_integration"}));
        assert_eq!(g["mode"], json!(mode));
    }
}

#[test]
fn time_integration_rejects_invalid_mode() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({"cmd": "set_time_integration", "mode": "impulse"}));
    assert_eq!(r["ok"], json!(false));
    assert!(r["error"].as_str().unwrap_or("").contains("invalid mode"));
    // Mode should not have changed.
    let g = c.call(json!({"cmd": "get_time_integration"}));
    assert_eq!(g["mode"], json!("off"));
}

#[test]
fn time_integration_mode_is_case_insensitive() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({"cmd": "set_time_integration", "mode": "SLOW"}));
    assert_eq!(r["ok"], json!(true));
    assert_eq!(r["mode"], json!("slow"));
}

#[test]
fn reset_leq_accepted_when_idle() {
    // No active monitor — the reset flag is latched for the next worker.
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({"cmd": "reset_leq"}));
    assert_eq!(r["ok"], json!(true));
}

// ---------------------------------------------------------------------------
// transfer_stream — ports of the pytest scenarios deleted when the Python
// runtime was removed. See issue #52.
// ---------------------------------------------------------------------------

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
        let remaining = deadline.saturating_duration_since(Instant::now()).as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "data"
                && v["type"].as_str() == Some("transfer_stream") => {
                for key in ["freqs", "magnitude_db", "phase_deg", "coherence",
                            "delay_samples", "delay_ms"] {
                    assert!(v.get(key).is_some(), "frame missing {key}: {v}");
                }
                got_frame = true;
                break;
            }
            Some(_) => continue,
            None => break,
        }
    }
    assert!(got_frame, "never saw a transfer_stream data frame");

    let _ = c.call(json!({"cmd": "stop"}));
    let done = c.wait_for_topic("done", Duration::from_secs(5))
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

// ---------------------------------------------------------------------------
// server_enable / server_disable — toggle listen_mode between local and
// public and check the reported bind_addr. #52.
// ---------------------------------------------------------------------------

#[test]
fn server_enable_reports_public_mode() {
    // server_enable reply lands before the main loop rebinds the
    // sockets (see ZMQ.md §server_enable), but the rebind closes the
    // connection underneath the existing REQ. Reconnect after the
    // command to verify the new mode is reflected in `status`.
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let s0 = c.call(json!({"cmd": "status"}));
    assert_eq!(s0["listen_mode"], json!("local"));

    let r = c.call(json!({"cmd": "server_enable"}));
    assert_eq!(r["ok"], json!(true));
    assert_eq!(r["listen_mode"], json!("public"));
    assert_eq!(r["bind_addr"], json!("*"));
    drop(c);

    // Give the daemon a moment to release and rebind.
    thread::sleep(Duration::from_millis(500));
    let c2 = Client::new(&d);
    let s1 = c2.call(json!({"cmd": "status"}));
    assert_eq!(s1["listen_mode"], json!("public"));
}

#[test]
fn server_disable_restores_local_mode() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    c.call(json!({"cmd": "server_enable"}));
    drop(c);
    thread::sleep(Duration::from_millis(500));

    let c2 = Client::new(&d);
    let r = c2.call(json!({"cmd": "server_disable"}));
    assert_eq!(r["ok"], json!(true));
    assert_eq!(r["listen_mode"], json!("local"));
    assert_eq!(r["bind_addr"], json!("127.0.0.1"));
    drop(c2);

    thread::sleep(Duration::from_millis(500));
    let c3 = Client::new(&d);
    let s = c3.call(json!({"cmd": "status"}));
    assert_eq!(s["listen_mode"], json!("local"));
}

#[test]
fn plot_with_bpo_emits_spectrum_bands() {
    // Plot with `bpo` set: the daemon runs the concatenated sweep capture
    // through an IEC 61260-1 1/3-octave filterbank and publishes a
    // `measurement/spectrum_bands` frame plus a second `measurement/report`
    // whose `data.kind == spectrum_bands`. Assert the payload is well-formed
    // and the peak band lies inside the stimulus range.
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let start_hz = 200.0;
    let stop_hz  = 4_000.0;
    let r = c.call(json!({
        "cmd":        "plot",
        "start_hz":   start_hz,
        "stop_hz":    stop_hz,
        "level_dbfs": -6.0,
        "ppd":        3,
        "duration":   0.2,
        "bpo":        3,
    }));
    assert_eq!(r["ok"], json!(true));

    let mut got_frame  = false;
    let mut got_report = false;
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && !(got_frame && got_report) {
        let remaining = deadline.saturating_duration_since(Instant::now()).as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "measurement/spectrum_bands" => {
                assert_eq!(v["bpo"], json!(3));
                assert_eq!(v["class"], json!("Class 1"));
                let centres = v["centres_hz"].as_array().expect("centres_hz array");
                let levels  = v["levels_dbfs"].as_array().expect("levels_dbfs array");
                assert_eq!(centres.len(), levels.len());
                assert!(!centres.is_empty(), "filterbank produced no bands");
                // Peak band must land near the 1 kHz loopback tone.
                let (peak_idx, _) = levels.iter().enumerate().fold((0usize, f64::NEG_INFINITY), |acc, (i, x)| {
                    let v = x.as_f64().unwrap_or(f64::NEG_INFINITY);
                    if v > acc.1 { (i, v) } else { acc }
                });
                let peak_fc = centres[peak_idx].as_f64().unwrap();
                assert!(
                    (start_hz / 2.0..=stop_hz * 2.0).contains(&peak_fc),
                    "peak band {peak_fc} Hz falls outside sweep range \
                     [{start_hz}, {stop_hz}] (±1 octave)"
                );
                got_frame = true;
            }
            Some((t, v)) if t == "measurement/report" => {
                if v["report"]["data"]["kind"] == json!("spectrum_bands") {
                    assert_eq!(v["report"]["data"]["bpo"], json!(3));
                    assert_eq!(v["report"]["schema_version"], json!(1));
                    got_report = true;
                }
            }
            Some((t, _)) if t == "done" => break,
            Some(_) => continue,
            None => break,
        }
    }
    assert!(got_frame,  "never saw measurement/spectrum_bands frame");
    assert!(got_report, "never saw measurement/report with spectrum_bands data");
}
