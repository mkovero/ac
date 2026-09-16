//! Port leases for the ac-cli integration tests that spawn daemons.
//!
//! Each test binary hands out daemon ports from its own `PORT_CURSOR`, which
//! is unique only inside one process. Two processes running the same binary —
//! nextest's process-per-test, or two worktrees testing at once — start from
//! the same base and collide: the loser's daemon dies on bind and its client
//! talks to the winner's daemon ("belongs to a different HOME", "daemon never
//! came up").
//!
//! A lease takes an exclusive lock on a file named after the port pair and
//! holds it for as long as the lease lives, so a second process wanting the
//! same pair waits instead. Pairs are taken in increasing order within a
//! process, so two holders cannot wait on each other.
//!
//! Stopgap: a daemon orphaned by a killed test run keeps its port with no lock
//! held. The real fix is OS-assigned ports with an identity check, as
//! `ac-daemon/tests/common/mod.rs` does.

use std::fs::File;
use std::sync::atomic::{AtomicU16, Ordering};

pub struct PortLease {
    pub ctrl: u16,
    pub data: u16,
    _lock: File,
}

/// Take the next `(ctrl, data)` pair from `cursor`, waiting for any other
/// process that holds the same pair.
pub fn lease(cursor: &AtomicU16) -> PortLease {
    let base = cursor.fetch_add(2, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("ac-cli-it-ports-{base}.lock"));
    let lock = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .unwrap_or_else(|e| panic!("open port lock {}: {e}", path.display()));
    lock.lock()
        .unwrap_or_else(|e| panic!("lock {}: {e}", path.display()));
    PortLease {
        ctrl: base,
        data: base + 1,
        _lock: lock,
    }
}
