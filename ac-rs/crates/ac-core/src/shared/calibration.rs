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
    /// Captured input level (dBFS) when a 94 dB SPL pistonphone reference
    /// is applied to this channel. With this value, any other dBFS reading
    /// converts to dB SPL via `dbspl = dbfs - mic_sens_dbfs + 94.0`.
    /// `None` until the SPL calibration step has been run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mic_sensitivity_dbfs_at_94db_spl: Option<f64>,
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
    pub mic_sensitivity_dbfs_at_94db_spl: Option<f64>,
}

/// Reference SPL of an acoustic pistonphone calibrator. ANSI S1.40 / IEC
/// 60942 Class 1 calibrators emit either 94 dB SPL or 114 dB SPL — we
/// hard-code 94 because that's the universally-supported value and lets
/// `dbfs_to_dbspl` stay parameterless.
pub const PISTONPHONE_REF_SPL: f64 = 94.0;

impl Calibration {
    pub fn new(output_channel: u32, input_channel: u32) -> Self {
        Self {
            output_channel,
            input_channel,
            ref_freq: 1000.0,
            vrms_at_0dbfs_out: None,
            vrms_at_0dbfs_in: None,
            ref_dbfs: -10.0,
            mic_sensitivity_dbfs_at_94db_spl: None,
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

    /// True when this channel has an SPL reference recorded.
    pub fn spl_calibrated(&self) -> bool {
        self.mic_sensitivity_dbfs_at_94db_spl.is_some()
    }

    /// Convert a dBFS reading to dB SPL using the pistonphone reference.
    /// Returns `None` when SPL calibration is unset.
    pub fn dbfs_to_dbspl(&self, dbfs: f64) -> Option<f64> {
        self.mic_sensitivity_dbfs_at_94db_spl
            .map(|m| dbfs - m + PISTONPHONE_REF_SPL)
    }

    /// Additive offset that converts dBFS → dB SPL (so `dbspl = dbfs +
    /// spl_offset_db()`). Returned for transport in wire frames; the UI
    /// applies it to whichever readout it's rendering. `None` when SPL
    /// calibration is unset.
    pub fn spl_offset_db(&self) -> Option<f64> {
        self.mic_sensitivity_dbfs_at_94db_spl
            .map(|m| PISTONPHONE_REF_SPL - m)
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
            mic_sensitivity_dbfs_at_94db_spl: self.mic_sensitivity_dbfs_at_94db_spl,
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
            output_channel:                   e.output_channel,
            input_channel:                    e.input_channel,
            ref_freq:                         e.ref_freq,
            vrms_at_0dbfs_out:                e.vrms_at_0dbfs_out,
            vrms_at_0dbfs_in:                 e.vrms_at_0dbfs_in,
            ref_dbfs:                         e.ref_dbfs,
            mic_sensitivity_dbfs_at_94db_spl: e.mic_sensitivity_dbfs_at_94db_spl,
        }
    }

    /// Load the existing calibration entry for a channel pair, or return a
    /// fresh one with defaults. Used by partial-update handlers (voltage
    /// cal + SPL cal write to disjoint fields and must not clobber each
    /// other's prior values).
    pub fn load_or_new(
        output_channel: u32,
        input_channel: u32,
        path: Option<&Path>,
    ) -> Self {
        Self::load(output_channel, input_channel, path)
            .ok()
            .flatten()
            .unwrap_or_else(|| Self::new(output_channel, input_channel))
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

    #[test]
    fn dbfs_to_dbspl_round_trip() {
        // Pistonphone applied at 94 dB SPL captured -32 dBFS → mic
        // sensitivity is -32 dBFS @ 94 dB SPL. Re-applying the same dBFS
        // input must read 94 dB SPL.
        let mut cal = Calibration::new(0, 0);
        cal.mic_sensitivity_dbfs_at_94db_spl = Some(-32.0);
        let dbspl = cal.dbfs_to_dbspl(-32.0).unwrap();
        assert!((dbspl - 94.0).abs() < 0.5, "round-trip got {dbspl}, expected 94");

        // Linear: every 1 dB louder dBFS → 1 dB louder SPL.
        let dbspl_quieter = cal.dbfs_to_dbspl(-50.0).unwrap();
        assert!((dbspl_quieter - 76.0).abs() < 1e-9);
        let dbspl_louder = cal.dbfs_to_dbspl(-10.0).unwrap();
        assert!((dbspl_louder - 116.0).abs() < 1e-9);
    }

    #[test]
    fn spl_offset_db_matches_dbfs_to_dbspl() {
        let mut cal = Calibration::new(0, 0);
        cal.mic_sensitivity_dbfs_at_94db_spl = Some(-28.5);
        let off = cal.spl_offset_db().unwrap();
        for dbfs in &[-80.0, -45.5, -10.0, 0.0] {
            let direct = cal.dbfs_to_dbspl(*dbfs).unwrap();
            let via_off = dbfs + off;
            assert!((direct - via_off).abs() < 1e-12);
        }
    }

    #[test]
    fn spl_calibrated_predicate() {
        let mut cal = Calibration::new(0, 0);
        assert!(!cal.spl_calibrated());
        cal.mic_sensitivity_dbfs_at_94db_spl = Some(-30.0);
        assert!(cal.spl_calibrated());
    }

    #[test]
    fn spl_field_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cal.json");

        let mut cal = Calibration::new(2, 3);
        cal.vrms_at_0dbfs_out = Some(1.0);
        cal.vrms_at_0dbfs_in  = Some(0.5);
        cal.mic_sensitivity_dbfs_at_94db_spl = Some(-31.7);
        cal.save(Some(&path)).unwrap();

        let loaded = Calibration::load(2, 3, Some(&path)).unwrap().unwrap();
        assert_eq!(loaded.mic_sensitivity_dbfs_at_94db_spl, Some(-31.7));
    }

    #[test]
    fn voltage_save_preserves_existing_spl() {
        // Workflow: user runs SPL cal first (sets only the SPL field), then
        // later runs voltage cal. The voltage handler uses load_or_new and
        // mutates only the voltage fields, so the SPL value must survive.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cal.json");

        let mut spl_cal = Calibration::new(0, 1);
        spl_cal.mic_sensitivity_dbfs_at_94db_spl = Some(-29.4);
        spl_cal.save(Some(&path)).unwrap();

        // Voltage-cal handler simulation — load existing, set voltage,
        // save; SPL field stays.
        let mut cal = Calibration::load_or_new(0, 1, Some(&path));
        cal.vrms_at_0dbfs_out = Some(1.234);
        cal.vrms_at_0dbfs_in  = Some(0.567);
        cal.save(Some(&path)).unwrap();

        let loaded = Calibration::load(0, 1, Some(&path)).unwrap().unwrap();
        assert_eq!(loaded.mic_sensitivity_dbfs_at_94db_spl, Some(-29.4));
        assert_eq!(loaded.vrms_at_0dbfs_out,                Some(1.234));
        assert_eq!(loaded.vrms_at_0dbfs_in,                 Some(0.567));
    }
}
