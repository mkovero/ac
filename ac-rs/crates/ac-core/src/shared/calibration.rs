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
    /// Mic frequency-response correction curve, imported from a
    /// manufacturer .frd / .txt file. Stored inline so cal.json stays
    /// self-contained — moving cal.json between machines / sessions
    /// doesn't strand the cal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mic_response: Option<MicResponse>,
}

/// Parsed and validated mic frequency-response correction curve.
///
/// `freqs_hz[i]` is monotonically increasing (asserted on import). At any
/// reading frequency `f`, the mic over-reads the true level by
/// `correction_at(f)` dB, so consumers SUBTRACT this from the captured
/// magnitude to recover the truth: `corrected_dbfs = raw_dbfs - correction`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MicResponse {
    pub freqs_hz:    Vec<f32>,
    pub gain_db:     Vec<f32>,
    /// Original .frd / .txt path the curve was imported from. Informational
    /// only — never re-read at runtime; the curve data above is the source
    /// of truth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    /// RFC3339 timestamp of the import. Lets the user tell at a glance
    /// when a curve was attached without diffing cal.json.
    pub imported_at: String,
}

impl MicResponse {
    /// Hard upper bound on point count — typical mic .frd files have
    /// 100–500 points (1/24 to 1/48 octave); 4096 is generous and keeps
    /// cal.json under ~50 KB per channel.
    pub const MAX_POINTS: usize = 4096;
    /// Hard lower bound — fewer than 16 points produces an
    /// uncomfortably coarse log-linear interpolation across the audio band.
    pub const MIN_POINTS: usize = 16;

    /// Linear interpolation of `gain_db` in log-frequency space. Frequencies
    /// outside the curve's range clamp to the nearest endpoint (constant
    /// extrapolation — better than zero-extrapolation for room-acoustic work
    /// where the curve usually defines just the audio band).
    pub fn correction_at(&self, freq_hz: f32) -> f32 {
        if self.freqs_hz.is_empty() {
            return 0.0;
        }
        if !freq_hz.is_finite() || freq_hz <= 0.0 {
            return self.gain_db[0];
        }
        if freq_hz <= self.freqs_hz[0] {
            return self.gain_db[0];
        }
        let last = self.freqs_hz.len() - 1;
        if freq_hz >= self.freqs_hz[last] {
            return self.gain_db[last];
        }
        // Binary search for the bracketing pair.
        let i = self.freqs_hz.partition_point(|&f| f <= freq_hz).saturating_sub(1);
        let f_lo = self.freqs_hz[i];
        let f_hi = self.freqs_hz[i + 1];
        let g_lo = self.gain_db[i];
        let g_hi = self.gain_db[i + 1];
        let log_lo = f_lo.ln();
        let log_hi = f_hi.ln();
        let log_f  = freq_hz.ln();
        let t = ((log_f - log_lo) / (log_hi - log_lo)).clamp(0.0, 1.0);
        g_lo + (g_hi - g_lo) * t
    }
}

/// Parse the two-column ASCII format used by Behringer / Dayton / miniDSP
/// mic calibration files. One `<freq_hz> <gain_db>` pair per line, optional
/// whitespace, comments starting with `*` or `#` are ignored. An optional
/// third column (phase) is ignored. Validates monotonically increasing
/// frequencies, finite values, and the [`MicResponse::MIN_POINTS`] /
/// [`MicResponse::MAX_POINTS`] bounds.
pub fn parse_mic_curve(text: &str, source_path: Option<String>) -> Result<MicResponse> {
    let mut freqs = Vec::new();
    let mut gains = Vec::new();
    for (line_no, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() { continue; }
        if line.starts_with('*') || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let mut cols = line.split_whitespace();
        let f_tok = cols.next();
        let g_tok = cols.next();
        let (f_str, g_str) = match (f_tok, g_tok) {
            (Some(f), Some(g)) => (f, g),
            _ => anyhow::bail!("line {}: expected `freq_hz gain_db [phase]`, got {raw:?}", line_no + 1),
        };
        let f: f32 = f_str.parse().map_err(|e| anyhow::anyhow!(
            "line {}: failed to parse freq {f_str:?}: {e}",
            line_no + 1
        ))?;
        let g: f32 = g_str.parse().map_err(|e| anyhow::anyhow!(
            "line {}: failed to parse gain {g_str:?}: {e}",
            line_no + 1
        ))?;
        if !f.is_finite() || f <= 0.0 {
            anyhow::bail!("line {}: freq must be > 0 Hz, got {f}", line_no + 1);
        }
        if !g.is_finite() {
            anyhow::bail!("line {}: gain must be finite, got {g}", line_no + 1);
        }
        if let Some(&prev) = freqs.last() {
            if f <= prev {
                anyhow::bail!(
                    "line {}: frequencies must increase strictly (got {f} after {prev})",
                    line_no + 1
                );
            }
        }
        freqs.push(f);
        gains.push(g);
    }
    if freqs.len() < MicResponse::MIN_POINTS {
        anyhow::bail!(
            "mic curve too sparse: got {} points, need ≥ {}",
            freqs.len(),
            MicResponse::MIN_POINTS
        );
    }
    if freqs.len() > MicResponse::MAX_POINTS {
        anyhow::bail!(
            "mic curve too dense: got {} points, max {}",
            freqs.len(),
            MicResponse::MAX_POINTS
        );
    }
    Ok(MicResponse {
        freqs_hz:    freqs,
        gain_db:     gains,
        source_path,
        imported_at: chrono::Utc::now().to_rfc3339(),
    })
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
    pub mic_response: Option<MicResponse>,
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
            mic_response: None,
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

    /// Mic frequency-response correction at `freq_hz`, in dB. The mic
    /// over-reads by this much; subtract from a captured magnitude to
    /// recover truth. `None` when no curve is loaded.
    pub fn mic_correction_at(&self, freq_hz: f32) -> Option<f32> {
        self.mic_response.as_ref().map(|r| r.correction_at(freq_hz))
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
            mic_response:    self.mic_response.clone(),
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
            mic_response:                     e.mic_response.clone(),
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

    // ─── mic_curve parser ───────────────────────────────────────────────

    fn dummy_curve_text(n: usize) -> String {
        // Geometric sweep from 20 Hz to 20 kHz, gain rising linearly with
        // log-frequency from 0 to 4 dB. Useful as a "known curve".
        let mut s = String::from("* a comment\n");
        let log_min = 20.0_f32.ln();
        let log_max = 20_000.0_f32.ln();
        for i in 0..n {
            let t = i as f32 / (n - 1) as f32;
            let f = (log_min + t * (log_max - log_min)).exp();
            let g = 4.0 * t;
            s.push_str(&format!("{f}\t{g}\n"));
        }
        s
    }

    #[test]
    fn parse_mic_curve_round_trip() {
        let text = dummy_curve_text(64);
        let r = parse_mic_curve(&text, Some("/tmp/test.frd".into())).unwrap();
        assert_eq!(r.freqs_hz.len(), 64);
        assert_eq!(r.gain_db.len(), 64);
        assert!((r.freqs_hz[0] - 20.0).abs() < 0.01);
        assert!((r.gain_db[0]).abs() < 0.01);
        assert!((r.freqs_hz.last().unwrap() - 20_000.0).abs() < 1.0);
        assert!((r.gain_db.last().unwrap() - 4.0).abs() < 0.01);
        assert_eq!(r.source_path.as_deref(), Some("/tmp/test.frd"));
    }

    #[test]
    fn parse_mic_curve_skips_comments() {
        let text = "# header\n* freq gain\n100 0.5\n200 0.8\n300 1.1\n400 1.4\n500 1.7\n\
                    600 2.0\n700 2.3\n800 2.6\n900 2.9\n1000 3.2\n1100 3.5\n1200 3.8\n\
                    1300 4.1\n1400 4.4\n1500 4.7\n1600 5.0\n1700 5.3\n";
        let r = parse_mic_curve(text, None).unwrap();
        assert_eq!(r.freqs_hz.len(), 17);
    }

    #[test]
    fn parse_mic_curve_third_column_ignored() {
        let mut text = String::new();
        for i in 0..20 {
            let f = 100.0_f32 * 1.2_f32.powi(i);
            text.push_str(&format!("{f}\t0.{i}\t-12.5\n"));
        }
        let r = parse_mic_curve(&text, None).unwrap();
        assert_eq!(r.freqs_hz.len(), 20);
    }

    #[test]
    fn parse_mic_curve_rejects_too_few_points() {
        let text = "100 0\n200 1\n300 2\n";
        let err = parse_mic_curve(text, None).unwrap_err();
        assert!(err.to_string().contains("too sparse"), "got {err}");
    }

    #[test]
    fn parse_mic_curve_rejects_non_monotonic() {
        let mut text = dummy_curve_text(20);
        text.push_str("50 0\n");                                // out-of-order
        let err = parse_mic_curve(&text, None).unwrap_err();
        assert!(err.to_string().contains("strictly"), "got {err}");
    }

    #[test]
    fn parse_mic_curve_rejects_zero_freq() {
        let text = format!("0 0\n{}", dummy_curve_text(20));
        let err = parse_mic_curve(&text, None).unwrap_err();
        assert!(err.to_string().contains("> 0 Hz"), "got {err}");
    }

    #[test]
    fn correction_at_endpoints_clamps() {
        let r = parse_mic_curve(&dummy_curve_text(50), None).unwrap();
        // Below the first freq: clamps to first gain (0).
        assert!((r.correction_at(1.0) - 0.0).abs() < 0.01);
        // Above the last: clamps to last gain (4.0).
        assert!((r.correction_at(50_000.0) - 4.0).abs() < 0.01);
    }

    #[test]
    fn correction_at_interpolates_in_log_freq() {
        // Curve: linear gain ramp 0..4 dB over log(20..20k). At
        // geometric mid-point (sqrt(20*20000) ≈ 632), correction = 2 dB.
        let r = parse_mic_curve(&dummy_curve_text(50), None).unwrap();
        let mid = (20.0_f32 * 20_000.0).sqrt();
        let g = r.correction_at(mid);
        assert!((g - 2.0).abs() < 0.1, "got {g} dB at f={mid}, expected ≈ 2.0");
    }

    #[test]
    fn correction_at_negative_freq_clamps_to_first() {
        let r = parse_mic_curve(&dummy_curve_text(50), None).unwrap();
        assert!((r.correction_at(-100.0) - r.gain_db[0]).abs() < 1e-6);
    }

    #[test]
    fn mic_response_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cal.json");
        let curve = parse_mic_curve(&dummy_curve_text(40), Some("/foo/bar.frd".into())).unwrap();

        let mut cal = Calibration::new(0, 1);
        cal.mic_response = Some(curve.clone());
        cal.save(Some(&path)).unwrap();

        let loaded = Calibration::load(0, 1, Some(&path)).unwrap().unwrap();
        let r = loaded.mic_response.expect("curve missing after reload");
        assert_eq!(r.freqs_hz.len(), curve.freqs_hz.len());
        assert!((r.freqs_hz[0] - curve.freqs_hz[0]).abs() < 1e-3);
        assert_eq!(r.source_path, curve.source_path);
        assert_eq!(r.imported_at, curve.imported_at);
    }

    #[test]
    fn mic_curve_save_preserves_voltage_and_spl() {
        // Same composition guarantee #63 introduced for voltage↔SPL must
        // hold for mic-curve as well: writing one field via load_or_new
        // doesn't lose the others.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cal.json");

        let mut a = Calibration::new(0, 0);
        a.vrms_at_0dbfs_in = Some(0.5);
        a.mic_sensitivity_dbfs_at_94db_spl = Some(-30.0);
        a.save(Some(&path)).unwrap();

        let mut b = Calibration::load_or_new(0, 0, Some(&path));
        b.mic_response = Some(parse_mic_curve(&dummy_curve_text(20), None).unwrap());
        b.save(Some(&path)).unwrap();

        let loaded = Calibration::load(0, 0, Some(&path)).unwrap().unwrap();
        assert_eq!(loaded.vrms_at_0dbfs_in, Some(0.5));
        assert_eq!(loaded.mic_sensitivity_dbfs_at_94db_spl, Some(-30.0));
        assert!(loaded.mic_response.is_some());
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
