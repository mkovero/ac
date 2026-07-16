//! Regenerator for `ac-scene`'s **genuinely daemon-emitted**
//! `transfer_stream` frame fixture (QA follow-up item 2 on handoff:
//! ac-scene M2).
//!
//! `ac-scene`'s other fixture (`tests/fixtures/transfer-frame-v2.json`)
//! is derived directly from the checked-in `.acsnap` via
//! `Snapshot::derive_pair`, deliberately — that's what makes the
//! wire-vs-snapshot equivalence test (AC4) sound, since both scenes
//! come from the same underlying data. But it means `ac-scene`'s
//! `WireFrame` deserializer has never actually parsed a frame that came
//! off a real ZMQ socket: field-name drift, JSON number formatting,
//! null handling, and tag-string vocabulary all go untested by a
//! fixture built from Rust struct literals. This regenerator captures
//! **one real DATA frame's raw bytes, verbatim**, from an actual
//! running `ac-daemon --fake-audio` session — the deserializer's actual
//! counterparty.
//!
//! `cargo test -p ac-daemon --test it_scene_fixture -- --ignored`

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU16, AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

static PORT_CURSOR: AtomicU16 = AtomicU16::new(28_400);
static HOME_CURSOR: AtomicU32 = AtomicU32::new(0);

fn alloc_ports() -> (u16, u16) {
    let base = PORT_CURSOR.fetch_add(2, Ordering::Relaxed);
    (base, base + 1)
}

fn alloc_home() -> PathBuf {
    let n = HOME_CURSOR.fetch_add(1, Ordering::Relaxed);
    let mut p = env::temp_dir();
    p.push(format!(
        "ac-daemon-scene-fixture-{}-{n}",
        std::process::id()
    ));
    let _ = fs::create_dir_all(p.join(".config").join("ac"));
    p
}

struct Daemon {
    child: Child,
    ctrl_port: u16,
    data_port: u16,
    home: PathBuf,
}

impl Daemon {
    fn spawn() -> Self {
        let home = alloc_home();
        let (ctrl, data) = alloc_ports();
        let bin = env!("CARGO_BIN_EXE_ac-daemon");
        let child = Command::new(bin)
            .env("HOME", &home)
            .args([
                "--fake-audio",
                "--local",
                "--ctrl-port",
                &ctrl.to_string(),
                "--data-port",
                &data.to_string(),
            ])
            .spawn()
            .expect("spawn ac-daemon");
        let deadline = Instant::now() + Duration::from_secs(3);
        let ctx = zmq::Context::new();
        loop {
            if Instant::now() > deadline {
                panic!("daemon never came up");
            }
            thread::sleep(Duration::from_millis(50));
            let s = ctx.socket(zmq::REQ).unwrap();
            s.set_linger(0).ok();
            s.set_rcvtimeo(300).ok();
            s.set_sndtimeo(300).ok();
            if s.connect(&format!("tcp://127.0.0.1:{ctrl}")).is_err() {
                continue;
            }
            if s.send(br#"{"cmd":"status"}"#.as_ref(), 0).is_err() {
                continue;
            }
            if s.recv_bytes(0).is_ok() {
                break;
            }
        }
        Self {
            child,
            ctrl_port: ctrl,
            data_port: data,
            home,
        }
    }

    fn ctrl_endpoint(&self) -> String {
        format!("tcp://127.0.0.1:{}", self.ctrl_port)
    }
    fn data_endpoint(&self) -> String {
        format!("tcp://127.0.0.1:{}", self.data_port)
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
    req: zmq::Socket,
    sub: zmq::Socket,
}

impl Client {
    fn new(d: &Daemon) -> Self {
        let ctx = zmq::Context::new();
        let req = ctx.socket(zmq::REQ).unwrap();
        req.set_linger(0).unwrap();
        req.set_rcvtimeo(5_000).unwrap();
        req.set_sndtimeo(5_000).unwrap();
        req.connect(&d.ctrl_endpoint()).unwrap();

        let sub = ctx.socket(zmq::SUB).unwrap();
        sub.set_linger(0).unwrap();
        sub.set_rcvtimeo(5_000).unwrap();
        sub.set_subscribe(b"").unwrap();
        sub.connect(&d.data_endpoint()).unwrap();

        thread::sleep(Duration::from_millis(100));
        Self {
            _ctx: ctx,
            req,
            sub,
        }
    }

    fn call(&self, cmd: Value) -> Value {
        self.req.send(serde_json::to_vec(&cmd).unwrap(), 0).unwrap();
        let bytes = self.req.recv_bytes(0).expect("CTRL recv");
        serde_json::from_slice(&bytes).expect("CTRL decode")
    }

    /// Raw PUB payload bytes (not re-encoded through `Value`) so the
    /// fixture is byte-for-byte what the daemon actually put on the
    /// wire, not a round-tripped reconstruction.
    fn recv_pub_raw(&self, timeout_ms: i32) -> Option<(String, Vec<u8>)> {
        self.sub.set_rcvtimeo(timeout_ms).ok();
        let bytes = self.sub.recv_bytes(0).ok()?;
        let split = bytes.iter().position(|&b| b == b' ')?;
        let topic = String::from_utf8(bytes[..split].to_vec()).ok()?;
        Some((topic, bytes[split + 1..].to_vec()))
    }

    fn wait_for_topic(&self, want: &str, timeout: Duration) -> Option<Value> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let remaining = deadline
                .saturating_duration_since(Instant::now())
                .as_millis() as i32;
            match self.recv_pub_raw(remaining.max(1)) {
                Some((t, payload)) if t == want => {
                    return serde_json::from_slice(&payload).ok();
                }
                Some(_) => continue,
                None => return None,
            }
        }
        None
    }
}

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

fn fixture_path() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../tests/fixtures/transfer-frame-v2-live.json"
    ))
}

#[test]
#[ignore = "regenerates tests/fixtures/transfer-frame-v2-live.json — run manually, needs a live daemon"]
fn generate_live_captured_frame_fixture() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    // Full cal stack loaded (voltage + SPL + mic curve) so the captured
    // frame's `cal_tags` exercises the "on" branch of every tag, not
    // just the "none" defaults — the whole point is to catch vocabulary
    // drift, which an all-"none" frame can't.
    let (freqs, gains) = synthetic_curve_flat(3.0);
    let r = c.call(json!({
        "cmd": "calibrate_mic_curve", "op": "set", "input_channel": 0,
        "freqs_hz": freqs, "gain_db": gains,
    }));
    assert_eq!(r["ok"], json!(true), "calibrate_mic_curve: {r}");

    let r = c.call(json!({"cmd": "calibrate", "ref_dbfs": -10.0,
                           "output_channel": 0, "input_channel": 0}));
    assert_eq!(r["ok"], json!(true), "calibrate start: {r}");
    c.wait_for_topic("cal_prompt", Duration::from_secs(3))
        .expect("voltage cal step 1 prompt");
    let _ = c.call(json!({"cmd": "cal_reply", "vrms": 2.0}));
    c.wait_for_topic("cal_prompt", Duration::from_secs(3))
        .expect("voltage cal step 2 prompt");
    let _ = c.call(json!({"cmd": "cal_reply", "vrms": 2.0}));
    c.wait_for_topic("cal_done", Duration::from_secs(5))
        .expect("voltage cal_done");

    let r = c.call(json!({"cmd": "calibrate_spl", "input_channel": 0, "capture_s": 0.05}));
    assert_eq!(r["ok"], json!(true), "calibrate_spl: {r}");
    c.wait_for_topic("cal_prompt", Duration::from_secs(3))
        .expect("spl cal_prompt");
    let _ = c.call(json!({"cmd": "cal_reply", "vrms": Value::Null}));
    c.wait_for_topic("cal_done", Duration::from_secs(5))
        .expect("spl cal_done");
    while c.recv_pub_raw(50).is_some() {}

    let r = c.call(json!({
        "cmd": "transfer_stream", "meas_channel": 0, "ref_channel": 1,
        "weighting": "A", "integration": "fast",
        "fake_correlated_pair": {"gain": 0.5, "delay_samples": 200},
    }));
    assert_eq!(r["ok"], json!(true), "transfer_stream start: {r}");
    thread::sleep(Duration::from_secs_f64(3.3));

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut raw_frame: Option<Vec<u8>> = None;
    while Instant::now() < deadline {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub_raw(remaining.max(1)) {
            Some((t, payload)) if t == "data" => {
                let v: Value = serde_json::from_slice(&payload).unwrap_or(Value::Null);
                if v["type"] == json!("transfer_stream") {
                    raw_frame = Some(payload);
                    break;
                }
            }
            Some(_) => continue,
            None => break,
        }
    }
    let _ = c.call(json!({"cmd": "stop"}));
    let raw_frame = raw_frame.expect("no transfer_stream frame within 10 s");

    // Sanity: SPL cal is loaded, so this should be a real number, not
    // null — a fixture with an accidentally-null spl would defeat the
    // whole point of loading full cal state above.
    let parsed: Value = serde_json::from_slice(&raw_frame).unwrap();
    assert!(
        parsed["spl"].is_number(),
        "fixture's spl must be a number (SPL cal was loaded): {parsed}"
    );
    assert_eq!(parsed["cal_tags"]["meas"]["voltage"], json!("on"));
    assert_eq!(parsed["cal_tags"]["meas"]["spl"], json!("on"));
    assert_eq!(parsed["cal_tags"]["meas"]["mic_curve"], json!("on"));

    fs::write(fixture_path(), &raw_frame).expect("write fixture file");
    eprintln!(
        "wrote {} ({} bytes, verbatim off-wire DATA payload)",
        fixture_path().display(),
        raw_frame.len()
    );
}
