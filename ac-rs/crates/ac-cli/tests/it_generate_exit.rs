//! Exit status of `ac generate` once it is waiting for the daemon (#455).
//!
//! `ac generate` prints `Playing …` and then blocks in `wait_for_stop` until
//! the daemon publishes `done` or `error`, or the operator stops it. A
//! mid-run `error` used to be printed and then exit 0, so a rig script could
//! not tell a clean emission from one that failed after the ack.
//!
//! The failure is reached through the fake backend's opt-in fault hook
//! `AC_FAKE_START_FAIL_CALLS` (`ac-daemon/src/audio/fake/hooks.rs`), which
//! makes the listed `FakeEngine::start` calls fail. Derivation of the index:
//! in a fresh daemon with no loopback configured, the `generate` handler
//! starts no engine of its own; `Gate::run` only probes the loop when a
//! loopback is configured, and this scratch config has none. The only
//! `start` in the whole run is therefore the worker's, after the ack, which
//! is call 0. On that failure the worker publishes `error` with
//! `cmd: "generate"` and the hook's variable name in the message.
//!
//! The error test asserts that the `error:` line carrying the hook name is
//! printed *after* the `Playing` line. A failure consumed before the wait
//! loop (session check, consumer-check refusal, both of which exit 1 on
//! their own) would otherwise pass the exit-status assertion without ever
//! reaching the code under test, and an index that drifted onto a different
//! call would not print the hook name at all.

mod support;

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const START_FAIL: &str = "AC_FAKE_START_FAIL_CALLS";

/// How long a test waits for `ac` to print a line or to exit. A deadline,
/// not a delay. Provenance: assumed; `ac generate` against the fake backend
/// reaches `Playing` well inside a second.
const DEADLINE: Duration = Duration::from_secs(20);

/// The generate invocation both tests run: output 0, well below the
/// emission ceiling, explicit so no default-channel resolution is involved.
const GENERATE_ARGS: &[&str] = &["generate", "sine", "0", "-40dbfs", "1khz"];

struct Rig {
    daemon: support::DaemonGuard,
    home: PathBuf,
}

impl Rig {
    fn start(env: &[(&str, &str)]) -> Self {
        let home = support::alloc_home("ac-cli-genexit");
        fs::write(home.join(".config").join("ac").join("config.json"), b"{}")
            .expect("seed config.json");
        let daemon = support::spawn_daemon_with_env(&home, true, "127.0.0.1", None, env);
        Self { daemon, home }
    }

    /// Start the real `ac` binary against this rig, stdout piped, stdin
    /// closed so the wait loop never reads a keystroke from the test.
    fn spawn_ac(&self, args: &[&str]) -> Child {
        Command::new(env!("CARGO_BIN_EXE_ac"))
            .env("HOME", &self.home)
            .env("AC_CTRL_PORT", self.daemon.ctrl.to_string())
            .env("AC_DATA_PORT", self.daemon.data.to_string())
            .current_dir(&self.home)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn ac")
    }

    /// One ctrl-port request to this rig's daemon.
    fn send(&self, cmd: &serde_json::Value) -> serde_json::Value {
        let ctx = zmq::Context::new();
        let s = ctx.socket(zmq::REQ).unwrap();
        s.set_linger(0).ok();
        s.set_rcvtimeo(5_000).ok();
        s.set_sndtimeo(5_000).ok();
        s.connect(&format!("tcp://127.0.0.1:{}", self.daemon.ctrl))
            .expect("connect ctrl");
        s.send(cmd.to_string().as_bytes(), 0).expect("send ctrl");
        let bytes = s.recv_bytes(0).expect("ctrl reply");
        serde_json::from_slice(&bytes).expect("ctrl reply is JSON")
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.daemon.kill();
        let _ = fs::remove_dir_all(&self.home);
    }
}

/// Forward `child`'s stdout lines to a channel, so the test can wait for one
/// with a deadline.
fn stdout_lines(child: &mut Child) -> mpsc::Receiver<String> {
    let out = child.stdout.take().expect("stdout piped");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    rx
}

/// Collect lines until one contains `needle`; panics with everything seen
/// on timeout or end of stream.
fn read_until(rx: &mpsc::Receiver<String>, needle: &str, seen: &mut Vec<String>) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(line) => {
                let hit = line.contains(needle);
                seen.push(line);
                if hit {
                    return;
                }
            }
            Err(e) => panic!(
                "no {needle:?} line ({e:?}); stdout so far:\n{}",
                seen.join("\n")
            ),
        }
    }
}

/// Wait for `child` to exit, killing it at the deadline.
fn wait_exit(child: &mut Child) -> ExitStatus {
    let deadline = Instant::now() + DEADLINE;
    loop {
        if let Some(st) = child.try_wait().expect("try_wait ac") {
            return st;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("`ac` did not exit within {DEADLINE:?}");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn stderr_of(child: &mut Child) -> String {
    let mut s = String::new();
    if let Some(mut e) = child.stderr.take() {
        use std::io::Read;
        let _ = e.read_to_string(&mut s);
    }
    s
}

#[test]
fn a_mid_run_daemon_error_exits_non_zero_after_printing_it() {
    let rig = Rig::start(&[(START_FAIL, "0")]);
    let mut child = rig.spawn_ac(GENERATE_ARGS);
    let rx = stdout_lines(&mut child);

    // Order is the assertion: an `error:` line printed before `Playing` is
    // swallowed by the first read, and the second read then times out.
    let mut seen = Vec::new();
    read_until(&rx, "Playing", &mut seen);
    read_until(&rx, "error:", &mut seen);
    let status = wait_exit(&mut child);
    let stderr = stderr_of(&mut child);
    let stdout = seen.join("\n");

    let error_line = seen.last().unwrap();
    assert!(
        error_line.contains(START_FAIL),
        "error line must name the start-fail hook, else the index hit another call: \
         {error_line:?}\nstdout:\n{stdout}"
    );
    assert_eq!(
        error_line.trim_start(),
        format!("error: fake engine start failed on call 0 ({START_FAIL})"),
        "printed error text is unchanged by #455"
    );
    assert!(
        !status.success(),
        "a mid-run daemon error must exit non-zero, got {status}\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert_eq!(status.code(), Some(1), "same status as a rejected ack");
}

#[test]
fn a_done_frame_exits_zero() {
    let rig = Rig::start(&[]);
    let mut child = rig.spawn_ac(GENERATE_ARGS);
    let rx = stdout_lines(&mut child);

    // Reading until `Playing` puts `ac` inside the wait loop, so the `done`
    // the stop produces is read there and not by the session-check wait.
    let mut seen = Vec::new();
    read_until(&rx, "Playing", &mut seen);
    let reply = rig.send(&serde_json::json!({"cmd": "stop", "name": "generate"}));
    assert_eq!(reply["ok"], true, "stop refused: {reply}");

    let status = wait_exit(&mut child);
    let stderr = stderr_of(&mut child);
    seen.extend(rx.try_iter());
    let stdout = seen.join("\n");
    assert!(
        status.success(),
        "`done` must exit 0, got {status}\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        !stdout.contains("error:"),
        "no error expected on a clean stop:\n{stdout}"
    );
}
