//! ac-daemon — ZMQ REP+PUB server, Rust implementation.
//!
//! Implements the same wire protocol as the Python `ac/server/engine.py`.
//! The Python client (`ac/client/ac.py`) speaks to this daemon unchanged.

mod audio;
mod handlers;
mod server;
mod workers;

use std::time::SystemTime;

pub use server::run;

/// Build-time stamp used as `src_mtime` in the `status` reply.
/// The Python client compares this to its own scan of server/ files and
/// restarts if the running server is older than the installed source.
/// We set it to the binary's mtime at startup so rebuilds always trigger.
pub fn binary_mtime() -> f64 {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.metadata().ok())
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let fake_audio  = args.iter().any(|a| a == "--fake-audio");
    let local_only  = args.iter().any(|a| a == "--local");
    let ctrl_port: u16 = args.windows(2)
        .find(|w| w[0] == "--ctrl-port")
        .and_then(|w| w[1].parse().ok())
        .unwrap_or(5556);
    let data_port: u16 = args.windows(2)
        .find(|w| w[0] == "--data-port")
        .and_then(|w| w[1].parse().ok())
        .unwrap_or(5557);

    if let Err(e) = run(ctrl_port, data_port, local_only, fake_audio) {
        eprintln!("ac-daemon error: {e:#}");
        std::process::exit(1);
    }
}
