//! ZMQ integration tests for `relock` and the drive off→on flush (#226).
//!
//! `fake_correlated_pair` (see `it_set_drive.rs`'s doc for why it, not the
//! plain idle tone) gives a deterministic delay to lock onto — the fake
//! backend's correlated-pair generator is a seeded pseudorandom stream
//! (`audio/fake.rs::correlated_source_at`), so re-estimating against the
//! same configured `delay_samples` reliably reproduces the same lock. That
//! determinism is what lets these tests assert "the new lag equals the
//! old" rather than merely "some lag exists" — a plain idle-tone pair could
//! not distinguish a re-lock that worked from one that silently failed and
//! defaulted to 0, since a digital loopback's own genuine delay is also 0.
//! It also means the correlated signal on the capture side is independent
//! of the daemon's own output stage, so toggling `set_drive` here changes
//! only `engine_on`/the drive-edge trigger, never the thing being locked
//! onto.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU16, AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

// 29_200 — distinct from every other `ac-daemon` IT file's base
// (25_600 / 25_900 / 26_400 / 27_000 / 28_400 / 28_800), so parallel
// `cargo test` never collides (#195).
static PORT_CURSOR: AtomicU16 = AtomicU16::new(29_200);
static HOME_CURSOR: AtomicU32 = AtomicU32::new(0);

const CEILING_DBFS: f64 = -10.0;
/// The configured `fake_correlated_pair` delay most tests lock onto.
const LOCK_DELAY_SAMPLES: i64 = 400;
/// Generous — a throttled CI runner's FFT/rayon fan-out can fall well
/// behind realtime under load; these tests care about correctness of the
/// transition, not its latency.
const LOCK_TIMEOUT: Duration = Duration::from_secs(20);

fn alloc_ports() -> (u16, u16) {
    let base = PORT_CURSOR.fetch_add(2, Ordering::Relaxed);
    (base, base + 1)
}

fn alloc_home() -> PathBuf {
    let n = HOME_CURSOR.fetch_add(1, Ordering::Relaxed);
    let mut p = env::temp_dir();
    p.push(format!("ac-daemon-relock-it-{}-{n}", std::process::id()));
    let _ = fs::create_dir_all(p.join(".config").join("ac"));
    p
}

struct Daemon {
    child: Child,
    ctrl_port: u16,
    data_port: u16,
    home: PathBuf,
    log_path: PathBuf,
}

impl Daemon {
    fn spawn() -> Self {
        let home = alloc_home();
        let (ctrl, data) = alloc_ports();
        let bin = env!("CARGO_BIN_EXE_ac-daemon");
        let log_path = home.join("daemon.log");
        let log_file = fs::File::create(&log_path).expect("create daemon log");
        let log_file2 = log_file.try_clone().expect("clone daemon log handle");
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
            .stdout(log_file)
            .stderr(log_file2)
            .spawn()
            .expect("spawn ac-daemon");

        let deadline = Instant::now() + Duration::from_secs(10);
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
            log_path,
        }
    }

    fn log_tail(&self) -> String {
        fs::read_to_string(&self.log_path).unwrap_or_else(|e| format!("<no log: {e}>"))
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.home);
    }
}

struct Client<'a> {
    _ctx: zmq::Context,
    req: zmq::Socket,
    sub: zmq::Socket,
    daemon: &'a Daemon,
}

impl<'a> Client<'a> {
    fn new(d: &'a Daemon) -> Self {
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
            daemon: d,
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

    /// The next frame matching `pred`, within `timeout` — polls one frame
    /// at a time rather than a fixed settle window, since these tests care
    /// about a transition (locked → unlocked → locked) that a fixed-delay
    /// snapshot could straddle or miss entirely.
    fn frame_matching(&self, timeout: Duration, pred: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "no matching frame within timeout. daemon log:\n{}",
                self.daemon.log_tail()
            );
            let step = remaining.min(Duration::from_millis(300));
            let Some(f) = self.next_frame(step) else {
                continue;
            };
            if pred(&f) {
                return f;
            }
        }
    }
}

fn start_correlated(c: &Client, drivable: bool, delay_samples: i64, gain: f64) -> Value {
    c.call(json!({
        "cmd": "transfer_stream", "meas_channel": 0, "ref_channel": 1,
        "weighting": "Z", "integration": "fast",
        "drivable": drivable,
        "fake_correlated_pair": {"gain": gain, "delay_samples": delay_samples},
    }))
}

fn locked(f: &Value) -> bool {
    f["delay_locked"] == json!(true)
}

fn attempts(f: &Value) -> u64 {
    f["delay_attempts"].as_u64().expect("delay_attempts")
}

fn delay_samples(f: &Value) -> i64 {
    f["delay_samples"].as_i64().expect("delay_samples")
}

fn mtw_present(f: &Value) -> bool {
    !f["mtw"].is_null()
}

// ---------------------------------------------------------------------
// 6. No session running.
// ---------------------------------------------------------------------

#[test]
fn relock_with_no_session_running_errors_instead_of_panicking() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let r = c.call(json!({"cmd": "relock"}));
    assert_eq!(r["ok"], json!(false), "{r}");
    assert_eq!(r["error"], json!("no transfer_stream session running"));

    // The daemon is still alive and answering afterwards.
    assert_eq!(c.call(json!({"cmd": "status"}))["ok"], json!(true));
}

// ---------------------------------------------------------------------
// 2. delay_locked cycles false→true and the new lag equals the old.
// ---------------------------------------------------------------------

#[test]
fn relock_unlocks_and_relocks_to_the_same_lag_on_a_static_fake_path() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    assert_eq!(
        start_correlated(&c, false, LOCK_DELAY_SAMPLES, 0.6)["ok"],
        json!(true)
    );

    let before = c.frame_matching(LOCK_TIMEOUT, locked);
    assert_eq!(delay_samples(&before), LOCK_DELAY_SAMPLES);
    let attempts_before = attempts(&before);

    assert_eq!(c.call(json!({"cmd": "relock"}))["ok"], json!(true));

    // `flush_pair` clears `next_delay_attempt` precisely so the retry runs
    // on the SAME worker tick as the flush, not up to `RELOCK_RETRY` later
    // — and on a clean deterministic signal that retry succeeds
    // immediately, so the unlocked intermediate is internal state
    // (`pair_delays[i] = None`) that never survives to a published wire
    // frame. `delay_attempts` strictly increasing is what's actually
    // observable here, and is the flush's real fingerprint — see test
    // `delay_attempts_never_resets_across_manual_and_drive_edge_relocks`
    // for the property this stands in for.
    let after = c.frame_matching(LOCK_TIMEOUT, |f| locked(f) && attempts(f) > attempts_before);
    assert_eq!(
        delay_samples(&after),
        LOCK_DELAY_SAMPLES,
        "re-lock against the same deterministic signal found a different lag"
    );
}

// ---------------------------------------------------------------------
// 3. mtw columns disappear on the flush and return after the re-lock.
// ---------------------------------------------------------------------

#[test]
fn relock_drops_and_rebuilds_the_mtw_ladder() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    assert_eq!(
        start_correlated(&c, false, LOCK_DELAY_SAMPLES, 0.6)["ok"],
        json!(true)
    );

    // Wait for the ladder to produce at least one column before flushing —
    // otherwise "mtw disappeared" would be indistinguishable from "mtw
    // never appeared yet".
    let settled = c.frame_matching(LOCK_TIMEOUT, |f| locked(f) && mtw_present(f));
    let attempts_before = attempts(&settled);

    assert_eq!(c.call(json!({"cmd": "relock"}))["ok"], json!(true));

    // The delay lock itself can re-settle within the same worker tick as
    // the flush on a clean deterministic signal (see
    // `relock_unlocks_and_relocks_to_the_same_lag_on_a_static_fake_path`),
    // so `!locked` is not a reliable flush signal here. The ladder is:
    // `mtw[i] = None` is followed by a *fresh* `MtwPair` built the same
    // tick the new lock lands, and a fresh ladder needs several ticks of
    // pushes before `columns()` has anything to report — so `locked &&
    // !mtw_present` is the window this flush actually produces.
    let flushed = c.frame_matching(LOCK_TIMEOUT, |f| {
        locked(f) && attempts(f) > attempts_before && !mtw_present(f)
    });
    assert!(
        !mtw_present(&flushed),
        "mtw columns survived the flush: {flushed}"
    );

    let rebuilt = c.frame_matching(LOCK_TIMEOUT, |f| locked(f) && mtw_present(f));
    assert_eq!(delay_samples(&rebuilt), LOCK_DELAY_SAMPLES);
}

// ---------------------------------------------------------------------
// 4. A lock taken with the drive off is discarded on the first
//    `set_drive on`.
// ---------------------------------------------------------------------

#[test]
fn a_lock_taken_with_drive_off_is_discarded_on_the_first_drive_on() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    assert_eq!(
        start_correlated(&c, true, LOCK_DELAY_SAMPLES, 0.6)["ok"],
        json!(true)
    );

    // Locked while the session's drive is still off (no set_drive sent
    // yet) — `lock.driving` must be false.
    let before = c.frame_matching(LOCK_TIMEOUT, locked);
    let attempts_before = attempts(&before);

    assert_eq!(
        c.call(json!({"cmd": "set_drive", "on": true, "level_dbfs": CEILING_DBFS}))["ok"],
        json!(true)
    );

    // The edge must flush: the attempt count strictly increases once it
    // relocks. (Flush and re-lock can land in the same worker tick on a
    // clean deterministic signal — see
    // `relock_unlocks_and_relocks_to_the_same_lag_on_a_static_fake_path`
    // — so `attempts` increasing, not an observed `!locked` frame, is
    // the flush's reliable wire fingerprint.)
    let after = c.frame_matching(LOCK_TIMEOUT, |f| locked(f) && attempts(f) > attempts_before);
    assert!(
        attempts(&after) > attempts_before,
        "drive-on edge did not trigger a new attempt: before {attempts_before}, after {}",
        attempts(&after)
    );
}

// ---------------------------------------------------------------------
// 5. A lock taken WHILE driving survives a dead-man expiry and the
//    subsequent `set_drive on` — the thrash case, and the one a bare
//    edge (no provenance qualifier) gets wrong.
// ---------------------------------------------------------------------

#[test]
fn a_lock_taken_while_driving_survives_dead_man_expiry_and_resume() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    assert_eq!(
        start_correlated(&c, true, LOCK_DELAY_SAMPLES, 0.6)["ok"],
        json!(true)
    );

    // Drive on BEFORE the pair locks, so the lock's provenance is
    // `driving: true`.
    assert_eq!(
        c.call(json!({"cmd": "set_drive", "on": true, "level_dbfs": CEILING_DBFS}))["ok"],
        json!(true)
    );
    let locked_frame = c.frame_matching(LOCK_TIMEOUT, locked);
    let attempts_at_lock = attempts(&locked_frame);
    assert_eq!(delay_samples(&locked_frame), LOCK_DELAY_SAMPLES);

    // No CTRL traffic at all for longer than the 1.5 s dead-man window —
    // drive drops (on→off), which this issue deliberately excludes from
    // the flush regardless of provenance.
    thread::sleep(Duration::from_millis(1_700));
    let after_expiry = c.frame_matching(LOCK_TIMEOUT, |f| f["drive"]["on"] == json!(false));
    assert!(
        locked(&after_expiry),
        "dead-man expiry (on→off) flushed a held lock: {after_expiry}"
    );
    assert_eq!(delay_samples(&after_expiry), LOCK_DELAY_SAMPLES);
    assert_eq!(attempts(&after_expiry), attempts_at_lock);

    // Resume — off→on, the edge this issue watches. Provenance says this
    // lock was acquired while driving, so it must survive untouched: no
    // unlocked frame, no new attempt, same lag.
    assert_eq!(
        c.call(json!({"cmd": "set_drive", "on": true, "level_dbfs": CEILING_DBFS}))["ok"],
        json!(true)
    );
    for _ in 0..10 {
        let f = c
            .next_frame(Duration::from_secs(3))
            .expect("no frame after resume");
        assert!(locked(&f), "resume flushed a lock taken while driving: {f}");
        assert_eq!(delay_samples(&f), LOCK_DELAY_SAMPLES);
        assert_eq!(
            attempts(&f),
            attempts_at_lock,
            "resume re-attempted a lock that should have survived untouched"
        );
    }
}

// ---------------------------------------------------------------------
// 7. A stop and a restart the OPERATOR asked for do not compose into a
//    firing edge either — distinct path from test 5's dead-man case.
// ---------------------------------------------------------------------

#[test]
fn an_operator_stop_and_restart_do_not_flush_a_lock_taken_while_driving() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    assert_eq!(
        start_correlated(&c, true, LOCK_DELAY_SAMPLES, 0.6)["ok"],
        json!(true)
    );
    assert_eq!(
        c.call(json!({"cmd": "set_drive", "on": true, "level_dbfs": CEILING_DBFS}))["ok"],
        json!(true)
    );
    let locked_frame = c.frame_matching(LOCK_TIMEOUT, locked);
    let attempts_at_lock = attempts(&locked_frame);
    let mtw_at_lock = mtw_present(&locked_frame);

    // Operator-requested off — `on: true → false` never discards a lock.
    assert_eq!(
        c.call(json!({"cmd": "set_drive", "on": false, "level_dbfs": CEILING_DBFS}))["ok"],
        json!(true)
    );
    let after_off = c.frame_matching(LOCK_TIMEOUT, |f| f["drive"]["on"] == json!(false));
    assert!(locked(&after_off), "operator off flushed a held lock");
    assert_eq!(delay_samples(&after_off), LOCK_DELAY_SAMPLES);

    // Operator-requested restart — off→on, but this lock's provenance is
    // `driving: true`, so it survives.
    assert_eq!(
        c.call(json!({"cmd": "set_drive", "on": true, "level_dbfs": CEILING_DBFS}))["ok"],
        json!(true)
    );
    for _ in 0..10 {
        let f = c
            .next_frame(Duration::from_secs(3))
            .expect("no frame after restart");
        assert!(
            locked(&f),
            "operator restart flushed a lock taken while driving: {f}"
        );
        assert_eq!(delay_samples(&f), LOCK_DELAY_SAMPLES);
        assert_eq!(attempts(&f), attempts_at_lock);
        if mtw_at_lock {
            assert!(
                mtw_present(&f),
                "mtw columns dropped across a stop/restart that should not flush"
            );
        }
    }
}

// ---------------------------------------------------------------------
// 1. delay_attempts strictly increases and never resets across either
//    trigger (manual key, then the drive edge).
// ---------------------------------------------------------------------

#[test]
fn delay_attempts_never_resets_across_manual_and_drive_edge_relocks() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    assert_eq!(
        start_correlated(&c, true, LOCK_DELAY_SAMPLES, 0.6)["ok"],
        json!(true)
    );

    // Locked with drive off — `lock.driving == false`.
    let f0 = c.frame_matching(LOCK_TIMEOUT, locked);
    let mut high_water = attempts(&f0);
    assert!(high_water >= 1);

    // Trigger 1: the manual key. (Flush and re-lock can land in the same
    // worker tick on a clean deterministic signal, so `attempts`
    // increasing — not an observed `!locked` frame — is the flush's
    // reliable wire fingerprint; see
    // `relock_unlocks_and_relocks_to_the_same_lag_on_a_static_fake_path`.)
    assert_eq!(c.call(json!({"cmd": "relock"}))["ok"], json!(true));
    let f1 = c.frame_matching(LOCK_TIMEOUT, |f| locked(f) && attempts(f) > high_water);
    assert!(
        attempts(&f1) > high_water,
        "manual relock did not advance attempts"
    );
    high_water = attempts(&f1);

    // Trigger 2: the drive off→on edge (this lock was taken with drive
    // off, so the edge flushes it).
    assert_eq!(
        c.call(json!({"cmd": "set_drive", "on": true, "level_dbfs": CEILING_DBFS}))["ok"],
        json!(true)
    );
    let f2 = c.frame_matching(LOCK_TIMEOUT, |f| locked(f) && attempts(f) > high_water);
    assert!(
        attempts(&f2) > high_water,
        "drive-edge relock did not advance attempts"
    );
    high_water = attempts(&f2);

    // A third manual press on a now-driving-acquired lock must not lose
    // ground either.
    assert_eq!(c.call(json!({"cmd": "relock"}))["ok"], json!(true));
    let f3 = c.frame_matching(LOCK_TIMEOUT, |f| locked(f) && attempts(f) > high_water);
    assert!(
        attempts(&f3) > high_water,
        "attempts went backwards or stalled across a third relock: {} -> {}",
        high_water,
        attempts(&f3)
    );
}
