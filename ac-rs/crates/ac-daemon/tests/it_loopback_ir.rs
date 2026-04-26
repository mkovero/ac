//! `#[ignore]`'d JACK-loopback integration test for `sweep_ir`.
//!
//! Runs a Farina exponential sweep through the daemon's real `JackEngine`
//! with the JACK output port connected to the JACK input port, then asserts
//! the recovered linear IR has a sharp dominant peak well above the
//! pre-impulse floor.
//!
//! This test is `#[ignore]`'d so it does not run as part of `cargo test`.
//! It needs a live JACK server. See `ARCHITECTURE.md` → "Testing strategy"
//! → "Loopback IR runbook" for invocation.
//!
//! The internal loopback works because both the daemon's output and input
//! ports are registered under the same JACK client (`ac-daemon`). Setting
//! `output_port = "ac-daemon:in"` makes `JackEngine::start()` connect
//! `ac-daemon:out → ac-daemon:in` directly — no external `jack_connect`
//! and no system audio devices required (works with `jackd -d dummy`).

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU16, AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

static PORT_CURSOR: AtomicU16 = AtomicU16::new(25_900);
static HOME_CURSOR: AtomicU32 = AtomicU32::new(0);

fn alloc_ports() -> (u16, u16) {
    let base = PORT_CURSOR.fetch_add(2, Ordering::Relaxed);
    (base, base + 1)
}

fn alloc_home() -> PathBuf {
    let n = HOME_CURSOR.fetch_add(1, Ordering::Relaxed);
    let mut p = env::temp_dir();
    p.push(format!("ac-daemon-loopback-{}-{n}", std::process::id()));
    let _ = fs::create_dir_all(p.join(".config").join("ac"));
    p
}

/// Pre-write `$HOME/.config/ac/config.json` so the daemon picks up sticky
/// port names that self-loop the JACK client (`ac-daemon:out → ac-daemon:in`).
fn write_loopback_config(home: &PathBuf) {
    let cfg = json!({
        "device":           0,
        "output_channel":   0,
        "input_channel":    0,
        "output_port":      "ac-daemon:in",
        "input_port":       "ac-daemon:out",
        "dbu_ref_vrms":     0.7745966692414834,
        "range_start_hz":   20.0,
        "range_stop_hz":    20_000.0,
        "server_enabled":   false,
    });
    let path = home.join(".config").join("ac").join("config.json");
    fs::write(&path, serde_json::to_vec_pretty(&cfg).unwrap()).expect("write config");
}

struct Daemon {
    child:     Child,
    ctrl_port: u16,
    data_port: u16,
    home:      PathBuf,
}

impl Daemon {
    fn spawn_jack() -> Self {
        let (ctrl, data) = alloc_ports();
        let home = alloc_home();
        write_loopback_config(&home);

        let bin = env!("CARGO_BIN_EXE_ac-daemon");
        let child = Command::new(bin)
            .env("HOME", &home)
            .args([
                "--local",
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
        sub.set_rcvtimeo(10_000).unwrap();
        sub.set_subscribe(b"").unwrap();
        sub.connect(&d.data_endpoint()).unwrap();

        thread::sleep(Duration::from_millis(100));
        Self { _ctx: ctx, req, sub }
    }

    fn call(&self, cmd: Value) -> Value {
        let raw = serde_json::to_vec(&cmd).unwrap();
        self.req.send(raw, 0).unwrap();
        let bytes = self.req.recv_bytes(0).expect("CTRL recv");
        serde_json::from_slice(&bytes).expect("CTRL decode")
    }

    fn recv_pub(&self, timeout_ms: i32) -> Option<(String, Value)> {
        self.sub.set_rcvtimeo(timeout_ms).ok();
        let bytes = self.sub.recv_bytes(0).ok()?;
        let split = bytes.iter().position(|&b| b == b' ')?;
        let topic = String::from_utf8(bytes[..split].to_vec()).ok()?;
        let payload = &bytes[split + 1..];
        let v: Value = serde_json::from_slice(payload).unwrap_or(Value::Null);
        Some((topic, v))
    }

    /// Wait for a frame on `want_topic`, or fail loudly on `error` (so missing
    /// JACK is reported with the engine's own message instead of a bare timeout).
    fn wait_for_or_error(&self, want_topic: &str, timeout: Duration) -> Value {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now()).as_millis() as i32;
            match self.recv_pub(remaining.max(1)) {
                Some((t, v)) if t == want_topic => return v,
                Some((t, v)) if t == "error" => {
                    let msg = v.get("message").and_then(Value::as_str).unwrap_or("(no message)");
                    panic!("daemon error before {want_topic}: {msg}");
                }
                Some(_) => continue,
                None    => panic!("timeout waiting for {want_topic}"),
            }
        }
        panic!("deadline waiting for {want_topic}");
    }
}

#[test]
#[ignore = "needs a live JACK server — see ARCHITECTURE.md"]
fn loopback_ir_recovers_sharp_peak() {
    let d = Daemon::spawn_jack();
    let c = Client::new(&d);

    // Short sweep so the test stays under ~1 s of audio time. `window_len`
    // is sized to comfortably contain both the gate centre (placed by
    // `extract_irs` at the sweep endpoint) and the JACK round-trip latency
    // shift (one JACK period for a self-connected client) plus a wide
    // pre-impulse stretch that's clear of the bandlimited-sinc skirts.
    let ack = c.call(json!({
        "cmd":        "sweep_ir",
        "f1_hz":      50.0,
        "f2_hz":      16_000.0,
        "duration":   0.5,
        "level_dbfs": -6.0,
        "tail_s":     0.2,
        "n_harmonics": 3,
        "window_len":  16_384,
    }));
    assert_eq!(ack["ok"], json!(true), "sweep_ir REQ rejected: {ack}");

    let frame = c.wait_for_or_error("measurement/impulse_response", Duration::from_secs(15));
    let data = &frame["data"];
    let ir: Vec<f64> = data["linear_ir"]
        .as_array()
        .expect("linear_ir array")
        .iter()
        .map(|v| v.as_f64().expect("linear_ir element f64"))
        .collect();
    assert!(ir.len() >= 256, "linear_ir suspiciously short: {}", ir.len());

    // Peak position and magnitude.
    let (peak_idx, peak_abs) = ir.iter().enumerate().fold((0usize, 0.0f64), |acc, (i, v)| {
        let a = v.abs();
        if a > acc.1 { (i, a) } else { acc }
    });
    assert!(peak_abs > 0.0, "all-zero IR — JACK loopback never delivered audio");

    // The IR peak is the deconvolution delta. `extract_irs` centers the gate
    // at the sweep endpoint, so the peak nominally sits at `window_len / 2`
    // — but JACK adds one period of port-to-port latency, shifting it later
    // by a few hundred to a couple thousand samples. Just require the peak
    // to land in the upper-middle half of the window (well clear of the
    // edges where a truncation artefact would manifest).
    let lo_bound = ir.len() / 4;
    let hi_bound = 3 * ir.len() / 4;
    assert!(
        peak_idx > lo_bound && peak_idx < hi_bound,
        "peak at index {peak_idx} outside expected range [{lo_bound}, {hi_bound}] \
         (window_len={}); deconvolution may have failed",
        ir.len()
    );

    // Floor: max-abs over the leading 1/8 of the IR window. With window_len
    // 16384, that's ~3000 samples ≥ 6000 samples ahead of the peak — far
    // enough that the bandlimited-sinc skirts of the delta have decayed
    // below the noise on a clean loopback.
    let far_end = ir.len() / 8;
    let floor = ir[..far_end].iter().map(|v| v.abs()).fold(0.0_f64, f64::max);
    let snr_db = 20.0 * (peak_abs / floor.max(1e-15)).log10();
    assert!(
        snr_db >= 40.0,
        "loopback IR floor too high: peak={peak_abs:.3e}, far_max={floor:.3e}, \
         SNR={snr_db:.1} dB (need ≥ 40 dB)"
    );

    // Drain the trailing report + done frames so Drop doesn't race on shutdown.
    let _ = c.recv_pub(2_000);
    let _ = c.recv_pub(2_000);
}
