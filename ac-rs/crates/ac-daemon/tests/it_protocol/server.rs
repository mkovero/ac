use serde_json::json;
use serde_json::Value;
use std::fs;
use std::process::Command;
use std::thread;
use std::time::Duration;

use crate::common::{alloc_home, alloc_ports, Client, Daemon};

/// #385: `server_connections` carries the same identity fields as `status`.
#[test]
fn server_connections_reports_daemon_identity() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({"cmd":"server_connections"}));
    assert_eq!(r["ok"], json!(true));
    assert_eq!(r["home"], json!(d.home.display().to_string()));
    assert_eq!(r["pid"], json!(d.pid()));
    assert_eq!(r["spawn_mode"], json!("manual"));
}

#[test]
fn setup_validates_and_canonicalizes_backend_requirement() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let invalid = c.call(json!({"cmd":"setup","update":{"backend":"alsa"}}));
    assert_eq!(invalid["ok"], json!(false));
    assert!(invalid["error"]
        .as_str()
        .unwrap()
        .contains("jack, cpal, fake"));

    let alias = c.call(json!({"cmd":"setup","update":{"backend":"sounddevice"}}));
    assert_eq!(alias["ok"], json!(true));
    assert_eq!(alias["config"]["backend"], json!("cpal"));

    let explicit_fake = c.call(json!({"cmd":"setup","update":{"backend":"fake"}}));
    assert_eq!(explicit_fake["ok"], json!(true));
    assert_eq!(explicit_fake["config"]["backend"], json!("fake"));
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

/// #385 / PR #396 QA correctness #2: a DATA-port-only conflict (CTRL binds
/// fine in this process, but the DATA port belongs to someone else) must
/// not misreport itself as "could not identify existing listener" — that
/// message is honest only when a probe was actually attempted and got no
/// answer. Here no probe is attempted at all: CTRL already bound in *this*
/// process, so a `ctrl_port` probe would reach our own not-yet-serving
/// socket rather than the incumbent, and DATA is a PUB socket with no
/// `status` responder to query in the first place. The message must say
/// that, not sound like a failed probe.
#[test]
fn data_port_conflict_does_not_misreport_self_as_unidentified_incumbent() {
    let d = Daemon::spawn();

    let (free_ctrl, _unused_data) = alloc_ports();
    let home2 = alloc_home();
    let stderr_path = home2.join("daemon2.stderr");
    let stderr_file = fs::File::create(&stderr_path).expect("create stderr capture");
    let mut second = Command::new(env!("CARGO_BIN_EXE_ac-daemon"))
        .env("HOME", &home2)
        .args([
            "--fake-audio",
            "--local",
            "--ctrl-port",
            &free_ctrl.to_string(),
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
        "a daemon on an already-bound DATA port must exit non-zero"
    );

    let stderr = fs::read_to_string(&stderr_path).unwrap_or_default();
    assert!(
        !stderr.contains("could not identify existing listener"),
        "a DATA-only conflict never probes anyone, so it must not use the \
         failed-probe wording: {stderr}"
    );
    assert!(
        stderr.contains("DATA port already in use"),
        "expected an honest DATA-conflict message, got: {stderr}"
    );

    let _ = fs::remove_dir_all(&home2);
}

/// A directly invoked `ac-daemon` with the given bind flags (#433),
/// answering on loopback with its own pid. Killed and its HOME removed on
/// drop.
struct DirectDaemon {
    child: std::process::Child,
    home: std::path::PathBuf,
    ctrl: u16,
    log: std::path::PathBuf,
}

impl DirectDaemon {
    fn spawn(flags: &[&str]) -> Self {
        for _ in 0..5 {
            let home = alloc_home();
            let (ctrl, data) = alloc_ports();
            let log = home.join("daemon.log");
            let out = fs::File::create(&log).unwrap();
            let mut child = Command::new(env!("CARGO_BIN_EXE_ac-daemon"))
                .env("HOME", &home)
                .arg("--fake-audio")
                .args(flags)
                .args(["--ctrl-port", &ctrl.to_string()])
                .args(["--data-port", &data.to_string()])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(out)
                .spawn()
                .expect("spawn ac-daemon");
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while std::time::Instant::now() < deadline {
                if let Ok(Some(_)) = child.try_wait() {
                    break;
                }
                if let Some(r) = raw_call("127.0.0.1", ctrl, json!({"cmd":"status"}), 300) {
                    if r["pid"].as_u64() == Some(u64::from(child.id())) {
                        return Self {
                            child,
                            home,
                            ctrl,
                            log,
                        };
                    }
                    break;
                }
                thread::sleep(Duration::from_millis(50));
            }
            let _ = child.kill();
            let _ = child.wait();
            let _ = fs::remove_dir_all(&home);
        }
        panic!("direct ac-daemon {flags:?} never came up");
    }
}

impl Drop for DirectDaemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.home);
    }
}

/// One REQ/REP round-trip to `host:port`; `None` if nothing answers.
fn raw_call(host: &str, port: u16, cmd: Value, timeout_ms: i32) -> Option<Value> {
    let ctx = zmq::Context::new();
    let s = ctx.socket(zmq::REQ).ok()?;
    s.set_linger(0).ok();
    s.set_rcvtimeo(timeout_ms).ok();
    s.set_sndtimeo(timeout_ms).ok();
    s.connect(&format!("tcp://{host}:{port}")).ok()?;
    s.send(cmd.to_string().as_bytes(), 0).ok()?;
    let bytes = s.recv_bytes(0).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// #433: a hand-run daemon with no bind flag listens on loopback only — a
/// client on another address (127.0.0.2 stands in for the network) gets no
/// answer — and says so on startup.
#[test]
fn direct_daemon_binds_loopback_by_default() {
    let d = DirectDaemon::spawn(&[]);
    let s = raw_call("127.0.0.1", d.ctrl, json!({"cmd":"status"}), 1_000).unwrap();
    assert_eq!(s["listen_mode"], json!("local"));
    assert!(
        raw_call("127.0.0.2", d.ctrl, json!({"cmd":"status"}), 500).is_none(),
        "default daemon must not answer a non-loopback client"
    );
    let log = fs::read_to_string(&d.log).unwrap();
    assert!(
        log.contains(&format!("listen     local  tcp://127.0.0.1:{}", d.ctrl)),
        "{log}"
    );
    assert!(!log.contains("WARNING"), "{log}");
}

/// #433: `--public` is the explicit opt-in; it warns on startup and reports
/// public mode.
#[test]
fn public_flag_binds_all_interfaces_with_a_warning() {
    let d = DirectDaemon::spawn(&["--public"]);
    let s = raw_call("127.0.0.2", d.ctrl, json!({"cmd":"status"}), 1_000)
        .expect("--public daemon must answer a non-loopback client");
    assert_eq!(s["listen_mode"], json!("public"));
    let log = fs::read_to_string(&d.log).unwrap();
    assert!(
        log.contains("WARNING  public daemon exposure enabled"),
        "{log}"
    );
    assert!(
        log.contains(&format!("control  tcp://*:{}", d.ctrl)),
        "{log}"
    );
    assert!(log.contains("spool    confined to "), "{log}");
}

#[test]
fn public_and_local_flags_conflict() {
    let home = alloc_home();
    let status = Command::new(env!("CARGO_BIN_EXE_ac-daemon"))
        .env("HOME", &home)
        .args(["--fake-audio", "--public", "--local"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(!status.success());
    let _ = fs::remove_dir_all(&home);
}

/// #433 network-boundary regression: even with the daemon deliberately
/// public, a remote client cannot steer spool reset at a directory of its
/// choosing — the setup is refused and a session start leaves it intact.
#[test]
fn remote_client_cannot_select_a_deletion_target() {
    let d = DirectDaemon::spawn(&["--public"]);
    let victim = d.home.join("victim");
    fs::create_dir_all(victim.join("nested")).unwrap();
    fs::write(victim.join("nested").join("data"), b"keep").unwrap();

    let r = raw_call(
        "127.0.0.2",
        d.ctrl,
        json!({"cmd":"setup","update":{"snapshot_spool_dir": victim.display().to_string()}}),
        2_000,
    )
    .unwrap();
    assert_eq!(r["ok"], json!(false), "{r}");

    let r = raw_call(
        "127.0.0.2",
        d.ctrl,
        json!({"cmd":"transfer_stream","meas_channel":0,"ref_channel":1}),
        2_000,
    )
    .unwrap();
    assert_eq!(r["ok"], json!(true), "{r}");
    thread::sleep(Duration::from_millis(500));
    let _ = raw_call("127.0.0.2", d.ctrl, json!({"cmd":"stop"}), 2_000);

    assert_eq!(
        fs::read(victim.join("nested").join("data")).unwrap(),
        b"keep"
    );
}

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
