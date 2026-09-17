//! `calibrate_mic_curve` / `set_mic_correction_enabled` — the mic
//! frequency-response layer's two commands.

use serde_json::{json, Value};

use ac_core::shared::calibration::{Calibration, MicResponse};

use crate::server::ServerState;

use super::super::wire;
use super::channels_from;

/// State line of every `calibrate_mic_curve` wire-value refusal: nothing
/// was written.
const CURVE_UNCHANGED: (&str, &str) = ("data", "existing curve unchanged");

/// Read `freqs_hz` / `gain_db` positionally (#431). Order: both present and
/// arrays; equal, non-zero lengths; then elements by ascending index,
/// `freqs_hz[i]` before `gain_db[i]`. Checking lengths before elements is
/// what guarantees a refused element always has a partner to name.
///
/// Any invalid element refuses the whole upload — filtering each array on
/// its own drops elements at different indices, and two equal-length
/// survivors pair every frequency after the first gap with the wrong gain.
fn parse_curve(cmd: &Value) -> Result<(Vec<f32>, Vec<f32>), String> {
    const HEADLINE: &str = "mic curve not saved";
    let refuse = |e: wire::WireError, paired: Option<String>| {
        let mut trailers: Vec<(&str, &str)> = Vec::new();
        if let Some(p) = paired.as_deref() {
            trailers.push(("paired field", p));
        }
        trailers.push(CURVE_UNCHANGED);
        e.refusal(HEADLINE, &trailers)
    };
    let freqs = wire::opt_array(cmd, "freqs_hz").map_err(|e| refuse(e, None))?;
    let gains = wire::opt_array(cmd, "gain_db").map_err(|e| refuse(e, None))?;
    let (freqs, gains) = match (freqs, gains) {
        (Some(f), Some(g)) if !f.is_empty() && f.len() == g.len() => (f, g),
        (f, g) => {
            let len =
                |a: Option<&Vec<Value>>| a.map_or("missing".to_string(), |a| a.len().to_string());
            return Err(format!(
                "calibrate_mic_curve set: missing/mismatched freqs_hz/gain_db                  (freqs_hz: {}, gain_db: {})",
                len(f),
                len(g)
            ));
        }
    };
    let mut freqs_hz = Vec::with_capacity(freqs.len());
    let mut gain_db = Vec::with_capacity(gains.len());
    for (i, (fv, gv)) in freqs.iter().zip(gains).enumerate() {
        let f_path = format!("freqs_hz[{i}]");
        let g_path = format!("gain_db[{i}]");
        let f = match wire::finite_f32(fv, &f_path) {
            Ok(f) => f,
            Err(e) => {
                let paired = wire::finite_f32(gv, &g_path)
                    .ok()
                    .map(|g| format!("{g_path} = {g:.3} dB"));
                return Err(refuse(e, paired));
            }
        };
        let g = wire::finite_f32(gv, &g_path)
            .map_err(|e| refuse(e, Some(format!("{f_path} = {f:.3} Hz"))))?;
        freqs_hz.push(f);
        gain_db.push(g);
    }
    Ok((freqs_hz, gain_db))
}

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
    let op = cmd.get("op").and_then(Value::as_str).unwrap_or("set");
    // Before `load_or_new`: a malformed channel must not key (or create)
    // any entry.
    let (out_ch, in_ch) = match channels_from(cmd, &cfg) {
        Ok(pair) => pair,
        Err(e) => {
            let headline = if op == "clear" {
                "mic curve not cleared"
            } else {
                "mic curve not saved"
            };
            return json!({"ok": false, "error": e.refusal(headline, &[CURVE_UNCHANGED])});
        }
    };

    let mut cal = Calibration::load_or_new(out_ch, in_ch, None);
    match op {
        "clear" => {
            cal.mic_response = None;
        }
        "set" => {
            let (freqs_hz, gain_db) = match parse_curve(cmd) {
                Ok(curve) => curve,
                Err(e) => return json!({"ok": false, "error": e}),
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
