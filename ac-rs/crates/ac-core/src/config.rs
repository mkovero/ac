//! Persistent hardware configuration — mirrors `ac/config.py`.
//!
//! Reads/writes `~/.config/ac/config.json`.  Missing keys are filled from
//! [`Config::default`] so new fields never break old config files.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::shared::constants::DBU_REF_EXACT;

fn default_dbu_ref() -> f64 {
    DBU_REF_EXACT
}
fn default_range_start() -> f64 {
    20.0
}
fn default_range_stop() -> f64 {
    20_000.0
}
fn default_snapshot_ring_s() -> f64 {
    30.0
}

/// Validate and canonicalize a backend requirement supplied by setup or
/// loaded from persistent config. The legacy `sounddevice` spelling maps to
/// CPAL; callers writing config persist the canonical spelling.
pub fn canonical_backend(value: Option<&str>) -> std::result::Result<Option<&'static str>, String> {
    match value.map(str::to_ascii_lowercase).as_deref() {
        None => Ok(None),
        Some("jack") => Ok(Some("jack")),
        Some("cpal") | Some("sounddevice") => Ok(Some("cpal")),
        Some("fake") => Ok(Some("fake")),
        Some(_) => Err("backend must be jack, cpal, fake, or null".to_string()),
    }
}

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

    /// Playback port index for the reference **output** leg — the port that
    /// feeds the reference loopback. A separate index space from
    /// [`Config::reference_channel`], which is a capture index: on a rig where
    /// the loopback source and the reference capture sit at different indices,
    /// deriving one from the other drives a port nothing is connected to and
    /// the reference stays silent (#225). `None` means the reference stimulus
    /// leaves on the main output.
    #[serde(default)]
    pub reference_output_channel: Option<u32>,

    /// Sticky JACK port name for the reference output leg. Gated on
    /// [`Config::reference_output_channel`], mirroring how `reference_port` is
    /// gated on `reference_channel` — and set without it, the daemon refuses
    /// to resolve rather than ignoring it, since a configured value that
    /// quietly does nothing is the failure #225 was.
    #[serde(default)]
    pub reference_output_port: Option<String>,

    #[serde(default = "default_dbu_ref")]
    pub dbu_ref_vrms: f64,

    /// **Retired** (#459). This key no longer sets a limit — the emission
    /// ceiling is [`crate::shared::emission_level::MAX_EMISSION_DBFS`], a
    /// fixed build constant, not a setting (operator, 2026-09-15: "less
    /// configurable / settable limits and more singular global ones").
    ///
    /// Kept as an `Option` under its original JSON key so a config file
    /// that still has it round-trips instead of the key silently
    /// vanishing on the next save — the #225 defect class applies to a
    /// config that quietly *stops doing anything* just as much as to one
    /// that does the wrong thing. While this is `Some`, every daemon
    /// handler that can emit refuses and names the key
    /// (`ac-daemon/src/handlers/mod.rs`); `ac setup` shows it too.
    #[serde(
        rename = "drive_max_dbfs",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub retired_drive_max_dbfs: Option<f64>,

    pub dmm_host: Option<String>,

    #[serde(default = "default_range_start")]
    pub range_start_hz: f64,

    #[serde(default = "default_range_stop")]
    pub range_stop_hz: f64,

    #[serde(default)]
    pub server_enabled: bool,

    pub gpio_port: Option<String>,

    /// Active session name.
    pub session: Option<String>,

    /// Required audio backend: `"jack"`, `"cpal"`, `"fake"`, or `None`
    /// for the platform's real default. `"sounddevice"` is accepted while
    /// loading legacy config and canonicalized to `"cpal"` by setup.
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

    /// Duration of the `transfer_stream` snapshot ring, in seconds
    /// (handoff: snapshot-backend M1, deliverable 1). Raw pre-processing
    /// samples for every session channel are retained for this long;
    /// `snapshot` can only capture what's still in the ring.
    #[serde(default = "default_snapshot_ring_s")]
    pub snapshot_ring_s: f64,

    /// Daemon-local directory `.acsnap` files spool to before/while a
    /// client fetches them (handoff: snapshot-backend M1, deliverable 2).
    /// `None` = `~/.config/ac/snapshots/`. Never exposed to clients — the
    /// `snapshot` reply carries an opaque `id`, never a path (D6).
    #[serde(default)]
    pub snapshot_spool_dir: Option<PathBuf>,

    /// Room air temperature in °C. #391 removed the delay readout's ms → m
    /// conversion this used to feed exclusively; what remains is
    /// archival — flowed into a captured report's
    /// [`crate::measurement::report::PositionSnapshot`] (#280) and echoed
    /// (with the speed of sound it implies) by `ac setup` and the
    /// HTML/PDF report renderers. `None` falls back to
    /// [`crate::shared::conversions::SPEED_OF_SOUND_DEFAULT_M_S`] wherever
    /// a speed is still derived from it.
    ///
    /// Stored as a temperature rather than as a speed on purpose. The
    /// temperature is the quantity an operator can read off a thermometer
    /// standing in the room; the speed is derived from it by
    /// [`crate::shared::conversions::speed_of_sound_at`] at each call site.
    /// Storing both would be two constants that are only correct when
    /// written together — the coupling this repo has already paid for.
    ///
    /// It is a room property, not a session one, which is why it lives here
    /// and not in a per-measurement argument: a rig's room does not change
    /// temperature between `ac transfer` invocations, and re-typing it every
    /// time is how it ends up untyped.
    #[serde(default)]
    pub temperature_c: Option<f64>,
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
            reference_output_channel: None,
            reference_output_port: None,
            dbu_ref_vrms: DBU_REF_EXACT,
            retired_drive_max_dbfs: None,
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
            snapshot_ring_s: 30.0,
            snapshot_spool_dir: None,
            temperature_c: None,
        }
    }
}

/// Resolve the effective snapshot spool directory: `cfg.snapshot_spool_dir`
/// if set, else `~/.config/ac/snapshots/`.
pub fn snapshot_spool_dir(cfg: &Config) -> PathBuf {
    cfg.snapshot_spool_dir.clone().unwrap_or_else(|| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home)
            .join(".config")
            .join("ac")
            .join("snapshots")
    })
}

/// Return the default config file path: `~/.config/ac/config.json`.
pub fn default_config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home)
        .join(".config")
        .join("ac")
        .join("config.json")
}

/// Load config from disk, merging with defaults for any missing keys.
/// Returns [`Config::default`] silently if the file does not exist.
pub fn load(path: Option<&Path>) -> Result<Config> {
    let path = path
        .map(|p| p.to_path_buf())
        .unwrap_or_else(default_config_path);
    if !path.exists() {
        return Ok(Config::default());
    }
    let raw =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    // serde fills missing fields from defaults; extra fields are ignored.
    let cfg: Config =
        serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
    canonical_backend(cfg.backend.as_deref())
        .map_err(anyhow::Error::msg)
        .with_context(|| format!("parsing {}", path.display()))?;
    Ok(cfg)
}

/// Why [`save`] did not persist. In both cases the file on disk is exactly
/// what it was before the call.
#[derive(Debug)]
pub enum SaveError {
    /// The existing file exists but could not be read or parsed. Nothing was
    /// written: merging into defaults would replace every setting the file
    /// holds with the patch alone.
    Unreadable {
        path: PathBuf,
        source: anyhow::Error,
    },
    /// Creating the directory, writing or syncing the temporary, or renaming
    /// it over the target failed. The previous file is untouched.
    Write {
        path: PathBuf,
        source: anyhow::Error,
    },
}

impl SaveError {
    /// The config file the save targeted.
    pub fn path(&self) -> &Path {
        match self {
            SaveError::Unreadable { path, .. } | SaveError::Write { path, .. } => path,
        }
    }

    /// The underlying failure, with its full context chain.
    pub fn cause(&self) -> &anyhow::Error {
        match self {
            SaveError::Unreadable { source, .. } | SaveError::Write { source, .. } => source,
        }
    }
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SaveError::Unreadable { path, .. } => write!(
                f,
                "existing config {} is unreadable; not overwritten",
                path.display()
            ),
            SaveError::Write { path, .. } => {
                write!(f, "config {} not saved", path.display())
            }
        }
    }
}

impl std::error::Error for SaveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.cause().as_ref())
    }
}

/// Merge `updates` into the on-disk config and write back atomically
/// (see [`crate::shared::atomic_write`]). Returns the merged config — what is
/// now on disk.
///
/// A missing file merges into defaults. An existing file that cannot be read
/// or parsed is refused with [`SaveError::Unreadable`] and left as it was.
pub fn save(updates: &Config, path: Option<&Path>) -> std::result::Result<Config, SaveError> {
    let path = path
        .map(|p| p.to_path_buf())
        .unwrap_or_else(default_config_path);
    // Merge: start from existing, apply updates field by field via JSON patch.
    let existing = match load(Some(&path)) {
        Ok(cfg) => cfg,
        Err(source) => return Err(SaveError::Unreadable { path, source }),
    };
    let write_err = |source: anyhow::Error| SaveError::Write {
        path: path.clone(),
        source,
    };
    let (final_cfg, out) = merge_and_render(&existing, updates).map_err(write_err)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating dir {}", dir.display()))
            .map_err(write_err)?;
    }
    crate::shared::atomic_write::write_atomic(&path, out.as_bytes()).map_err(write_err)?;
    Ok(final_cfg)
}

/// Apply `updates` over `existing` key by key and serialise the result.
fn merge_and_render(existing: &Config, updates: &Config) -> Result<(Config, String)> {
    // Serialize both to Value, merge, then deserialise back.
    let mut merged = serde_json::to_value(existing)?;
    let patch = serde_json::to_value(updates)?;
    if let (Some(m), Some(p)) = (merged.as_object_mut(), patch.as_object()) {
        for (k, v) in p {
            m.insert(k.clone(), v.clone());
        }
    }
    let final_cfg: Config = serde_json::from_value(merged)?;
    let out = serde_json::to_string_pretty(&final_cfg)?;
    Ok((final_cfg, out))
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

    #[test]
    fn save_over_corrupt_json_refuses_and_preserves_the_file() {
        // The failing case: a save that treats a parse failure as defaults
        // replaces every setting in the file with defaults plus the patch.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let corrupt = b"{\"output_channel\": 3, \"input_chan";
        std::fs::write(&path, corrupt).unwrap();

        let updates = Config {
            output_channel: 5,
            ..Config::default()
        };
        let err = save(&updates, Some(&path)).expect_err("save must refuse");
        assert!(
            matches!(err, SaveError::Unreadable { .. }),
            "expected Unreadable, got {err:?}"
        );
        assert_eq!(err.path(), path);
        assert!(format!("{:#}", err.cause()).contains("parsing"), "{err:?}");
        assert_eq!(std::fs::read(&path).unwrap(), corrupt);
    }

    #[test]
    fn save_into_unwritable_storage_is_a_write_error() {
        // Parent of the config dir is a regular file: fails for root too.
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("f");
        std::fs::write(&blocker, b"not a dir").unwrap();
        let path = blocker.join("config.json");

        let err = save(&Config::default(), Some(&path)).expect_err("save must fail");
        assert!(
            matches!(err, SaveError::Write { .. }),
            "expected Write, got {err:?}"
        );
        assert_eq!(err.path(), path);
        assert!(!path.exists());
        assert_eq!(std::fs::read(&blocker).unwrap(), b"not a dir");
    }

    #[test]
    fn save_merges_into_a_missing_file_and_returns_what_is_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("config.json");
        let updates = Config {
            output_channel: 4,
            ..Config::default()
        };
        let saved = save(&updates, Some(&path)).unwrap();
        let reread = load(Some(&path)).unwrap();
        assert_eq!(saved.output_channel, 4);
        assert_eq!(
            serde_json::to_value(&saved).unwrap(),
            serde_json::to_value(&reread).unwrap()
        );
    }
}
