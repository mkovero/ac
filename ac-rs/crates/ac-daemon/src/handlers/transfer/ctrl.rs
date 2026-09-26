//! CTRL commands that target a **running** `transfer_stream` worker
//! without spawning one: `set_drive` and `set_delay`.
//!
//! Neither has a `cmd_group` entry and neither consults `check_busy` — see
//! [`set_drive`] for why routing them through the busy guard would be wrong
//! rather than merely unnecessary.

use serde_json::{json, Value};

use ac_core::shared::emission_level::MAX_EMISSION_DBFS;

use crate::handlers::check_emission_or_refuse;
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
///
/// #459: a level above the fixed maximum is refused, never clamped —
/// except when `on: false`, which is never checked (ZMQ.md): turning
/// drive off must never be the one request that can be rejected, or a
/// client trying to silence a session could be refused into leaving it
/// driving.
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

    let applied = if on {
        let cfg = state.cfg.lock().unwrap().clone();
        match check_emission_or_refuse(state, &cfg, level) {
            Ok(v) => v,
            Err(reply) => return reply,
        }
    } else {
        level
    };
    drive.set(on, applied);

    json!({"ok": true, "on": on, "level_dbfs": applied, "max_dbfs": MAX_EMISSION_DBFS})
}

/// `set_delay` (#669) — set, or re-find, the delay the **running**
/// `transfer_stream` session aligns with. The operator owns the delay, as in
/// Smaart: the daemon finds it once at start and publishes the live IR's
/// residual (`delay_residual`) on every frame; a client inserts that, types
/// a value, or nudges by one sample, all through this command.
///
/// `samples`: an integer holds that delay and marks it operator-set, which
/// no drive edge discards. `null` discards the held delay so the session
/// finds it again from the unaligned live IR — what `relock` (#226) did.
/// `pair`: a pair index in launch order; absent applies to every pair.
///
/// Dispatched like `set_drive`: targets a live worker without spawning one,
/// so it has no `cmd_group` entry and never consults `check_busy`.
pub fn set_delay(state: &ServerState, cmd: &Value) -> Value {
    let samples = match cmd.get("samples") {
        Some(Value::Null) => None,
        Some(v) => match v.as_i64() {
            Some(n) => Some(n),
            None => return json!({"ok": false, "error": "'samples' must be an integer or null"}),
        },
        None => return json!({"ok": false, "error": "'samples' required (integer or null)"}),
    };
    let pair = match cmd.get("pair") {
        None | Some(Value::Null) => None,
        Some(v) => match v.as_u64() {
            Some(n) => Some(n as usize),
            None => return json!({"ok": false, "error": "'pair' must be a non-negative integer"}),
        },
    };
    let n_pairs = state
        .snapshot_ring
        .lock()
        .unwrap()
        .as_ref()
        .map(|r| r.lock().unwrap().pairs.len());
    if let (Some(p), Some(n)) = (pair, n_pairs) {
        if p >= n {
            return json!({"ok": false, "error": format!("'pair' {p} out of range: session has {n} pair(s)")});
        }
    }
    let slot = state.delay_requests.lock().unwrap();
    match slot.as_ref() {
        Some(r) => {
            r.push(crate::workers::DelayCmd { pair, samples });
            json!({"ok": true, "samples": samples, "pair": pair})
        }
        None => json!({"ok": false, "error": "no transfer_stream session running"}),
    }
}
