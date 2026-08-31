//! Persistence for `cal.json` — the flat `{"out{N}_in{M}": entry}` file
//! under `~/.config/ac/`.
//!
//! Every write is a read-modify-write of the whole file, so this module
//! owns two guarantees the calibration layers above it depend on:
//!
//! * **A file this module cannot parse is never overwritten.** The map is
//!   the only copy of every *other* channel's calibration, so treating a
//!   parse failure as "empty" and writing back would replace the whole
//!   store with the single entry being saved. [`read_all_entries`] returns
//!   the parse error and [`Calibration::save`] propagates it; the daemon
//!   already routes a save error to the operator as a terminal `error`
//!   frame. Losing an hour of rig calibration silently is strictly worse
//!   than a calibration run that refuses and says why.
//! * **A write is all-or-nothing.** The file is written to a temporary in
//!   the same directory and renamed over the target, so a crash or a full
//!   disk leaves the previous file intact rather than a truncated one.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::{Calibration, CalibrationEntry};

/// Default calibration file path: `~/.config/ac/cal.json`.
pub fn default_cal_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home)
        .join(".config")
        .join("ac")
        .join("cal.json")
}

/// The one place the `out{N}_in{M}` key format is spelled out.
pub fn cal_key(output_channel: u32, input_channel: u32) -> String {
    format!("out{output_channel}_in{input_channel}")
}

fn resolve_path(path: Option<&Path>) -> PathBuf {
    path.map(Path::to_path_buf).unwrap_or_else(default_cal_path)
}

/// Read every stored entry. A missing file is an empty map; a file that
/// exists but does not parse is an **error**, never an empty map — see the
/// module doc.
fn read_all_entries(path: &Path) -> Result<HashMap<String, CalibrationEntry>> {
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let raw =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| {
        format!(
            "parsing {} — refusing to treat it as empty, because saving over it would \
             discard every calibration it holds",
            path.display()
        )
    })
}

/// Serialize `all` to `path` atomically: write a sibling temporary, then
/// rename over the target. `rename(2)` within a directory is atomic, so a
/// reader either sees the whole previous file or the whole new one.
fn write_all_entries(path: &Path, all: &HashMap<String, CalibrationEntry>) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let out = serde_json::to_string_pretty(all)?;
    // Same directory as the target: `rename` across filesystems is not
    // atomic (and on Linux fails outright), so a temp dir would not do.
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    std::fs::write(&tmp, out).with_context(|| format!("writing {}", tmp.display()))?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        // Leaving the temp behind would accumulate one file per failed
        // save next to the real cal.json.
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("renaming {} -> {}", tmp.display(), path.display()));
    }
    Ok(())
}

impl Calibration {
    /// File key: `out{N}_in{M}`.
    pub fn key(&self) -> String {
        cal_key(self.output_channel, self.input_channel)
    }

    /// Persist this calibration entry into the shared cal.json file.
    /// Existing entries for other channel pairs are preserved.
    pub fn save(&self, path: Option<&Path>) -> Result<()> {
        let path = resolve_path(path);
        let mut all = read_all_entries(&path)?;
        all.insert(self.key(), self.to_entry());
        write_all_entries(&path, &all)?;
        eprintln!(
            "  Calibration saved -> {}  (key: {})",
            path.display(),
            self.key()
        );
        Ok(())
    }

    /// Load calibration for a specific output/input channel pair.
    /// Returns `Ok(None)` if the file or key doesn't exist.
    pub fn load(
        output_channel: u32,
        input_channel: u32,
        path: Option<&Path>,
    ) -> Result<Option<Self>> {
        let all = read_all_entries(&resolve_path(path))?;
        Ok(all
            .get(&cal_key(output_channel, input_channel))
            .map(Calibration::from_entry))
    }

    /// Load all stored calibration entries.
    pub fn load_all(path: Option<&Path>) -> Result<Vec<Self>> {
        let all = read_all_entries(&resolve_path(path))?;
        Ok(all.values().map(Calibration::from_entry).collect())
    }

    /// Load the existing calibration entry for a channel pair, or return a
    /// fresh one with defaults. Used by partial-update handlers (voltage
    /// cal + SPL cal write to disjoint fields and must not clobber each
    /// other's prior values).
    pub fn load_or_new(output_channel: u32, input_channel: u32, path: Option<&Path>) -> Self {
        match Self::load(output_channel, input_channel, path) {
            Ok(Some(existing)) => existing,
            Ok(None) => Self::new(output_channel, input_channel),
            // Not silent: the caller gets a blank entry, but the
            // subsequent `save` will refuse with the same error, and the
            // operator should see the cause before the refusal.
            Err(e) => {
                eprintln!("  Calibration: {e:#} — starting from an empty entry");
                Self::new(output_channel, input_channel)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::mic_response::fixtures::dummy_curve_text;
    use super::super::parse_mic_curve;
    use super::super::tau::fixtures::{dummy_conditions, dummy_tau_entry};
    use super::*;

    #[test]
    fn save_and_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cal.json");

        let mut cal = Calibration::new(0, 0);
        cal.vrms_at_0dbfs_out = Some(1.234);
        cal.vrms_at_0dbfs_in = Some(0.567);
        cal.save(Some(&path)).unwrap();

        let loaded = Calibration::load(0, 0, Some(&path)).unwrap().unwrap();
        assert!((loaded.vrms_at_0dbfs_out.unwrap() - 1.234).abs() < 1e-10);
        assert!((loaded.vrms_at_0dbfs_in.unwrap() - 0.567).abs() < 1e-10);
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
    fn spl_field_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cal.json");

        let mut cal = Calibration::new(2, 3);
        cal.vrms_at_0dbfs_out = Some(1.0);
        cal.vrms_at_0dbfs_in = Some(0.5);
        cal.mic_sensitivity_dbfs_at_94db_spl = Some(-31.7);
        cal.save(Some(&path)).unwrap();

        let loaded = Calibration::load(2, 3, Some(&path)).unwrap().unwrap();
        assert_eq!(loaded.mic_sensitivity_dbfs_at_94db_spl, Some(-31.7));
    }

    // ─── mic_curve parser ───────────────────────────────────────────────
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
        cal.vrms_at_0dbfs_in = Some(0.567);
        cal.save(Some(&path)).unwrap();

        let loaded = Calibration::load(0, 1, Some(&path)).unwrap().unwrap();
        assert_eq!(loaded.mic_sensitivity_dbfs_at_94db_spl, Some(-29.4));
        assert_eq!(loaded.vrms_at_0dbfs_out, Some(1.234));
        assert_eq!(loaded.vrms_at_0dbfs_in, Some(0.567));
    }

    #[test]
    fn tau_history_round_trips_alongside_voltage_and_spl() {
        // Same composition guarantee as `mic_curve_save_preserves_voltage_
        // and_spl`, extended to the third layer: appending a τ entry must
        // not disturb the other two.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cal.json");

        let mut a = Calibration::new(0, 0);
        a.vrms_at_0dbfs_out = Some(1.234);
        a.vrms_at_0dbfs_in = Some(0.567);
        a.mic_sensitivity_dbfs_at_94db_spl = Some(-30.0);
        a.save(Some(&path)).unwrap();

        let mut b = Calibration::load_or_new(0, 0, Some(&path));
        b.tau_history
            .push(dummy_tau_entry(dummy_conditions(), 0.0011931));
        b.save(Some(&path)).unwrap();

        let loaded = Calibration::load(0, 0, Some(&path)).unwrap().unwrap();
        assert_eq!(loaded.vrms_at_0dbfs_out, Some(1.234));
        assert_eq!(loaded.vrms_at_0dbfs_in, Some(0.567));
        assert_eq!(loaded.mic_sensitivity_dbfs_at_94db_spl, Some(-30.0));
        assert_eq!(loaded.tau_history.len(), 1);
        assert!((loaded.tau_history[0].tau_s - 0.0011931).abs() < 1e-12);
    }

    // ─── file-level failure modes ───────────────────────────────────────

    #[test]
    fn save_over_an_unparseable_file_refuses_instead_of_discarding_it() {
        // The failing case: cal.json holds every channel's calibration, so
        // a save that treats a parse failure as "empty map" replaces the
        // whole store with the one entry being written. Before this
        // refusal, an hour of rig calibration went away with no message.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cal.json");
        let corrupt = "{\"out0_in0\": {\"output_channel\": 0, tru";
        std::fs::write(&path, corrupt).unwrap();

        let mut cal = Calibration::new(1, 1);
        cal.vrms_at_0dbfs_out = Some(2.0);
        let err = cal.save(Some(&path)).expect_err("save must refuse");
        assert!(
            format!("{err:#}").contains("parsing"),
            "error should name the parse failure: {err:#}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            corrupt,
            "the unparseable file must be left exactly as it was"
        );
    }

    #[test]
    fn load_of_an_unparseable_file_is_an_error_not_an_empty_map() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cal.json");
        std::fs::write(&path, "not json at all").unwrap();

        assert!(Calibration::load(0, 0, Some(&path)).is_err());
        assert!(Calibration::load_all(Some(&path)).is_err());
    }

    /// Bounds what this test can show: atomicity itself is not observable
    /// from inside the process — nothing here proves a crash mid-write
    /// leaves the old file intact. What it does catch is the temp file
    /// surviving the success path, which would accumulate one stray file
    /// per save next to the real cal.json.
    #[test]
    fn save_cleans_up_its_temporary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cal.json");

        let mut cal = Calibration::new(0, 0);
        cal.vrms_at_0dbfs_out = Some(1.0);
        cal.save(Some(&path)).unwrap();

        let names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec!["cal.json".to_string()],
            "stray files: {names:?}"
        );
    }

    #[test]
    fn save_creates_the_parent_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("deeper").join("cal.json");

        let mut cal = Calibration::new(0, 0);
        cal.vrms_at_0dbfs_in = Some(0.5);
        cal.save(Some(&path)).unwrap();

        assert_eq!(
            Calibration::load(0, 0, Some(&path))
                .unwrap()
                .unwrap()
                .vrms_at_0dbfs_in,
            Some(0.5)
        );
    }

    #[test]
    fn cal_key_matches_the_stored_key() {
        assert_eq!(cal_key(1, 3), "out1_in3");
        assert_eq!(Calibration::new(1, 3).key(), cal_key(1, 3));
    }
}
