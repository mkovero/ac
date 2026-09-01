//! CTRL commands that target a **running** `transfer_stream` worker
//! without spawning one: `set_drive` and `relock`.
//!
//! Neither has a `cmd_group` entry and neither consults `check_busy` — see
//! [`set_drive`] for why routing them through the busy guard would be wrong
//! rather than merely unnecessary.

use serde_json::{json, Value};

use crate::server::ServerState;

/// `set_drive` (§4.3) — start, stop, or re-level the stimulus of a
/// running `transfer_stream` session.
///
/// Dispatched like `snapshot`: a CTRL command that targets a live worker
/// without spawning one, so it has no `cmd_group` entry and never
/// consults `check_busy`. That is not an exception carved out for it —
/// routing it through the busy guard would make it contend with the very
/// `Group::Transfer` worker it targets, and since this is also the
/// command that STOPS the drive, the contention would land on the
/// panic-stop path.
///
/// `level_dbfs` is required on every request, including `on: false`:
/// every message doubles as the keepalive, so every message is a full
/// state assertion rather than a delta against state the server would
/// otherwise have to remember.
pub fn set_drive(state: &ServerState, cmd: &Value) -> Value {
    let drive = {
        let slot = state.drive_state.lock().unwrap();
        match slot.as_ref() {
            Some(d) => d.clone(),
            None => return json!({"ok": false, "error": "no transfer_stream session running"}),
        }
    };

    let on = match cmd.get("on").and_then(Value::as_bool) {
        Some(v) => v,
        None => return json!({"ok": false, "error": "'on' required (bool)"}),
    };
    // A missing or non-finite level is a client bug. Coercing it would
    // hide that, and this is the one command where a silently
    // substituted number reaches a loudspeaker.
    let level = match cmd.get("level_dbfs").and_then(Value::as_f64) {
        Some(v) if v.is_finite() => v,
        _ => return json!({"ok": false, "error": "'level_dbfs' required (finite number)"}),
    };

    let ceiling = state.cfg.lock().unwrap().drive_max_dbfs;
    // Clamping is normal operation, not an error: a stimulus command
    // that fails instead of applying a safe level is a worse field
    // failure than one that quietly applies the ceiling. The echo below
    // is always the APPLIED value, so the client can see what happened.
    let applied = level.min(ceiling);
    drive.set(on, applied);

    json!({"ok": true, "on": on, "level_dbfs": applied})
}

/// `relock` (#226) — discard every pair's held delay lock in the
/// **running** `transfer_stream` session, so the worker's next tick
/// retries acquisition from scratch. A held lock is a maintained
/// quantity, not a cached one: the operator asking is one of the two
/// events that invalidate it (the other is the drive coming on, handled
/// inside the worker loop itself).
///
/// Dispatched like `set_drive`: targets a live worker without spawning
/// one, so it has no `cmd_group` entry and never consults `check_busy`.
/// Session-wide, no `pair` selector — the flush is a session event and a
/// per-pair variant is scope this issue does not need.
pub fn relock(state: &ServerState, _cmd: &Value) -> Value {
    let slot = state.relock_state.lock().unwrap();
    match slot.as_ref() {
        Some(r) => {
            r.request();
            json!({"ok": true})
        }
        None => json!({"ok": false, "error": "no transfer_stream session running"}),
    }
}
