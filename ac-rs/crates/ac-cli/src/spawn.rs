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

pub fn ensure_server(client: &mut AcClient, host: &str) {
    let is_local = matches!(host, "localhost" | "127.0.0.1" | "::1");

    let status = client.send_cmd(&serde_json::json!({"cmd": "status"}), Some(500));

    if let Some(reply) = &status {
        if reply.get("ok").and_then(|v| v.as_bool()) == Some(true) {
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
    Command::new(&bin)
        .arg("--local")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok();
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
