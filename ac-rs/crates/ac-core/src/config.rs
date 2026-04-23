//! Persistent hardware configuration — mirrors `ac/config.py`.
//!
//! Reads/writes `~/.config/ac/config.json`.  Missing keys are filled from
//! [`Config::default`] so new fields never break old config files.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::shared::constants::DBU_REF_EXACT;

fn default_dbu_ref() -> f64 { DBU_REF_EXACT }
fn default_range_start() -> f64 { 20.0 }
fn default_range_stop() -> f64 { 20_000.0 }

/// Complete hardware configuration.  All fields match the Python DEFAULTS dict.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub device: u32,

    #[serde(default)]
    pub output_channel: u32,

    #[serde(default)]
    pub input_channel: u32,

    /// Sticky JACK port name for output, e.g. `"Fireface400 (123):AN1"`.
    pub output_port: Option<String>,

    /// Sticky JACK port name for input.
    pub input_port: Option<String>,

    /// Capture port index for H1 transfer function reference channel.
    pub reference_channel: Option<u32>,

    /// Sticky JACK port name for reference channel.
    pub reference_port: Option<String>,

    #[serde(default = "default_dbu_ref")]
    pub dbu_ref_vrms: f64,

    pub dmm_host: Option<String>,

    #[serde(default = "default_range_start")]
    pub range_start_hz: f64,

    #[serde(default = "default_range_stop")]
    pub range_stop_hz: f64,

    #[serde(default)]
    pub server_enabled: bool,

    pub gpio_port: Option<String>,

    /// Active session name (read by `ds`).
    pub session: Option<String>,

    /// Force audio backend: `"jack"`, `"sounddevice"`, or `None` for auto.
    pub backend: Option<String>,

    /// Remote server host for CLI connections. `None` means localhost.
    pub server_host: Option<String>,

    /// Directory where `MeasurementReport` JSON files are written when
    /// reproducible measurements complete. `None` disables disk emission.
    #[serde(default)]
    pub report_dir: Option<PathBuf>,

    /// Auto-disable `server_enable` (public bind) after this many seconds
    /// of idle CTRL activity. `None` = never auto-disable. Checked by the
    /// daemon's keepalive tick; only fires when no workers are running.
    #[serde(default)]
    pub server_idle_timeout_secs: Option<u64>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            device: 0,
            output_channel: 0,
            input_channel: 0,
            output_port: None,
            input_port: None,
            reference_channel: None,
            reference_port: None,
            dbu_ref_vrms: DBU_REF_EXACT,
            dmm_host: None,
            range_start_hz: 20.0,
            range_stop_hz: 20_000.0,
            server_enabled: false,
            gpio_port: None,
            session: None,
            backend: None,
            server_host: None,
            report_dir: None,
            server_idle_timeout_secs: None,
        }
    }
}

/// Return the default config file path: `~/.config/ac/config.json`.
pub fn default_config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".config").join("ac").join("config.json")
}

/// Load config from disk, merging with defaults for any missing keys.
/// Returns [`Config::default`] silently if the file does not exist.
pub fn load(path: Option<&Path>) -> Result<Config> {
    let path = path.map(|p| p.to_path_buf()).unwrap_or_else(default_config_path);
    if !path.exists() {
        return Ok(Config::default());
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;
    // serde fills missing fields from defaults; extra fields are ignored.
    let cfg: Config = serde_json::from_str(&raw)
        .with_context(|| format!("parsing {}", path.display()))?;
    Ok(cfg)
}

/// Merge `updates` into the on-disk config and write back.
/// Returns the merged config.
pub fn save(updates: &Config, path: Option<&Path>) -> Result<Config> {
    let path = path.map(|p| p.to_path_buf()).unwrap_or_else(default_config_path);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating dir {}", dir.display()))?;
    }
    // Merge: start from existing, apply updates field by field via JSON patch.
    let existing = load(Some(&path)).unwrap_or_default();
    // Serialize both to Value, merge, then deserialise back.
    let mut merged = serde_json::to_value(&existing)?;
    let patch = serde_json::to_value(updates)?;
    if let (Some(m), Some(p)) = (merged.as_object_mut(), patch.as_object()) {
        for (k, v) in p {
            m.insert(k.clone(), v.clone());
        }
    }
    let final_cfg: Config = serde_json::from_value(merged)?;
    let out = serde_json::to_string_pretty(&final_cfg)?;
    std::fs::write(&path, out)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(final_cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_round_trip() {
        let cfg = Config::default();
        let json = serde_json::to_string(&cfg).unwrap();
        let parsed: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.output_channel, cfg.output_channel);
        assert!((parsed.dbu_ref_vrms - cfg.dbu_ref_vrms).abs() < 1e-10);
    }

    #[test]
    fn missing_keys_use_defaults() {
        let json = r#"{"device": 2}"#;
        let cfg: Config = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.device, 2);
        assert_eq!(cfg.output_channel, 0);
        assert!((cfg.range_stop_hz - 20_000.0).abs() < 1e-9);
    }
}
