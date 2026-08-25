use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::client::AcClient;

pub fn find_binary(name: &str) -> Option<PathBuf> {
    if let Ok(path) = which(name) {
        return Some(path);
    }
    let dev_path = dev_build_path(name);
    if dev_path.exists() {
        return Some(dev_path);
    }
    None
}

fn which(name: &str) -> Result<PathBuf, ()> {
    let path_var = std::env::var("PATH").map_err(|_| ())?;
    for dir in path_var.split(':') {
        let candidate = PathBuf::from(dir).join(name);
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(())
}

fn dev_build_path(name: &str) -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();
    // Walk up from the running binary to find the workspace target dir.
    // Typical: ac-rs/target/debug/ac → ac-rs/target/debug/<name>
    if let Some(dir) = exe.parent() {
        let candidate = dir.join(name);
        if candidate.exists() {
            return candidate;
        }
    }
    // Fallback: relative to cwd
    PathBuf::from(format!("ac-rs/target/debug/{name}"))
}

fn daemon_mtime() -> f64 {
    find_binary("ac-daemon")
        .and_then(|p| p.metadata().ok())
        .and_then(|m| m.modified().ok())
        .map(|t| {
            t.duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64()
        })
        .unwrap_or(0.0)
}

/// Caller's own `$HOME`, with the same fallback `ac-daemon` uses for its
/// identity field (#385) — degrade to `"."` rather than treat an unset
/// `$HOME` as a mismatch against every daemon.
fn my_home() -> String {
    std::env::var("HOME").unwrap_or_else(|_| ".".to_string())
}

/// If `reply` (a `status`/`server_connections`-shaped ok reply) carries a
/// `home` field that differs from the caller's own `$HOME`, format the
/// mismatch warning (ux spec Frame 2). `None` when the reply carries no
/// `home` at all (an older daemon, pre-#385 — nothing to compare) or when
/// the two match.
fn mismatch_warning(reply: &serde_json::Value, host: &str, ctrl_port: u16) -> Option<String> {
    let daemon_home = reply.get("home").and_then(|v| v.as_str())?;
    let this_home = my_home();
    if daemon_home == this_home {
        return None;
    }
    let pid = reply.get("pid").and_then(|v| v.as_u64()).unwrap_or(0);
    let spawn_label = match reply.get("spawn_mode").and_then(|v| v.as_str()) {
        Some("auto") => "auto-spawned",
        Some("manual") => "manual",
        _ => "?",
    };
    let started_at = reply
        .get("started_at")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    Some(format!(
        "  warning: daemon at {host}:{ctrl_port} belongs to a different HOME\n    \
         daemon:  {daemon_home}  (pid {pid}, {spawn_label} {started_at})\n    \
         this ac: {this_home}"
    ))
}

pub fn ensure_server(client: &mut AcClient, host: &str, ctrl_port: u16) {
    let is_local = matches!(host, "localhost" | "127.0.0.1" | "::1");

    let status = client.send_cmd(&serde_json::json!({"cmd": "status"}), Some(1500));

    if let Some(reply) = &status {
        if reply.get("ok").and_then(|v| v.as_bool()) == Some(true) {
            // Gate BOTH what follows: the silent-proceed fallthrough below,
            // and the stale-`src_mtime` auto-`quit`+respawn branch inside
            // the `is_local` arm — `quit` must never fire against a `home`
            // that isn't the caller's own (#385).
            if let Some(warning) = mismatch_warning(reply, host, ctrl_port) {
                eprintln!("{warning}");
                std::process::exit(1);
            }
            if is_local {
                let server_mtime = reply
                    .get("src_mtime")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                let bin_mtime = daemon_mtime();
                if bin_mtime > 0.0 && bin_mtime > server_mtime + 1.0 {
                    eprintln!("  restarting stale daemon...");
                    client.send_cmd(&serde_json::json!({"cmd": "quit"}), Some(1000));
                    std::thread::sleep(Duration::from_millis(300));
                    spawn_daemon();
                    wait_for_server(client);
                }
            }
            return;
        }
    }

    if !is_local {
        eprintln!("  error: server at {host} not responding");
        std::process::exit(1);
    }

    spawn_daemon();
    wait_for_server(client);
}

fn spawn_daemon() {
    let bin = match find_binary("ac-daemon") {
        Some(p) => p,
        None => {
            eprintln!(
                "  error: ac-daemon not found — build with: cd ac-rs && cargo build -p ac-daemon"
            );
            std::process::exit(1);
        }
    };
    eprintln!("  starting daemon: {}", bin.display());
    let mut cmd = Command::new(&bin);
    cmd.arg("--local").arg("--auto-spawned");
    // Carry the client's port override (see `main::port_override`) into
    // the daemon we start. Without this the override would move the
    // client and leave an auto-spawned daemon on 5556/5557, so the two
    // would never meet — a setting that silently does nothing.
    for (var, flag) in [
        ("AC_CTRL_PORT", "--ctrl-port"),
        ("AC_DATA_PORT", "--data-port"),
    ] {
        if let Ok(p) = std::env::var(var) {
            cmd.arg(flag).arg(p);
        }
    }
    if let Err(e) = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        eprintln!("  error: failed to spawn daemon: {e}");
        std::process::exit(1);
    }
}

fn wait_for_server(client: &mut AcClient) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
        if let Some(reply) = client.send_cmd(&serde_json::json!({"cmd": "status"}), Some(500)) {
            if reply.get("ok").and_then(|v| v.as_bool()) == Some(true) {
                return;
            }
        }
    }
    eprintln!("  error: daemon did not start within 3 seconds");
    std::process::exit(1);
}
