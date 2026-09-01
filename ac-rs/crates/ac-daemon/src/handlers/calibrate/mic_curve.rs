//! `calibrate_mic_curve` / `set_mic_correction_enabled` — the mic
//! frequency-response layer's two commands.

use serde_json::{json, Value};

use ac_core::shared::calibration::{Calibration, MicResponse};

use crate::server::ServerState;

use super::channels_from;

/// `calibrate_mic_curve` — attach (or clear) a mic frequency-response
/// correction curve on a channel.
///
/// CLI parses the .frd / .txt file and uploads the validated arrays via
/// this cmd, so the daemon never has to read user-supplied paths and
/// works the same way for local and remote daemons. Two operations:
/// `op = "set"` with `freqs_hz` + `gain_db` arrays, or `op = "clear"`
/// to drop a stored curve. Voltage and SPL fields on the same entry
/// stay untouched (`load_or_new` round-trip).
pub fn calibrate_mic_curve(state: &ServerState, cmd: &Value) -> Value {
    let cfg = state.cfg.lock().unwrap().clone();
    let (out_ch, in_ch) = channels_from(cmd, &cfg);
    let op = cmd.get("op").and_then(Value::as_str).unwrap_or("set");

    let mut cal = Calibration::load_or_new(out_ch, in_ch, None);
    match op {
        "clear" => {
            cal.mic_response = None;
        }
        "set" => {
            let freqs: Option<Vec<f32>> = cmd.get("freqs_hz").and_then(Value::as_array).map(|a| {
                a.iter()
                    .filter_map(|v| v.as_f64().map(|v| v as f32))
                    .collect()
            });
            let gains: Option<Vec<f32>> = cmd.get("gain_db").and_then(Value::as_array).map(|a| {
                a.iter()
                    .filter_map(|v| v.as_f64().map(|v| v as f32))
                    .collect()
            });
            let (freqs_hz, gain_db) = match (freqs, gains) {
                (Some(f), Some(g)) if !f.is_empty() && f.len() == g.len() => (f, g),
                _ => {
                    return json!({"ok": false,
                    "error": "calibrate_mic_curve set: missing/mismatched freqs_hz/gain_db"})
                }
            };
            // Re-validate length bounds + monotonicity here too — the CLI
            // already parsed and validated, but a hostile or buggy client
            // shouldn't be able to write a malformed curve to disk.
            if freqs_hz.len() < MicResponse::MIN_POINTS {
                return json!({"ok": false,
                    "error": format!("curve too sparse: {} < {}", freqs_hz.len(), MicResponse::MIN_POINTS)});
            }
            if freqs_hz.len() > MicResponse::MAX_POINTS {
                return json!({"ok": false,
                    "error": format!("curve too dense: {} > {}", freqs_hz.len(), MicResponse::MAX_POINTS)});
            }
            for w in freqs_hz.windows(2) {
                if w[1] <= w[0] || !w[0].is_finite() || !w[1].is_finite() {
                    return json!({"ok": false,
                        "error": "freqs_hz must be strictly increasing and finite"});
                }
            }
            for &g in &gain_db {
                if !g.is_finite() {
                    return json!({"ok": false,
                        "error": "gain_db values must be finite"});
                }
            }
            let source_path = cmd
                .get("source_path")
                .and_then(Value::as_str)
                .map(str::to_string);
            cal.mic_response = Some(MicResponse {
                freqs_hz,
                gain_db,
                source_path,
                imported_at: ac_core::shared::time::now_utc_iso8601(),
            });
        }
        other => {
            return json!({"ok": false,
                "error": format!("calibrate_mic_curve: unknown op {other:?}, expected set/clear")});
        }
    }
    if let Err(e) = cal.save(None) {
        return json!({"ok": false, "error": format!("save failed: {e}")});
    }
    json!({
        "ok":      true,
        "key":     cal.key(),
        "loaded":  cal.mic_response.as_ref().map(|r| r.freqs_hz.len()).unwrap_or(0),
    })
}

/// `set_mic_correction_enabled` — toggle daemon-side mic-curve application.
/// When disabled, monitor frames carry the raw uncorrected magnitudes (a
/// loaded curve does not vanish — just stops being applied). Used by the
/// UI's `Shift+M` keybinding for diagnostics. The flag is process-wide;
/// per-channel curves are still respected, the toggle just gates them.
pub fn set_mic_correction_enabled(state: &ServerState, cmd: &Value) -> Value {
    let enabled = cmd.get("enabled").and_then(Value::as_bool).unwrap_or(true);
    state
        .mic_correction_enabled
        .store(enabled, std::sync::atomic::Ordering::Relaxed);
    json!({"ok": true, "enabled": enabled})
}
