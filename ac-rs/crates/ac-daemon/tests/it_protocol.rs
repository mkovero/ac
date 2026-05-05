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
fn generate_routes_all_channels_in_request() {
    // Reproduces the post-2026-05-01 reboot scenario on the FF400 rig:
    // DAC chip enumeration order shifted, so the user shotguns
    // `ac generate sine 0-17 ...` to hit *some* analog output. The CLI
    // sent `channels: [0..17]` but the daemon ignored the field and
    // only opened `cfg.output_channel` — so even the shotgun missed.
    // Lock in: every channel in the request must show up in `out_ports`.
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let chans: Vec<u32> = (0..6).collect();
    let r = c.call(json!({
        "cmd": "generate",
        "freq_hz": 1000.0,
        "level_dbfs": -20.0,
        "channels": chans,
    }));
    assert_eq!(r["ok"], json!(true), "generate ack: {r}");
    let ports = r["out_ports"].as_array().expect("out_ports array");
    assert_eq!(
        ports.len(), chans.len(),
        "expected {} ports for channels {:?}, got {:?}", chans.len(), chans, ports,
    );
    // Each port name must be unique — otherwise the daemon collapsed
    // distinct channel indices to the sticky default.
    let names: std::collections::HashSet<&str> =
        ports.iter().filter_map(|v| v.as_str()).collect();
    assert_eq!(names.len(), ports.len(), "duplicate port in out_ports: {ports:?}");

    let _ = c.call(json!({"cmd":"stop"}));
}

#[test]
fn generate_no_channels_falls_back_to_configured_default() {
    // Bare `ac generate sine ...` (no channel spec) must still route to
    // the configured `output_channel` — this path doesn't go through
    // `resolve_output_by_channel` and was a regression risk when adding
    // multi-channel support.
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let r = c.call(json!({"cmd": "generate", "freq_hz": 1000.0, "level_dbfs": -20.0}));
    assert_eq!(r["ok"], json!(true));
    let ports = r["out_ports"].as_array().expect("out_ports array");
    assert_eq!(ports.len(), 1, "default-channel generate should give one port");

    let _ = c.call(json!({"cmd":"stop"}));
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
        let Some((topic, payload)) = c.recv_pub(2_000) else { break };
        if topic != "data" { continue; }
        if payload.get("type").and_then(Value::as_str) != Some("visualize/spectrum") { continue; }
        if payload.get("spectrum").and_then(Value::as_array).map_or(true, |a| a.is_empty()) {
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
    assert!(frame.get("dbu_offset_db").map_or(true, |v| v.is_null()),
        "dbu_offset_db must be null without cal: {frame}");
    assert!(frame.get("spl_offset_db").map_or(true, |v| v.is_null()),
        "spl_offset_db must be null without cal: {frame}");
    assert!(frame.get("in_dbu").map_or(true, |v| v.is_null()),
        "in_dbu must be null without cal: {frame}");

    // ── 2. fundamental_dbfs ≈ -20 dBFS (with up to ~1.5 dB Hann scallop) ──
    let fund_dbfs = frame["fundamental_dbfs"].as_f64().expect("fundamental_dbfs");
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
    let p0_hz   = p0[0].as_f64().expect("peak freq");
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
        let remaining = deadline.saturating_duration_since(Instant::now()).as_millis() as i32;
        if remaining <= 0 { break; }
        let Some((topic, payload)) = c.recv_pub(remaining.max(1)) else { break };
        if topic != "data" { continue; }
        if payload.get("type").and_then(Value::as_str) != Some("visualize/scope") { continue; }
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
fn calibrate_scales_user_reading_to_zero_dbfs() {
    // Reference tone plays at `ref_dbfs` (default -10 dBFS), so a Vrms
    // reading taken there is `1 / dbfs_to_amplitude(ref_dbfs)` smaller
    // than the Vrms at 0 dBFS. The handler MUST apply that scaling
    // before saving — otherwise a user who calibrates at -10 dBFS and
    // reads 2.095 V would get `0 dBu = 2.095 V` from `ac generate`,
    // ~10 dB hotter than what they asked for.
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let r = c.call(json!({"cmd": "calibrate", "ref_dbfs": -10.0,
                           "output_channel": 0, "input_channel": 0}));
    assert_eq!(r["ok"], json!(true));

    // Step 1 prompt → reply with a known DAC reading.
    let _ = c.wait_for_topic("cal_prompt", Duration::from_secs(3))
        .expect("step 1 prompt");
    let user_out_vrms = 2.095_f64;
    let _ = c.call(json!({"cmd": "cal_reply", "vrms": user_out_vrms}));

    // Step 2 prompt — fake backend loops the played tone back, so the
    // captured input level matches the played `ref_dbfs - 3.01` (RMS
    // vs peak), and the handler should flag `loopback: true`.
    let p2 = c.wait_for_topic("cal_prompt", Duration::from_secs(3))
        .expect("step 2 prompt");
    assert_eq!(p2["loopback"], json!(true), "expected loopback flag in step 2: {p2}");
    let _ = c.call(json!({"cmd": "cal_reply", "vrms": user_out_vrms}));

    let done = c.wait_for_topic("cal_done", Duration::from_secs(5))
        .expect("cal_done frame");

    // ref_dbfs = -10 → out_scale = 10^(10/20) ≈ 3.16228.
    let expected_out = user_out_vrms * 10f64.powf(10.0 / 20.0);
    let saved_out = done["vrms_at_0dbfs_out"].as_f64().expect("out");
    assert!(
        (saved_out - expected_out).abs() < 1e-6,
        "vrms_at_0dbfs_out: got {saved_out}, want {expected_out}",
    );

    // Cross-check via get_calibration so we know it round-tripped to disk.
    let r = c.call(json!({"cmd": "get_calibration",
                           "output_channel": 0, "input_channel": 0}));
    assert_eq!(r["found"], json!(true));
    let stored_out = r["vrms_at_0dbfs_out"].as_f64().expect("stored out");
    assert!((stored_out - expected_out).abs() < 1e-6);
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
                assert_eq!(v["report"]["schema_version"], json!(3));
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
// Band weighting (A/C/Z) — IEC 61672-style curves applied to each
// fractional-octave band before publish. See issue #61.
// ---------------------------------------------------------------------------

#[test]
fn band_weighting_default_is_off() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({"cmd": "get_band_weighting"}));
    assert_eq!(r["ok"], json!(true));
    assert_eq!(r["mode"], json!("off"));
}

#[test]
fn band_weighting_accepts_valid_modes() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    for mode in ["off", "a", "c", "z"] {
        let r = c.call(json!({"cmd": "set_band_weighting", "mode": mode}));
        assert_eq!(r["ok"], json!(true), "set {mode} failed: {r}");
        assert_eq!(r["mode"], json!(mode));
        let g = c.call(json!({"cmd": "get_band_weighting"}));
        assert_eq!(g["mode"], json!(mode));
    }
}

#[test]
fn band_weighting_rejects_invalid_mode() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({"cmd": "set_band_weighting", "mode": "b"}));
    assert_eq!(r["ok"], json!(false));
    assert!(r["error"].as_str().unwrap_or("").contains("invalid mode"));
    let g = c.call(json!({"cmd": "get_band_weighting"}));
    assert_eq!(g["mode"], json!("off"));
}

#[test]
fn band_weighting_mode_is_case_insensitive() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({"cmd": "set_band_weighting", "mode": "A"}));
    assert_eq!(r["ok"], json!(true));
    assert_eq!(r["mode"], json!("a"));
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
                            "re", "im", "delay_samples", "delay_ms"] {
                    assert!(v.get(key).is_some(), "frame missing {key}: {v}");
                }
                // unified.md Phase 3: re/im consistency — every bin
                // must satisfy |H| ≈ √(re² + im²) and arg(H) ≈
                // atan2(im, re), since all four are derived from the
                // same H₁ complex value.
                let mag_db = v["magnitude_db"].as_array().unwrap();
                let phase_deg = v["phase_deg"].as_array().unwrap();
                let re = v["re"].as_array().unwrap();
                let im = v["im"].as_array().unwrap();
                assert_eq!(mag_db.len(), re.len(), "re must match mag length");
                assert_eq!(mag_db.len(), im.len(), "im must match mag length");
                for i in 0..mag_db.len() {
                    let m_db = mag_db[i].as_f64().unwrap();
                    let p_deg = phase_deg[i].as_f64().unwrap();
                    let r = re[i].as_f64().unwrap();
                    let im_v = im[i].as_f64().unwrap();
                    let mag_lin_from_re_im = (r * r + im_v * im_v).sqrt();
                    let mag_lin_from_db = 10.0_f64.powf(m_db / 20.0);
                    // 0.01 relative tolerance: handles f32 → f64
                    // round-trips through serde_json + the
                    // h1.norm().max(1e-6) floor at very small |H|.
                    let denom = mag_lin_from_db.max(1e-6);
                    let rel_err = (mag_lin_from_re_im - mag_lin_from_db).abs() / denom;
                    assert!(
                        rel_err < 0.01,
                        "bin {i}: |H| from re/im = {mag_lin_from_re_im} vs from dB = {mag_lin_from_db}",
                    );
                    // Phase: skip when |H| is at the floor (atan2 of
                    // tiny re/im is meaningless / numerical noise).
                    if mag_lin_from_db > 1e-4 {
                        let p_from_re_im = im_v.atan2(r).to_degrees();
                        let mut diff = (p_from_re_im - p_deg).abs();
                        if diff > 180.0 {
                            diff = 360.0 - diff;
                        }
                        assert!(
                            diff < 1.0,
                            "bin {i}: phase from re/im = {p_from_re_im}° vs frame = {p_deg}°",
                        );
                    }
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
        let remaining = deadline.saturating_duration_since(Instant::now()).as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "data"
                && v["type"].as_str() == Some("visualize/ir") => {
                for key in ["samples", "sr", "dt_ms", "t_origin_ms",
                            "ref_channel", "meas_channel"] {
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
                    assert_eq!(v["report"]["schema_version"], json!(3));
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

#[test]
fn plot_frames_carry_processing_context_envelope() {
    // After Phase 3 (#97 + #98) Tier 1 frames must carry the same
    // processing-context envelope Tier 2 monitor frames already do —
    // mic_correction, spl_offset_db, weighting, time_integration,
    // smoothing_bpo. The MeasurementReport's CalibrationSnapshot must
    // record SPL and mic-curve provenance when those are set.
    let d = Daemon::spawn();
    let c = Client::new(&d);

    // Set up SPL cal.
    let r = c.call(json!({"cmd": "calibrate_spl", "input_channel": 0, "capture_s": 0.05}));
    assert_eq!(r["ok"], json!(true));
    let _ = c.wait_for_topic("cal_prompt", Duration::from_secs(3)).expect("cal_prompt");
    let _ = c.call(json!({"cmd": "cal_reply", "vrms": Value::Null}));
    let _ = c.wait_for_topic("cal_done", Duration::from_secs(5)).expect("cal_done");

    // Attach a synthetic 24-point mic-curve.
    let mut freqs = Vec::new();
    let mut gains = Vec::new();
    let log_min = 100.0_f64.ln();
    let log_max = 10_000.0_f64.ln();
    for i in 0..24 {
        let t = i as f64 / 23.0;
        freqs.push((log_min + t * (log_max - log_min)).exp());
        gains.push(2.0 * t);                                    // ramp 0..2 dB
    }
    let r = c.call(json!({
        "cmd":           "calibrate_mic_curve",
        "op":            "set",
        "input_channel": 0,
        "freqs_hz":      freqs,
        "gain_db":       gains,
    }));
    assert_eq!(r["ok"], json!(true));

    // Drive a tiny `plot` and grab the first per-point frame + the report.
    let r = c.call(json!({
        "cmd":        "plot",
        "start_hz":   1000.0,
        "stop_hz":    1000.0,
        "level_dbfs": -10.0,
        "ppd":        1,
        "duration":   0.1,
    }));
    assert_eq!(r["ok"], json!(true));

    let mut point_frame: Option<Value> = None;
    let mut report_frame: Option<Value> = None;
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline && !(point_frame.is_some() && report_frame.is_some()) {
        let remaining = deadline.saturating_duration_since(Instant::now()).as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "data" => {
                if v["type"] == json!("measurement/frequency_response/point")
                    && point_frame.is_none()
                {
                    point_frame = Some(v);
                } else if v["type"] == json!("measurement/report")
                    && report_frame.is_none()
                {
                    report_frame = Some(v);
                }
            }
            Some(_) => continue,
            None    => break,
        }
    }
    let pf = point_frame.expect("missing per-point frame");
    let rf = report_frame.expect("missing measurement/report");

    // Envelope keys present on the per-point frame (#98).
    assert_eq!(pf["mic_correction"],   json!("on"));
    assert!(pf["spl_offset_db"].is_f64(), "spl_offset_db not f64: {pf:?}");
    assert_eq!(pf["weighting"],        json!("off"));
    assert_eq!(pf["time_integration"], json!("off"));
    assert!(pf.get("smoothing_bpo").is_some(), "smoothing_bpo key missing");

    // CalibrationSnapshot in the report carries SPL + mic_response (#94 →
    // populated here per #97).
    let cal = rf["report"]["calibration"].as_object()
        .expect("calibration block missing");
    assert!(cal["mic_sensitivity_dbfs_at_94db_spl"].is_f64(), "{cal:?}");
    let mr = cal["mic_response"].as_object().expect("mic_response missing");
    assert_eq!(mr["n_points"], json!(24));
    assert!(mr["imported_at"].is_string());
}

// ---------------------------------------------------------------------------
// server_idle_timeout — daemon folds the public bind back to localhost after
// the configured idle CTRL-activity window expires. See issue #58.
// ---------------------------------------------------------------------------

#[test]
fn server_idle_timeout_auto_disables_public_bind() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    // Configure a 1-second idle timeout and go public.
    let r = c.call(json!({
        "cmd": "setup",
        "update": {"server_idle_timeout_secs": 1},
    }));
    assert_eq!(r["ok"], json!(true));
    assert_eq!(r["config"]["server_idle_timeout_secs"], json!(1));

    let r = c.call(json!({"cmd": "server_enable"}));
    assert_eq!(r["ok"], json!(true));
    assert_eq!(r["listen_mode"], json!("public"));
    drop(c);

    // Wait past the idle window. The CTRL socket must stay silent, so don't
    // send anything — the keepalive tick is what trips the auto-disable.
    thread::sleep(Duration::from_millis(3_500));

    // Reconnect on localhost and verify the daemon reverted to local.
    let c2 = Client::new(&d);
    let s = c2.call(json!({"cmd": "status"}));
    assert_eq!(s["listen_mode"], json!("local"),
        "idle timeout did not auto-disable public bind: {s}");
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
        let remaining = deadline.saturating_duration_since(Instant::now()).as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "data" && v["type"] == json!("visualize/cqt") => {
                frame = Some(v);
                break;
            }
            Some(_) => continue,
            None    => break,
        }
    }
    let _ = c.call(json!({"cmd": "stop"}));
    let frame = frame.expect("no visualize/cqt frame within 5 s");

    let mags  = frame["magnitudes"].as_array().expect("magnitudes array");
    let freqs = frame["frequencies"].as_array().expect("frequencies array");
    assert_eq!(mags.len(), freqs.len(), "magnitudes/frequencies length mismatch");
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
        let remaining = deadline.saturating_duration_since(Instant::now()).as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "data" && v["type"] == json!("visualize/reassigned") => {
                frame = Some(v);
                break;
            }
            Some(_) => continue,
            None    => break,
        }
    }
    let _ = c.call(json!({"cmd": "stop"}));
    let frame = frame.expect("no visualize/reassigned frame within 5 s");

    let mags  = frame["magnitudes"].as_array().expect("magnitudes array");
    let freqs = frame["frequencies"].as_array().expect("frequencies array");
    assert_eq!(mags.len(), freqs.len(), "magnitudes/frequencies length mismatch");
    assert!(mags.len() >= 256, "reassigned column suspiciously short: {}", mags.len());
    let f0 = freqs[0].as_f64().unwrap();
    let f_last = freqs[freqs.len() - 1].as_f64().unwrap();
    assert!(f_last > f0 * 100.0, "freqs span less than 2 decades: {f0}..{f_last}");
}

#[test]
fn calibrate_spl_records_capture_dbfs() {
    // End-to-end SPL cal flow:
    //   1. send `calibrate_spl`,
    //   2. respond to `cal_prompt` (any reply ⇒ proceed),
    //   3. wait for `cal_done` carrying `mic_sensitivity_dbfs_at_94db_spl`.
    //
    // The fake backend's `capture_block` returns a 0.1-amplitude sine, so
    // the captured RMS ≈ 0.0707 → ≈ -23 dBFS. Verify the cal_done payload
    // sits in that range (±2 dB headroom for the second-harmonic tracer
    // the fake adds and rounding).
    let d = Daemon::spawn();
    let c = Client::new(&d);

    // Tell the daemon which channel to probe; pick something non-zero so
    // a regression that drops the field would show up as wrong-key writes.
    let r = c.call(json!({
        "cmd":           "calibrate_spl",
        "input_channel": 2,
        "capture_s":     0.2,
    }));
    assert_eq!(r["ok"], json!(true), "calibrate_spl ack: {r}");

    // Wait for the prompt, then release the worker.
    let prompt = c.wait_for_topic("cal_prompt", Duration::from_secs(3))
        .expect("no cal_prompt within 3 s");
    assert_eq!(prompt["kind"], json!("spl"), "prompt kind: {prompt}");

    let r = c.call(json!({"cmd": "cal_reply", "vrms": Value::Null}));
    assert_eq!(r["ok"], json!(true));

    let done = c.wait_for_topic("cal_done", Duration::from_secs(5))
        .expect("no cal_done within 5 s");
    let dbfs = done["mic_sensitivity_dbfs_at_94db_spl"]
        .as_f64()
        .expect("dbfs field missing");
    assert!(
        (-26.0..-19.0).contains(&dbfs),
        "captured dBFS {dbfs} outside fake-backend window",
    );
    assert!(done["key"].as_str().unwrap_or("").contains("_in2"));
}

#[test]
fn get_and_list_calibrations_return_all_three_layers() {
    // After loading voltage cal (via the existing `calibrate` fake-mode
    // path is awkward — easier to just inject via `calibrate_spl` +
    // `calibrate_mic_curve` which write their own fields), `get_calibration`
    // and `list_calibrations` must return the SPL field and the
    // mic_response provenance, matching the schema documented in ZMQ.md.
    let d = Daemon::spawn();
    let c = Client::new(&d);

    // SPL cal — spawn worker, prompt arrives, we reply, daemon captures.
    let r = c.call(json!({
        "cmd": "calibrate_spl",
        "input_channel": 0,
        "capture_s": 0.1,
    }));
    assert_eq!(r["ok"], json!(true));
    let _ = c.wait_for_topic("cal_prompt", Duration::from_secs(3))
        .expect("cal_prompt");
    let _ = c.call(json!({"cmd": "cal_reply", "vrms": Value::Null}));
    let _ = c.wait_for_topic("cal_done", Duration::from_secs(5))
        .expect("cal_done");

    // Mic-curve — synthetic 24-point curve.
    let mut freqs = Vec::new();
    let mut gains = Vec::new();
    let log_min = 100.0_f64.ln();
    let log_max = 10_000.0_f64.ln();
    for i in 0..24 {
        let t = i as f64 / 23.0;
        freqs.push((log_min + t * (log_max - log_min)).exp());
        gains.push(2.5 * t);
    }
    let r = c.call(json!({
        "cmd":           "calibrate_mic_curve",
        "op":            "set",
        "input_channel": 0,
        "freqs_hz":      freqs,
        "gain_db":       gains,
        "source_path":   "/tmp/synthetic.frd",
    }));
    assert_eq!(r["ok"], json!(true));

    // get_calibration must surface both new fields.
    let r = c.call(json!({"cmd": "get_calibration", "input_channel": 0}));
    assert_eq!(r["ok"], json!(true));
    assert_eq!(r["found"], json!(true));
    assert!(r["mic_sensitivity_dbfs_at_94db_spl"].is_f64(),
        "missing or wrong-typed mic_sensitivity_dbfs_at_94db_spl in: {r}");
    let mr = r["mic_response"].as_object().expect("mic_response object");
    assert_eq!(mr["freqs_hz"].as_array().unwrap().len(), 24);
    assert_eq!(mr["gain_db"].as_array().unwrap().len(), 24);
    assert_eq!(mr["source_path"], json!("/tmp/synthetic.frd"));
    assert!(mr["imported_at"].is_string());

    // list_calibrations must surface them too — find the entry we just wrote.
    let r = c.call(json!({"cmd": "list_calibrations"}));
    assert_eq!(r["ok"], json!(true));
    let cals = r["calibrations"].as_array().expect("calibrations array");
    let entry = cals.iter().find(|e| e["key"].as_str() == Some("out0_in0"))
        .expect("out0_in0 entry not in list");
    assert!(entry["mic_sensitivity_dbfs_at_94db_spl"].is_f64(),
        "list_calibrations entry missing mic_sensitivity field: {entry}");
    assert!(entry["mic_response"].is_object(),
        "list_calibrations entry missing mic_response: {entry}");
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

#[test]
fn calibrate_mic_curve_set_then_clear() {
    // End-to-end: upload a synthetic curve, verify cal entry is written,
    // verify the `loaded` count comes back; then `op = clear` and verify
    // the count drops to zero.
    let d = Daemon::spawn();
    let c = Client::new(&d);

    // Synthetic 32-point curve, log-spaced 100..10k Hz, +0..+3 dB ramp.
    let mut freqs = Vec::with_capacity(32);
    let mut gains = Vec::with_capacity(32);
    let log_min = 100.0_f64.ln();
    let log_max = 10_000.0_f64.ln();
    for i in 0..32 {
        let t = i as f64 / 31.0;
        freqs.push((log_min + t * (log_max - log_min)).exp());
        gains.push(3.0 * t);
    }

    let r = c.call(json!({
        "cmd":           "calibrate_mic_curve",
        "op":            "set",
        "input_channel": 1,
        "freqs_hz":      freqs,
        "gain_db":       gains,
        "source_path":   "/tmp/synthetic.frd",
    }));
    assert_eq!(r["ok"], json!(true), "set failed: {r}");
    assert_eq!(r["loaded"], json!(32));
    assert!(r["key"].as_str().unwrap_or("").contains("_in1"));

    // Sparse curve: should be rejected (under MIN_POINTS).
    let r = c.call(json!({
        "cmd":           "calibrate_mic_curve",
        "op":            "set",
        "input_channel": 1,
        "freqs_hz":      [100.0, 200.0, 300.0],
        "gain_db":       [0.0, 0.5, 1.0],
    }));
    assert_eq!(r["ok"], json!(false));
    assert!(r["error"].as_str().unwrap_or("").contains("too sparse"), "{r}");

    // Clear.
    let r = c.call(json!({
        "cmd":           "calibrate_mic_curve",
        "op":            "clear",
        "input_channel": 1,
    }));
    assert_eq!(r["ok"], json!(true));
    assert_eq!(r["loaded"], json!(0));
}

#[test]
fn set_mic_correction_enabled_round_trips() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({"cmd": "set_mic_correction_enabled", "enabled": false}));
    assert_eq!(r["ok"], json!(true));
    assert_eq!(r["enabled"], json!(false));
    let r = c.call(json!({"cmd": "set_mic_correction_enabled", "enabled": true}));
    assert_eq!(r["enabled"], json!(true));
}

#[test]
fn loudness_lkfs_drops_by_curve_db_when_mic_correction_on() {
    // #104 (Phase 6): with the per-sample inverse-curve FIR running
    // BEFORE K-weighting, a flat +3 dB mic-curve attenuates the audio
    // by 3 dB → LKFS / true_peak drop by 3 dB. Without the FIR the
    // LKFS would be unchanged from baseline (the cheap "tag-only"
    // alternative this issue rejected).
    fn last_loudness(c: &Client, dur_ms: u64) -> Value {
        let r = c.call(json!({"cmd": "monitor_spectrum", "freq_hz": 1000.0}));
        assert_eq!(r["ok"], json!(true));
        let deadline = Instant::now() + Duration::from_millis(dur_ms);
        let mut last: Option<Value> = None;
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now()).as_millis() as i32;
            match c.recv_pub(remaining.max(1)) {
                Some((t, v))
                    if t == "data" && v["type"] == json!("measurement/loudness")
                        && v["momentary_lkfs"].is_f64() =>
                {
                    last = Some(v);
                }
                Some(_) => continue,
                None    => break,
            }
        }
        let _ = c.call(json!({"cmd": "stop"}));
        // Drain trailing frames.
        let drain = Instant::now() + Duration::from_millis(300);
        while Instant::now() < drain {
            if c.recv_pub(50).is_none() { break; }
        }
        last.expect("no measurement/loudness frame with momentary_lkfs in window")
    }

    let d = Daemon::spawn();
    let c = Client::new(&d);

    // Baseline — no curve loaded.
    let baseline = last_loudness(&c, 1500);
    let baseline_lkfs = baseline["momentary_lkfs"].as_f64().unwrap();
    assert_eq!(baseline["mic_correction"], json!("none"),
        "baseline tag must be 'none': {baseline}");

    // Load a flat +3 dB mic-curve.
    let mut freqs = Vec::new();
    let mut gains = Vec::new();
    let log_min = 100.0_f64.ln();
    let log_max = 10_000.0_f64.ln();
    for i in 0..24 {
        let t = i as f64 / 23.0;
        freqs.push((log_min + t * (log_max - log_min)).exp());
        gains.push(3.0);
    }
    let r = c.call(json!({
        "cmd":           "calibrate_mic_curve",
        "op":            "set",
        "input_channel": 0,
        "freqs_hz":      freqs,
        "gain_db":       gains,
    }));
    assert_eq!(r["ok"], json!(true));

    // Drain anything that came in between monitor sessions.
    while c.recv_pub(50).is_some() {}

    // With curve loaded → FIR runs before K-weighting → LKFS drops.
    let corrected = last_loudness(&c, 1500);
    let corrected_lkfs = corrected["momentary_lkfs"].as_f64().unwrap();
    assert_eq!(corrected["mic_correction"], json!("on"),
        "corrected tag must be 'on': {corrected}");

    let delta = baseline_lkfs - corrected_lkfs;
    assert!(
        (delta - 3.0).abs() < 0.5,
        "expected ≈ 3 dB LKFS drop, got Δ={delta:.3} dB \
         (baseline={baseline_lkfs:.2}, corrected={corrected_lkfs:.2})"
    );
    // True-peak shifts the same way (FIR runs before the 4× polyphase
    // oversampler that produces dBTP).
    let baseline_tp  = baseline["true_peak_dbtp"].as_f64().unwrap_or(f64::NAN);
    let corrected_tp = corrected["true_peak_dbtp"].as_f64().unwrap_or(f64::NAN);
    if baseline_tp.is_finite() && corrected_tp.is_finite() {
        let tp_delta = baseline_tp - corrected_tp;
        assert!(
            (tp_delta - 3.0).abs() < 0.7,
            "expected ≈ 3 dB true-peak drop, got Δ={tp_delta:.3} dB"
        );
    }
}

#[test]
fn loudness_unchanged_when_mic_correction_toggled_off() {
    // Curve loaded but global toggle off → FIR bypassed, LKFS reads
    // the same as the no-curve baseline. Tag flips to "off".
    let d = Daemon::spawn();
    let c = Client::new(&d);

    // Baseline.
    let r = c.call(json!({"cmd": "monitor_spectrum", "freq_hz": 1000.0}));
    assert_eq!(r["ok"], json!(true));
    let baseline = {
        let deadline = Instant::now() + Duration::from_millis(1500);
        let mut last: Option<Value> = None;
        while Instant::now() < deadline {
            let r = deadline.saturating_duration_since(Instant::now()).as_millis() as i32;
            match c.recv_pub(r.max(1)) {
                Some((t, v))
                    if t == "data" && v["type"] == json!("measurement/loudness")
                        && v["momentary_lkfs"].is_f64() =>
                {
                    last = Some(v);
                }
                Some(_) => continue,
                None    => break,
            }
        }
        let _ = c.call(json!({"cmd": "stop"}));
        let drain = Instant::now() + Duration::from_millis(300);
        while Instant::now() < drain { if c.recv_pub(50).is_none() { break; } }
        last.expect("no baseline loudness frame")
    };
    let baseline_lkfs = baseline["momentary_lkfs"].as_f64().unwrap();

    // Load the curve, then disable the toggle.
    let mut freqs = Vec::new();
    let mut gains = Vec::new();
    let log_min = 100.0_f64.ln();
    let log_max = 10_000.0_f64.ln();
    for i in 0..24 {
        let t = i as f64 / 23.0;
        freqs.push((log_min + t * (log_max - log_min)).exp());
        gains.push(3.0);
    }
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

    // Re-run monitor; FIR is bypassed.
    let r = c.call(json!({"cmd": "monitor_spectrum", "freq_hz": 1000.0}));
    assert_eq!(r["ok"], json!(true));
    let off = {
        let deadline = Instant::now() + Duration::from_millis(1500);
        let mut last: Option<Value> = None;
        while Instant::now() < deadline {
            let r = deadline.saturating_duration_since(Instant::now()).as_millis() as i32;
            match c.recv_pub(r.max(1)) {
                Some((t, v))
                    if t == "data" && v["type"] == json!("measurement/loudness")
                        && v["momentary_lkfs"].is_f64() =>
                {
                    last = Some(v);
                }
                Some(_) => continue,
                None    => break,
            }
        }
        let _ = c.call(json!({"cmd": "stop"}));
        last.expect("no off-mode loudness frame")
    };
    let off_lkfs = off["momentary_lkfs"].as_f64().unwrap();
    assert_eq!(off["mic_correction"], json!("off"),
        "tag must be 'off' when toggle disables FIR: {off}");
    let delta = (baseline_lkfs - off_lkfs).abs();
    assert!(
        delta < 0.3,
        "FIR should be bypassed: expected LKFS ≈ baseline, Δ={delta:.3} dB"
    );
}

#[test]
fn set_analysis_mode_rejects_garbage() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({"cmd": "set_analysis_mode", "mode": "wavelet-of-doom"}));
    assert_eq!(r["ok"], json!(false));
    let err = r["error"].as_str().unwrap_or("");
    assert!(err.contains("invalid mode"), "got {err}");
}

#[test]
fn server_idle_timeout_disabled_keeps_public_bind() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    // Explicit null means "no timeout".
    let r = c.call(json!({
        "cmd": "setup",
        "update": {"server_idle_timeout_secs": Value::Null},
    }));
    assert_eq!(r["ok"], json!(true));
    assert_eq!(r["config"]["server_idle_timeout_secs"], Value::Null);

    let r = c.call(json!({"cmd": "server_enable"}));
    assert_eq!(r["ok"], json!(true));
    drop(c);

    thread::sleep(Duration::from_millis(2_500));

    // Reconnect — still public.
    thread::sleep(Duration::from_millis(200));
    let c2 = Client::new(&d);
    let s = c2.call(json!({"cmd": "status"}));
    assert_eq!(s["listen_mode"], json!("public"),
        "disabled timeout still auto-disabled public bind: {s}");
}
