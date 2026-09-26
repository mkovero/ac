//! Snapshot flow (deliverable 6): trigger → chunked fetch with sha256
//! verification → open (remote or local file, no daemon needed for the
//! latter, D8) → per-channel weighting/integration re-derivation, with
//! the readout updating via `ac-scene` — this module never computes a
//! level or a string itself, only orchestrates `ac-core::snapshot` and
//! `ac-scene` calls.

use std::path::Path;

use ac_core::snapshot::{read_acsnap, Snapshot};
use ac_core::visualize::pair_derivation::PairDerivation;
use ac_core::visualize::weighting_curves::WeightingCurve;
use ac_scene::{DisplayModes, Scene, TransferScene};
use anyhow::{Context, Result};
use serde_json::json;

use crate::view::LoadedRun;
use crate::zmq_client::Client;

/// `snapshot` (trigger) + `snapshot_fetch` (chunked, sha256-verified) in
/// one call — returns the parsed `.acsnap`, ready to derive scenes
/// from. Requires a live `transfer_stream` session (the daemon's own
/// precondition, not re-checked here).
pub fn trigger_and_fetch(client: &Client) -> Result<Snapshot> {
    trigger_and_fetch_bytes(client).map(|(_, snap)| snap)
}

/// [`trigger_and_fetch`], keeping the verified `.acsnap` bytes as well, so
/// a caller can store the file exactly as the daemon produced it.
pub fn trigger_and_fetch_bytes(client: &Client) -> Result<(Vec<u8>, Snapshot)> {
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
    let snap = read_acsnap(&bytes)?;
    Ok((bytes, snap))
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

/// Open a local `.acsnap` and derive one pair's H1 into a [`LoadedRun`] —
/// the transfer-view analogue of `open_local` + [`rederive_scene`] in one
/// call (#321), since a stored run's identity (filename, capture
/// timestamp) has to travel with its derivation from the moment it's
/// opened. `weighting` only affects `PairDerivation::spl`/`spl_weighting`,
/// neither of which the transfer view reads — `Z` (unweighted) is passed
/// so the derivation carries an honest "not reprocessed under a chosen
/// weighting" value rather than one implying an operator picked one.
///
pub fn open_stored_transfer_run(path: &Path, pair_idx: usize) -> Result<LoadedRun> {
    let snap = open_local(path)?;
    let label = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    stored_run_from_snapshot(&snap, pair_idx, label)
}

/// One pair of an already-parsed snapshot as a [`LoadedRun`] labelled
/// `label` — the shared half of [`open_stored_transfer_run`] and the `S`
/// capture (#256), so a run captured live and the same file opened later
/// derive identically.
pub fn stored_run_from_snapshot(
    snap: &Snapshot,
    pair_idx: usize,
    label: String,
) -> Result<LoadedRun> {
    let (meas_ch, _ref_ch) = *snap
        .meta
        .session
        .pairs
        .get(pair_idx)
        .context("pair index out of range")?;
    let channel_role = snap
        .meta
        .per_channel
        .iter()
        .find(|c| c.input_channel == meas_ch)
        .map(|c| c.role.clone())
        .unwrap_or_else(|| format!("meas_{meas_ch}"));
    let derivation = snap.derive_pair(pair_idx, WeightingCurve::Z, None)?;
    Ok(LoadedRun::new(
        label,
        snap.meta.captured_at_utc.clone(),
        derivation,
        channel_role,
        snap.meta.sr,
    ))
}

/// A stored run's trace as drawn: its derivation, moved by its `←`/`→`
/// offset (#256). The one place a run becomes a `TransferInput`, shared by
/// the scene build, CSV export and the average (#671).
pub fn run_input(run: &LoadedRun) -> ac_scene::TransferInput {
    let mut input =
        ac_scene::TransferInput::from_pair_derivation(&run.derivation, &run.channel_role, run.sr);
    input.shift_delay(run.delay_offset_samples);
    input
}

/// The average of the visible stored runs (#671) as a trace, with a label
/// for the legend: `Ok(None)` when the average is off; `Err` with the
/// reason when it cannot be formed (fewer than two visible runs, or runs on
/// different grids).
pub fn average_input(
    state: &crate::view::TransferViewState,
) -> Result<Option<(ac_scene::TransferInput, String)>, String> {
    let Some(weighted) = state.average else {
        return Ok(None);
    };
    let inputs: Vec<ac_scene::TransferInput> = state
        .loaded
        .iter()
        // Slots only: a run opened from a file is not part of "the slots".
        .filter(|r| r.visible && r.slot.is_some())
        .map(run_input)
        .collect();
    let refs: Vec<&ac_scene::TransferInput> = inputs.iter().collect();
    let avg = ac_scene::TransferInput::average(&refs, weighted)?;
    let how = if weighted {
        "coherence weighted"
    } else {
        "plain"
    };
    Ok(Some((
        avg,
        format!("average of {} slots, {how}", refs.len()),
    )))
}

/// Build (or rebuild) a stored run's [`TransferScene`] from its held
/// [`PairDerivation`] under `modes` — the transfer-view analogue of
/// [`rederive_scene`], called again every time the operator changes that
/// run's smoothing (#321). A stored run is always drawn self-compensated
/// (`DerotMode::Session`, τ_derot 0), per `transfer.rs`'s own module doc
/// and deliverable 3 (#229) — callers pass that mode explicitly rather
/// than this function assuming it, so the one rule lives at the one call
/// site that decides it (`TransferViewState::derot`-style state never
/// touches a stored run).
///
/// A stored derivation is a static capture — it has no live meters and no
/// drive/lock state to report (`TransferInput::from_pair_derivation`
/// already sets `fault: None`) — so this gives `TransferScene::from_input`
/// a fresh, throwaway meter/fault pair each call rather than one persisted
/// across frames: there is nothing here for a decay window to mean.
///
/// `delay_offset_samples` moves the run's delay from the recorded one
/// (`←`/`→`, #256) — `TransferInput::shift_delay`.
pub fn rederive_transfer_scene(
    derivation: &PairDerivation,
    channel_role: &str,
    sr: u32,
    delay_offset_samples: i64,
    modes: DisplayModes,
    freq_range: (f64, f64),
    db_range: (f64, f64),
) -> TransferScene {
    let mut input = ac_scene::TransferInput::from_pair_derivation(derivation, channel_role, sr);
    input.shift_delay(delay_offset_samples);
    let mut meters = (
        ac_scene::MeterState::default(),
        ac_scene::MeterState::default(),
    );
    let mut fault = ac_scene::FaultState::default();
    TransferScene::from_input(
        &input,
        modes,
        freq_range,
        db_range,
        &mut meters,
        &mut fault,
        0.0,
    )
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
