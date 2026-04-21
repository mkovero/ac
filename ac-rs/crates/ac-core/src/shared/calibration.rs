//! Calibration data — mirrors `ac/server/jack_calibration.py`.
//!
//! Reads and writes `~/.config/ac/cal.json`.  Key format: `out{N}_in{M}`.
//! The file is a flat JSON object; each value is a [`CalibrationEntry`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::shared::conversions::{dbfs_to_vrms, vrms_to_dbu, fmt_vrms, fmt_vpp};

/// Raw JSON representation stored in `cal.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationEntry {
    pub output_channel: u32,
    pub input_channel: u32,
    #[serde(default = "default_ref_freq")]
    pub ref_freq: f64,
    pub vrms_at_0dbfs_out: Option<f64>,
    pub vrms_at_0dbfs_in: Option<f64>,
    #[serde(default = "default_ref_dbfs")]
    pub ref_dbfs: f64,
}

fn default_ref_freq() -> f64 { 1000.0 }
fn default_ref_dbfs() -> f64 { -10.0 }

/// High-level calibration object with computed helpers.
#[derive(Debug, Clone)]
pub struct Calibration {
    pub output_channel: u32,
    pub input_channel: u32,
    pub ref_freq: f64,
    pub vrms_at_0dbfs_out: Option<f64>,
    pub vrms_at_0dbfs_in: Option<f64>,
    pub ref_dbfs: f64,
}

impl Calibration {
    pub fn new(output_channel: u32, input_channel: u32) -> Self {
        Self {
            output_channel,
            input_channel,
            ref_freq: 1000.0,
            vrms_at_0dbfs_out: None,
            vrms_at_0dbfs_in: None,
            ref_dbfs: -10.0,
        }
    }

    /// File key: `out{N}_in{M}`.
    pub fn key(&self) -> String {
        format!("out{}_in{}", self.output_channel, self.input_channel)
    }

    pub fn output_ok(&self) -> bool { self.vrms_at_0dbfs_out.is_some() }
    pub fn input_ok(&self)  -> bool { self.vrms_at_0dbfs_in.is_some() }

    /// Convert a dBFS output level to physical Vrms using calibration.
    pub fn out_vrms(&self, dbfs: f64) -> Option<f64> {
        self.vrms_at_0dbfs_out.map(|v| dbfs_to_vrms(dbfs, v))
    }

    /// Convert a captured linear RMS (0–1 dBFS scale) to physical Vrms.
    pub fn in_vrms(&self, linear_rms: f64) -> Option<f64> {
        self.vrms_at_0dbfs_in.map(|v| linear_rms * v)
    }

    // -----------------------------------------------------------------------
    // Persistence
    // -----------------------------------------------------------------------

    /// Persist this calibration entry into the shared cal.json file.
    /// Existing entries for other channel pairs are preserved.
    pub fn save(&self, path: Option<&Path>) -> Result<()> {
        let path = path.map(|p| p.to_path_buf()).unwrap_or_else(default_cal_path);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("creating {}", dir.display()))?;
        }

        let mut all: HashMap<String, CalibrationEntry> = if path.exists() {
            let raw = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            serde_json::from_str(&raw).unwrap_or_default()
        } else {
            HashMap::new()
        };

        all.insert(self.key(), CalibrationEntry {
            output_channel: self.output_channel,
            input_channel:  self.input_channel,
            ref_freq:        self.ref_freq,
            vrms_at_0dbfs_out: self.vrms_at_0dbfs_out,
            vrms_at_0dbfs_in:  self.vrms_at_0dbfs_in,
            ref_dbfs:        self.ref_dbfs,
        });

        let out = serde_json::to_string_pretty(&all)?;
        std::fs::write(&path, out)
            .with_context(|| format!("writing {}", path.display()))?;
        eprintln!("  Calibration saved -> {}  (key: {})", path.display(), self.key());
        Ok(())
    }

    /// Load calibration for a specific output/input channel pair.
    /// Returns `Ok(None)` if the file or key doesn't exist.
    pub fn load(
        output_channel: u32,
        input_channel: u32,
        path: Option<&Path>,
    ) -> Result<Option<Self>> {
        let path = path.map(|p| p.to_path_buf()).unwrap_or_else(default_cal_path);
        let all = read_all_entries(&path)?;
        let key = format!("out{}_in{}", output_channel, input_channel);
        Ok(all.get(&key).map(Calibration::from_entry))
    }

    /// Load the first calibration matching `output_channel`, any input.
    pub fn load_output_only(
        output_channel: u32,
        path: Option<&Path>,
    ) -> Result<Option<Self>> {
        let path = path.map(|p| p.to_path_buf()).unwrap_or_else(default_cal_path);
        let all = read_all_entries(&path)?;
        let prefix = format!("out{}_in", output_channel);
        Ok(all
            .values()
            .find(|e| {
                format!("out{}_in{}", e.output_channel, e.input_channel)
                    .starts_with(&prefix)
            })
            .map(Calibration::from_entry))
    }

    /// Load all stored calibration entries.
    pub fn load_all(path: Option<&Path>) -> Result<Vec<Self>> {
        let path = path.map(|p| p.to_path_buf()).unwrap_or_else(default_cal_path);
        let all = read_all_entries(&path)?;
        Ok(all.values().map(Calibration::from_entry).collect())
    }

    /// Print a human-readable calibration summary to stderr.
    pub fn summary(&self) {
        eprintln!("\n  -- Calibration  [{}] ----------------------------------", self.key());
        if let Some(v) = self.vrms_at_0dbfs_out {
            eprintln!("  Output: 0 dBFS = {}  =  {:+.2} dBu  =  {}",
                fmt_vrms(v), vrms_to_dbu(v), fmt_vpp(v));
        } else {
            eprintln!("  Output: not calibrated");
        }
        if let Some(v) = self.vrms_at_0dbfs_in {
            eprintln!("  Input:  0 dBFS = {}  =  {:+.2} dBu  =  {}",
                fmt_vrms(v), vrms_to_dbu(v), fmt_vpp(v));
        } else {
            eprintln!("  Input:  not calibrated");
        }
        eprintln!("  --------------------------------------------------------------\n");
    }

    fn from_entry(e: &CalibrationEntry) -> Self {
        Self {
            output_channel:    e.output_channel,
            input_channel:     e.input_channel,
            ref_freq:          e.ref_freq,
            vrms_at_0dbfs_out: e.vrms_at_0dbfs_out,
            vrms_at_0dbfs_in:  e.vrms_at_0dbfs_in,
            ref_dbfs:          e.ref_dbfs,
        }
    }
}

// -----------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------

/// Default calibration file path: `~/.config/ac/cal.json`.
pub fn default_cal_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".config").join("ac").join("cal.json")
}

fn read_all_entries(path: &Path) -> Result<HashMap<String, CalibrationEntry>> {
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    // Silently ignore malformed files — return empty map.
    Ok(serde_json::from_str(&raw).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_and_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cal.json");

        let mut cal = Calibration::new(0, 0);
        cal.vrms_at_0dbfs_out = Some(1.234);
        cal.vrms_at_0dbfs_in  = Some(0.567);
        cal.save(Some(&path)).unwrap();

        let loaded = Calibration::load(0, 0, Some(&path)).unwrap().unwrap();
        assert!((loaded.vrms_at_0dbfs_out.unwrap() - 1.234).abs() < 1e-10);
        assert!((loaded.vrms_at_0dbfs_in.unwrap()  - 0.567).abs() < 1e-10);
    }

    #[test]
    fn missing_key_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cal.json");
        // Write a different key
        let mut cal = Calibration::new(1, 0);
        cal.vrms_at_0dbfs_out = Some(1.0);
        cal.save(Some(&path)).unwrap();

        let result = Calibration::load(0, 0, Some(&path)).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn load_all_returns_all_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cal.json");

        for (out_ch, in_ch) in [(0u32, 0u32), (0, 1), (1, 0)] {
            let mut cal = Calibration::new(out_ch, in_ch);
            cal.vrms_at_0dbfs_out = Some(1.0);
            cal.save(Some(&path)).unwrap();
        }

        let all = Calibration::load_all(Some(&path)).unwrap();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn out_vrms_computes_correctly() {
        let mut cal = Calibration::new(0, 0);
        cal.vrms_at_0dbfs_out = Some(1.0);
        // 0 dBFS → 1.0 Vrms
        assert!((cal.out_vrms(0.0).unwrap() - 1.0).abs() < 1e-12);
        // -20 dBFS → 0.1 Vrms
        assert!((cal.out_vrms(-20.0).unwrap() - 0.1).abs() < 1e-10);
    }
}
