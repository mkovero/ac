//! ZMQ integration tests for `set_drive` and the §4.2 raw input peaks
//! (#183, M4d-daemon).
//!
//! # Why published peaks are the drive observable
//!
//! The fake backend's capture is derived from its own stimulus
//! (`audio/fake.rs::make_samples_for`), so changing what the engine
//! drives changes what the session captures. A published
//! `meas_peak_dbfs` moving from the idle ≈ −20 dBFS (the fake default
//! 0.1-amplitude tone) to ≈ −10 dBFS (a `set_drive` at the −10 ceiling)
//! is therefore direct evidence that the worker acted on the drive
//! state — not a proxy for it. `set_silence` on the fake backend
//! restores that default tone rather than true digital silence, which
//! is why the assertions below are "returned to the idle level", not
//! "went to −inf".

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU16, AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

// 28_800, not 28_400 — the latter collides with it_scene_fixture's base
// under parallel `cargo test` (#195). Each ac-daemon test binary keeps a
// distinct base rather than the ac-view PID-seed approach, since these
// bases are inline per binary, not a shared module.
static PORT_CURSOR: AtomicU16 = AtomicU16::new(28_800);
static HOME_CURSOR: AtomicU32 = AtomicU32::new(0);

/// The default `drive_max_dbfs` ceiling (`ac_core::config`).
const CEILING_DBFS: f64 = -10.0;
/// Idle fake stimulus is a 0.1-amplitude tone ⇒ 20·log10(0.1) ≈ −20 dBFS.
const IDLE_PEAK_DBFS: f64 = -20.0;

fn alloc_ports() -> (u16, u16) {
    let base = PORT_CURSOR.fetch_add(2, Ordering::Relaxed);
    (base, base + 1)
}

fn alloc_home() -> PathBuf {
    let n = HOME_CURSOR.fetch_add(1, Ordering::Relaxed);
    let mut p = env::temp_dir();
    p.push(format!("ac-daemon-drive-it-{}-{n}", std::process::id()));
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

        let deadline = Instant::now() + Duration::from_secs(5);
        let ctx = zmq::Context::new();
        loop {
            assert!(Instant::now() <= deadline, "daemon never came up");
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
        req.connect(&format!("tcp://127.0.0.1:{}", d.ctrl_port))
            .unwrap();

        let sub = ctx.socket(zmq::SUB).unwrap();
        sub.set_linger(0).unwrap();
        sub.set_rcvtimeo(5_000).unwrap();
        sub.set_subscribe(b"").unwrap();
        sub.connect(&format!("tcp://127.0.0.1:{}", d.data_port))
            .unwrap();

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

    /// Next `transfer_stream` DATA frame, or `None` on timeout.
    fn next_frame(&self, timeout: Duration) -> Option<Value> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let remaining = deadline
                .saturating_duration_since(Instant::now())
                .as_millis()
                .max(1) as i32;
            self.sub.set_rcvtimeo(remaining).ok();
            let bytes = self.sub.recv_bytes(0).ok()?;
            let split = bytes.iter().position(|&b| b == b' ')?;
            let topic = String::from_utf8(bytes[..split].to_vec()).ok()?;
            let payload: Value = serde_json::from_slice(&bytes[split + 1..]).ok()?;
            if topic == "data" && payload["type"] == json!("transfer_stream") {
                return Some(payload);
            }
        }
        None
    }

    /// Drain frames for `settle`, then return the next one — so an
    /// assertion sees the state after a drive change rather than a frame
    /// computed from blocks captured before it.
    fn frame_after(&self, settle: Duration) -> Value {
        let until = Instant::now() + settle;
        while Instant::now() < until {
            let _ = self.next_frame(Duration::from_millis(300));
        }
        self.next_frame(Duration::from_secs(10))
            .expect("no transfer_stream frame")
    }
}

fn start_transfer(c: &Client) -> Value {
    c.call(json!({
        "cmd": "transfer_stream", "meas_channel": 0, "ref_channel": 1,
        "weighting": "Z", "integration": "fast",
    }))
}

fn peak(frame: &Value, key: &str) -> Option<f64> {
    match &frame[key] {
        Value::Null => None,
        v => Some(
            v.as_f64()
                .unwrap_or_else(|| panic!("{key} not a number: {v}")),
        ),
    }
}

// ---------------------------------------------------------------------
// Preconditions and schema
// ---------------------------------------------------------------------

#[test]
fn set_drive_without_a_session_is_refused_not_a_panic() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let r = c.call(json!({"cmd": "set_drive", "on": true, "level_dbfs": -20.0}));
    assert_eq!(r["ok"], json!(false), "{r}");
    assert_eq!(r["error"], json!("no transfer_stream session running"));

    // The daemon is still alive and answering afterwards.
    assert_eq!(c.call(json!({"cmd": "status"}))["ok"], json!(true));
}

#[test]
fn set_drive_requires_on_and_a_finite_level() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    assert_eq!(start_transfer(&c)["ok"], json!(true));

    // Missing `on`.
    let r = c.call(json!({"cmd": "set_drive", "level_dbfs": -20.0}));
    assert_eq!(r["ok"], json!(false), "{r}");

    // Missing level — required even though `on` is present, because
    // every message is a full state assertion.
    let r = c.call(json!({"cmd": "set_drive", "on": true}));
    assert_eq!(r["ok"], json!(false), "{r}");

    // Missing level on an OFF request is equally refused.
    let r = c.call(json!({"cmd": "set_drive", "on": false}));
    assert_eq!(r["ok"], json!(false), "{r}");

    // Non-numeric level is a client bug, not something to coerce.
    let r = c.call(json!({"cmd": "set_drive", "on": true, "level_dbfs": "loud"}));
    assert_eq!(r["ok"], json!(false), "{r}");
}

#[test]
fn server_clamps_to_the_ceiling_and_echoes_the_applied_level() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    assert_eq!(start_transfer(&c)["ok"], json!(true));

    // Request −3 dBFS against the −10 ceiling. A clamp is SUCCESS, not a
    // partial failure — and the echo is the applied value, so the client
    // can see what actually happened.
    let r = c.call(json!({"cmd": "set_drive", "on": true, "level_dbfs": -3.0}));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(r["on"], json!(true));
    assert_eq!(r["level_dbfs"], json!(CEILING_DBFS));

    // Below the ceiling passes through untouched.
    let r = c.call(json!({"cmd": "set_drive", "on": true, "level_dbfs": -25.0}));
    assert_eq!(r["level_dbfs"], json!(-25.0), "{r}");

    // The clamp applies on `off` too — the level survives a stop/start
    // cycle without a round trip to read it back.
    let r = c.call(json!({"cmd": "set_drive", "on": false, "level_dbfs": 6.0}));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(r["on"], json!(false));
    assert_eq!(r["level_dbfs"], json!(CEILING_DBFS));
}

// ---------------------------------------------------------------------
// Busy-guard bypass — the safety finding from #180's architect pass
// ---------------------------------------------------------------------

#[test]
fn set_drive_bypasses_the_busy_guard_it_would_otherwise_contend_with() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    assert_eq!(start_transfer(&c)["ok"], json!(true));

    // A second transfer_stream IS refused by the busy guard — this
    // establishes that the guard is actually engaged for this session,
    // so the set_drive results below are a bypass rather than an idle
    // guard that would have let anything through.
    let busy = start_transfer(&c);
    assert_eq!(busy["ok"], json!(false), "guard not engaged: {busy}");

    // set_drive, targeting that same busy worker, succeeds anyway.
    let on = c.call(json!({"cmd": "set_drive", "on": true, "level_dbfs": -20.0}));
    assert_eq!(on["ok"], json!(true), "{on}");

    // And the panic-stop direction — the one that must never block on
    // the worker it is stopping — succeeds too.
    let off = c.call(json!({"cmd": "set_drive", "on": false, "level_dbfs": -20.0}));
    assert_eq!(off["ok"], json!(true), "{off}");
}

// ---------------------------------------------------------------------
// §4.2 raw input peaks
// ---------------------------------------------------------------------

#[test]
fn frames_carry_raw_input_peaks_for_both_channels() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    assert_eq!(start_transfer(&c)["ok"], json!(true));

    let f = c.frame_after(Duration::from_millis(1_500));

    let m = peak(&f, "meas_peak_dbfs").expect("meas peak present");
    let r = peak(&f, "ref_peak_dbfs").expect("ref peak present");

    // Idle fake stimulus is a 0.1-amplitude tone on both channels.
    for (name, v) in [("meas", m), ("ref", r)] {
        assert!(
            (v - IDLE_PEAK_DBFS).abs() < 1.0,
            "{name} peak {v} not near the idle {IDLE_PEAK_DBFS} dBFS"
        );
        assert!(v <= 0.0, "{name} peak {v} exceeds full scale");
    }
}

/// Swapped-channel detection — the AC this actually kills. Both channels
/// at the idle level (as above) cannot fail on a meas/ref swap, because
/// the swap produces the same co-located peaks. `fake_correlated_pair`
/// makes the channels asymmetric on purpose: ref is the source, meas is
/// `gain × delayed(source)`. With gain 0.5, meas must sit ≈ 6 dB BELOW
/// ref — and the SIGN of that difference is what a swap flips.
#[test]
fn peaks_distinguish_meas_from_ref_so_a_swap_would_fail() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd": "transfer_stream", "meas_channel": 0, "ref_channel": 1,
        "weighting": "Z", "integration": "fast",
        "fake_correlated_pair": {"gain": 0.5, "delay_samples": 120},
    }));
    assert_eq!(r["ok"], json!(true), "{r}");

    let f = c.frame_after(Duration::from_millis(1_500));
    let m = peak(&f, "meas_peak_dbfs").expect("meas peak");
    let rf = peak(&f, "ref_peak_dbfs").expect("ref peak");

    // gain 0.5 ⇒ meas is ~6 dB quieter than ref. Assert the sign and a
    // generous magnitude band (the uniform source's per-block max is not
    // exactly full scale, so the gap is ~6 dB but not to the decimal).
    assert!(
        rf - m > 3.0,
        "ref should be clearly louder than meas (gain 0.5): ref {rf}, meas {m} — \
         a meas/ref swap would invert this"
    );
    assert!(
        rf - m < 9.0,
        "ref−meas {} dB is not the expected ~6 dB for gain 0.5: ref {rf}, meas {m}",
        rf - m
    );
}

/// The peaks must come from the raw capture blocks, before any
/// calibration or aggregation. A voltage-calibrated session moves
/// `meas_spectrum` (and the H1 magnitude) but must leave the meter's
/// peak exactly where an uncalibrated session put it — meters exist to
/// judge gain staging, and a calibrated value hides clipping.
#[test]
fn peaks_are_raw_and_do_not_follow_a_voltage_calibration() {
    fn meas_peak_with_cal(cal_json: Option<&str>) -> f64 {
        let d = Daemon::spawn();
        if let Some(body) = cal_json {
            // Write cal.json directly rather than driving the `calibrate`
            // state machine: that command spawns an Exclusive worker, and
            // the busy guard would then refuse the transfer session this
            // test needs. The daemon reads this file at session start
            // (`Calibration::load` in the transfer handler).
            let dir = d.home.join(".config").join("ac");
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("cal.json"), body).unwrap();
        }
        let c = Client::new(&d);
        assert_eq!(start_transfer(&c)["ok"], json!(true));
        let f = c.frame_after(Duration::from_millis(1_500));
        // A calibrated session must still be a calibrated session — if
        // the fixture silently failed to apply, this test would compare
        // two identical uncalibrated runs and pass for the wrong reason.
        if cal_json.is_some() {
            assert_eq!(
                f["cal_tags"]["meas"]["voltage"],
                json!("on"),
                "voltage calibration did not apply — test would be vacuous"
            );
        }
        peak(&f, "meas_peak_dbfs").expect("meas peak")
    }

    let uncal = meas_peak_with_cal(None);
    // 10 Vrms at 0 dBFS — a 20 dB-scale change to every calibrated
    // value in the frame.
    let cal = meas_peak_with_cal(Some(
        r#"{"out0_in0": {"output_channel": 0, "input_channel": 0, "vrms_at_0dbfs_in": 10.0}}"#,
    ));

    assert!(
        (uncal - cal).abs() < 0.5,
        "peak moved with calibration: uncal {uncal}, cal {cal} — \
         peaks must be taken from raw capture blocks, before calibration"
    );
}

// ---------------------------------------------------------------------
// Drive lifecycle, live level change, dead-man
// ---------------------------------------------------------------------

#[test]
fn drive_on_raises_the_captured_level_and_off_returns_it_within_one_frame() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    assert_eq!(start_transfer(&c)["ok"], json!(true));
    let _ = c.frame_after(Duration::from_millis(1_200));

    // On at the ceiling: amplitude 10^(−10/20) ≈ 0.316 ⇒ ≈ −10 dBFS.
    assert_eq!(
        c.call(json!({"cmd": "set_drive", "on": true, "level_dbfs": CEILING_DBFS}))["ok"],
        json!(true)
    );
    let driving =
        peak(&c.frame_after(Duration::from_millis(900)), "meas_peak_dbfs").expect("driving peak");
    assert!(
        (driving - CEILING_DBFS).abs() < 2.0,
        "driving peak {driving} not near {CEILING_DBFS} dBFS"
    );

    // Off: back to the idle stimulus level.
    assert_eq!(
        c.call(json!({"cmd": "set_drive", "on": false, "level_dbfs": CEILING_DBFS}))["ok"],
        json!(true)
    );
    let stopped =
        peak(&c.frame_after(Duration::from_millis(900)), "meas_peak_dbfs").expect("stopped peak");
    assert!(
        (stopped - IDLE_PEAK_DBFS).abs() < 1.5,
        "peak {stopped} did not return to idle {IDLE_PEAK_DBFS} after set_drive off"
    );
}

#[test]
fn level_changes_take_effect_without_restarting_the_session() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    assert_eq!(start_transfer(&c)["ok"], json!(true));
    let _ = c.frame_after(Duration::from_millis(1_200));

    c.call(json!({"cmd": "set_drive", "on": true, "level_dbfs": -30.0}));
    let quiet =
        peak(&c.frame_after(Duration::from_millis(900)), "meas_peak_dbfs").expect("quiet peak");

    // Keep driving, just louder — no stop, no session restart.
    c.call(json!({"cmd": "set_drive", "on": true, "level_dbfs": CEILING_DBFS}));
    let loud =
        peak(&c.frame_after(Duration::from_millis(900)), "meas_peak_dbfs").expect("loud peak");

    assert!(
        loud - quiet > 10.0,
        "level change did not follow: {quiet} → {loud} dBFS"
    );
}

/// Dead-man: drive drops after 1.5 s of keepalive silence, and the
/// SESSION KEEPS RUNNING. Both halves matter — a dead-man that killed
/// the session would lose the operator's measurement along with the
/// stimulus.
#[test]
fn dead_man_drops_drive_after_keepalive_silence_but_keeps_the_session() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    assert_eq!(start_transfer(&c)["ok"], json!(true));
    let _ = c.frame_after(Duration::from_millis(1_200));

    assert_eq!(
        c.call(json!({"cmd": "set_drive", "on": true, "level_dbfs": CEILING_DBFS}))["ok"],
        json!(true)
    );
    let driving =
        peak(&c.frame_after(Duration::from_millis(900)), "meas_peak_dbfs").expect("driving peak");
    assert!(
        (driving - CEILING_DBFS).abs() < 2.0,
        "not driving: {driving}"
    );

    // No CTRL traffic at all for 1.6 s — the timeout must be evaluated
    // on the worker's own poll, not on message arrival.
    thread::sleep(Duration::from_millis(1_600));

    let after = c.frame_after(Duration::from_millis(600));
    let dropped = peak(&after, "meas_peak_dbfs").expect("post-dead-man peak");
    assert!(
        (dropped - IDLE_PEAK_DBFS).abs() < 1.5,
        "drive did not drop after keepalive silence: {dropped} dBFS"
    );

    // Session still publishing, and still answering CTRL.
    assert!(
        after["type"] == json!("transfer_stream"),
        "session stopped publishing"
    );
    assert_eq!(c.call(json!({"cmd": "status"}))["ok"], json!(true));
}

/// Dead-man LOWER bound: drive must still be up well before the 1.5 s
/// window. A timeout accidentally set too short (e.g. 1000 ms) would
/// pass every other test in this file but fail here.
#[test]
fn drive_survives_up_to_the_dead_man_window() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    assert_eq!(start_transfer(&c)["ok"], json!(true));
    let _ = c.frame_after(Duration::from_millis(1_200));

    assert_eq!(
        c.call(json!({"cmd": "set_drive", "on": true, "level_dbfs": CEILING_DBFS}))["ok"],
        json!(true)
    );

    // 1.2 s of silence — inside the 1.5 s window, so drive must persist.
    thread::sleep(Duration::from_millis(1_200));

    let still = peak(&c.frame_after(Duration::from_millis(200)), "meas_peak_dbfs")
        .expect("peak before the window");
    assert!(
        (still - CEILING_DBFS).abs() < 2.0,
        "drive dropped early ({still} dBFS at 1.2 s) — dead-man window too short"
    );
}

/// An idempotent resend keeps drive alive indefinitely — this is what
/// makes the 250 ms resend a keepalive without a second command.
#[test]
fn idempotent_resends_hold_drive_past_the_dead_man_window() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    assert_eq!(start_transfer(&c)["ok"], json!(true));
    let _ = c.frame_after(Duration::from_millis(1_200));

    let msg = json!({"cmd": "set_drive", "on": true, "level_dbfs": CEILING_DBFS});
    c.call(msg.clone());

    // Byte-identical resends across a window well past 1.5 s.
    for _ in 0..10 {
        thread::sleep(Duration::from_millis(250));
        let r = c.call(msg.clone());
        assert_eq!(r["ok"], json!(true), "resend refused: {r}");
        assert_eq!(r["level_dbfs"], json!(CEILING_DBFS));
    }

    let still = peak(&c.frame_after(Duration::from_millis(300)), "meas_peak_dbfs")
        .expect("peak while resending");
    assert!(
        (still - CEILING_DBFS).abs() < 2.0,
        "resends did not hold drive: {still} dBFS"
    );
}

/// A session launched with the legacy `drive: true` param sends no
/// keepalives — the dead-man must not silence it, or every scripted
/// caller breaks. The dead-man arms on the first `set_drive`.
#[test]
fn legacy_launch_time_drive_is_not_killed_by_the_dead_man() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd": "transfer_stream", "meas_channel": 0, "ref_channel": 1,
        "weighting": "Z", "integration": "fast",
        "drive": true, "level_dbfs": CEILING_DBFS,
    }));
    assert_eq!(r["ok"], json!(true), "{r}");

    // Well past the dead-man window, with no CTRL traffic at all.
    thread::sleep(Duration::from_millis(2_000));

    let p = peak(&c.frame_after(Duration::from_millis(600)), "meas_peak_dbfs")
        .expect("peak on a launch-driven session");
    assert!(
        (p - CEILING_DBFS).abs() < 2.0,
        "legacy launch-time drive was silenced by the dead-man: {p} dBFS"
    );
}

// ---------------------------------------------------------------------
// Observed drive state on the wire (#228)
//
// The indicator #228 builds needs to know whether the daemon is emitting.
// A client's own last `set_drive` is not that: the dead-man can silence a
// drive the client still believes is up, and the clamp can lower a level
// the client still believes it set. These assert the published state
// follows the engine, not the request.
// ---------------------------------------------------------------------

fn drive_state(frame: &Value) -> &Value {
    &frame["drive"]
}

#[test]
fn frames_report_the_applied_drive_level_not_the_requested_one() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd": "transfer_stream", "meas_channel": 0, "ref_channel": 1,
        "weighting": "Z", "integration": "fast", "drivable": true,
    }));
    assert_eq!(r["ok"], json!(true), "{r}");

    let idle = c.frame_after(Duration::from_millis(900));
    assert_eq!(drive_state(&idle)["on"], json!(false));
    assert_eq!(
        drive_state(&idle)["level_dbfs"],
        Value::Null,
        "level must be null while off, not a stale number to misread"
    );
    assert_eq!(
        drive_state(&idle)["drivable"],
        json!(true),
        "a drivable session that is silent must be distinguishable from one \
         that never drives"
    );

    // Ask for 6 dB above the ceiling. The frame must carry what the engine
    // was given, not what the client asked for — a drive clamped to
    // something inaudible is a real measurement with a bad SNR, and an
    // indicator reading the request would call it healthy.
    let requested = CEILING_DBFS + 6.0;
    assert_eq!(
        c.call(json!({"cmd": "set_drive", "on": true, "level_dbfs": requested}))["ok"],
        json!(true)
    );
    let driving = c.frame_after(Duration::from_millis(900));
    assert_eq!(drive_state(&driving)["on"], json!(true));
    let applied = drive_state(&driving)["level_dbfs"]
        .as_f64()
        .expect("level_dbfs while driving");
    assert!(
        (applied - CEILING_DBFS).abs() < 1e-9,
        "frame must report the applied (clamped) level {CEILING_DBFS}, got {applied}"
    );
}

#[test]
fn the_published_drive_state_follows_the_dead_man_not_the_last_request() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd": "transfer_stream", "meas_channel": 0, "ref_channel": 1,
        "weighting": "Z", "integration": "fast", "drivable": true,
    }));
    assert_eq!(r["ok"], json!(true), "{r}");
    let _ = c.frame_after(Duration::from_millis(900));

    assert_eq!(
        c.call(json!({"cmd": "set_drive", "on": true, "level_dbfs": CEILING_DBFS}))["ok"],
        json!(true)
    );
    assert_eq!(
        drive_state(&c.frame_after(Duration::from_millis(900)))["on"],
        json!(true)
    );

    // No keepalive for longer than the dead-man window. The client's last
    // request still says "on"; the engine is silent. The frame must say
    // silent, or an indicator built on it reports a drive that is not there.
    thread::sleep(Duration::from_millis(1_600));
    let after = c.frame_after(Duration::from_millis(600));
    assert_eq!(
        drive_state(&after)["on"],
        json!(false),
        "published drive state still says on after the dead-man expired it"
    );
    assert_eq!(drive_state(&after)["level_dbfs"], Value::Null);
}

#[test]
fn a_passive_session_reports_that_it_cannot_drive() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    assert_eq!(start_transfer(&c)["ok"], json!(true));

    let f = c.frame_after(Duration::from_millis(900));
    assert_eq!(
        drive_state(&f)["drivable"],
        json!(false),
        "an external-DUT session opens no output ports; silence from the \
         daemon says nothing about whether signal is present"
    );
    assert_eq!(drive_state(&f)["on"], json!(false));
}
