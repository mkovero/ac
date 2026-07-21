//! Snapshot flow (deliverable 6): trigger → chunked fetch with sha256
//! verification → open (remote or local file, no daemon needed for the
//! latter, D8) → per-channel weighting/integration re-derivation, with
//! the readout updating via `ac-scene` — this module never computes a
//! level or a string itself, only orchestrates `ac-core::snapshot` and
//! `ac-scene` calls.

use std::path::Path;

use ac_core::snapshot::{read_acsnap, Snapshot};
use ac_core::visualize::weighting_curves::WeightingCurve;
use ac_scene::Scene;
use anyhow::{Context, Result};
use serde_json::json;

use crate::zmq_client::Client;

/// `snapshot` (trigger) + `snapshot_fetch` (chunked, sha256-verified) in
/// one call — returns the parsed `.acsnap`, ready to derive scenes
/// from. Requires a live `transfer_stream` session (the daemon's own
/// precondition, not re-checked here).
pub fn trigger_and_fetch(client: &Client) -> Result<Snapshot> {
    let reply = client.call(&json!({"cmd": "snapshot"}))?;
    if reply["ok"] != serde_json::Value::Bool(true) {
        anyhow::bail!(
            "snapshot trigger failed: {}",
            reply["error"].as_str().unwrap_or("unknown error")
        );
    }
    let id = reply["id"].as_str().context("snapshot reply missing id")?;
    let sha256 = reply["sha256"]
        .as_str()
        .context("snapshot reply missing sha256")?;

    let bytes = client.fetch_snapshot(id, sha256)?;
    read_acsnap(&bytes)
}

/// Open a local `.acsnap` file — no daemon connection needed (D8).
pub fn open_local(path: &Path) -> Result<Snapshot> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    read_acsnap(&bytes)
}

/// Re-derive a scene for `pair_idx` under `weighting`, over the whole
/// captured window (`sample_range: None`). This is the entire
/// "readout updates accordingly" mechanism — a fresh `derive_pair` +
/// `Scene::from_pair_derivation` call, same functions the live path
/// and M2's own tests already use (D8: no reimplementation).
pub fn rederive_scene(
    snap: &Snapshot,
    pair_idx: usize,
    weighting: WeightingCurve,
    freq_range: (f64, f64),
    db_range: (f64, f64),
) -> Result<Scene> {
    let (meas_ch, ref_ch) = *snap
        .meta
        .session
        .pairs
        .get(pair_idx)
        .context("pair index out of range")?;
    let meas_role = snap
        .meta
        .per_channel
        .iter()
        .find(|c| c.input_channel == meas_ch)
        .map(|c| c.role.clone())
        .unwrap_or_else(|| format!("meas_{meas_ch}"));
    let ref_role = snap
        .meta
        .per_channel
        .iter()
        .find(|c| c.input_channel == ref_ch)
        .map(|c| c.role.clone())
        .unwrap_or_else(|| format!("ref_{ref_ch}"));

    let derivation = snap.derive_pair(pair_idx, weighting, None)?;
    Ok(Scene::from_pair_derivation(
        &derivation,
        &meas_role,
        &ref_role,
        snap.meta.sr,
        freq_range,
        db_range,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_local_rejects_missing_file() {
        let result = open_local(Path::new("/nonexistent/path/does-not-exist.acsnap"));
        assert!(result.is_err());
    }
}
