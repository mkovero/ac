//! Shared harness for the `ac-daemon` ZMQ integration tests.
//!
//! Every test here spawns its own daemon on an OS-assigned port pair, drives
//! the CTRL/DATA sockets, and kills the process on drop. No shared state, no
//! hardware needed.
//!
//! Included per test binary with
//!
//! ```ignore
//! #[path = "../common/mod.rs"]
//! mod common;
//! ```
//!
//! which is the pattern `ac-view/tests/support.rs` already uses. Each binary
//! links its own copy, so every item is `pub` and the module carries a
//! blanket `dead_code` allow — no single binary uses the whole surface.
//!
//! `it_loopback_ir.rs` is deliberately not a client of the `Daemon` here: it
//! drives a *real* JACK server rather than `--fake-audio`, and folding its
//! routing/`spawn_jack` setup in would put hardware-only concerns in the path
//! of every fake-audio test. It shares [`alloc_ports`] and [`alloc_home`] only.

#![allow(dead_code)]

use std::env;
use std::fs;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

static HOME_CURSOR: AtomicU32 = AtomicU32::new(0);

/// Two **OS-assigned** free ports (#195). A shared or derived port base
/// collided across the daemon-spawning test binaries under parallel
/// `cargo test` — statics are per-process, so a literal base has to be
/// hand-allocated per binary and a `pid % N` seed can still alias. Binding
/// `:0` lets the OS pick a currently-free port, with no modulo to alias on.
///
/// The listeners are dropped before the daemon rebinds, leaving a small
/// TOCTOU window; ephemeral ports are assigned round-robin over a large
/// range, so immediate reuse by another process is vanishingly unlikely —
/// strictly better than a base that can alias deterministically. This is the
/// same reasoning, and the same code, as `ac-view/tests/support.rs`.
pub fn alloc_ports() -> (u16, u16) {
    let port = || {
        TcpListener::bind("127.0.0.1:0")
            .expect("bind ephemeral port")
            .local_addr()
            .expect("local_addr")
            .port()
    };
    let ctrl = port();
    let mut data = port();
    // Guard the (rare) case the OS handed the same port twice across the
    // two independent binds.
    while data == ctrl {
        data = port();
    }
    (ctrl, data)
}

/// Unique scratch HOME per daemon so tests don't write to the real config.
pub fn alloc_home() -> PathBuf {
    let n = HOME_CURSOR.fetch_add(1, Ordering::Relaxed);
    let mut p = env::temp_dir();
    p.push(format!("ac-daemon-it-{}-{n}", std::process::id()));
    let _ = fs::create_dir_all(p.join(".config").join("ac"));
    p
}

/// How many times [`Daemon::spawn`] will re-draw a port pair before giving up.
///
/// [`alloc_ports`] closes its `:0` listeners before the daemon rebinds, so two
/// concurrent spawns can be handed the same port — rare per pair, but this
/// suite starts well over a hundred daemons per `cargo test`, and the loser of
/// such a race exits (the daemon aborts on either bind failure) while its
/// client happily talks to the *winner's* CTRL socket. That misrouting shows up
/// as an unrelated test timing out, so it is detected rather than tolerated:
/// see [`await_own_daemon`].
const SPAWN_ATTEMPTS: usize = 5;

/// Wait until `ctrl_port` is answered by `child` itself.
///
/// The probe is a `status` call, whose reply carries the responder's pid
/// (#385). A reply from a *different* pid means this child lost the bind race
/// and the port belongs to someone else's daemon; so does the child having
/// already exited. Both are reported as a retryable reason rather than
/// accepted, which is what keeps an OS-assigned port pair safe at this scale.
fn await_own_daemon(child: &mut Child, ctrl_port: u16) -> Result<(), String> {
    let deadline = Instant::now() + READY_TIMEOUT;
    let ctx = zmq::Context::new();
    loop {
        if Instant::now() > deadline {
            return Err(format!(
                "no status reply on ctrl {ctrl_port} within {READY_TIMEOUT:?}"
            ));
        }
        if let Ok(Some(st)) = child.try_wait() {
            return Err(format!(
                "daemon exited before serving ({st}) — lost a bind race?"
            ));
        }
        thread::sleep(Duration::from_millis(50));

        let s = ctx.socket(zmq::REQ).unwrap();
        s.set_linger(0).ok();
        s.set_rcvtimeo(300).ok();
        s.set_sndtimeo(300).ok();
        if s.connect(&format!("tcp://127.0.0.1:{ctrl_port}")).is_err() {
            continue;
        }
        if s.send(br#"{"cmd":"status"}"#.as_ref(), 0).is_err() {
            continue;
        }
        let Ok(bytes) = s.recv_bytes(0) else { continue };
        let reply: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        let pid = reply["pid"].as_u64().unwrap_or_else(|| {
            panic!("status carried no pid, so the spawn cannot tell its own daemon from an incumbent: {reply}")
        });
        if pid == u64::from(child.id()) {
            return Ok(());
        }
        return Err(format!(
            "ctrl {ctrl_port} answered by pid {pid}, not our {} — bind race",
            child.id()
        ));
    }
}

/// How long [`Daemon::spawn`] waits for the CTRL socket to answer a probe.
///
/// This is a deadline, not a delay: the loop breaks on the first reply, so a
/// generous value costs nothing on the happy path and only lengthens the
/// failure case. The per-binary values it replaces ranged 3–10 s; the
/// longest wins so the relock tests, which spawn under load, keep their
/// margin.
const READY_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Daemon {
    child: Child,
    pub ctrl_port: u16,
    pub data_port: u16,
    pub home: PathBuf,
    log_path: PathBuf,
}

impl Daemon {
    pub fn spawn() -> Self {
        Self::spawn_with(None, &[])
    }

    /// Spawn with a pre-seeded `~/.config/ac/config.json`. Useful when the
    /// daemon's behaviour at startup depends on persisted state — e.g., the
    /// sticky `*_port` keys whose interaction with `setup` is the regression
    /// guard for `setup_channel_clears_sticky_port`.
    pub fn spawn_with_config(config: Option<Value>) -> Self {
        Self::spawn_with(config, &[])
    }

    /// Spawn with extra environment variables. Needed for the diagnostics the
    /// daemon writes to its log rather than to the wire — `AC_DRAIN_TELEMETRY`
    /// (#208 D1) is the only one today, and it is deliberately not published,
    /// so reading [`Self::log`] is the only way a test can assert on it.
    pub fn spawn_with_env(env: &[(&str, &str)]) -> Self {
        Self::spawn_with(None, env)
    }

    /// Spawn against a caller-chosen `HOME` — needed to test the crash-safety
    /// spool wipe, where a second daemon instance must see the *same* on-disk
    /// spool a killed first instance left behind.
    pub fn spawn_at_home(home: PathBuf) -> Self {
        Self::spawn_at(home, None, &[])
    }

    pub fn spawn_with(config: Option<Value>, extra_env: &[(&str, &str)]) -> Self {
        Self::spawn_at(alloc_home(), config, extra_env)
    }

    fn spawn_at(home: PathBuf, config: Option<Value>, extra_env: &[(&str, &str)]) -> Self {
        if let Some(cfg) = config {
            let path = home.join(".config").join("ac").join("config.json");
            fs::write(&path, serde_json::to_vec_pretty(&cfg).unwrap())
                .expect("write seeded config.json");
        }

        let mut last = String::new();
        for _ in 0..SPAWN_ATTEMPTS {
            let (ctrl, data) = alloc_ports();

            // Both streams go to one file, unconditionally. The daemon's
            // output is not captured by libtest (it is a child process), so
            // without this it interleaves into the terminal during a parallel
            // run; with it, any test can read it back via `log`/`log_tail`.
            let log_path = home.join("daemon.log");
            let log_file = fs::File::create(&log_path).expect("create daemon log");
            let log_file2 = log_file.try_clone().expect("clone daemon log handle");

            let mut cmd = Command::new(env!("CARGO_BIN_EXE_ac-daemon"));
            cmd.env("HOME", &home);
            for (k, v) in extra_env {
                cmd.env(k, v);
            }
            let mut child = cmd
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

            match await_own_daemon(&mut child, ctrl) {
                Ok(()) => {
                    return Self {
                        child,
                        ctrl_port: ctrl,
                        data_port: data,
                        home,
                        log_path,
                    }
                }
                Err(why) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    last = why;
                }
            }
        }
        panic!("daemon never came up after {SPAWN_ATTEMPTS} attempts: {last}");
    }

    /// This daemon process's own pid — what `status` and `server_connections`
    /// must report back.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Kill the daemon process without the rest of [`Drop`] — notably without
    /// removing `home`. The caller is expected to `std::mem::forget` the value
    /// afterwards, which is how the snapshot tests simulate "the process died
    /// and no cleanup ran" while a second instance reuses the same `home`.
    pub fn kill_without_cleanup(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// Whatever the daemon has written to stdout+stderr so far.
    pub fn log(&self) -> String {
        fs::read_to_string(&self.log_path).unwrap_or_default()
    }

    /// Same as [`Self::log`], but reports the read error instead of an empty
    /// string — for use inside an assertion message, where a silent `""`
    /// would read as "the daemon logged nothing".
    pub fn log_tail(&self) -> String {
        fs::read_to_string(&self.log_path).unwrap_or_else(|e| format!("<no log: {e}>"))
    }

    pub fn ctrl_endpoint(&self) -> String {
        format!("tcp://127.0.0.1:{}", self.ctrl_port)
    }

    pub fn data_endpoint(&self) -> String {
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

/// Default CTRL/DATA socket receive timeout.
const DEFAULT_TIMEOUT_MS: i32 = 5_000;

pub struct Client<'a> {
    _ctx: zmq::Context,
    req: zmq::Socket,
    sub: zmq::Socket,
    daemon: &'a Daemon,
}

impl<'a> Client<'a> {
    pub fn new(d: &'a Daemon) -> Self {
        Self::with_ctrl_timeout(d, DEFAULT_TIMEOUT_MS)
    }

    /// A client whose CTRL receive timeout is not the default. Needed for
    /// commands whose reply is slower than that: `test_hardware` replies only
    /// after its worker thread is spawned, which has been measured past 3 s
    /// here. The timeout is set before `connect`, which is where ZMQ latches
    /// it for this socket.
    pub fn with_ctrl_timeout(d: &'a Daemon, ctrl_timeout_ms: i32) -> Self {
        let ctx = zmq::Context::new();
        let req = ctx.socket(zmq::REQ).unwrap();
        req.set_linger(0).unwrap();
        req.set_rcvtimeo(ctrl_timeout_ms).unwrap();
        req.set_sndtimeo(DEFAULT_TIMEOUT_MS).unwrap();
        req.connect(&d.ctrl_endpoint()).unwrap();

        let sub = ctx.socket(zmq::SUB).unwrap();
        sub.set_linger(0).unwrap();
        sub.set_rcvtimeo(DEFAULT_TIMEOUT_MS).unwrap();
        sub.set_subscribe(b"").unwrap();
        sub.connect(&d.data_endpoint()).unwrap();

        // Allow a tick for the SUB to latch before returning.
        thread::sleep(Duration::from_millis(100));
        Self {
            _ctx: ctx,
            req,
            sub,
            daemon: d,
        }
    }

    /// The daemon this client is attached to — for assertion messages that
    /// want to quote its log.
    pub fn daemon(&self) -> &Daemon {
        self.daemon
    }

    pub fn call(&self, cmd: Value) -> Value {
        let raw = serde_json::to_vec(&cmd).unwrap();
        self.req.send(raw, 0).unwrap();
        let bytes = self.req.recv_bytes(0).expect("CTRL recv");
        serde_json::from_slice(&bytes).expect("CTRL decode")
    }

    /// Pop one PUB frame as topic + undecoded payload bytes; `None` on
    /// timeout. Wire format: single frame `<topic> <json>\n` (see ZMQ.md
    /// §DATA). Callers that need the raw bytes — a snapshot fixture, say —
    /// use this; everything else uses [`Self::recv_pub`].
    pub fn recv_pub_raw(&self, timeout_ms: i32) -> Option<(String, Vec<u8>)> {
        self.sub.set_rcvtimeo(timeout_ms).ok();
        let bytes = self.sub.recv_bytes(0).ok()?;
        let split = bytes.iter().position(|&b| b == b' ')?;
        let topic = String::from_utf8(bytes[..split].to_vec()).ok()?;
        Some((topic, bytes[split + 1..].to_vec()))
    }

    /// Pop one PUB frame (topic + JSON payload); `None` on timeout. An
    /// undecodable payload yields `Value::Null` rather than `None`, so a
    /// caller can tell "no frame arrived" from "a frame arrived and was
    /// junk".
    pub fn recv_pub(&self, timeout_ms: i32) -> Option<(String, Value)> {
        let (topic, payload) = self.recv_pub_raw(timeout_ms)?;
        Some((
            topic,
            serde_json::from_slice(&payload).unwrap_or(Value::Null),
        ))
    }

    /// Wait for a frame on `topic`, discarding others, until `timeout` elapses.
    pub fn wait_for_topic(&self, want: &str, timeout: Duration) -> Option<Value> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let remaining = deadline
                .saturating_duration_since(Instant::now())
                .as_millis() as i32;
            match self.recv_pub(remaining.max(1)) {
                Some((t, v)) if t == want => return Some(v),
                Some(_) => continue,
                None => return None,
            }
        }
        None
    }

    /// Next `transfer_stream` DATA frame, or `None` on timeout.
    pub fn next_frame(&self, timeout: Duration) -> Option<Value> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let remaining = deadline
                .saturating_duration_since(Instant::now())
                .as_millis()
                .max(1) as i32;
            let (topic, payload) = self.recv_pub_raw(remaining)?;
            let payload: Value = serde_json::from_slice(&payload).ok()?;
            if topic == "data" && payload["type"] == json!("transfer_stream") {
                return Some(payload);
            }
        }
        None
    }

    /// The next frame matching `pred`, within `timeout` — polls one frame
    /// at a time rather than a fixed settle window, since the relock tests
    /// care about a transition (locked → unlocked → locked) that a
    /// fixed-delay snapshot could straddle or miss entirely.
    pub fn frame_matching(&self, timeout: Duration, pred: impl Fn(&Value) -> bool) -> Value {
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

    /// Drain frames for `settle`, then return the next one — so an
    /// assertion sees the state after a drive change rather than a frame
    /// computed from blocks captured before it.
    pub fn frame_after(&self, settle: Duration) -> Value {
        let until = Instant::now() + settle;
        while Instant::now() < until {
            let _ = self.next_frame(Duration::from_millis(300));
        }
        self.next_frame(Duration::from_secs(10))
            .expect("no transfer_stream frame")
    }
}
