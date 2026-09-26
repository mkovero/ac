//! `Ctrl`+digit in the transfer view (#256): take a snapshot into a slot,
//! keep it, and overlay it — off the UI thread.
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
    /// The slot it was taken for.
    pub slot: u8,
    /// Opened from a saved file (`F`) rather than captured live.
    pub opened: bool,
    /// Where the `.acsnap` was written.
    pub path: PathBuf,
    /// Pair 0 of the snapshot, ready to overlay.
    pub run: LoadedRun,
}

/// `~/.local/share/ac/captures` — the client's own copy of every slot
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

/// The file name for slot `slot` captured at `captured_at_utc` (RFC3339):
/// colons are not portable in file names, so they become `-`.
pub fn file_name(slot: u8, captured_at_utc: &str) -> String {
    format!("slot{slot}-{}.acsnap", captured_at_utc.replace(':', "-"))
}

/// Write `bytes` under `dir` as a file that did not exist before: the
/// timestamp's name, or `-2`, `-3`, … appended when two captures share a
/// second. Never overwrites a capture already on disk.
pub(crate) fn write_new(dir: &Path, base: &str, bytes: &[u8]) -> Result<(PathBuf, String)> {
    use std::io::Write;
    let base = base.to_string();
    let (stem, ext) = match base.rsplit_once('.') {
        Some((stem, ext)) => (stem.to_string(), format!(".{ext}")),
        None => (base.clone(), String::new()),
    };
    for n in 1.. {
        let name = if n == 1 {
            base.clone()
        } else {
            format!("{stem}-{n}{ext}")
        };
        let path = dir.join(&name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut f) => {
                f.write_all(bytes)
                    .with_context(|| format!("write {}", path.display()))?;
                return Ok((path, name));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).with_context(|| format!("create {}", path.display())),
        }
    }
    unreachable!("an unbounded counter always finds a free name")
}

/// Trigger, fetch, write under `dir`, and derive pair 0 for `slot` — one
/// blocking call, for the capture thread. The run is labelled `slot N`;
/// the file keeps the full name, which the status message reports.
pub fn capture_into(client: &Client, dir: &Path, slot: u8) -> Result<Captured> {
    let (bytes, snap) = crate::snapshot_flow::trigger_and_fetch_bytes(client)?;
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let (path, _) = write_new(dir, &file_name(slot, &snap.meta.captured_at_utc), &bytes)?;
    let run = crate::snapshot_flow::stored_run_from_snapshot(&snap, 0, format!("slot {slot}"))?;
    Ok(Captured {
        slot,
        path,
        run,
        opened: false,
    })
}

/// Run [`capture_into`] [`captures_dir`] on a thread with its own
/// connection to `endpoint`. The receiver yields exactly one result; the
/// error is already rendered for the status line.
pub fn spawn(endpoint: Endpoint, slot: u8) -> Receiver<Result<Captured, String>> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let result = Client::connect(&endpoint)
            .and_then(|client| capture_into(&client, &captures_dir(), slot))
            .map_err(|e| format!("{e:#}"));
        let _ = tx.send(result);
    });
    rx
}

/// Open a saved capture into slot `slot` on a thread (`F`, #256): the
/// derivation replays the whole ring and can take a moment, so it stays off
/// the UI thread like a live capture does.
pub fn spawn_open(path: PathBuf, slot: u8) -> Receiver<Result<Captured, String>> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let result = crate::snapshot_flow::open_stored_transfer_run(&path, 0)
            .map(|mut run| {
                run.label = format!("slot {slot}");
                Captured {
                    slot,
                    path,
                    run,
                    opened: true,
                }
            })
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
            file_name(3, "2026-09-26T14:30:05Z"),
            "slot3-2026-09-26T14-30-05Z.acsnap"
        );
    }

    #[test]
    fn a_second_capture_in_the_same_second_does_not_overwrite_the_first() {
        let dir = std::env::temp_dir().join(format!("ac-capture-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let base = file_name(1, "2026-09-26T14:30:05Z");
        let (p1, _) = write_new(&dir, &base, b"one").unwrap();
        let (p2, n2) = write_new(&dir, &base, b"two").unwrap();
        assert_ne!(p1, p2);
        assert_eq!(n2, "slot1-2026-09-26T14-30-05Z-2.acsnap");
        assert_eq!(std::fs::read(&p1).unwrap(), b"one");
        assert_eq!(std::fs::read(&p2).unwrap(), b"two");
        std::fs::remove_dir_all(&dir).ok();
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
