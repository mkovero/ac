//! Persistent UI preferences — `~/.config/ac/ui.json`. Phase 6 of
//! `unified.md`. Survives restarts so the user doesn't have to re-cycle
//! W to their preferred view, re-tune `,`/`.` intensity, etc. on every
//! launch.
//!
//! v1 scope: only the user-tunable knobs that change per-session
//! (active view, ember intensity scale, ember τ_p scale, goniometer
//! M/S↔raw rotation). Per-cell dB windows and freq zooms are NOT in
//! v1 — they get tweaked dozens of times per session in normal use,
//! and persisting them means a stale window from yesterday clamps
//! today's measurements off-screen until the user resets. Worth
//! revisiting once we have actual user feedback.
//!
//! Schema is JSON with explicit `schema_version` so a future field
//! addition or rename can detect old configs and migrate / fall back
//! cleanly. Missing file, parse error, and unknown schema version all
//! fall back to defaults silently (warned via `log::warn` so a
//! corrupt-config session still shows up in the daemon log).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::types::ViewMode;

/// Bump when adding/removing fields in a way that needs migration.
/// Same convention as `ac-core::config::Config::schema_version`.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiState {
    pub schema_version:        u32,
    /// View token (string form of `ViewMode`). String rather than the
    /// `ViewMode` enum so renaming a variant doesn't bork old configs
    /// — unknown tokens fall back to `Spectrum`.
    #[serde(default)]
    pub view_mode:             Option<String>,
    /// Ember substrate global intensity multiplier. Default 1.0
    /// matches App::new's value; user adjusts via `,` / `.`.
    #[serde(default = "default_intensity")]
    pub ember_intensity_scale: f32,
    /// Ember substrate global τ_p multiplier. Default 1.0; user
    /// adjusts via `Shift+,` / `Shift+.`.
    #[serde(default = "default_tau_p")]
    pub ember_tau_p_scale:     f32,
    /// Goniometer M/S vs raw L/R toggle. Default true (M/S — analog
    /// meter convention); user toggles via `R` while in Goniometer.
    #[serde(default = "default_gonio_rotation")]
    pub ember_gonio_rotation_ms: bool,
    /// Window fullscreen state. Captured from `winit::Window::fullscreen()`
    /// at save time; applied via `set_fullscreen` after the window
    /// is created. Default false — first-launch users see a windowed
    /// instance.
    #[serde(default)]
    pub fullscreen: bool,
}

fn default_intensity() -> f32 { 1.0 }
fn default_tau_p() -> f32 { 1.0 }
fn default_gonio_rotation() -> bool { true }

impl Default for UiState {
    fn default() -> Self {
        Self {
            schema_version:          SCHEMA_VERSION,
            view_mode:               None,
            ember_intensity_scale:   default_intensity(),
            ember_tau_p_scale:       default_tau_p(),
            ember_gonio_rotation_ms: default_gonio_rotation(),
            fullscreen:              false,
        }
    }
}

/// `~/.config/ac/ui.json`. Co-located with `config.json` (used by
/// `ac-core::config`) and `cal.json` (used by
/// `ac-core::shared::calibration`) — same convention.
pub fn default_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".config").join("ac").join("ui.json")
}

/// Load from disk. Returns `Default` on:
///   - missing file (first launch)
///   - parse error (corrupt file)
///   - schema_version mismatch (forwards-incompatible config)
///
/// Doesn't propagate errors — UI persistence is best-effort. A
/// `log::warn` makes the failure visible in the log, but the user
/// gets a working session either way.
pub fn load() -> UiState {
    load_from(&default_path())
}

pub fn load_from(path: &std::path::Path) -> UiState {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return UiState::default();
        }
        Err(e) => {
            log::warn!("ui.json load: {e} (using defaults)");
            return UiState::default();
        }
    };
    let parsed: UiState = match serde_json::from_slice(&bytes) {
        Ok(s) => s,
        Err(e) => {
            log::warn!("ui.json parse: {e} (using defaults)");
            return UiState::default();
        }
    };
    if parsed.schema_version != SCHEMA_VERSION {
        log::warn!(
            "ui.json schema_version {} doesn't match expected {} (using defaults)",
            parsed.schema_version, SCHEMA_VERSION,
        );
        return UiState::default();
    }
    parsed
}

/// Write to disk. Best-effort — logs a warning on failure but doesn't
/// propagate. Creates parent directory if needed (first launch may not
/// have `~/.config/ac/`).
pub fn save(state: &UiState) {
    save_to(&default_path(), state);
}

pub fn save_to(path: &std::path::Path, state: &UiState) {
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            log::warn!("ui.json mkdir {}: {e}", parent.display());
            return;
        }
    }
    let bytes = match serde_json::to_vec_pretty(state) {
        Ok(b) => b,
        Err(e) => {
            log::warn!("ui.json serialize: {e}");
            return;
        }
    };
    if let Err(e) = std::fs::write(path, bytes) {
        log::warn!("ui.json write {}: {e}", path.display());
    }
}

/// Convert a `ViewMode` to its persisted string token. Stable across
/// renames as long as the *string* values stay consistent (they're
/// part of the persistence contract, not just the type's debug repr).
pub fn view_mode_token(view: ViewMode) -> &'static str {
    match view {
        ViewMode::Spectrum      => "spectrum",
        ViewMode::Waterfall     => "waterfall",
        ViewMode::Scope         => "scope",
        ViewMode::SpectrumEmber => "spectrum_ember",
        ViewMode::Goniometer    => "goniometer",
        ViewMode::IoTransfer    => "iotransfer",
        ViewMode::BodeMag       => "bode_mag",
        ViewMode::Coherence     => "coherence",
        ViewMode::BodePhase     => "bode_phase",
        ViewMode::GroupDelay    => "group_delay",
        ViewMode::Nyquist       => "nyquist",
        ViewMode::Ir            => "ir",
    }
}

/// Inverse of [`view_mode_token`]. Unknown tokens (config from a
/// future version, typo, removed view) fall back to `None` — caller
/// substitutes whatever default is appropriate (typically the value
/// passed via `--view` or `Spectrum`).
pub fn view_mode_from_token(token: &str) -> Option<ViewMode> {
    Some(match token {
        "spectrum"       => ViewMode::Spectrum,
        "waterfall"      => ViewMode::Waterfall,
        "scope"          => ViewMode::Scope,
        "spectrum_ember" => ViewMode::SpectrumEmber,
        "goniometer"     => ViewMode::Goniometer,
        "iotransfer"     => ViewMode::IoTransfer,
        "bode_mag"       => ViewMode::BodeMag,
        "coherence"      => ViewMode::Coherence,
        "bode_phase"     => ViewMode::BodePhase,
        "group_delay"    => ViewMode::GroupDelay,
        "nyquist"        => ViewMode::Nyquist,
        "ir"             => ViewMode::Ir,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_serializable_and_round_trips() {
        let s = UiState::default();
        let json = serde_json::to_string(&s).expect("serialize");
        let parsed: UiState = serde_json::from_str(&json).expect("parse");
        assert_eq!(parsed.schema_version, SCHEMA_VERSION);
        assert_eq!(parsed.ember_intensity_scale, 1.0);
        assert_eq!(parsed.ember_tau_p_scale, 1.0);
        assert!(parsed.ember_gonio_rotation_ms);
    }

    #[test]
    fn missing_file_yields_default() {
        let path = std::env::temp_dir().join("ac-ui-persist-missing.json");
        let _ = std::fs::remove_file(&path);
        let s = load_from(&path);
        assert_eq!(s.schema_version, SCHEMA_VERSION);
        assert_eq!(s.view_mode, None);
    }

    #[test]
    fn corrupt_file_yields_default() {
        let path = std::env::temp_dir().join("ac-ui-persist-corrupt.json");
        std::fs::write(&path, b"this is not json {{{").unwrap();
        let s = load_from(&path);
        assert_eq!(s.schema_version, SCHEMA_VERSION);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn schema_version_mismatch_yields_default() {
        let path = std::env::temp_dir().join("ac-ui-persist-mismatch.json");
        std::fs::write(
            &path,
            br#"{"schema_version":999,"ember_intensity_scale":7.5,
                "ember_tau_p_scale":2.0,"ember_gonio_rotation_ms":false}"#,
        )
        .unwrap();
        let s = load_from(&path);
        assert_eq!(s.schema_version, SCHEMA_VERSION);
        // Mismatch → fields fall back to defaults, NOT the old values.
        assert_eq!(s.ember_intensity_scale, 1.0);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn save_then_load_round_trip() {
        let path = std::env::temp_dir().join("ac-ui-persist-roundtrip.json");
        let _ = std::fs::remove_file(&path);
        let mut s = UiState::default();
        s.view_mode = Some("nyquist".into());
        s.ember_intensity_scale = 2.5;
        s.ember_tau_p_scale = 0.4;
        s.ember_gonio_rotation_ms = false;
        s.fullscreen = true;
        save_to(&path, &s);
        let loaded = load_from(&path);
        assert_eq!(loaded.view_mode.as_deref(), Some("nyquist"));
        assert_eq!(loaded.ember_intensity_scale, 2.5);
        assert_eq!(loaded.ember_tau_p_scale, 0.4);
        assert!(!loaded.ember_gonio_rotation_ms);
        assert!(loaded.fullscreen);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn view_mode_token_round_trips_every_variant() {
        // Locked-in contract: every ViewMode must round-trip through
        // the persistence string. New ViewMode variants must be added
        // to both view_mode_token and view_mode_from_token to pass.
        let cases = [
            ViewMode::Spectrum, ViewMode::Waterfall, ViewMode::Scope,
            ViewMode::SpectrumEmber, ViewMode::Goniometer,
            ViewMode::IoTransfer, ViewMode::BodeMag, ViewMode::Coherence,
            ViewMode::BodePhase, ViewMode::GroupDelay, ViewMode::Nyquist,
            ViewMode::Ir,
        ];
        for v in cases {
            let tok = view_mode_token(v);
            let back = view_mode_from_token(tok).expect(tok);
            assert_eq!(back, v, "round-trip failed for {v:?} via {tok:?}");
        }
    }

    #[test]
    fn unknown_view_token_returns_none() {
        assert_eq!(view_mode_from_token("polezero"), None);
        assert_eq!(view_mode_from_token(""), None);
    }
}
