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
    child: Child,
    ctrl_port: u16,
    data_port: u16,
    home: PathBuf,
}

impl Daemon {
    fn spawn() -> Self {
        Self::spawn_with_config(None)
    }

    /// Spawn with a pre-seeded `~/.config/ac/config.json`. Useful when the
    /// daemon's behaviour at startup depends on persisted state — e.g., the
    /// sticky `*_port` keys whose interaction with `setup` is the regression
    /// guard for `setup_channel_clears_sticky_port`.
    fn spawn_with_config(config: Option<Value>) -> Self {
        Self::spawn_with(config, &[])
    }

    /// Spawn with extra environment variables and the daemon's stderr
    /// redirected to `<home>/daemon.stderr`, readable via [`Self::stderr`].
    ///
    /// Needed for the diagnostics the daemon writes to stderr rather than to
    /// the wire — `AC_DRAIN_TELEMETRY` (#208 D1) is the only one today, and it
    /// is deliberately not published, so reading the file is the only way a
    /// test can assert on it.
    fn spawn_with_env(env: &[(&str, &str)]) -> Self {
        Self::spawn_with(None, env)
    }

    fn spawn_with(config: Option<Value>, extra_env: &[(&str, &str)]) -> Self {
        let (ctrl, data) = alloc_ports();
        let home = alloc_home();
        if let Some(cfg) = config {
            let path = home.join(".config").join("ac").join("config.json");
            fs::write(&path, serde_json::to_vec_pretty(&cfg).unwrap())
                .expect("write seeded config.json");
        }
        let bin = env!("CARGO_BIN_EXE_ac-daemon");
        let mut cmd = Command::new(bin);
        cmd.env("HOME", &home);
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        if !extra_env.is_empty() {
            let f = fs::File::create(home.join("daemon.stderr")).expect("create stderr capture");
            cmd.stderr(std::process::Stdio::from(f));
        }
        let child = cmd
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
        // Wait for the CTRL socket to accept a probe.
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
            if let Ok(_msg) = s.recv_bytes(0) {
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

    /// Whatever the daemon has written to stderr so far. Empty unless the
    /// daemon was started via [`Self::spawn_with_env`].
    fn stderr(&self) -> String {
        fs::read_to_string(self.home.join("daemon.stderr")).unwrap_or_default()
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
        Self::with_ctrl_timeout(d, 3_000)
    }

    /// A client whose CTRL receive timeout is not the 3 s default. Needed for
    /// commands whose reply is slower than that: `test_hardware` replies only
    /// after its worker thread is spawned, which has been measured past 3 s
    /// here. The timeout is set before `connect`, which is where ZMQ latches
    /// it for this socket.
    fn with_ctrl_timeout(d: &Daemon, ctrl_timeout_ms: i32) -> Self {
        let ctx = zmq::Context::new();
        let req = ctx.socket(zmq::REQ).unwrap();
        req.set_linger(0).unwrap();
        req.set_rcvtimeo(ctrl_timeout_ms).unwrap();
        req.set_sndtimeo(3_000).unwrap();
        req.connect(&d.ctrl_endpoint()).unwrap();

        let sub = ctx.socket(zmq::SUB).unwrap();
        sub.set_linger(0).unwrap();
        sub.set_rcvtimeo(3_000).unwrap();
        sub.set_subscribe(b"").unwrap();
        sub.connect(&d.data_endpoint()).unwrap();

        // Allow a tick for the SUB to latch before returning.
        thread::sleep(Duration::from_millis(100));
        Self {
            _ctx: ctx,
            req,
            sub,
        }
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
            Ok(b) => b,
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

/// #385: `status` carries the identity fields a client needs to tell this
/// daemon apart from one squatting the same hardcoded ports under a
/// different `HOME`.
#[test]
fn status_reports_daemon_identity() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({"cmd":"status"}));
    assert_eq!(r["ok"], json!(true));
    assert_eq!(
        r["home"],
        json!(d.home.display().to_string()),
        "home must be the daemon's own $HOME: {r}"
    );
    assert_eq!(
        r["pid"],
        json!(d.child.id()),
        "pid must be this daemon process's own pid: {r}"
    );
    assert_eq!(
        r["spawn_mode"],
        json!("manual"),
        "Daemon::spawn never passes --auto-spawned: {r}"
    );
    let config_path = r["config_path"].as_str().expect("config_path string");
    assert!(
        config_path.ends_with("config.json"),
        "config_path: {config_path}"
    );
    assert!(
        config_path.starts_with(&d.home.display().to_string()),
        "config_path should be under the daemon's HOME: {config_path}"
    );
    let started_at = r["started_at"].as_str().expect("started_at string");
    assert!(
        started_at.ends_with('Z'),
        "started_at should be RFC3339 Zulu: {started_at}"
    );
}

/// #385: `server_connections` carries the same identity fields as `status`.
#[test]
fn server_connections_reports_daemon_identity() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({"cmd":"server_connections"}));
    assert_eq!(r["ok"], json!(true));
    assert_eq!(r["home"], json!(d.home.display().to_string()));
    assert_eq!(r["pid"], json!(d.child.id()));
    assert_eq!(r["spawn_mode"], json!("manual"));
}

/// #385: a second daemon that loses the bind race must report the
/// incumbent's identity to stderr rather than fail silently or guess.
#[test]
fn second_daemon_on_taken_port_reports_incumbent_identity() {
    let d = Daemon::spawn();

    let home2 = alloc_home();
    let stderr_path = home2.join("daemon2.stderr");
    let stderr_file = fs::File::create(&stderr_path).expect("create stderr capture");
    let mut second = Command::new(env!("CARGO_BIN_EXE_ac-daemon"))
        .env("HOME", &home2)
        .args([
            "--fake-audio",
            "--local",
            "--ctrl-port",
            &d.ctrl_port.to_string(),
            "--data-port",
            &d.data_port.to_string(),
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::from(stderr_file))
        .spawn()
        .expect("spawn second ac-daemon");

    let status = second.wait().expect("wait for second daemon to exit");
    assert!(
        !status.success(),
        "a daemon on an already-bound port must exit non-zero"
    );

    let stderr = fs::read_to_string(&stderr_path).unwrap_or_default();
    assert!(
        stderr.contains("existing listener"),
        "expected the incumbent's identity in stderr, got: {stderr}"
    );
    assert!(
        stderr.contains(&d.home.display().to_string()),
        "expected the incumbent's home in stderr, got: {stderr}"
    );
    assert!(
        stderr.contains("manual"),
        "expected the incumbent's spawn_mode (manual) in stderr, got: {stderr}"
    );

    let _ = fs::remove_dir_all(&home2);
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
    assert!(!r["playback"].as_array().unwrap().is_empty());
    assert!(!r["capture"].as_array().unwrap().is_empty());
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
    let done = c
        .wait_for_topic("done", Duration::from_secs(3))
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
        ports.len(),
        chans.len(),
        "expected {} ports for channels {:?}, got {:?}",
        chans.len(),
        chans,
        ports,
    );
    // Each port name must be unique — otherwise the daemon collapsed
    // distinct channel indices to the sticky default.
    let names: std::collections::HashSet<&str> = ports.iter().filter_map(|v| v.as_str()).collect();
    assert_eq!(
        names.len(),
        ports.len(),
        "duplicate port in out_ports: {ports:?}"
    );

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
    assert_eq!(
        ports.len(),
        1,
        "default-channel generate should give one port"
    );

    let _ = c.call(json!({"cmd":"stop"}));
}

#[test]
fn setup_channel_clears_sticky_port() {
    // A stale sticky port in config.json — left over from a prior session,
    // a manual edit, or the older Python era — used to silently override
    // any subsequent `ac setup output|input|reference N`. `resolve_output`
    // and friends prefer the sticky string over the channel-index lookup,
    // so the configured channel got effectively muted (audio routed to
    // whatever the stale name pointed at, often nothing). Setting a new
    // channel must invalidate the stale override.
    let d = Daemon::spawn_with_config(Some(json!({
        "output_channel":    7,
        "output_port":       "system:playback_99",
        "input_channel":     7,
        "input_port":        "system:capture_99",
        "reference_channel": 7,
        "reference_port":    "system:capture_99",
    })));
    let c = Client::new(&d);

    let r = c.call(json!({"cmd":"setup","update":{
        "output_channel":    0,
        "input_channel":     0,
        "reference_channel": 0,
    }}));
    assert_eq!(r["ok"], json!(true));
    let cfg = &r["config"];
    assert!(
        cfg["output_port"].is_null(),
        "setup output_channel must clear sticky output_port (got {:?})",
        cfg["output_port"]
    );
    assert!(
        cfg["input_port"].is_null(),
        "setup input_channel must clear sticky input_port (got {:?})",
        cfg["input_port"]
    );
    assert!(
        cfg["reference_port"].is_null(),
        "setup reference_channel must clear sticky reference_port (got {:?})",
        cfg["reference_port"]
    );
}

/// handoff: snapshot-backend M1 — `snapshot_ring_s`/`snapshot_spool_dir`
/// round-trip through `setup` like every other config field, including
/// persistence (a second `setup` read reflects the earlier write).
#[test]
fn setup_updates_snapshot_ring_and_spool_dir() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let r = c.call(json!({"cmd":"setup","update":{
        "snapshot_ring_s": 60.0,
        "snapshot_spool_dir": "/tmp/custom-acsnap-spool",
    }}));
    assert_eq!(r["ok"], json!(true), "setup: {r}");
    assert_eq!(r["config"]["snapshot_ring_s"], json!(60.0));
    assert_eq!(
        r["config"]["snapshot_spool_dir"],
        json!("/tmp/custom-acsnap-spool")
    );

    // Persisted, not just echoed back.
    let r2 = c.call(json!({"cmd": "setup", "update": {}}));
    assert_eq!(r2["config"]["snapshot_ring_s"], json!(60.0));
    assert_eq!(
        r2["config"]["snapshot_spool_dir"],
        json!("/tmp/custom-acsnap-spool")
    );

    // snapshot_ring_s <= 0 is ignored (invalid), not silently accepted.
    let r3 = c.call(json!({"cmd":"setup","update":{"snapshot_ring_s": -5.0}}));
    assert_eq!(r3["ok"], json!(true));
    assert_eq!(
        r3["config"]["snapshot_ring_s"],
        json!(60.0),
        "non-positive snapshot_ring_s must be rejected, not applied"
    );

    // null clears the spool dir override back to the default.
    let r4 = c.call(json!({"cmd":"setup","update":{"snapshot_spool_dir": Value::Null}}));
    assert_eq!(r4["ok"], json!(true));
    assert!(r4["config"]["snapshot_spool_dir"].is_null());
}

#[test]
fn setup_clearing_reference_channel_clears_reference_port() {
    // The `reference_channel: null` branch (the way the user disables
    // the H1 reference channel) must also clear the sticky.
    let d = Daemon::spawn_with_config(Some(json!({
        "reference_channel": 3,
        "reference_port":    "system:capture_99",
    })));
    let c = Client::new(&d);

    let r = c.call(json!({"cmd":"setup","update":{ "reference_channel": null }}));
    assert_eq!(r["ok"], json!(true));
    assert!(r["config"]["reference_channel"].is_null());
    assert!(r["config"]["reference_port"].is_null());
}

/// #225 — the reference *output* leg is configured on its own playback index.
/// Moving the capture-side `reference_channel` must not move it, and each leg
/// clears only its own sticky port.
#[test]
fn setup_reference_output_channel_is_independent_of_reference_channel() {
    let d = Daemon::spawn_with_config(Some(json!({
        "reference_channel":        2,
        "reference_port":           "system:capture_9",
        "reference_output_channel": 1,
        "reference_output_port":    "system:playback_9",
    })));
    let c = Client::new(&d);

    let r = c.call(json!({"cmd":"setup","update":{ "reference_channel": 3 }}));
    assert_eq!(r["ok"], json!(true), "setup: {r}");
    assert_eq!(r["config"]["reference_channel"], json!(3));
    assert!(r["config"]["reference_port"].is_null());
    assert_eq!(
        r["config"]["reference_output_channel"],
        json!(1),
        "reference_channel must not move the reference output leg"
    );
    assert_eq!(
        r["config"]["reference_output_port"],
        json!("system:playback_9")
    );

    let r2 = c.call(json!({"cmd":"setup","update":{ "reference_output_channel": 5 }}));
    assert_eq!(r2["config"]["reference_output_channel"], json!(5));
    assert!(
        r2["config"]["reference_output_port"].is_null(),
        "reference_output_channel must clear its own sticky port"
    );
    assert_eq!(
        r2["config"]["reference_channel"],
        json!(3),
        "reference_output_channel must not move the capture leg"
    );

    let r3 = c.call(json!({"cmd":"setup","update":{ "reference_output_channel": null }}));
    assert!(r3["config"]["reference_output_channel"].is_null());
    assert!(r3["config"]["reference_output_port"].is_null());
}

/// #225 — the regression itself: the resolved reference output port comes from
/// `reference_output_channel`. It used to come from `reference_channel`, a
/// *capture* index, so on a rig where the two differ the daemon drove a
/// playback port nothing was patched to and the reference leg stayed at
/// digital silence while the session believed it had a reference.
///
/// Asserted through `test_hardware` rather than `transfer_stream` because
/// `transfer_stream`'s start reply does not carry `ref_out_port` on `main` —
/// that field arrives with #205 (PR #214). Both commands resolve the leg
/// through the same `resolve_ref_output`, so this covers the fix without
/// taking a dependency on an unmerged branch.
#[test]
fn ref_out_port_resolves_from_reference_output_channel() {
    let d = Daemon::spawn_with_config(Some(json!({
        "output_channel":           4,
        "reference_channel":        2,
        "reference_output_channel": 1,
    })));
    let c = Client::with_ctrl_timeout(&d, 15_000);

    let r = c.call(json!({"cmd":"test_hardware"}));
    assert_eq!(r["ok"], json!(true), "test_hardware start: {r}");
    assert_eq!(r["out_port"], json!("fake:playback_4"));
    assert_eq!(
        r["ref_out_port"],
        json!("fake:playback_1"),
        "reference output must resolve from reference_output_channel; \
         resolving from reference_channel would give fake:playback_2"
    );
    c.call(json!({"cmd":"stop"}));
}

/// With no reference output configured the leg falls back to the main output,
/// as it always did — and a configured `reference_channel` alone does not
/// change that.
#[test]
fn ref_out_port_falls_back_to_main_output() {
    let d = Daemon::spawn_with_config(Some(json!({
        "output_channel":    4,
        "reference_channel": 2,
    })));
    let c = Client::with_ctrl_timeout(&d, 15_000);

    let r = c.call(json!({"cmd":"test_hardware"}));
    assert_eq!(r["ok"], json!(true), "test_hardware start: {r}");
    assert_eq!(
        r["ref_out_port"],
        json!("fake:playback_4"),
        "unconfigured reference output must follow the main output"
    );
    c.call(json!({"cmd":"stop"}));
}

/// #225 changed what an existing config *means*: `reference_channel: N` alone
/// used to drive the reference out `playback[N]` and now leaves it on the main
/// output. A rig where the loopback happened to sit at that index worked before
/// and silently does not now, so the reply says so.
///
/// The warning is a stopgap for that migration, not a fault detector — #228's
/// `NO REFERENCE` observes the symptom instead of predicting it from config.
#[test]
fn a_config_whose_meaning_changed_carries_a_migration_warning() {
    let d = Daemon::spawn_with_config(Some(json!({
        "output_channel":    4,
        "reference_channel": 2,
    })));
    let c = Client::with_ctrl_timeout(&d, 15_000);

    let r = c.call(json!({"cmd":"test_hardware"}));
    assert_eq!(r["ok"], json!(true), "test_hardware: {r}");
    let warnings = r["warnings"]
        .as_array()
        .expect("warnings on a migrated config");
    let text = warnings[0].as_str().unwrap_or_default();
    assert!(
        text.contains("playback[2]") && text.contains("ac setup refout 2"),
        "warning must name the old port and the exact fix, got {text:?}"
    );
    c.call(json!({"cmd":"stop"}));
}

/// ...and is absent once the leg is configured either way, so it cannot become
/// background noise the operator learns to skip.
#[test]
fn a_configured_reference_output_carries_no_migration_warning() {
    let d = Daemon::spawn_with_config(Some(json!({
        "output_channel":           4,
        "reference_channel":        2,
        "reference_output_channel": 1,
    })));
    let c = Client::with_ctrl_timeout(&d, 15_000);

    let r = c.call(json!({"cmd":"test_hardware"}));
    assert_eq!(r["ok"], json!(true), "test_hardware: {r}");
    assert!(
        r.get("warnings").is_none(),
        "an explicitly configured reference output must not warn: {r}"
    );
    c.call(json!({"cmd":"stop"}));
}

/// A sticky port whose gating channel is unset is an explicitly configured
/// value with no effect — the same class of silent misconfiguration as #225
/// itself. Both gated legs refuse it instead of resolving past it.
#[test]
fn sticky_reference_ports_without_their_channel_are_refused() {
    let d = Daemon::spawn_with_config(Some(json!({
        "reference_output_port": "fake:playback_1",
    })));
    let c = Client::new(&d);

    let r = c.call(json!({"cmd":"transfer_stream","meas_channel":0,"ref_channel":1}));
    assert_eq!(
        r["ok"],
        json!(false),
        "reference_output_port without reference_output_channel must not resolve \
         silently to the main output: {r}"
    );
    let err = r["error"].as_str().unwrap_or_default();
    assert!(
        err.contains("reference_output_port") && err.contains("reference_output_channel"),
        "error must name both keys, got {err:?}"
    );

    // Same rule on the capture leg. `test_hardware`'s own guard passes on
    // either field, so before this it fell through to `Ok(None)` and measured
    // single-ended against the measurement input.
    let d2 = Daemon::spawn_with_config(Some(json!({
        "reference_port": "fake:capture_3",
    })));
    let c2 = Client::new(&d2);

    let r2 = c2.call(json!({"cmd":"test_hardware"}));
    assert_eq!(
        r2["ok"],
        json!(false),
        "reference_port without reference_channel must not downgrade to \
         single-ended: {r2}"
    );
    let err2 = r2["error"].as_str().unwrap_or_default();
    assert!(
        err2.contains("reference_port") && err2.contains("reference_channel"),
        "error must name both keys, got {err2:?}"
    );
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
    let done = c
        .wait_for_topic("done", Duration::from_secs(5))
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
    // fake engine (via `dbfs_to_amplitude` + `set_tone_pair` in
    // handlers/audio/monitor.rs) and produce two independently-detectable
    // spectral peaks at their requested levels — the I1/I3 stimulus this
    // harness needs. Frequencies chosen well clear of each other and of
    // the LF/HF crossover so both land cleanly in one FFT.
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let r = c.call(json!({
        "cmd": "monitor_spectrum",
        "channels": [0],
        "interval_ms": 100,
        "fft_n": 8192,
        "fake_tones": [
            {"freq_hz": 2000.0, "level_dbfs": -6.0},
            {"freq_hz": 9000.0, "level_dbfs": -24.0},
        ],
    }));
    assert_eq!(r["ok"], json!(true), "monitor_spectrum ack: {r}");

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
        "interval_ms": 100,
        "fft_n": 8192,
        "fake_noise_dbfs": -20.0,
    }));
    assert_eq!(r["ok"], json!(true), "monitor_spectrum ack: {r}");

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
            None => break,
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
    let _ = c
        .wait_for_topic("cal_prompt", Duration::from_secs(3))
        .expect("step 1 prompt");
    let user_out_vrms = 2.095_f64;
    let _ = c.call(json!({"cmd": "cal_reply", "vrms": user_out_vrms}));

    // Step 2 prompt — fake backend loops the played tone back, so the
    // captured input level matches the played `ref_dbfs - 3.01` (RMS
    // vs peak), and the handler should flag `loopback: true`.
    let p2 = c
        .wait_for_topic("cal_prompt", Duration::from_secs(3))
        .expect("step 2 prompt");
    assert_eq!(
        p2["loopback"],
        json!(true),
        "expected loopback flag in step 2: {p2}"
    );
    let _ = c.call(json!({"cmd": "cal_reply", "vrms": user_out_vrms}));

    let done = c
        .wait_for_topic("cal_done", Duration::from_secs(5))
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

/// Drive a `calibrate` run to completion, skipping every voltage prompt
/// (`vrms: null`), and return the `cal_done` payload. `cmd` supplies any
/// extra fields (`output_channel` / `input_channel` / `ref_dbfs`) merged
/// into the `calibrate` request.
fn run_calibrate_skip_all(c: &Client, mut cmd: Value) -> Value {
    cmd["cmd"] = json!("calibrate");
    let r = c.call(cmd);
    assert_eq!(r["ok"], json!(true), "calibrate ack: {r}");
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((topic, _)) if topic == "cal_prompt" => {
                let _ = c.call(json!({"cmd":"cal_reply", "vrms": null}));
            }
            Some((topic, payload)) if topic == "cal_done" => return payload,
            Some(_) => continue,
            None => break,
        }
    }
    panic!("calibrate run never reached cal_done");
}

/// #370, acceptance criterion 1: `cal_done` carries the resolved input/output
/// port names actually used — server-side, not the client's copy of the
/// request — so a scan across channels stops reading as a flat, plausible
/// result when every run actually measured the same port.
#[test]
fn cal_done_reports_resolved_ports() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let done = run_calibrate_skip_all(&c, json!({"output_channel": 2, "input_channel": 3}));
    assert_eq!(
        done["output_port"],
        json!("fake:playback_2"),
        "cal_done: {done}"
    );
    assert_eq!(
        done["input_port"],
        json!("fake:capture_3"),
        "cal_done: {done}"
    );
}

/// #370, acceptance criterion 3 (the failing case named in the triage spec):
/// a config.json edit made between two measurements against one long-lived
/// daemon must reach the second one. Before the per-request reload in
/// `dispatch()`, this is exactly the reporter's repro — an auto-spawned
/// daemon outlives the `ac` command that spawned it, so a channel-scan
/// script editing `input_channel` between runs silently re-measured the
/// first channel every time.
#[test]
fn calibrate_picks_up_a_config_edit_made_between_two_runs() {
    let d = Daemon::spawn_with_config(Some(json!({"input_channel": 1})));
    let c = Client::new(&d);

    let done1 = run_calibrate_skip_all(&c, json!({}));
    assert_eq!(
        done1["input_port"],
        json!("fake:capture_1"),
        "first run: {done1}"
    );

    // Same daemon process, no restart — just the config file changing
    // underneath it, exactly as an operator's editor would.
    let cfg_path = d.home.join(".config").join("ac").join("config.json");
    fs::write(
        &cfg_path,
        serde_json::to_vec_pretty(&json!({"input_channel": 2})).unwrap(),
    )
    .expect("rewrite config.json");

    let done2 = run_calibrate_skip_all(&c, json!({}));
    assert_eq!(
        done2["input_port"],
        json!("fake:capture_2"),
        "second run: {done2}"
    );
    assert_ne!(
        done1["input_port"], done2["input_port"],
        "config edit between runs must change the resolved input port"
    );
}

/// #370, acceptance criterion 4: where the running daemon cannot serve the
/// current on-disk config (unparseable JSON, e.g. a file caught mid-write),
/// a routing command must say so and refuse rather than silently serving
/// against the last-known-good in-memory config. Non-routing commands
/// (`status`) stay reachable so the operator can tell what's wrong without
/// a restart.
#[test]
fn routing_command_refuses_when_config_json_is_unparseable() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let cfg_path = d.home.join(".config").join("ac").join("config.json");
    fs::write(&cfg_path, b"{ not json").expect("write malformed config.json");

    let r = c.call(json!({"cmd": "calibrate"}));
    assert_eq!(r["ok"], json!(false), "expected refusal: {r}");
    let err = r["error"].as_str().unwrap_or("");
    assert!(
        err.contains("config.json"),
        "error should name config.json: {r}"
    );
    // `{e:#}` (not `{e}`) on the reload's Err arm: the reply must carry the
    // actual parse failure, not just the file path — that's what makes the
    // refusal diagnosable rather than merely visible.
    assert!(
        err.contains("line") || err.contains("column") || err.to_lowercase().contains("expected"),
        "error should name *why* config.json failed to parse, not just that it did: {r}"
    );

    let s = c.call(json!({"cmd": "status"}));
    assert_eq!(s["ok"], json!(true), "status must still answer: {s}");
}

/// #281 QA correctness issue 1: `measure_tau`'s sweep→deconvolve→peak→seconds
/// path had zero test coverage — the only τ tests (`calibration.rs`)
/// construct `TauEntry`/`TauConditions` directly and never call
/// `measure_tau`. The fake backend's `play_and_capture` delays by a fixed
/// `FAKE_LOOPBACK_DELAY_SAMPLES = 32` (see `audio/fake.rs`), the same
/// deterministic delay `plot_ir_emits_impulse_response_with_expected_delay_peak`
/// already checks its IR peak against — this is that precedent applied to
/// `calibrate`'s τ measurement.
#[test]
fn calibrate_measures_tau_against_fake_loopback_delay() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let r = c.call(json!({"cmd": "calibrate", "ref_dbfs": -10.0,
                           "output_channel": 0, "input_channel": 0}));
    assert_eq!(r["ok"], json!(true));

    // Both prompts skipped — τ must still be measured (it keys only on
    // `is_loopback`, established at step 2, independent of the replies).
    for step in 1..=2 {
        c.wait_for_topic("cal_prompt", Duration::from_secs(5))
            .unwrap_or_else(|| panic!("step {step} prompt"));
        let _ = c.call(json!({"cmd": "cal_reply", "vrms": null}));
    }
    let done = c
        .wait_for_topic("cal_done", Duration::from_secs(5))
        .expect("cal_done frame");

    assert_eq!(done["tau_state"], json!("measured"), "frame: {done}");
    let tau_s = done["tau_s"].as_f64().expect("tau_s present when measured");
    let expected = 32.0 / 48_000.0; // FAKE_LOOPBACK_DELAY_SAMPLES / fake sample rate
    assert!(
        (tau_s - expected).abs() < 1e-4,
        "tau_s {tau_s} far from expected {expected} (32-sample fake loopback delay): {done}"
    );
    assert_eq!(done["tau_sample_rate"], json!(48_000), "frame: {done}");
    // #347: "measured" now means two independently-lifecycled readings
    // agreed — the fake backend's fixed loopback delay makes both
    // lifecycles land on the same 32-sample reading, so this must corroborate.
    assert_eq!(done["tau_agreement_count"], json!(2), "frame: {done}");
    assert!(done["tau_reading1_s"].as_f64().is_some(), "frame: {done}");
    assert!(done["tau_reading2_s"].as_f64().is_some(), "frame: {done}");
    // ZMQ.md: tau_delta_samples is present only on disagree_* — an Agree
    // outcome must not serialize a stray Some(0) (QA #348 correctness 1).
    assert!(done.get("tau_delta_samples").is_none(), "frame: {done}");
}

/// QA #348 test-coverage gap: every other disagreement test drives
/// `compare_tau_readings` or `tau_result` as a pure function, never
/// `measure_tau_twice` itself — the function that actually spins up two
/// engine lifecycles and feeds them into the comparison. A bug that mixed
/// up which lifecycle's `TauConditions` or reading fed the comparison
/// would pass every other test in this file. Drives it for real through
/// `calibrate`, using the fake backend's env-var delay/period-size test
/// hooks (`ac-daemon/src/audio/fake.rs`) to make the two lifecycles land
/// exactly one `period_size` apart.
#[test]
fn calibrate_reports_disagree_period_shift_end_to_end() {
    let d = Daemon::spawn_with_env(&[
        ("AC_FAKE_TAU_DELAY_SAMPLES_OVERRIDE", "32,1056"),
        ("AC_FAKE_PERIOD_SIZE_OVERRIDE", "1024"),
    ]);
    let cal_path = d.home.join(".config").join("ac").join("cal.json");
    let c = Client::new(&d);

    let r = c.call(json!({"cmd": "calibrate", "ref_dbfs": -10.0,
                           "output_channel": 0, "input_channel": 0}));
    assert_eq!(r["ok"], json!(true));

    for step in 1..=2 {
        c.wait_for_topic("cal_prompt", Duration::from_secs(5))
            .unwrap_or_else(|| panic!("step {step} prompt"));
        let _ = c.call(json!({"cmd": "cal_reply", "vrms": null}));
    }
    let done = c
        .wait_for_topic("cal_done", Duration::from_secs(5))
        .expect("cal_done frame");

    // 1056 - 32 = 1024 samples = exactly one period_size — the graph-
    // buffering shift the whole PR is about, not a generic fault.
    assert_eq!(
        done["tau_state"],
        json!("disagree_period_shift"),
        "frame: {done}"
    );
    assert_eq!(done["tau_periods"], json!(1), "frame: {done}");
    assert_eq!(done["tau_delta_samples"], json!(1024), "frame: {done}");
    assert_eq!(
        done["tau_s"],
        json!(null),
        "a disagreement must not report a τ: {done}"
    );
    assert_eq!(done["tau_agreement_count"], json!(0), "frame: {done}");
    assert!(done["tau_reading1_s"].as_f64().is_some(), "frame: {done}");
    assert!(done["tau_reading2_s"].as_f64().is_some(), "frame: {done}");
    assert!(done["tau_error"].as_str().is_some(), "frame: {done}");

    // Refused, not stored — no entry in tau_history at all.
    let after = read_cal_entry(&cal_path);
    assert!(
        after.get("tau_history").is_none()
            || after["tau_history"]
                .as_array()
                .is_some_and(|a| a.is_empty()),
        "a disagreement must not append to tau_history: {after}"
    );
}

/// #281 QA correctness issue 2: the cheap-refresh criterion (#279: both
/// voltage prompts skipped still refreshes stored state cheaply) is an
/// explicit issue acceptance criterion for τ too — a skipped-both-prompts
/// run must still append a fresh `tau_history` entry, not just leave the
/// voltage legs alone. Previously asserted only by reading the code (τ's
/// branch is keyed on `is_loopback`, not on either reply); this test pins
/// it down on the wire and on disk.
#[test]
fn calibrate_cheap_refresh_still_measures_tau() {
    let d = Daemon::spawn();
    let cal_path = seed_voltage_cal(&d, 2.345_67, 1.234_56, -20.0);
    let c = Client::new(&d);

    let r = c.call(json!({"cmd": "calibrate", "ref_dbfs": -10.0,
                          "output_channel": 0, "input_channel": 0}));
    assert_eq!(r["ok"], json!(true));

    for step in 1..=2 {
        c.wait_for_topic("cal_prompt", Duration::from_secs(5))
            .unwrap_or_else(|| panic!("step {step} prompt"));
        let _ = c.call(json!({"cmd": "cal_reply", "vrms": null}));
    }
    let done = c
        .wait_for_topic("cal_done", Duration::from_secs(5))
        .expect("cal_done frame");

    // Both voltage legs unchanged (the #279 path this test rides on)...
    assert_eq!(done["out_state"], json!("unchanged"), "frame: {done}");
    assert_eq!(done["in_state"], json!("unchanged"), "frame: {done}");
    // ...but τ was measured anyway.
    assert_eq!(done["tau_state"], json!("measured"), "frame: {done}");
    assert!(done["tau_s"].as_f64().is_some(), "frame: {done}");

    let after = read_cal_entry(&cal_path);
    let history = after["tau_history"]
        .as_array()
        .expect("tau_history present");
    assert_eq!(
        history.len(),
        1,
        "a cheap-refresh run must still append a tau_history entry: {after}"
    );
    // #347: a stored entry must record how many readings agreed — never
    // `1`, since a lone reading is no longer a storable outcome.
    assert_eq!(
        history[0]["agreement_count"],
        json!(2),
        "stored entry must record its corroboration count: {after}"
    );
}

/// Seed a `cal.json` entry with both voltage legs set, at a `ref_dbfs`
/// deliberately different from the one the test's `calibrate` run uses.
fn seed_voltage_cal(d: &Daemon, out_vrms: f64, in_vrms: f64, ref_dbfs: f64) -> PathBuf {
    let path = d.home.join(".config").join("ac").join("cal.json");
    let seeded = json!({
        "out0_in0": {
            "output_channel":                   0,
            "input_channel":                    0,
            "ref_freq":                         1000.0,
            "vrms_at_0dbfs_out":                out_vrms,
            "vrms_at_0dbfs_in":                 in_vrms,
            "ref_dbfs":                         ref_dbfs,
            "mic_sensitivity_dbfs_at_94db_spl": -32.5,
            "mic_response":                     null,
        }
    });
    fs::write(&path, serde_json::to_vec_pretty(&seeded).unwrap()).expect("seed cal.json");
    path
}

fn read_cal_entry(path: &PathBuf) -> Value {
    let raw = fs::read_to_string(path).expect("read cal.json");
    let all: Value = serde_json::from_str(&raw).expect("parse cal.json");
    all["out0_in0"].clone()
}

/// #279: a skipped prompt must preserve the stored reading, not erase it.
///
/// Mutation check — against the pre-fix handler, which assigned
/// `reading.map(..)` unconditionally, both `vrms_at_0dbfs_*` come back
/// `null` and every assertion below on a preserved value fails.
#[test]
fn calibrate_skipped_prompts_preserve_stored_voltage_cal() {
    let d = Daemon::spawn();
    let cal_path = seed_voltage_cal(&d, 2.345_67, 1.234_56, -20.0);
    let before = read_cal_entry(&cal_path);
    let c = Client::new(&d);

    // Run at a *different* ref_dbfs than the seeded entry records, so a
    // handler that rewrites `ref_dbfs` on a no-measurement run is caught
    // too — the stored level tag must keep describing the stored readings.
    let r = c.call(json!({"cmd": "calibrate", "ref_dbfs": -10.0,
                          "output_channel": 0, "input_channel": 0}));
    assert_eq!(r["ok"], json!(true));

    for step in 1..=2 {
        c.wait_for_topic("cal_prompt", Duration::from_secs(5))
            .unwrap_or_else(|| panic!("step {step} prompt"));
        let _ = c.call(json!({"cmd": "cal_reply", "vrms": null}));
    }
    let done = c
        .wait_for_topic("cal_done", Duration::from_secs(5))
        .expect("cal_done frame");

    // Three states, three words: nothing was measured, so both legs read
    // "unchanged" — not the null-valued frame that reads as "not measured".
    assert_eq!(done["out_state"], json!("unchanged"), "frame: {done}");
    assert_eq!(done["in_state"], json!("unchanged"), "frame: {done}");
    assert_eq!(done["vrms_at_0dbfs_out"], before["vrms_at_0dbfs_out"]);
    assert_eq!(done["vrms_at_0dbfs_in"], before["vrms_at_0dbfs_in"]);

    // On disk: both voltage fields survive bit-identical, and so does the
    // `ref_dbfs` that describes what level they were taken at.
    let after = read_cal_entry(&cal_path);
    assert_eq!(
        after["vrms_at_0dbfs_out"], before["vrms_at_0dbfs_out"],
        "skipping step 1 must not touch the stored output cal"
    );
    assert_eq!(
        after["vrms_at_0dbfs_in"], before["vrms_at_0dbfs_in"],
        "skipping step 2 must not touch the stored input cal"
    );
    assert_eq!(
        after["ref_dbfs"], before["ref_dbfs"],
        "a run that measured nothing must not relabel the stored readings"
    );
    // The other cal layers were already preserved via `load_or_new`;
    // assert it so this test fails if the preservation path is rewritten.
    assert_eq!(
        after["mic_sensitivity_dbfs_at_94db_spl"], before["mic_sensitivity_dbfs_at_94db_spl"],
        "SPL layer must be untouched by a voltage calibrate run"
    );
}

/// #279: erasing is still possible, but only when asked for by name —
/// `clear: true`, not the same reply that means "I did not measure this".
/// The two legs are independent: one cleared, one measured, in one run.
#[test]
fn calibrate_clear_erases_only_the_leg_it_names() {
    let d = Daemon::spawn();
    let cal_path = seed_voltage_cal(&d, 2.345_67, 1.234_56, -20.0);
    let c = Client::new(&d);

    let r = c.call(json!({"cmd": "calibrate", "ref_dbfs": -10.0,
                          "output_channel": 0, "input_channel": 0}));
    assert_eq!(r["ok"], json!(true));

    c.wait_for_topic("cal_prompt", Duration::from_secs(5))
        .expect("step 1 prompt");
    let _ = c.call(json!({"cmd": "cal_reply", "vrms": null, "clear": true}));

    c.wait_for_topic("cal_prompt", Duration::from_secs(5))
        .expect("step 2 prompt");
    let in_reading = 1.5_f64;
    let _ = c.call(json!({"cmd": "cal_reply", "vrms": in_reading}));

    let done = c
        .wait_for_topic("cal_done", Duration::from_secs(5))
        .expect("cal_done frame");
    assert_eq!(done["out_state"], json!("absent"), "frame: {done}");
    assert_eq!(done["in_state"], json!("measured"), "frame: {done}");
    assert_eq!(done["vrms_at_0dbfs_out"], json!(null), "frame: {done}");

    let after = read_cal_entry(&cal_path);
    assert_eq!(
        after["vrms_at_0dbfs_out"],
        json!(null),
        "an explicit clear must erase the output cal"
    );
    let stored_in = after["vrms_at_0dbfs_in"]
        .as_f64()
        .expect("input cal stored");
    assert!(
        stored_in > in_reading,
        "the measured leg must be rewritten (scaled up from the captured level), \
         got {stored_in} for a {in_reading} V reading"
    );
    // A measurement happened, so the level tag follows this run.
    assert_eq!(after["ref_dbfs"], json!(-10.0));
}

/// #279 criterion 3: `absent` has two origins, not one. A skip on a leg
/// that was never calibrated must report `absent`, not `unchanged` — the
/// no-prior-value branch of `apply_cal_reading` is otherwise untested.
/// Mutating that branch to `=> "unchanged"` passes every other calibrate
/// test in this file and fails only this one.
#[test]
fn calibrate_skip_on_uncalibrated_leg_reports_absent() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let r = c.call(json!({"cmd": "calibrate", "ref_dbfs": -10.0,
                          "output_channel": 0, "input_channel": 0}));
    assert_eq!(r["ok"], json!(true));

    for step in 1..=2 {
        c.wait_for_topic("cal_prompt", Duration::from_secs(5))
            .unwrap_or_else(|| panic!("step {step} prompt"));
        let _ = c.call(json!({"cmd": "cal_reply", "vrms": null}));
    }
    let done = c
        .wait_for_topic("cal_done", Duration::from_secs(5))
        .expect("cal_done frame");

    assert_eq!(done["out_state"], json!("absent"), "frame: {done}");
    assert_eq!(done["in_state"], json!("absent"), "frame: {done}");
    assert_eq!(done["vrms_at_0dbfs_out"], json!(null), "frame: {done}");
    assert_eq!(done["vrms_at_0dbfs_in"], json!(null), "frame: {done}");
}

/// #294 QA correctness issue 1: a cancel (`{"cmd": "stop"}`, what the
/// CLI's `q` sends) at the *second* prompt must not commit the first
/// prompt's reading. Pre-fix, the worker had no stop check after step 2
/// and fell through to `cal.save()` — the operator was told "Calibration
/// cancelled." while `vrms_at_0dbfs_out` and `ref_dbfs` were overwritten
/// anyway.
#[test]
fn calibrate_cancel_at_second_prompt_saves_nothing() {
    let d = Daemon::spawn();
    let cal_path = seed_voltage_cal(&d, 2.345_67, 1.234_56, -20.0);
    let before = read_cal_entry(&cal_path);
    let c = Client::new(&d);

    let r = c.call(json!({"cmd": "calibrate", "ref_dbfs": -10.0,
                          "output_channel": 0, "input_channel": 0}));
    assert_eq!(r["ok"], json!(true));

    c.wait_for_topic("cal_prompt", Duration::from_secs(5))
        .expect("step 1 prompt");
    let _ = c.call(json!({"cmd": "cal_reply", "vrms": 2.095}));

    c.wait_for_topic("cal_prompt", Duration::from_secs(5))
        .expect("step 2 prompt");
    let _ = c.call(json!({"cmd": "stop"}));

    // No cal_done at all — the run aborted, it did not complete with an
    // "unchanged"/"absent" verdict.
    assert!(
        c.wait_for_topic("cal_done", Duration::from_millis(500))
            .is_none(),
        "a cancelled run must not emit cal_done"
    );

    let after = read_cal_entry(&cal_path);
    assert_eq!(
        after, before,
        "a cancel at the second prompt must leave the stored entry \
         byte-identical, including the leg answered before the cancel"
    );
}

/// #295: symmetric with `calibrate_cancel_at_second_prompt_saves_nothing`,
/// but the cancel lands at the *first* prompt instead — the path that
/// worked all along, per the step-1 stop check at `handlers/calibrate.rs`
/// (checked immediately after `wait_cal_reply` for the output leg, before
/// `cal.save()`). No test pinned it, so a future edit to that check could
/// regress silently.
///
/// Mutation check — remove the step-1 stop check (or replace it with a
/// no-op) and this test must fail: `cal_done` would arrive instead of
/// timing out, and/or the stored entry would gain the seeded run's
/// `ref_dbfs`/output reading even though the operator cancelled before
/// answering it.
#[test]
fn calibrate_cancel_at_first_prompt_saves_nothing() {
    let d = Daemon::spawn();
    let cal_path = seed_voltage_cal(&d, 2.345_67, 1.234_56, -20.0);
    let before = read_cal_entry(&cal_path);
    let c = Client::new(&d);

    let r = c.call(json!({"cmd": "calibrate", "ref_dbfs": -10.0,
                          "output_channel": 0, "input_channel": 0}));
    assert_eq!(r["ok"], json!(true));

    c.wait_for_topic("cal_prompt", Duration::from_secs(5))
        .expect("step 1 prompt");
    let _ = c.call(json!({"cmd": "stop"}));

    // The step-1 check must return *before* step 2 is ever built — no
    // second cal_prompt. This is the assertion that actually pins the
    // step-1 check: the step-2 stop check (same `stop` flag, checked
    // again after step 2's wait_cal_reply) would independently catch a
    // cancel that fell through step 1 and still block cal_done/the save,
    // so only the absence of a second prompt distinguishes "step 1 caught
    // it" from "step 1 was bypassed and step 2 caught it instead."
    assert!(
        c.wait_for_topic("cal_prompt", Duration::from_millis(500))
            .is_none(),
        "a cancel at the first prompt must not advance to a second prompt"
    );

    // No cal_done at all — the run aborted before ever reaching step 2.
    assert!(
        c.wait_for_topic("cal_done", Duration::from_millis(500))
            .is_none(),
        "a cancelled run must not emit cal_done"
    );

    let after = read_cal_entry(&cal_path);
    assert_eq!(
        after, before,
        "a cancel at the first prompt must leave the stored entry \
         byte-identical — nothing was answered before the cancel"
    );
}

#[test]
fn plot_ir_emits_impulse_response_with_expected_delay_peak() {
    // Fake backend implements `play_and_capture` as a delayed loopback
    // (see audio/fake.rs). Running a Farina sweep through it and
    // deconvolving should produce a linear IR with its peak at the
    // window centre (the gate re-centres the peak on linear_ir.len()/2).
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd":"plot_ir",
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
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "measurement/impulse_response" => {
                let ir = v["data"]["linear_ir"].as_array().expect("linear_ir array");
                assert_eq!(ir.len(), 1024, "window_len respected");
                // Find the max-absolute sample index.
                let (peak_idx, peak_val) =
                    ir.iter().enumerate().fold((0usize, 0.0f64), |acc, (i, x)| {
                        let mag = x.as_f64().unwrap_or(0.0).abs();
                        if mag > acc.1 {
                            (i, mag)
                        } else {
                            acc
                        }
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
                assert_eq!(
                    v["report"]["data"][0]["data"]["kind"],
                    json!("impulse_response")
                );
                assert_eq!(v["report"]["schema_version"], json!(5));
                // #282 acceptance criterion 6: the ISO 18233 §6.3.2
                // tail-decay verdict rides in `notes`, not a silent default.
                let notes = v["report"]["notes"].as_str().expect("notes present");
                assert!(notes.contains("18233"), "notes: {notes:?}");
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

// ---------------------------------------------------------------------
// Drive ceiling (#360) — plot_ir and calibrate previously emitted an
// unclamped level; both are commands whose whole point is to put a
// stimulus on a physical output, and `drive_max_dbfs` governed neither.
// ---------------------------------------------------------------------

/// `plot_ir` clamps its requested level to `drive_max_dbfs`.
///
/// The deconvolved IR itself cannot be used as the observable here: the
/// handler deliberately re-scales the recovered impulse response by
/// `1/amp` (`plot.rs`, "so the reported IR has unity peak for an identity
/// loopback regardless of `level_dbfs`"), and the fake backend's
/// `play_and_capture` is a noiseless echo of exactly what was played — so
/// on this backend the published IR is invariant to level by construction,
/// clamped or not, and asserting on it would prove nothing.
///
/// `report.stimulus.level_dbfs` is emitted from inside the worker, after
/// the capture, from the same binding that scaled the actually-played
/// sweep (`let amp = dbfs_to_amplitude(level_dbfs)`) — a different
/// computation from the synchronous CTRL reply, so this does not just
/// re-check the same echo twice under two names.
#[test]
fn plot_ir_clamps_level_to_drive_max_dbfs() {
    const CEILING_DBFS: f64 = -35.0;
    let d = Daemon::spawn_with_config(Some(json!({ "drive_max_dbfs": CEILING_DBFS })));
    let c = Client::new(&d);

    let r = c.call(json!({
        "cmd": "plot_ir",
        "f1_hz": 200.0,
        "f2_hz": 8_000.0,
        "duration": 0.5,
        "level_dbfs": 12.0,
        "tail_s": 0.1,
        "window_len": 1024,
        "n_harmonics": 3,
    }));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(
        r["level_dbfs"],
        json!(CEILING_DBFS),
        "sync reply must echo the applied (clamped) level, not the request: {r}"
    );

    let v = c
        .wait_for_topic("measurement/report", Duration::from_secs(15))
        .expect("measurement/report frame");
    let applied = v["report"]["stimulus"]["level_dbfs"]
        .as_f64()
        .expect("stimulus.level_dbfs");
    assert!(
        (applied - CEILING_DBFS).abs() < 1e-9,
        "report recorded level {applied}, requested 12.0 dBFS against a {CEILING_DBFS} \
         ceiling — plot_ir emitted the raw request instead of the clamped level"
    );
}

/// `calibrate` clamps its `ref_dbfs` to `drive_max_dbfs`, and an omitted
/// `ref_dbfs` defaults to the ceiling rather than a hardcoded -10.0.
///
/// `cal_prompt` step 2's `captured_dbfs` is a genuine round trip through
/// the fake engine — `capture_rms` reads back whatever `eng.set_tone` was
/// actually given, via the same capture path `analyze_mono` and `plot`
/// use — not a re-statement of the request. A sine's RMS sits ~3.01 dB
/// below its peak amplitude, so a tone actually played at the ceiling
/// reads back at `ceiling - 3.01`, not at the ~-3.0 dBFS a full-scale,
/// unclamped 0 dBFS request would produce — the two are far enough apart
/// that a clamp that silently didn't apply cannot pass this by accident.
#[test]
fn calibrate_default_and_explicit_ref_dbfs_are_clamped_to_the_ceiling() {
    const CEILING_DBFS: f64 = -25.0;
    const PEAK_TO_RMS_DB: f64 = 3.0103; // 20·log10(√2)
    let d = Daemon::spawn_with_config(Some(json!({ "drive_max_dbfs": CEILING_DBFS })));
    let c = Client::new(&d);

    // No `ref_dbfs` at all: must default to the session ceiling, not the
    // historical hardcoded -10.0 (#360 acceptance criterion 2).
    let r = c.call(json!({"cmd": "calibrate", "output_channel": 0, "input_channel": 0}));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(
        r["ref_dbfs"],
        json!(CEILING_DBFS),
        "an omitted ref_dbfs must default to drive_max_dbfs: {r}"
    );

    let step1 = c
        .wait_for_topic("cal_prompt", Duration::from_secs(5))
        .expect("step 1 prompt");
    assert_eq!(step1["ref_dbfs"], json!(CEILING_DBFS));
    let _ = c.call(json!({"cmd": "cal_reply", "vrms": null}));

    let step2 = c
        .wait_for_topic("cal_prompt", Duration::from_secs(5))
        .expect("step 2 prompt");
    let captured_dbfs = step2["captured_dbfs"].as_f64().expect("captured_dbfs");
    let expected = CEILING_DBFS - PEAK_TO_RMS_DB;
    assert!(
        (captured_dbfs - expected).abs() < 1.5,
        "captured {captured_dbfs} dBFS does not match a tone actually played at the \
         {CEILING_DBFS} dBFS ceiling (expected ~{expected}) — the default was not clamped \
         before the tone was set"
    );
    let _ = c.call(json!({"cmd": "cal_reply", "vrms": null}));
    let _ = c.wait_for_topic("cal_done", Duration::from_secs(5));

    // Explicit request above the ceiling: also clamped, defense in depth.
    let r = c.call(json!({
        "cmd": "calibrate", "ref_dbfs": 0.0, "output_channel": 0, "input_channel": 0,
    }));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(
        r["ref_dbfs"],
        json!(CEILING_DBFS),
        "an explicit ref_dbfs above the ceiling must be clamped: {r}"
    );
    let step1 = c
        .wait_for_topic("cal_prompt", Duration::from_secs(5))
        .expect("step 1 prompt");
    assert_eq!(step1["ref_dbfs"], json!(CEILING_DBFS));
    let _ = c.call(json!({"cmd": "cal_reply", "vrms": null}));
    let step2 = c
        .wait_for_topic("cal_prompt", Duration::from_secs(5))
        .expect("step 2 prompt");
    let captured_dbfs = step2["captured_dbfs"].as_f64().expect("captured_dbfs");
    assert!(
        (captured_dbfs - expected).abs() < 1.5,
        "captured {captured_dbfs} dBFS does not match a tone actually played at the \
         {CEILING_DBFS} dBFS ceiling (expected ~{expected}) — an explicit request above \
         the ceiling reached the engine unclamped"
    );
}

/// The remaining #360 call sites (`generate`, `generate_pink`,
/// `sweep_level`, `sweep_frequency`, `plot`, `plot_level`) all echo the
/// applied level on their sync reply, same as `plot_ir`/`calibrate` above
/// and `set_drive` before them. One clamp-above-ceiling check per command
/// — the shared `apply_drive_ceiling` chokepoint itself is unit-tested in
/// `handlers/mod.rs`, so this is coverage that each site actually calls it,
/// not a re-test of the clamp arithmetic.
#[test]
fn generate_and_generate_pink_clamp_level_to_the_ceiling() {
    const CEILING_DBFS: f64 = -20.0;
    let d = Daemon::spawn_with_config(Some(json!({ "drive_max_dbfs": CEILING_DBFS })));
    let c = Client::new(&d);

    let r = c.call(json!({"cmd": "generate", "freq_hz": 1000.0, "level_dbfs": 6.0}));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(r["level_dbfs"], json!(CEILING_DBFS), "{r}");
    let _ = c.call(json!({"cmd": "stop"}));

    let r = c.call(json!({"cmd": "generate_pink", "level_dbfs": 6.0}));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(r["level_dbfs"], json!(CEILING_DBFS), "{r}");
    let _ = c.call(json!({"cmd": "stop"}));
}

#[test]
fn sweep_level_clamps_each_ramp_point_and_echoes_the_applied_range() {
    const CEILING_DBFS: f64 = -20.0;
    let d = Daemon::spawn_with_config(Some(json!({ "drive_max_dbfs": CEILING_DBFS })));
    let c = Client::new(&d);

    // Entire requested range sits above the ceiling — the degenerate case
    // where the ramp's applied shape collapses to a flat line at the
    // ceiling (UX spec, issue #360).
    let r = c.call(json!({
        "cmd": "sweep_level", "freq_hz": 1000.0,
        "start_dbfs": -10.0, "stop_dbfs": 6.0, "duration": 0.2,
    }));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(r["start_dbfs"], json!(CEILING_DBFS), "{r}");
    assert_eq!(r["stop_dbfs"], json!(CEILING_DBFS), "{r}");
    let _ = c.wait_for_topic("done", Duration::from_secs(5));

    // Partial overlap: only the top end is clamped.
    let r = c.call(json!({
        "cmd": "sweep_level", "freq_hz": 1000.0,
        "start_dbfs": -40.0, "stop_dbfs": -10.0, "duration": 0.2,
    }));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(r["start_dbfs"], json!(-40.0), "{r}");
    assert_eq!(r["stop_dbfs"], json!(CEILING_DBFS), "{r}");
    let _ = c.wait_for_topic("done", Duration::from_secs(5));
}

#[test]
fn sweep_frequency_clamps_level_to_the_ceiling() {
    const CEILING_DBFS: f64 = -20.0;
    let d = Daemon::spawn_with_config(Some(json!({ "drive_max_dbfs": CEILING_DBFS })));
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd": "sweep_frequency", "start_hz": 100.0, "stop_hz": 200.0,
        "level_dbfs": 6.0, "duration": 0.2,
    }));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(r["level_dbfs"], json!(CEILING_DBFS), "{r}");
    let _ = c.wait_for_topic("done", Duration::from_secs(5));
}

#[test]
fn plot_clamps_level_to_the_ceiling() {
    const CEILING_DBFS: f64 = -20.0;
    let d = Daemon::spawn_with_config(Some(json!({ "drive_max_dbfs": CEILING_DBFS })));
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd": "plot", "start_hz": 500.0, "stop_hz": 600.0,
        "level_dbfs": 6.0, "ppd": 2, "duration": 0.05,
    }));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(r["level_dbfs"], json!(CEILING_DBFS), "{r}");
    let _ = c.wait_for_topic("done", Duration::from_secs(10));
}

#[test]
fn plot_level_clamps_the_range_and_echoes_it_applied() {
    const CEILING_DBFS: f64 = -20.0;
    let d = Daemon::spawn_with_config(Some(json!({ "drive_max_dbfs": CEILING_DBFS })));
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd": "plot_level", "freq_hz": 1000.0,
        "start_dbfs": -40.0, "stop_dbfs": -10.0, "steps": 3, "duration": 0.05,
    }));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(r["start_dbfs"], json!(-40.0), "{r}");
    assert_eq!(r["stop_dbfs"], json!(CEILING_DBFS), "{r}");
    let _ = c.wait_for_topic("done", Duration::from_secs(10));
}

/// #283: `plot_ir` resolves τ by *exact* match on `TauConditions`, and
/// the entry it must hit was written by `calibrate`. Nothing but a test
/// couples those two condition tuples — they are built in different
/// handlers, from different locals — so a drift in either (a port
/// resolved differently, a device field read from elsewhere) would leave
/// every `plot_ir` reporting "distance unavailable" forever, with no
/// error anywhere. The failure is silent by construction, so it needs an
/// explicit check that the round trip lands.
#[test]
fn plot_ir_resolves_the_tau_that_calibrate_stored() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    // 1. Measure τ. Both voltage prompts skipped — τ is keyed on
    //    loopback detection, not on either reply (see
    //    `calibrate_cheap_refresh_still_measures_tau`).
    let r = c.call(json!({"cmd": "calibrate", "ref_dbfs": -10.0,
                          "output_channel": 0, "input_channel": 0}));
    assert_eq!(r["ok"], json!(true));
    for step in 1..=2 {
        c.wait_for_topic("cal_prompt", Duration::from_secs(5))
            .unwrap_or_else(|| panic!("step {step} prompt"));
        let _ = c.call(json!({"cmd": "cal_reply", "vrms": null}));
    }
    let done = c
        .wait_for_topic("cal_done", Duration::from_secs(5))
        .expect("cal_done frame");
    assert_eq!(done["tau_state"], json!("measured"), "frame: {done}");
    let stored_tau = done["tau_s"].as_f64().expect("tau_s");

    // 2. Run an IR capture under the same conditions.
    let r = c.call(json!({
        "cmd":"plot_ir",
        "f1_hz": 200.0,
        "f2_hz": 8_000.0,
        "duration": 0.5,
        "level_dbfs": -6.0,
        "tail_s": 0.1,
        "window_len": 1024,
        "n_harmonics": 3,
    }));
    assert_eq!(r["ok"], json!(true));
    let v = c
        .wait_for_topic("measurement/report", Duration::from_secs(15))
        .expect("measurement/report frame");

    let latency = &v["report"]["interface_latency"];
    assert_eq!(
        latency["state"],
        json!("measured"),
        "plot_ir did not match calibrate's stored τ — the two TauConditions \
         tuples have drifted apart: {latency}"
    );
    let used_tau = latency["tau_s"].as_f64().expect("tau_s in report");
    assert!(
        (used_tau - stored_tau).abs() < 1e-12,
        "plot_ir used τ {used_tau}, calibrate stored {stored_tau}"
    );
    // The τ provenance must be archived, not just the number.
    assert!(latency["measured_at"].is_string(), "{latency}");
    assert_eq!(latency["method"], json!("farina_short_ess_v2"), "{latency}");

    // With a τ this close to the arrival (both are the same 32-sample
    // fake loopback), the τ-corrected flight time must land near zero —
    // the fake backend has no acoustic path. A τ that failed to subtract
    // would read ~0.67 ms instead (32 samples at 48 kHz).
    let report: ac_core::measurement::report::MeasurementReport =
        serde_json::from_value(v["report"].clone()).expect("decode report");
    let stats = report.ir_stats().expect("ir_stats");
    let flight_ms = (stats.arrival_s - used_tau) * 1000.0;
    assert!(
        flight_ms.abs() < 0.15,
        "fake loopback has no acoustic path, got {flight_ms} ms"
    );
}

#[test]
fn plot_ir_reports_the_gate_lengths_it_actually_used() {
    // #278: `window_len` is a request. Adjacent harmonic orders would
    // cross-contaminate if their gates overlapped, so each order is clamped
    // to the spacing of its nearest neighbour. At 200 Hz–8 kHz over 0.5 s at
    // 48 kHz, L = T/ln(f2/f1) = 135.5 ms, so Farina's Δt_k = L·ln(k) puts the
    // order centres at [0, 4510, 7148, 9019, 10471] samples and the gaps at
    // [4510, 2638, 1871, 1452].
    //
    // The linear IR must survive at the full 4096 — its only neighbour is
    // order 2, 4510 samples away — while orders 2..5 shrink. A global clamp
    // to the narrowest gap would cut the linear IR to 1452 and is what this
    // test is here to catch.
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd":"plot_ir",
        "f1_hz": 200.0,
        "f2_hz": 8_000.0,
        "duration": 0.5,
        "level_dbfs": -6.0,
        "tail_s": 0.1,
        "window_len": 4096,
        "n_harmonics": 5,
    }));
    assert_eq!(r["ok"], json!(true));

    let expected_used = json!([4096, 2638, 1871, 1452, 1452]);
    let mut got_ir = false;
    let mut got_report = false;
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline && !(got_ir && got_report) {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "measurement/impulse_response" => {
                assert_eq!(v["window_len_requested"], json!(4096));
                assert_eq!(
                    v["window_len_used"], expected_used,
                    "per-order gate lengths must be reported, not inferred"
                );
                let ir = v["data"]["linear_ir"].as_array().expect("linear_ir array");
                assert_eq!(
                    ir.len(),
                    4096,
                    "linear IR must keep the requested window; only the \
                     tight high orders are constrained"
                );
                let harmonics = v["data"]["harmonics"].as_array().expect("harmonics");
                for h in harmonics {
                    let order = h["order"].as_u64().expect("order") as usize;
                    let n = h["samples"].as_array().expect("samples").len();
                    assert_eq!(
                        json!(n),
                        expected_used[order - 1],
                        "order {order} gate length disagrees with window_len_used"
                    );
                }
                got_ir = true;
            }
            Some((t, v)) if t == "measurement/report" => {
                // A shortened gate changes what the harmonic IRs mean, so
                // it has to reach the operator rather than being applied
                // silently.
                let notes = v["report"]["notes"].as_str().expect("notes present");
                assert!(notes.contains("clamped"), "notes: {notes:?}");
                assert!(notes.contains("4096"), "notes: {notes:?}");
                assert!(notes.contains("order 2"), "notes: {notes:?}");
                assert!(
                    !notes.contains("order 1 \u{2192}"),
                    "the unclamped linear IR must not be listed: {notes:?}"
                );
                // The #282 tail-decay verdict shares the field and must
                // not have been displaced by the clamp note.
                assert!(notes.contains("18233"), "notes: {notes:?}");
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

/// #283 × #278: the `GateParams` archived on the IR payload must describe
/// the gate that actually ran, not the one that was asked for.
///
/// `window_len` is a request (#278) and the linear IR is clamped when
/// order 2 sits closer than the requested length. Recording the request
/// instead would archive a gate that never ran and an `f_low_hz` the
/// payload does not meet — and `f_low_hz` is the number #280 stores
/// precisely so a reader does not have to derive it.
///
/// The sweep is chosen so the two values differ: over 0.3 s from 200 Hz
/// to 8 kHz, L = T/ln(f2/f1) = 81.3 ms, so Farina's Δt_2 = L·ln 2 puts
/// order 2 about 2705 samples out — inside the requested 4096. An
/// implementation that recorded `window_len` would report a 4096-sample
/// gate and an f_low of 11.7 Hz here, both wrong.
#[test]
fn plot_ir_records_the_gate_it_used_not_the_one_requested() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd":"plot_ir",
        "f1_hz": 200.0,
        "f2_hz": 8_000.0,
        "duration": 0.3,
        "level_dbfs": -6.0,
        "tail_s": 0.1,
        "window_len": 4096,
        "n_harmonics": 3,
    }));
    assert_eq!(r["ok"], json!(true));

    let mut used: Option<u64> = None;
    let mut gate: Option<Value> = None;
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline && !(used.is_some() && gate.is_some()) {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "measurement/impulse_response" => {
                used = v["window_len_used"][0].as_u64();
            }
            Some((t, v)) if t == "measurement/report" => {
                gate = Some(v["report"]["data"][0]["gate"].clone());
            }
            Some((t, _)) if t == "done" => break,
            Some(_) => continue,
            None => break,
        }
    }
    let used = used.expect("window_len_used[0] on the IR frame");
    let gate = gate.expect("gate on the IR payload");

    // The premise: this sweep really does clamp. Without this the test
    // would pass against an implementation that records the request.
    assert!(
        used < 4096,
        "sweep did not clamp the linear IR ({used} samples) — the test no \
         longer distinguishes the recorded gate from the requested one"
    );

    let sr = 48_000.0;
    let gate_length_s = gate["gate_length_s"].as_f64().expect("gate_length_s");
    let f_low_hz = gate["f_low_hz"].as_f64().expect("f_low_hz");
    let gate_start_s = gate["gate_start_s"].as_f64().expect("gate_start_s");

    assert!(
        (gate_length_s - used as f64 / sr).abs() < 1e-9,
        "recorded gate_length_s {gate_length_s} does not match the {used} \
         samples actually used"
    );
    assert!(
        (f_low_hz - sr / used as f64).abs() < 1e-6,
        "recorded f_low_hz {f_low_hz} does not match the gate that ran"
    );
    // The gate is centred on the zero-delay reference, so it opens half a
    // window before it.
    assert!(
        (gate_start_s + (used / 2) as f64 / sr).abs() < 1e-9,
        "recorded gate_start_s {gate_start_s} is not half a window early"
    );
}

/// #284: `plot_ir`'s report carries a second payload — the gated
/// frequency response derived from the linear IR — alongside the
/// impulse-response payload. Its `f_low_hz` must match `1 / gate_length_s`
/// by hand arithmetic, and the points must carry both magnitude and phase.
#[test]
fn plot_ir_emits_a_gated_frequency_response_payload() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd":"plot_ir",
        "f1_hz": 200.0,
        "f2_hz": 8_000.0,
        "duration": 0.5,
        "level_dbfs": -6.0,
        "tail_s": 0.1,
        "window_len": 1024,
        "n_harmonics": 1,
    }));
    assert_eq!(r["ok"], json!(true));

    let v = c
        .wait_for_topic("measurement/report", Duration::from_secs(15))
        .expect("measurement/report frame");
    let data = v["report"]["data"].as_array().expect("data array");
    assert_eq!(data.len(), 2, "expected impulse response + gated response");
    assert_eq!(data[0]["data"]["kind"], json!("impulse_response"));
    assert_eq!(data[1]["data"]["kind"], json!("gated_frequency_response"));

    // Citations: the payload cites the Farina preprint (theoretical basis
    // only) and AES17-2020 Annex A.4.5 for the gating method itself — not
    // ISO 18233, which per the architect's #284 decision 4 only attaches
    // when a classical room standard also applies, which a quasi-anechoic
    // capture never has (PR #305 review, correctness issue 1).
    let standards = data[1]["standard"].as_array().expect("standard array");
    assert_eq!(standards.len(), 2, "{standards:?}");
    assert!(
        standards
            .iter()
            .any(|s| s["standard"].as_str().unwrap_or("").contains("AES17")),
        "{standards:?}"
    );
    assert!(
        !standards
            .iter()
            .any(|s| s["standard"].as_str().unwrap_or("").contains("ISO 18233")),
        "gated_frequency_response payload must not cite ISO 18233: {standards:?}"
    );

    // f_low_hz = 1 / gate_length_s, by hand arithmetic off the recorded
    // gate — not recomputed elsewhere.
    let gate = &data[1]["gate"];
    let gate_length_s = gate["gate_length_s"].as_f64().expect("gate_length_s");
    let f_low_hz = gate["f_low_hz"].as_f64().expect("f_low_hz");
    assert!(
        (f_low_hz - 1.0 / gate_length_s).abs() < 1e-9,
        "f_low_hz {f_low_hz} does not match 1/gate_length_s {}",
        1.0 / gate_length_s
    );
    assert_eq!(gate["window_kind"], json!("tukey0.25"));

    let points = data[1]["data"]["points"].as_array().expect("points array");
    assert!(points.len() > 4, "expected several frequency bins");
    for p in points {
        assert!(p["freq_hz"].is_number());
        assert!(p["magnitude_db"].is_number());
        assert!(p["phase_deg"].is_number());
    }

    // The impulse-response payload now also carries the noise-tail
    // boundary — derivable, not left to the reader (#284).
    let noise_tail = data[0]["data"]["noise_tail_start_s"]
        .as_f64()
        .expect("noise_tail_start_s present");
    assert!(
        (noise_tail - 0.5).abs() < 1e-9,
        "noise_tail_start_s should equal the sweep duration (0.5s): {noise_tail}"
    );
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
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "data" && v["type"].as_str() == Some("transfer_stream") => {
                for key in [
                    "freqs",
                    "magnitude_db",
                    "phase_deg",
                    "coherence",
                    "re",
                    "im",
                    "delay_samples",
                    "delay_ms",
                ] {
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
    let done = c
        .wait_for_topic("done", Duration::from_secs(5))
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
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "data" && v["type"].as_str() == Some("visualize/ir") => {
                for key in [
                    "samples",
                    "sr",
                    "dt_ms",
                    "t_origin_ms",
                    "ref_channel",
                    "meas_channel",
                ] {
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

/// handoff-transfer-frame-v2.md M0, AC #1 (presence) + AC #6 (wire economy,
/// measured/printed here for the PR description). Every `transfer_stream`
/// data frame must carry the new fields; `spec_freqs` is identical across
/// frames (deterministic function of a session-fixed sr/f_min/f_max/K).
#[test]
fn transfer_stream_frame_v2_fields_present_and_spec_freqs_stable() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd":          "transfer_stream",
        "meas_channel": 0,
        "ref_channel":  1,
    }));
    assert_eq!(r["ok"], json!(true), "REP: {r:?}");

    let mut frames: Vec<Value> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while frames.len() < 2 && Instant::now() < deadline {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "data" && v["type"].as_str() == Some("transfer_stream") => {
                frames.push(v);
            }
            Some(_) => continue,
            None => break,
        }
    }
    let _ = c.call(json!({"cmd": "stop"}));
    assert!(
        frames.len() >= 2,
        "need >=2 transfer_stream frames, got {}",
        frames.len()
    );

    for key in [
        "spec_freqs",
        "meas_spectrum",
        "ref_spectrum",
        "spl",
        "spl_weighting",
        "spl_integration",
        "cal_tags",
    ] {
        for f in &frames {
            assert!(f.get(key).is_some(), "frame missing {key}: {f}");
        }
    }
    let f0 = &frames[0];
    assert_eq!(f0["spl_weighting"], json!("Z"), "default weighting");
    assert_eq!(f0["spl_integration"], json!("fast"), "default integration");
    assert!(f0["spl"].is_null(), "no cal loaded, spl must be null: {f0}");
    for role in ["meas", "ref"] {
        let tags = &f0["cal_tags"][role];
        assert_eq!(tags["voltage"], json!("none"), "{role} voltage tag");
        assert_eq!(tags["spl"], json!("none"), "{role} spl tag");
        assert_eq!(tags["mic_curve"], json!("none"), "{role} mic_curve tag");
    }

    let sf0 = frames[0]["spec_freqs"].as_array().unwrap();
    let sf1 = frames[1]["spec_freqs"].as_array().unwrap();
    assert_eq!(sf0, sf1, "spec_freqs must be identical across frames");
    assert_eq!(
        sf0.len(),
        f0["meas_spectrum"].as_array().unwrap().len(),
        "spec_freqs / meas_spectrum length mismatch"
    );

    // AC #6: measure and print the actual per-frame wire size so the PR
    // description can state it — both the whole frame (existing + new
    // fields) and the new-fields-only delta (K × f64 × {spec_freqs,
    // meas_spectrum, ref_spectrum} dominate the addition).
    let bytes = serde_json::to_vec(f0).unwrap().len();
    let new_fields_only = json!({
        "spec_freqs":      f0["spec_freqs"],
        "meas_spectrum":   f0["meas_spectrum"],
        "ref_spectrum":    f0["ref_spectrum"],
        "spl":             f0["spl"],
        "spl_weighting":   f0["spl_weighting"],
        "spl_integration": f0["spl_integration"],
        "cal_tags":        f0["cal_tags"],
    });
    let new_bytes = serde_json::to_vec(&new_fields_only).unwrap().len();
    eprintln!(
        "transfer_stream frame v2: K={} columns, {} bytes/frame total (1 pair), \
         {} bytes/frame from the new M0 fields alone",
        sf0.len(),
        bytes,
        new_bytes
    );
}

/// #238: every published frame carries `delay_attempts`, and it is already
/// non-zero on the first one.
///
/// Both halves matter. The field is what lets a consumer separate a refusing
/// pair from a warming-up one — `delay_locked` is false for both — and the
/// fault indicator's two refusal states were unreachable without it. And
/// "non-zero on the first frame" is what makes the refusal clock honest: the
/// daemon publishes nothing until the rings hold a full Welch segment, which
/// is also when the first estimate runs, so a consumer counting from its first
/// frame counts from the first moment a lock was possible — not from session
/// start.
#[test]
fn transfer_stream_reports_delay_attempts_from_the_first_frame() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd":          "transfer_stream",
        "meas_channel": 0,
        "ref_channel":  1,
    }));
    assert_eq!(r["ok"], json!(true), "REP: {r:?}");

    let mut frames: Vec<Value> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while frames.len() < 2 && Instant::now() < deadline {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "data" && v["type"].as_str() == Some("transfer_stream") => {
                frames.push(v);
            }
            Some(_) => continue,
            None => break,
        }
    }
    let _ = c.call(json!({"cmd": "stop"}));
    assert!(
        frames.len() >= 2,
        "need >=2 transfer_stream frames, got {}",
        frames.len()
    );

    for f in &frames {
        let attempts = f["delay_attempts"]
            .as_u64()
            .unwrap_or_else(|| panic!("delay_attempts missing or not an integer: {f}"));
        assert!(
            attempts >= 1,
            "a frame was published before the estimator answered: {f}"
        );
        // The count says the estimator ran; `delay_locked` says what it
        // decided. Neither is inferred from the other.
        assert!(
            f["delay_locked"].is_boolean(),
            "delay_locked missing alongside delay_attempts: {f}"
        );
    }
}

/// AC #2 (amplitude truth): fake channel 0's default stimulus is a 1 kHz
/// sine at 0.1 peak amplitude (audio/fake.rs) = -20 dBFS, exactly bin-
/// aligned (nperseg=sr=48000 ⇒ Δf=1 Hz). `meas_spectrum`'s peak column
/// must land within tolerance of that, at the right frequency.
///
/// Tolerance derivation: at K≈491 (48 cols/octave, 20 Hz-24 kHz) a column
/// near 1 kHz spans ~14 Welch bins — wide enough to include the tone's
/// own Hann-window leakage into its ±1 Hz neighbour bins. A raised-cosine
/// window's DFT is the 3-tap kernel `[-0.25, 0.5, -0.25]`, so an exact-
/// bin tone of "ideal" amplitude X reads `0.5X` at the centre bin and
/// `0.25X` at each neighbour; band-power-summing those three,
/// `sqrt(0.5² + 0.25² + 0.25²) / 0.5 ≈ 1.2247` (+1.76 dB), is real signal
/// energy the aggregator is correctly summing (D18), not an artifact.
/// 2.2 dB clears that bound with margin while still catching a
/// several-dB normalization regression.
#[test]
fn transfer_stream_meas_spectrum_amplitude_truth() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd":          "transfer_stream",
        "meas_channel": 0,
        "ref_channel":  1,
    }));
    assert_eq!(r["ok"], json!(true), "REP: {r:?}");

    let mut frame: Option<Value> = None;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "data" && v["type"].as_str() == Some("transfer_stream") => {
                frame = Some(v);
                break;
            }
            Some(_) => continue,
            None => break,
        }
    }
    let _ = c.call(json!({"cmd": "stop"}));
    let frame = frame.expect("no transfer_stream frame within 10 s");

    let freqs: Vec<f64> = frame["spec_freqs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect();
    let spectrum: Vec<f64> = frame["meas_spectrum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect();
    let (peak_i, &peak_amp) = spectrum
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
        .expect("non-empty meas_spectrum");
    let peak_hz = freqs[peak_i];
    let peak_dbfs = 20.0 * peak_amp.max(1e-12).log10();

    assert!(
        (peak_hz - 1000.0).abs() < 50.0,
        "peak at {peak_hz} Hz, expected ~1000 Hz"
    );
    assert!(
        (peak_dbfs - -20.0).abs() < 2.2,
        "peak {peak_dbfs} dBFS, expected ~-20 dBFS (fake ch0 default tone) within 2.2 dB"
    );
}

/// Invalid `weighting`/`integration` session params are rejected
/// synchronously before worker spawn, matching the handoff's validation
/// style — including the strict A/C/Z-only contract (no "off", unlike
/// `set_band_weighting`'s 4-way enum). Also checks no partial session is
/// left behind: `status` reports idle right after a rejection, not just
/// "the trailing valid call happened to succeed".
#[test]
fn transfer_stream_rejects_invalid_spl_session_params() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let r = c.call(json!({
        "cmd": "transfer_stream", "meas_channel": 0, "ref_channel": 1, "weighting": "off",
    }));
    assert_eq!(r["ok"], json!(false), "weighting=off must be rejected: {r}");
    let s = c.call(json!({"cmd": "status"}));
    assert_eq!(
        s["busy"],
        json!(false),
        "rejected weighting must leave no partial session running: {s}"
    );

    let r = c.call(json!({
        "cmd": "transfer_stream", "meas_channel": 0, "ref_channel": 1, "weighting": "q",
    }));
    assert_eq!(r["ok"], json!(false), "weighting=q must be rejected: {r}");

    let r = c.call(json!({
        "cmd": "transfer_stream", "meas_channel": 0, "ref_channel": 1, "integration": "leq",
    }));
    assert_eq!(
        r["ok"],
        json!(false),
        "integration=leq not implemented in M0, must be rejected: {r}"
    );

    let r = c.call(json!({
        "cmd": "transfer_stream", "meas_channel": 0, "ref_channel": 1,
        "weighting": "a", "integration": "SLOW",
    }));
    assert_eq!(r["ok"], json!(true), "valid params (case-insensitive): {r}");
    let _ = c.call(json!({"cmd": "stop"}));
    let _ = c.wait_for_topic("done", Duration::from_secs(5));
}

/// QA gap fill: the presence/rejection tests above only exercise the
/// uncalibrated (`cal_tags` all "none", `spl: null`) path. This drives
/// the calibrated branch — voltage cal + SPL cal + mic curve all loaded
/// on the meas channel, nothing on the ref channel — and checks every
/// new tag flips to "on" (meas) / stays "none" (ref) accordingly, and
/// `spl` becomes a finite, plausible number.
#[test]
fn transfer_stream_cal_tags_and_spl_reflect_loaded_calibration() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    // Voltage cal on channel 0 (both directions get saved by `calibrate`).
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

    // SPL cal on channel 0.
    let r = c.call(json!({"cmd": "calibrate_spl", "input_channel": 0, "capture_s": 0.05}));
    assert_eq!(r["ok"], json!(true));
    let _ = c
        .wait_for_topic("cal_prompt", Duration::from_secs(3))
        .expect("spl cal_prompt");
    let _ = c.call(json!({"cmd": "cal_reply", "vrms": Value::Null}));
    let _ = c
        .wait_for_topic("cal_done", Duration::from_secs(5))
        .expect("spl cal_done");

    // Mic curve on channel 0.
    let (freqs, gains) = {
        let mut f = Vec::with_capacity(24);
        let mut g = Vec::with_capacity(24);
        let log_min = 100.0_f64.ln();
        let log_max = 10_000.0_f64.ln();
        for i in 0..24 {
            let t = i as f64 / 23.0;
            f.push((log_min + t * (log_max - log_min)).exp());
            g.push(3.0);
        }
        (f, g)
    };
    let r = c.call(json!({
        "cmd": "calibrate_mic_curve", "op": "set", "input_channel": 0,
        "freqs_hz": freqs, "gain_db": gains,
    }));
    assert_eq!(r["ok"], json!(true));
    while c.recv_pub(50).is_some() {}

    let r = c.call(json!({
        "cmd": "transfer_stream", "meas_channel": 0, "ref_channel": 1,
    }));
    assert_eq!(r["ok"], json!(true), "REP: {r:?}");

    let mut frame: Option<Value> = None;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "data" && v["type"].as_str() == Some("transfer_stream") => {
                frame = Some(v);
                break;
            }
            Some(_) => continue,
            None => break,
        }
    }
    let _ = c.call(json!({"cmd": "stop"}));
    let frame = frame.expect("no transfer_stream frame within 10 s");

    let meas_tags = &frame["cal_tags"]["meas"];
    assert_eq!(
        meas_tags["voltage"],
        json!("on"),
        "meas voltage tag: {frame}"
    );
    assert_eq!(meas_tags["spl"], json!("on"), "meas spl tag: {frame}");
    assert_eq!(
        meas_tags["mic_curve"],
        json!("on"),
        "meas mic_curve tag: {frame}"
    );

    let ref_tags = &frame["cal_tags"]["ref"];
    assert_eq!(
        ref_tags["voltage"],
        json!("none"),
        "ref voltage tag: {frame}"
    );
    assert_eq!(ref_tags["spl"], json!("none"), "ref spl tag: {frame}");
    assert_eq!(
        ref_tags["mic_curve"],
        json!("none"),
        "ref mic_curve tag: {frame}"
    );

    let spl = frame["spl"].as_f64().unwrap_or_else(|| {
        panic!("spl must be a finite number when meas channel is SPL-calibrated: {frame}")
    });
    assert!(
        spl.is_finite() && (0.0..=200.0).contains(&spl),
        "spl={spl} outside a plausible dB SPL range"
    );
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
    let stop_hz = 4_000.0;
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

    let mut got_frame = false;
    let mut got_report = false;
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && !(got_frame && got_report) {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "measurement/spectrum_bands" => {
                assert_eq!(v["bpo"], json!(3));
                assert_eq!(v["class"], json!("Class 1"));
                let centres = v["centres_hz"].as_array().expect("centres_hz array");
                let levels = v["levels_dbfs"].as_array().expect("levels_dbfs array");
                assert_eq!(centres.len(), levels.len());
                assert!(!centres.is_empty(), "filterbank produced no bands");
                // Peak band must land near the 1 kHz loopback tone.
                let (peak_idx, _) =
                    levels
                        .iter()
                        .enumerate()
                        .fold((0usize, f64::NEG_INFINITY), |acc, (i, x)| {
                            let v = x.as_f64().unwrap_or(f64::NEG_INFINITY);
                            if v > acc.1 {
                                (i, v)
                            } else {
                                acc
                            }
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
                if v["report"]["data"][0]["data"]["kind"] == json!("spectrum_bands") {
                    assert_eq!(v["report"]["data"][0]["data"]["bpo"], json!(3));
                    assert_eq!(v["report"]["schema_version"], json!(5));
                    got_report = true;
                }
            }
            Some((t, _)) if t == "done" => break,
            Some(_) => continue,
            None => break,
        }
    }
    assert!(got_frame, "never saw measurement/spectrum_bands frame");
    assert!(
        got_report,
        "never saw measurement/report with spectrum_bands data"
    );
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
    let _ = c
        .wait_for_topic("cal_prompt", Duration::from_secs(3))
        .expect("cal_prompt");
    let _ = c.call(json!({"cmd": "cal_reply", "vrms": Value::Null}));
    let _ = c
        .wait_for_topic("cal_done", Duration::from_secs(5))
        .expect("cal_done");

    // Attach a synthetic 24-point mic-curve.
    let mut freqs = Vec::new();
    let mut gains = Vec::new();
    let log_min = 100.0_f64.ln();
    let log_max = 10_000.0_f64.ln();
    for i in 0..24 {
        let t = i as f64 / 23.0;
        freqs.push((log_min + t * (log_max - log_min)).exp());
        gains.push(2.0 * t); // ramp 0..2 dB
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
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "data" => {
                if v["type"] == json!("measurement/frequency_response/point")
                    && point_frame.is_none()
                {
                    point_frame = Some(v);
                } else if v["type"] == json!("measurement/report") && report_frame.is_none() {
                    report_frame = Some(v);
                }
            }
            Some(_) => continue,
            None => break,
        }
    }
    let pf = point_frame.expect("missing per-point frame");
    let rf = report_frame.expect("missing measurement/report");

    // Envelope keys present on the per-point frame (#98).
    assert_eq!(pf["mic_correction"], json!("on"));
    assert!(
        pf["spl_offset_db"].is_f64(),
        "spl_offset_db not f64: {pf:?}"
    );
    assert_eq!(pf["weighting"], json!("off"));
    assert_eq!(pf["time_integration"], json!("off"));
    assert!(
        pf.get("smoothing_bpo").is_some(),
        "smoothing_bpo key missing"
    );

    // CalibrationSnapshot in the report carries SPL + mic_response (#94 →
    // populated here per #97).
    let cal = rf["report"]["calibration"]
        .as_object()
        .expect("calibration block missing");
    assert!(cal["mic_sensitivity_dbfs_at_94db_spl"].is_f64(), "{cal:?}");
    let mr = cal["mic_response"]
        .as_object()
        .expect("mic_response missing");
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
    assert_eq!(
        s["listen_mode"],
        json!("local"),
        "idle timeout did not auto-disable public bind: {s}"
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
    let prompt = c
        .wait_for_topic("cal_prompt", Duration::from_secs(3))
        .expect("no cal_prompt within 3 s");
    assert_eq!(prompt["kind"], json!("spl"), "prompt kind: {prompt}");

    let r = c.call(json!({"cmd": "cal_reply", "vrms": Value::Null}));
    assert_eq!(r["ok"], json!(true));

    let done = c
        .wait_for_topic("cal_done", Duration::from_secs(5))
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
    let _ = c
        .wait_for_topic("cal_prompt", Duration::from_secs(3))
        .expect("cal_prompt");
    let _ = c.call(json!({"cmd": "cal_reply", "vrms": Value::Null}));
    let _ = c
        .wait_for_topic("cal_done", Duration::from_secs(5))
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
    assert!(
        r["mic_sensitivity_dbfs_at_94db_spl"].is_f64(),
        "missing or wrong-typed mic_sensitivity_dbfs_at_94db_spl in: {r}"
    );
    let mr = r["mic_response"].as_object().expect("mic_response object");
    assert_eq!(mr["freqs_hz"].as_array().unwrap().len(), 24);
    assert_eq!(mr["gain_db"].as_array().unwrap().len(), 24);
    assert_eq!(mr["source_path"], json!("/tmp/synthetic.frd"));
    assert!(mr["imported_at"].is_string());

    // list_calibrations must surface them too — find the entry we just wrote.
    let r = c.call(json!({"cmd": "list_calibrations"}));
    assert_eq!(r["ok"], json!(true));
    let cals = r["calibrations"].as_array().expect("calibrations array");
    let entry = cals
        .iter()
        .find(|e| e["key"].as_str() == Some("out0_in0"))
        .expect("out0_in0 entry not in list");
    assert!(
        entry["mic_sensitivity_dbfs_at_94db_spl"].is_f64(),
        "list_calibrations entry missing mic_sensitivity field: {entry}"
    );
    assert!(
        entry["mic_response"].is_object(),
        "list_calibrations entry missing mic_response: {entry}"
    );
}

/// #297: `get_calibration` / `list_calibrations` must surface `tau_history`
/// — before this issue neither reply carried the field at all, so no client
/// could show it. Seed a key with two τ entries and a second key with none,
/// and assert both the present-array and the absent-array (`[]`) shapes
/// round-trip over the wire per ZMQ.md.
#[test]
fn get_and_list_calibrations_carry_tau_history() {
    let d = Daemon::spawn();
    let cal_path = d.home.join(".config").join("ac").join("cal.json");
    let seeded = json!({
        "out0_in0": {
            "output_channel":                   0,
            "input_channel":                    0,
            "ref_freq":                         1000.0,
            "vrms_at_0dbfs_out":                null,
            "vrms_at_0dbfs_in":                 null,
            "ref_dbfs":                         -10.0,
            "mic_sensitivity_dbfs_at_94db_spl": null,
            "mic_response":                     null,
            "tau_history": [
                {
                    "conditions": {
                        "device":      0,
                        "backend":     "jack",
                        "sample_rate": 48000,
                        "period_size": 1024,
                        "output_port": "system:playback_1",
                        "input_port":  "system:capture_2"
                    },
                    "tau_s":       0.0011931,
                    "measured_at": "2026-08-01T00:00:00Z",
                    "method":      "farina_short_ess"
                },
                {
                    "conditions": {
                        "device":      0,
                        "backend":     "jack",
                        "sample_rate": 48000,
                        "period_size": 128,
                        "output_port": "system:playback_1",
                        "input_port":  "system:capture_2"
                    },
                    "tau_s":       0.0025,
                    "measured_at": "2026-08-15T09:12:03Z",
                    "method":      "farina_short_ess"
                }
            ]
        },
        "out1_in1": {
            "output_channel":                   1,
            "input_channel":                    1,
            "ref_freq":                         1000.0,
            "vrms_at_0dbfs_out":                null,
            "vrms_at_0dbfs_in":                 null,
            "ref_dbfs":                         -10.0,
            "mic_sensitivity_dbfs_at_94db_spl": null,
            "mic_response":                     null,
            "tau_history":                      []
        }
    });
    fs::write(&cal_path, serde_json::to_vec_pretty(&seeded).unwrap()).expect("seed cal.json");

    let c = Client::new(&d);

    // get_calibration on the key with history: both entries present.
    let r = c.call(json!({"cmd": "get_calibration", "output_channel": 0, "input_channel": 0}));
    assert_eq!(r["ok"], json!(true));
    let history = r["tau_history"].as_array().expect("tau_history array");
    assert_eq!(history.len(), 2, "wire reply: {r}");
    assert_eq!(history[1]["tau_s"], json!(0.0025));
    assert_eq!(
        history[1]["conditions"]["period_size"],
        json!(128),
        "conditions must round-trip: {r}"
    );

    // get_calibration on the key with no history: empty array, not absent.
    let r = c.call(json!({"cmd": "get_calibration", "output_channel": 1, "input_channel": 1}));
    assert_eq!(r["ok"], json!(true));
    assert_eq!(
        r["tau_history"],
        json!([]),
        "unmeasured key must reply with [], not omit the field: {r}"
    );

    // list_calibrations must carry the same shape for both keys.
    let r = c.call(json!({"cmd": "list_calibrations"}));
    assert_eq!(r["ok"], json!(true));
    let cals = r["calibrations"].as_array().expect("calibrations array");
    let with_history = cals
        .iter()
        .find(|e| e["key"].as_str() == Some("out0_in0"))
        .expect("out0_in0 entry not in list");
    assert_eq!(
        with_history["tau_history"].as_array().map(|a| a.len()),
        Some(2),
        "list_calibrations: {with_history}"
    );
    let without_history = cals
        .iter()
        .find(|e| e["key"].as_str() == Some("out1_in1"))
        .expect("out1_in1 entry not in list");
    assert_eq!(
        without_history["tau_history"],
        json!([]),
        "list_calibrations: {without_history}"
    );
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
    assert!(
        r["error"].as_str().unwrap_or("").contains("too sparse"),
        "{r}"
    );

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
            let remaining = deadline
                .saturating_duration_since(Instant::now())
                .as_millis() as i32;
            match c.recv_pub(remaining.max(1)) {
                Some((t, v))
                    if t == "data"
                        && v["type"] == json!("measurement/loudness")
                        && v["momentary_lkfs"].is_f64() =>
                {
                    last = Some(v);
                }
                Some(_) => continue,
                None => break,
            }
        }
        let _ = c.call(json!({"cmd": "stop"}));
        // Drain trailing frames.
        let drain = Instant::now() + Duration::from_millis(300);
        while Instant::now() < drain {
            if c.recv_pub(50).is_none() {
                break;
            }
        }
        last.expect("no measurement/loudness frame with momentary_lkfs in window")
    }

    let d = Daemon::spawn();
    let c = Client::new(&d);

    // Baseline — no curve loaded.
    let baseline = last_loudness(&c, 1500);
    let baseline_lkfs = baseline["momentary_lkfs"].as_f64().unwrap();
    assert_eq!(
        baseline["mic_correction"],
        json!("none"),
        "baseline tag must be 'none': {baseline}"
    );

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
    assert_eq!(
        corrected["mic_correction"],
        json!("on"),
        "corrected tag must be 'on': {corrected}"
    );

    let delta = baseline_lkfs - corrected_lkfs;
    assert!(
        (delta - 3.0).abs() < 0.5,
        "expected ≈ 3 dB LKFS drop, got Δ={delta:.3} dB \
         (baseline={baseline_lkfs:.2}, corrected={corrected_lkfs:.2})"
    );
    // True-peak shifts the same way (FIR runs before the 4× polyphase
    // oversampler that produces dBTP).
    let baseline_tp = baseline["true_peak_dbtp"].as_f64().unwrap_or(f64::NAN);
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
            let r = deadline
                .saturating_duration_since(Instant::now())
                .as_millis() as i32;
            match c.recv_pub(r.max(1)) {
                Some((t, v))
                    if t == "data"
                        && v["type"] == json!("measurement/loudness")
                        && v["momentary_lkfs"].is_f64() =>
                {
                    last = Some(v);
                }
                Some(_) => continue,
                None => break,
            }
        }
        let _ = c.call(json!({"cmd": "stop"}));
        let drain = Instant::now() + Duration::from_millis(300);
        while Instant::now() < drain {
            if c.recv_pub(50).is_none() {
                break;
            }
        }
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
            let r = deadline
                .saturating_duration_since(Instant::now())
                .as_millis() as i32;
            match c.recv_pub(r.max(1)) {
                Some((t, v))
                    if t == "data"
                        && v["type"] == json!("measurement/loudness")
                        && v["momentary_lkfs"].is_f64() =>
                {
                    last = Some(v);
                }
                Some(_) => continue,
                None => break,
            }
        }
        let _ = c.call(json!({"cmd": "stop"}));
        last.expect("no off-mode loudness frame")
    };
    let off_lkfs = off["momentary_lkfs"].as_f64().unwrap();
    assert_eq!(
        off["mic_correction"],
        json!("off"),
        "tag must be 'off' when toggle disables FIR: {off}"
    );
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
    assert_eq!(
        s["listen_mode"],
        json!("public"),
        "disabled timeout still auto-disabled public bind: {s}"
    );
}

// ---------------------------------------------------------------------------
// #206 — out-of-range channels must fail loudly, never fabricate a port name
// ---------------------------------------------------------------------------

/// The fake backend exposes 20 playback and 20 capture ports (indices 0..19),
/// so this index cannot resolve on any backend under test.
const OUT_OF_RANGE_CH: u32 = 99;

/// Config with an out-of-range channel. `*_port` is left unset so resolution
/// falls through to the index path — the sticky-name path was never affected.
fn cfg_with_channel(key: &str, ch: u32) -> Value {
    json!({
        "device": 0,
        "output_channel": if key == "output_channel" { ch } else { 4 },
        "input_channel": if key == "input_channel" { ch } else { 0 },
        "reference_channel": if key == "reference_channel" { ch } else { 3 },
        "dbu_ref_vrms": 0.774_596_67,
        "range_start_hz": 20.0,
        "range_stop_hz": 20_000.0,
        "server_enabled": false,
    })
}

fn assert_out_of_range_error(r: &Value, cmd: &str) {
    assert_eq!(
        r["ok"],
        json!(false),
        "{cmd} must fail on an out-of-range channel, replied: {r}"
    );
    let err = r["error"].as_str().unwrap_or_default();
    assert!(
        err.contains("out of range") || err.contains("no physical"),
        "{cmd} error should say the channel is out of range, got: {err:?}"
    );
    // The operator's next question is "then what should I have said?" — the
    // available ports must be named, and the fabricated fallbacks must not
    // appear anywhere in the reply.
    assert!(
        err.contains("fake:"),
        "{cmd} error should list the available ports, got: {err:?}"
    );
    let whole = r.to_string();
    assert!(
        !whole.contains("system:playback_1") && !whole.contains("system:capture_1"),
        "{cmd} reply must not contain a fabricated port name: {whole}"
    );
}

/// **The drive-path case from #206.** A mistyped `output_channel` used to
/// silently retarget the stimulus to `system:playback_1` — noise leaving an
/// output the operator did not choose. It must refuse instead.
#[test]
fn generate_refuses_an_out_of_range_output_channel() {
    let d = Daemon::spawn_with_config(Some(cfg_with_channel("output_channel", OUT_OF_RANGE_CH)));
    let c = Client::new(&d);
    let r = c.call(json!({"cmd":"generate","freq_hz":1000.0,"level_dbfs":-40.0}));
    assert_out_of_range_error(&r, "generate");

    // And nothing was started: the busy guard must still be clear.
    let s = c.call(json!({"cmd":"status"}));
    assert_eq!(
        s["busy"],
        json!(false),
        "a refused generate must not leave a worker running: {s}"
    );
}

#[test]
fn transfer_stream_refuses_an_out_of_range_output_channel() {
    let d = Daemon::spawn_with_config(Some(cfg_with_channel("output_channel", OUT_OF_RANGE_CH)));
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd": "transfer_stream", "pairs": [[0, 1]], "drivable": true
    }));
    assert_out_of_range_error(&r, "transfer_stream");
}

#[test]
fn monitor_spectrum_refuses_an_out_of_range_input_channel() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    // Explicit `channels` goes through the same resolution path.
    let r = c.call(json!({"cmd":"monitor_spectrum","channels":[OUT_OF_RANGE_CH]}));
    assert_out_of_range_error(&r, "monitor_spectrum");
}

#[test]
fn plot_refuses_an_out_of_range_input_channel() {
    let d = Daemon::spawn_with_config(Some(cfg_with_channel("input_channel", OUT_OF_RANGE_CH)));
    let c = Client::new(&d);
    let r = c.call(json!({"cmd":"plot","freq_start":100.0,"freq_stop":1000.0,"ppd":2}));
    assert_out_of_range_error(&r, "plot");
}

#[test]
fn sweep_refuses_an_out_of_range_output_channel() {
    let d = Daemon::spawn_with_config(Some(cfg_with_channel("output_channel", OUT_OF_RANGE_CH)));
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd":"sweep_frequency","freq_start":100.0,"freq_stop":1000.0,"level_dbfs":-40.0
    }));
    assert_out_of_range_error(&r, "sweep_frequency");
}

/// A configured-but-missing *reference* channel used to present as "no
/// reference": `resolve_ref_input` returned `None` for both "not configured"
/// and "out of range", so the measurement ran single-ended while the operator
/// believed a reference was wired in.
#[test]
fn test_dut_refuses_an_out_of_range_reference_channel() {
    let d = Daemon::spawn_with_config(Some(cfg_with_channel("reference_channel", OUT_OF_RANGE_CH)));
    let c = Client::new(&d);
    let r = c.call(json!({"cmd":"test_dut","level_dbfs":-40.0}));
    assert_out_of_range_error(&r, "test_dut");
}

/// The sticky-name path is unaffected: an explicit `*_port` bypasses index
/// resolution entirely and must keep working even when the channel index
/// alongside it is nonsense.
#[test]
fn explicit_sticky_port_still_bypasses_channel_resolution() {
    let mut cfg = cfg_with_channel("output_channel", OUT_OF_RANGE_CH);
    cfg["output_port"] = json!("fake:playback_2");
    let d = Daemon::spawn_with_config(Some(cfg));
    let c = Client::new(&d);
    let r = c.call(json!({"cmd":"generate","freq_hz":1000.0,"level_dbfs":-40.0}));
    assert_eq!(
        r["ok"],
        json!(true),
        "an explicit output_port must still be honoured: {r}"
    );
    c.call(json!({"cmd":"stop"}));
}

/// In-range channels must be entirely unaffected — the fix must not have made
/// a working configuration fail.
#[test]
fn in_range_channels_are_unaffected() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({"cmd":"generate","freq_hz":1000.0,"level_dbfs":-40.0,"channels":[2]}));
    assert_eq!(r["ok"], json!(true), "in-range generate must work: {r}");
    c.call(json!({"cmd":"stop"}));
}

// ---------------------------------------------------------------------------
// Multi-time-window ladder (handoff-mtw-live-spectrum)
// ---------------------------------------------------------------------------

fn f64s(v: &Value, key: &str) -> Vec<f64> {
    v[key]
        .as_array()
        .unwrap_or_else(|| panic!("mtw.{key} missing: {v}"))
        .iter()
        .map(|x| x.as_f64().unwrap())
        .collect()
}

/// Wait for a frame in which every rung has settled, so the column set spans
/// the whole ladder.
///
/// The `mtw` block itself appears as soon as the *top* rung settles (0.11 s at
/// 96 kHz) — the display fills downward rather than staying blank for the
/// bottom rung's 2.56 s — so an assertion that needs the full band must key on
/// the frame's own `settled_stages` rather than on the block's presence, and
/// certainly not on elapsed time, which would be a race.
fn wait_for_mtw_fully_settled(c: &Client, timeout: Duration) -> Value {
    wait_for_mtw_where(c, timeout, |m| {
        m["settled_stages"]
            .as_array()
            .is_some_and(|s| !s.is_empty() && s.iter().all(|v| v == &json!(true)))
    })
}

fn wait_for_mtw_where(c: &Client, timeout: Duration, ok: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let left = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(left.max(1)) {
            Some((t, v)) if t == "data" && v["type"] == json!("transfer_stream") => {
                if v["mtw"].is_object() && ok(&v["mtw"]) {
                    return v;
                }
            }
            Some(_) => continue,
            None => break,
        }
    }
    panic!("no matching mtw block within {timeout:?}");
}

/// End-to-end ground truth for the ladder: a known flat `H1` must come back
/// flat across every rung, with every column backed by real bins and carrying
/// the resolution, window and averaging that produced it.
///
/// `fake_correlated_pair` makes meas a known `gain`-scaled, delayed copy of
/// ref, so `|H1| = gain` and coherence ~1 are checkable ground truth rather
/// than a noise-over-noise ratio.
#[test]
fn mtw_columns_are_backed_by_bins_and_carry_their_provenance() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let gain = 0.5_f64;
    let r = c.call(json!({
        "cmd": "transfer_stream", "meas_channel": 0, "ref_channel": 1,
        "weighting": "Z", "integration": "fast",
        "fake_correlated_pair": {"gain": gain, "delay_samples": 200},
    }));
    assert_eq!(r["ok"], json!(true), "transfer_stream start: {r}");

    let frame = wait_for_mtw_fully_settled(&c, Duration::from_secs(30));
    let _ = c.call(json!({"cmd": "stop"}));
    let m = &frame["mtw"];

    let freqs = f64s(m, "freqs");
    let mag = f64s(m, "magnitude_db");
    let coh = f64s(m, "coherence");
    let df = f64s(m, "df");
    let window = f64s(m, "window_s");
    let n = f64s(m, "n");
    let bins = f64s(m, "bins");
    let stage = f64s(m, "stage");
    assert!(freqs.len() > 100, "only {} columns", freqs.len());
    for (name, v) in [
        ("magnitude_db", &mag),
        ("coherence", &coh),
        ("df", &df),
        ("window_s", &window),
        ("n", &n),
        ("bins", &bins),
        ("stage", &stage),
    ] {
        assert_eq!(v.len(), freqs.len(), "mtw.{name} length");
    }

    // Criterion 1, over the wire: no column is synthesised from its
    // neighbours, so every one maps to at least one source bin.
    assert!(
        bins.iter().all(|&b| b >= 1.0),
        "columns with no source bins: {:?}",
        bins.iter().take(20).collect::<Vec<_>>()
    );

    // Criterion 1's other half: each column really is at least one bin wide.
    let lo = f64s(m, "f_lo");
    let hi = f64s(m, "f_hi");
    for i in 0..freqs.len() {
        assert!(
            hi[i] - lo[i] >= df[i] * 0.999,
            "column {i} at {} Hz spans {} Hz but Δf is {}",
            freqs[i],
            hi[i] - lo[i],
            df[i]
        );
    }

    // Ground truth, in every rung that the display range reaches.
    let mut per_stage = [0usize; 8];
    for i in 0..freqs.len() {
        per_stage[stage[i] as usize] += 1;
        if freqs[i] < 80.0 || freqs[i] > 18_000.0 {
            continue;
        }
        assert!(
            (mag[i] - 20.0 * gain.log10()).abs() < 1.5,
            "{} Hz (stage {}): {} dB, want {}",
            freqs[i],
            stage[i],
            mag[i],
            20.0 * gain.log10()
        );
        assert!(coh[i] > 0.8, "{} Hz: coherence {}", freqs[i], coh[i]);
    }
    assert!(
        per_stage[0] > 0 && per_stage[1] > 0 && per_stage[2] > 0,
        "every rung must be exercised, got {per_stage:?}"
    );

    // Deliverable 4: the provenance is real, not a constant. Windows shorten
    // with frequency and Δf coarsens, monotonically, so a reader can tell how
    // stale and how resolved any column is.
    for i in 1..freqs.len() {
        assert!(
            window[i] <= window[i - 1] + 1e-12,
            "window rose at {}",
            freqs[i]
        );
        assert!(df[i] >= df[i - 1] - 1e-12, "Δf fell at {}", freqs[i]);
    }
    assert!(
        window[0] > window[freqs.len() - 1] * 4.0,
        "the ladder must actually use different windows: {} vs {}",
        window[0],
        window[freqs.len() - 1]
    );

    // Criterion 5: N is present and equals the configured value, in every
    // column. Uniform across stages is the whole point — an N that varied with
    // frequency would put a coherence step at a fixed frequency.
    assert!(
        n.iter().all(|&v| v == 4.0),
        "N must be the configured 4 in every column, got {:?}",
        n.iter().take(8).collect::<Vec<_>>()
    );
    assert_eq!(m["n_blocks"], json!(4));

    // The stage table ships alongside so `stage` is interpretable.
    let stages = m["stages"].as_array().expect("mtw.stages");
    assert_eq!(stages.len(), 3, "48 kHz ladder is three rungs");
    assert_eq!(stages[0]["decim"], json!(1), "stage 0 is always full rate");
    assert_eq!(stages[1]["decim"], json!(4));
    assert_eq!(stages[2]["decim"], json!(12));
    // Settling: the bottom rung must not be slower than the full-rate
    // estimator it replaces (2.5 s today), and the top must be far faster.
    let settling = |i: usize| stages[i]["settling_s"].as_f64().unwrap();
    assert!(
        settling(2) < 2.6,
        "bottom rung settles in {} s",
        settling(2)
    );
    assert!(settling(0) < 0.25, "top rung settles in {} s", settling(0));
}

/// The ladder is additive. Everything the frame carried before it must be
/// untouched — in particular `spl` (criterion 7, the conformance guard) and
/// the calibrated per-channel spectra, which are absolute levels and so are
/// deliberately **not** routed through the ladder.
#[test]
fn mtw_does_not_disturb_the_existing_frame() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd": "transfer_stream", "meas_channel": 0, "ref_channel": 1,
        "weighting": "Z", "integration": "fast",
        "fake_correlated_pair": {"gain": 0.5, "delay_samples": 200},
    }));
    assert_eq!(r["ok"], json!(true), "{r}");
    let frame = wait_for_mtw_fully_settled(&c, Duration::from_secs(30));
    let _ = c.call(json!({"cmd": "stop"}));

    for key in [
        "freqs",
        "magnitude_db",
        "phase_deg",
        "coherence",
        "re",
        "im",
        "spec_freqs",
        "meas_spectrum",
        "ref_spectrum",
    ] {
        assert!(
            frame[key].as_array().is_some_and(|a| !a.is_empty()),
            "{key} went missing or empty when the ladder landed"
        );
    }
    for key in [
        "delay_samples",
        "delay_ms",
        "sr",
        "spl_weighting",
        "spl_integration",
    ] {
        assert!(!frame[key].is_null(), "{key} went missing");
    }
    // The pre-existing H1 arrays are the full-rate Welch estimate and must
    // still be the 2000-point decimation of a 1 Hz grid — the ladder is a
    // second view, not a replacement.
    let freqs = f64s(&frame, "freqs");
    assert!(
        freqs.len() > 1_900 && freqs.len() <= 2_000,
        "full-rate H1 grid changed: {} points",
        freqs.len()
    );
    // And it is a linear grid, unlike the ladder's log columns.
    let d0 = freqs[1] - freqs[0];
    let d1 = freqs[freqs.len() - 1] - freqs[freqs.len() - 2];
    assert!(
        (d0 - d1).abs() < d0 * 0.5,
        "full-rate grid stopped being linear"
    );
}

/// Density is a parameter; the crossovers are not. Raising `mtw_ppo` adds
/// columns where the ladder can back them and leaves the rung boundaries
/// exactly where they were — so two captures at different densities remain
/// comparable.
#[test]
fn mtw_density_is_a_parameter_that_does_not_move_the_crossovers() {
    fn run(ppo: Option<f64>) -> Value {
        let d = Daemon::spawn();
        let c = Client::new(&d);
        let mut cmd = json!({
            "cmd": "transfer_stream", "meas_channel": 0, "ref_channel": 1,
            "weighting": "Z", "integration": "fast",
            "fake_correlated_pair": {"gain": 0.5, "delay_samples": 200},
        });
        if let Some(p) = ppo {
            cmd["mtw_ppo"] = json!(p);
        }
        let r = c.call(cmd);
        assert_eq!(r["ok"], json!(true), "{r}");
        let f = wait_for_mtw_fully_settled(&c, Duration::from_secs(30));
        let _ = c.call(json!({"cmd": "stop"}));
        f["mtw"].clone()
    }

    let base = run(None);
    let dense = run(Some(192.0));
    assert!(
        f64s(&dense, "freqs").len() > f64s(&base, "freqs").len(),
        "a denser request must add columns"
    );
    assert_eq!(
        base["stages"], dense["stages"],
        "display density must not move the ladder's crossovers"
    );

    // Below the deepest rung's validity edge both grids are Δf-limited, so the
    // extra density buys nothing — which is the honest outcome, and the point
    // of dropping the interpolation branch.
    let edge = base["stages"][2]["f_valid"].as_f64().unwrap();
    let below = |m: &Value| f64s(m, "freqs").iter().filter(|&&f| f < edge).count();
    assert_eq!(
        below(&base),
        below(&dense),
        "columns below the validity edge must not multiply with density"
    );
}

// ---------------------------------------------------------------------------
// #216 — warmup ring phase
// ---------------------------------------------------------------------------

/// Pull the per-ring `occ=[..]` list off one `AC_DRAIN_TELEMETRY` raw line
/// (#208 D1). Returns `None` for anything that is not a per-tick record — the
/// window summary lines and any other daemon stderr.
///
/// The list itself is parsed, not the pre-reduced `occ_min`/`occ_max` fields,
/// so the test can tell "every ring agreed" from "no ring was reported at
/// all": both give `occ_min == occ_max`, and only one of them means anything.
fn parse_occ(line: &str) -> Option<Vec<usize>> {
    if !line.starts_with("drain-tick ") {
        return None;
    }
    let body = line.split_once("occ=[")?.1.split_once(']')?.0.trim();
    if body.is_empty() {
        return Some(Vec::new());
    }
    body.split(',')
        .map(|t| t.trim().parse::<usize>().ok())
        .collect()
}

/// Issue #216: every capture ring must come out of the session's warmup flush
/// holding the same number of samples.
///
/// The rig evidence was exactly this telemetry: `occ=[5120, 24320]` — each
/// reference ring a constant 19200 samples (0.2 s at 96 kHz) above meas,
/// unchanged across 929 ticks of two runs. The cause is the warmup
/// `capture_block(0.2)`, which clears the measurement ring only. Nothing
/// afterwards re-syncs: `capture_multi_contiguous` pops `min_occupied()` from
/// every ring, and a constant offset survives that untouched. The skew is what
/// put `delay_ms` 200 ms negative, coherence at ~0.64 and `magnitude_db` 2.5 dB
/// out on every ring-backed session since #207.
///
/// `fake_ring` is what makes it reproducible without hardware — the default
/// on-demand fake generator has no ring at all and is structurally incapable of
/// holding a skew (see `FakeRings`' docs). The stimulus is left at the default
/// on purpose: the defect lives in the capture path, not in the stimulus, and a
/// correlated pair used to take a different warmup branch that hid it. This
/// test therefore covers the branch that was actually broken.
#[test]
fn warmup_leaves_every_capture_ring_at_the_same_phase() {
    let d = Daemon::spawn_with_env(&[("AC_DRAIN_TELEMETRY", "1")]);
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd": "transfer_stream", "meas_channel": 0, "ref_channel": 1,
        "weighting": "Z", "integration": "fast",
        // Zero processing gap: this test is about the warmup's phase, not
        // about the per-tick backlog `process_secs` exists to model.
        "fake_ring": {"process_secs": 0.0},
    }));
    assert_eq!(r["ok"], json!(true), "transfer_stream start: {r}");
    thread::sleep(Duration::from_millis(600));
    let _ = c.call(json!({"cmd": "stop"}));

    let log = d.stderr();
    let ticks: Vec<Vec<usize>> = log.lines().filter_map(parse_occ).collect();
    assert!(
        ticks.len() >= 3,
        "expected AC_DRAIN_TELEMETRY per-tick lines, parsed {} from:\n{log}",
        ticks.len()
    );
    // Without this the whole test is vacuous: a backend that reports no
    // occupancy at all trivially has no spread between its rings. This is how
    // the test first passed against the unfixed daemon — `FakeEngine`
    // inherited the trait's empty `last_drain_occupancy`.
    assert!(
        ticks.iter().all(|occ| occ.len() >= 2),
        "telemetry must report meas + at least one ref per tick, got {:?}",
        &ticks[..ticks.len().min(3)]
    );

    let skewed: Vec<&Vec<usize>> = ticks
        .iter()
        .filter(|occ| occ.iter().min() != occ.iter().max())
        .collect();
    assert!(
        skewed.is_empty(),
        "{} of {} ticks show a meas/ref ring skew: {:?} — the warmup flush \
         must clear and pop meas and every ref together (#216)",
        skewed.len(),
        ticks.len(),
        &skewed[..skewed.len().min(5)]
    );
}

/// #254 — a `transfer_stream` over three or more **distinct** capture channels
/// replies `ok: true` and then publishes nothing, forever.
///
/// **This test is differential on purpose, and that is the whole design.** The
/// obvious shape — request three channels, wait, assert a frame arrived — is
/// the shape that cannot tell the defect from a slow machine, and this project
/// has shipped that mistake before: `FakeEngine` inherited the trait's empty
/// `last_drain_occupancy`, so the one mode built to reproduce ring defects
/// reported `occ=[]` and the ring test passed against the unfixed daemon
/// (`ring_drain_keeps_meas_and_ref_in_lockstep` above, and the comment at
/// `handlers/transfer.rs`'s drain arm). A timeout is not an observation.
///
/// So a **two-channel control session runs first, on the same daemon, in the
/// same process, against the same clock**, and its time-to-first-frame sets
/// the budget for the three-channel session. That cancels machine speed: if
/// the control produces a frame and the three-channel session produces neither
/// a frame nor an error in several times that budget, the difference is the
/// session shape and nothing else.
///
/// Three outcomes, all of them stated rather than inferred:
///
/// - control silent → **fails as inconclusive**, naming itself as unable to
///   judge #254. It must never pass by both sessions being silent.
/// - three channels silent while the control spoke → **fails as #254**, which
///   is the state of `main` today.
/// - three channels publish both pairs, or the launch is refused with a named
///   error → **passes**. Both are acceptable fixes; direction 1 in the issue
///   is the refusal, direction 2 is the fake modelling N channels. A refusal
///   that names the mismatch is a recoverable client error. Silence is not.
#[test]
fn three_distinct_channels_publish_or_refuse_but_never_stall() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    // --- control: two distinct channels, the shape that works today ---------
    let started = Instant::now();
    let r = c.call(json!({
        "cmd":        "transfer_stream",
        "pairs":      [[0, 1]],
        "drive":      true,
        "level_dbfs": -12.0,
    }));
    assert_eq!(r["ok"], json!(true), "control session refused: {r:?}");

    let mut control_first: Option<Duration> = None;
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "data" && v["type"].as_str() == Some("transfer_stream") => {
                control_first = Some(started.elapsed());
                break;
            }
            Some(_) => continue,
            None => break,
        }
    }
    let _ = c.call(json!({"cmd": "stop"}));
    let _ = c.wait_for_topic("done", Duration::from_secs(5));

    let control_first = control_first.expect(
        "INCONCLUSIVE, not a #254 failure: the two-channel control session \
         published no transfer_stream frame in 15 s. This test cannot say \
         anything about three-channel behaviour until the two-channel path \
         works here — fix the control first.",
    );

    // Budget generously against the control, so the verdict is about session
    // shape rather than about how loaded this machine is. Six ticks' worth of
    // slack, floored at 5 s for a fast control and capped so a pathologically
    // slow control cannot hang the suite.
    let budget = (control_first * 6).clamp(Duration::from_secs(5), Duration::from_secs(30));

    // --- the case: three distinct channels, {0, 1, 3} -----------------------
    // `[[3,3],[0,3]]` — the converter-constant shape rig session 3 ran — is
    // two distinct channels and is unaffected. `[[0,3],[1,3]]` is a second
    // measurement position against the same reference, which the rig has
    // already produced results from (`rig-session-results.md`, Run 5), and it
    // is three.
    let d2 = Daemon::spawn();
    let c2 = Client::new(&d2);
    let r2 = c2.call(json!({
        "cmd":        "transfer_stream",
        "pairs":      [[0, 3], [1, 3]],
        "drive":      true,
        "level_dbfs": -12.0,
    }));

    // A refusal at launch is a pass: it is the recoverable outcome direction 1
    // asks for. Anything else must go on to publish.
    if r2["ok"] != json!(true) {
        let msg = r2["error"].as_str().unwrap_or_default().to_string()
            + r2["message"].as_str().unwrap_or_default();
        assert!(
            !msg.trim().is_empty(),
            "three-channel launch was refused without a message: {r2:?}"
        );
        return;
    }

    let mut meas_seen: Vec<u64> = Vec::new();
    let mut error_msg: Option<String> = None;
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c2.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "error" => {
                error_msg = Some(v.to_string());
                break;
            }
            Some((t, v)) if t == "data" && v["type"].as_str() == Some("transfer_stream") => {
                if let Some(ch) = v["meas_channel"].as_u64() {
                    if !meas_seen.contains(&ch) {
                        meas_seen.push(ch);
                    }
                }
                if meas_seen.len() >= 2 {
                    break;
                }
            }
            Some(_) => continue,
            None => break,
        }
    }
    let _ = c2.call(json!({"cmd": "stop"}));

    if error_msg.is_some() {
        return; // loud refusal mid-session: recoverable, and visible.
    }

    meas_seen.sort_unstable();
    assert!(
        !meas_seen.is_empty(),
        "#254: `pairs=[[0,3],[1,3]]` (three distinct channels) replied ok:true \
         and then published no transfer_stream frame and no error in {:?}, \
         while the two-channel control on this same machine published its \
         first frame in {:?}. The session shape is the only difference. \
         `capture_multi` returns two buffers regardless of the session's \
         channel count (audio/fake.rs), ring 2 never reaches `nperseg`, and \
         the warmup gate in handlers/transfer.rs `continue`s forever.",
        budget,
        control_first,
    );
    assert_eq!(
        meas_seen.len(),
        2,
        "#254: only measurement channel(s) {meas_seen:?} published; both 0 and \
         1 must appear. A session that publishes one pair of a two-pair request \
         and silently drops the other is the same defect one pair further in.",
    );
}

/// #254, the half that converts rig work into desk work: `--fake-audio` must
/// actually *run* a three-channel session, not merely refuse it loudly.
///
/// The test above accepts a named refusal as a pass, because for a backend
/// that genuinely cannot capture N channels a refusal is the right answer.
/// That is deliberately too weak here: a regression that returned the fake to
/// two buffers would trip the handler guard, produce a clean error, and leave
/// that test green. This one takes no error for an answer.
///
/// `pairs=[[0,3],[1,3]]` is a second measurement position against a shared
/// reference — the shape `rig-session-results.md` Run 5 already produced
/// results from on hardware, and the shape nothing desk-side could rehearse
/// while this bug stood.
#[test]
fn three_distinct_channels_publish_both_pairs_on_fake_audio() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd":        "transfer_stream",
        "pairs":      [[0, 3], [1, 3]],
        "drive":      true,
        "level_dbfs": -12.0,
    }));
    assert_eq!(r["ok"], json!(true), "unexpected REP: {r:?}");

    let mut meas_seen: Vec<u64> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && meas_seen.len() < 2 {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "error" => errors.push(v.to_string()),
            Some((t, v)) if t == "data" && v["type"].as_str() == Some("transfer_stream") => {
                let meas = v["meas_channel"]
                    .as_u64()
                    .expect("frame without meas_channel");
                let refch = v["ref_channel"]
                    .as_u64()
                    .expect("frame without ref_channel");
                assert_eq!(refch, 3, "both pairs reference channel 3: {v}");
                if !meas_seen.contains(&meas) {
                    meas_seen.push(meas);
                }
            }
            Some(_) => continue,
            None => break,
        }
    }
    let _ = c.call(json!({"cmd": "stop"}));

    meas_seen.sort_unstable();
    assert!(
        errors.is_empty(),
        "the fake backend must run three channels, not refuse them: {errors:?}"
    );
    assert_eq!(
        meas_seen,
        vec![0, 1],
        "expected a frame for each measurement channel against the shared \
         reference; saw {meas_seen:?}"
    );
}

/// #254, the part presence assertions cannot reach: **the second measurement
/// channel's delay must be the configured one.**
///
/// Every other test here is satisfied by three buffers arriving. A shared
/// measurement read cursor in the fake produces three buffers too — it
/// advances once per channel per tick, so the second channel reads a window
/// one buffer further along and reports a delay that is an artefact of call
/// order. `delay_attempts` climbs, a frame publishes, `pair_delays[1]` fills
/// in, and every presence check above goes green on a wrong number.
///
/// That is the failure mode worth guarding, because it is the one that
/// contaminates rather than blocks: an offline experiment built on fake
/// multi-position sessions would have inherited the artefact silently, with
/// nothing to distinguish it from a real delay. So this pins the value, on the
/// channel where a shared cursor would move it, against a known
/// `fake_correlated_pair`.
#[test]
fn both_measurement_channels_report_the_configured_delay() {
    let gain = 0.5_f64;
    // ~4.2 ms at 48 kHz, well inside the delay-search window — the same
    // figure `it_snapshot.rs`'s ground-truth test uses.
    let delay_samples = 200_i64;

    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd":   "transfer_stream",
        "pairs": [[0, 3], [1, 3]],
        "fake_correlated_pair": {"gain": gain, "delay_samples": delay_samples},
    }));
    assert_eq!(r["ok"], json!(true), "unexpected REP: {r:?}");

    // One locked frame per measurement channel. Both pairs read the same
    // reference, so both must land on the same configured delay: the second
    // channel is not a second DUT, it is the same source read by another
    // capture channel.
    let mut locked: Vec<(u64, i64)> = Vec::new();
    let mut published: Vec<u64> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline && locked.len() < 2 {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "data" && v["type"].as_str() == Some("transfer_stream") => {
                let meas = v["meas_channel"]
                    .as_u64()
                    .expect("frame without meas_channel");
                if !published.contains(&meas) {
                    published.push(meas);
                }
                if v["delay_locked"].as_bool() != Some(true) {
                    continue;
                }
                let got = v["delay_samples"]
                    .as_i64()
                    .expect("frame without delay_samples");
                if !locked.iter().any(|(m, _)| *m == meas) {
                    locked.push((meas, got));
                }
            }
            Some(_) => continue,
            None => break,
        }
    }
    let _ = c.call(json!({"cmd": "stop"}));

    locked.sort_unstable();
    published.sort_unstable();

    // Split from the lock assertion so the two cannot be confused: no frames
    // at all is #254 itself regressing, and says nothing about delays.
    assert_eq!(
        published,
        vec![0, 1],
        "measurement channel(s) missing from published frames {published:?} — that \
         is the #254 stall, not a delay question"
    );

    // The shared-cursor artefact arrives by either of two roads, and both are
    // named here because the first one otherwise reads as flakiness. A shared
    // `correlated_meas_pos` shifts the second channel by a whole tick's worth
    // of samples — thousands, far outside the search window — so in practice
    // it decorrelates that channel and the estimator refuses it rather than
    // locking to a plausible wrong number. A smaller future artefact would
    // lock and land on the value check below instead. Verified red both ways.
    assert_eq!(
        locked.len(),
        2,
        "both channels published, but only {locked:?} locked within 30 s. A channel \
         that publishes and never locks is the shared-cursor artefact decorrelating \
         it: `FakeEngine::correlated_meas_pos` must be keyed per port, or the second \
         measurement channel reads a window one tick further along than the first.",
    );

    // Tolerance is one correlation bin, not a fitted margin: the estimator
    // reports whole samples and the fake's source is exact, so the honest
    // answer is the configured lag itself. A call-order artefact is off by a
    // whole tick's worth of samples — thousands — so it cannot hide in here.
    for (meas, got) in &locked {
        assert!(
            (got - delay_samples).abs() <= 1,
            "measurement channel {meas} reported delay {got}, configured {delay_samples}. \
             A delay that is wrong only on the second channel is the shared-cursor \
             artefact: `FakeEngine::correlated_meas_pos` must be keyed per port.",
        );
    }
}
