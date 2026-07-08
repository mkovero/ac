use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::{check_ack, get_cal, level_to_dbfs};
use crate::client::AcClient;
use crate::io;
use crate::parse::CommandKind;
use crate::spawn::find_binary;

/// `ac test software` — the daemon-side numeric self-test (unchanged),
/// plus the display-truth harness (#170): spawns an isolated
/// `ac-daemon --fake-audio` + `ac-ui --headless-test` pair (never the
/// caller's own daemon, which may be talking to real hardware) and prints
/// its T2/T3 results in the same table. Exits nonzero if anything failed —
/// this is the CI-equivalent gate value-display PRs are meant to pass
/// before `qa-approved` (handoff.md acceptance criteria).
pub fn run_software(client: &mut AcClient) {
    let ack = check_ack(
        client.send_cmd(&serde_json::json!({"cmd": "test_software"}), None),
        "test_software",
    );
    println!("\n  Software self-test");
    println!("  {}", "\u{2500}".repeat(40));

    let mut all_pass = ack
        .get("all_pass")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if let Some(results) = ack.get("results").and_then(|v| v.as_array()) {
        print_rows(results);
    }
    println!();

    println!(
        "  Display-truth harness (T2/T3, #170 — fake-audio only, never touches real hardware)"
    );
    println!("  {}", "\u{2500}".repeat(40));
    let dt_results = run_display_truth_harness();
    print_rows(&dt_results);
    all_pass &= dt_results
        .iter()
        .all(|r| r.get("pass").and_then(Value::as_bool).unwrap_or(false));
    println!();

    if !all_pass {
        std::process::exit(1);
    }
}

fn print_rows(results: &[Value]) {
    for r in results {
        let name = r.get("name").and_then(|v| v.as_str()).unwrap_or("?");
        let pass = r.get("pass").and_then(|v| v.as_bool()).unwrap_or(false);
        let mark = if pass { "PASS" } else { "FAIL" };
        let detail = r.get("detail").and_then(|v| v.as_str()).unwrap_or("");
        println!("  [{mark}]  {name}  {detail}");
    }
}

/// Ask the OS for two free loopback ports so the harness's throwaway
/// daemon can never collide with a real `ac-daemon` (or a second harness
/// run) on the default 5556/5557 pair. Small bind-then-drop race, same
/// idiom used for test-only port allocation elsewhere in this workspace;
/// acceptable here since this is itself a test/dev-tooling code path.
fn free_port_pair() -> std::io::Result<(u16, u16)> {
    let a = TcpListener::bind("127.0.0.1:0")?;
    let b = TcpListener::bind("127.0.0.1:0")?;
    Ok((a.local_addr()?.port(), b.local_addr()?.port()))
}

fn fail_row(name: &str, detail: String) -> Vec<Value> {
    vec![serde_json::json!({"name": name, "pass": false, "detail": detail})]
}

fn run_display_truth_harness() -> Vec<Value> {
    let daemon_bin = match find_binary("ac-daemon") {
        Some(p) => p,
        None => return fail_row("display-truth harness", "ac-daemon binary not found".into()),
    };
    let ui_bin = match find_binary("ac-ui") {
        Some(p) => p,
        None => return fail_row("display-truth harness", "ac-ui binary not found".into()),
    };
    let (ctrl_port, data_port) = match free_port_pair() {
        Ok(p) => p,
        Err(e) => {
            return fail_row(
                "display-truth harness",
                format!("could not allocate loopback ports: {e}"),
            )
        }
    };

    let mut daemon = match Command::new(&daemon_bin)
        .args([
            "--local",
            "--fake-audio",
            "--ctrl-port",
            &ctrl_port.to_string(),
            "--data-port",
            &data_port.to_string(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return fail_row(
                "display-truth harness",
                format!("failed to spawn isolated ac-daemon: {e}"),
            )
        }
    };

    let ctrl_addr = format!("tcp://127.0.0.1:{ctrl_port}");
    let data_addr = format!("tcp://127.0.0.1:{data_port}");
    if let Err(e) = wait_for_daemon(&ctrl_addr) {
        let _ = daemon.kill();
        let _ = daemon.wait();
        return fail_row(
            "display-truth harness",
            format!("isolated ac-daemon did not come up: {e}"),
        );
    }

    let ui_output = Command::new(&ui_bin)
        .args([
            "--headless-test",
            "--ctrl",
            &ctrl_addr,
            "--connect",
            &data_addr,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();

    let _ = daemon.kill();
    let _ = daemon.wait();

    let output = match ui_output {
        Ok(o) => o,
        Err(e) => {
            return fail_row(
                "display-truth harness",
                format!("failed to spawn ac-ui --headless-test: {e}"),
            )
        }
    };

    if !output.status.success() {
        // A crash (e.g. a driver-level segfault in a software Vulkan
        // stack — see the note at `headless::readback`'s `device.poll`
        // call) exits non-zero with no JSON on stdout. Surface that
        // distinctly rather than silently reporting zero checks as a
        // pass, or trying to parse empty/partial JSON.
        let stdout = String::from_utf8_lossy(&output.stdout);
        let parsed: Option<Value> = serde_json::from_str(stdout.trim()).ok();
        if let Some(v) = parsed {
            if let Some(results) = v.get("results").and_then(|r| r.as_array()) {
                return results.to_vec();
            }
        }
        return fail_row(
            "display-truth harness",
            format!(
                "ac-ui --headless-test exited with {:?} and no parseable result \
                 (likely a GPU/driver-level crash under this host's Vulkan stack — \
                 see runbook; T2 buffer checks may still be informative in its stderr log)",
                output.status.code(),
            ),
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    match serde_json::from_str::<Value>(stdout.trim()) {
        Ok(v) => v
            .get("results")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_else(|| {
                fail_row(
                    "display-truth harness",
                    "no results field in ac-ui output".into(),
                )
            }),
        Err(e) => fail_row(
            "display-truth harness",
            format!("could not parse ac-ui --headless-test output as JSON: {e}"),
        ),
    }
}

fn wait_for_daemon(ctrl_addr: &str) -> anyhow::Result<()> {
    let ctx = zmq::Context::new();
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
        let Ok(s) = ctx.socket(zmq::REQ) else {
            continue;
        };
        s.set_linger(0).ok();
        s.set_rcvtimeo(300).ok();
        s.set_sndtimeo(300).ok();
        if s.connect(ctrl_addr).is_err() {
            continue;
        }
        if s.send(
            serde_json::json!({"cmd": "status"}).to_string().as_bytes(),
            0,
        )
        .is_err()
        {
            continue;
        }
        if let Ok(reply) = s.recv_string(0) {
            if let Ok(Ok(v)) = reply.map(|r| serde_json::from_str::<Value>(&r)) {
                if v.get("ok").and_then(Value::as_bool) == Some(true) {
                    return Ok(());
                }
            }
        }
    }
    anyhow::bail!("no response within 3s")
}

pub fn run_hardware(cmd: &CommandKind, client: &mut AcClient) {
    let dmm = match cmd {
        CommandKind::TestHardware { dmm } => *dmm,
        _ => unreachable!(),
    };

    let mut json = serde_json::json!({"cmd": "test_hardware"});
    if dmm {
        json["dmm"] = true.into();
    }

    check_ack(client.send_cmd(&json, None), "test_hardware");
    println!("\n  Hardware test started...\n");

    io::print_freq_header(false);

    loop {
        let frame = match client.recv_data(300_000) {
            Some(f) => f,
            None => {
                eprintln!("  error: timeout");
                return;
            }
        };
        let (topic, data) = frame;

        if topic == "data" {
            if data.get("type").and_then(|v| v.as_str())
                == Some("measurement/frequency_response/point")
            {
                io::print_freq_row(&data);
            }
        } else if topic == "done" {
            if let Some(xruns) = data.get("xruns").and_then(|v| v.as_u64()) {
                if xruns > 0 {
                    println!("\n  !! {xruns} xrun(s)");
                }
            }
            println!();
            return;
        } else if topic == "error" {
            let msg = data
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("error");
            eprintln!("\n  error: {msg}");
            return;
        }
    }
}

pub fn run_dut(cmd: &CommandKind, cfg: &ac_core::config::Config, client: &mut AcClient) {
    let (compare, level) = match cmd {
        CommandKind::TestDut { compare, level } => (*compare, level),
        _ => unreachable!(),
    };

    let cal = get_cal(client);
    let have_cal = cal.is_some();
    let level_db = level_to_dbfs(level, cal.as_ref());

    let mut json = serde_json::json!({"cmd": "test_dut", "level_dbfs": level_db});
    if compare {
        json["compare"] = true.into();
    }

    check_ack(client.send_cmd(&json, None), "test_dut");
    println!("\n  DUT test at {level_db:.1} dBFS\n");

    io::print_freq_header(have_cal);

    let mut results = Vec::new();
    loop {
        let frame = match client.recv_data(300_000) {
            Some(f) => f,
            None => {
                eprintln!("  error: timeout");
                break;
            }
        };
        let (topic, data) = frame;

        if topic == "data" {
            if data.get("type").and_then(|v| v.as_str())
                == Some("measurement/frequency_response/point")
            {
                io::print_freq_row(&data);
                results.push(data);
            }
        } else if topic == "done" {
            if let Some(xruns) = data.get("xruns").and_then(|v| v.as_u64()) {
                if xruns > 0 {
                    println!("\n  !! {xruns} xrun(s)");
                }
            }
            break;
        } else if topic == "error" {
            let msg = data
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("error");
            eprintln!("\n  error: {msg}");
            break;
        }
    }

    if !results.is_empty() {
        io::print_summary(&results, "DUT", have_cal);
        let dir = io::output_dir(cfg);
        let ts = io::timestamp();
        let path = dir.join(format!("test_dut_{ts}.csv"));
        io::save_csv(&results, &path);
    }
}
