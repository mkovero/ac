//! `S` in the transfer view (#256): take a snapshot, keep it, and overlay
//! it — off the UI thread.
//!
//! A snapshot is a multi-megabyte fetch plus a re-derivation of the whole
//! captured window. Done on the UI thread it stalls the display and, worse,
//! the stimulus keepalive: a stall past the daemon's 1.5 s dead-man drops
//! the drive mid-measurement. So the capture runs on its own thread with
//! its own CTRL connection, and the app polls for the result.
//!
//! The daemon's spool is cleared at session end, so the file is written
//! here too, to [`captures_dir`], byte for byte as the daemon produced it
//! (sha256-verified on fetch). It reopens with `snapshot_flow::open_local`.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};

use anyhow::{Context, Result};

use crate::view::LoadedRun;
use crate::zmq_client::{Client, Endpoint};

/// What a finished capture hands back.
pub struct Captured {
    /// Where the `.acsnap` was written.
    pub path: PathBuf,
    /// Pair 0 of the snapshot, ready to overlay.
    pub run: LoadedRun,
}

/// `~/.local/share/ac/captures` — the client's own copy of every `S`
/// capture. Not the daemon's spool (`~/.local/state/ac/snapshots`), which
/// is emptied when the session ends.
pub fn captures_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home)
        .join(".local")
        .join("share")
        .join("ac")
        .join("captures")
}

/// The file name for a capture taken at `captured_at_utc` (RFC3339):
/// colons are not portable in file names, so they become `-`.
pub fn file_name(captured_at_utc: &str) -> String {
    format!("{}.acsnap", captured_at_utc.replace(':', "-"))
}

/// Trigger, fetch, write under `dir`, and derive pair 0 — one blocking
/// call, for the capture thread.
pub fn capture_into(client: &Client, dir: &Path) -> Result<Captured> {
    let (bytes, snap) = crate::snapshot_flow::trigger_and_fetch_bytes(client)?;
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let name = file_name(&snap.meta.captured_at_utc);
    let path = dir.join(&name);
    std::fs::write(&path, &bytes).with_context(|| format!("write {}", path.display()))?;
    let run = crate::snapshot_flow::stored_run_from_snapshot(&snap, 0, name)?;
    Ok(Captured { path, run })
}

/// Run [`capture_into`] [`captures_dir`] on a thread with its own
/// connection to `endpoint`. The receiver yields exactly one result; the
/// error is already rendered for the status line.
pub fn spawn(endpoint: Endpoint) -> Receiver<Result<Captured, String>> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let result = Client::connect(&endpoint)
            .and_then(|client| capture_into(&client, &captures_dir()))
            .map_err(|e| format!("{e:#}"));
        let _ = tx.send(result);
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_carry_no_colons() {
        assert_eq!(
            file_name("2026-09-26T14:30:05Z"),
            "2026-09-26T14-30-05Z.acsnap"
        );
    }

    #[test]
    fn captures_live_under_share_not_the_daemon_spool() {
        let dir = captures_dir();
        assert!(
            dir.ends_with(".local/share/ac/captures"),
            "{}",
            dir.display()
        );
    }
}
