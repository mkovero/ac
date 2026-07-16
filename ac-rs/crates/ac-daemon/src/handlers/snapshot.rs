//! `snapshot` / `snapshot_fetch` / `snapshot_list` / `snapshot_delete`
//! (handoff: snapshot-backend M1).
//!
//! Ungated by design (decision 3, architect addendum): these commands
//! don't spawn workers and don't touch audio I/O — they read/write
//! shared state and a spool file, so they aren't added to
//! `workers::cmd_group`'s match table and run regardless of what else is
//! active (same as `get_calibration`/`status`/`devices` today).
//!
//! **Retention policy** (deliverable 3, "pick one, document it"): the
//! spool is cleared at `transfer_stream` session end — every `.acsnap`
//! taken during a session is deleted when that session's worker stops,
//! matching "a snapshot is only valid while its transfer session runs"
//! (deliverable 2). As a crash-safety fallback (a killed daemon skips
//! its own cleanup), the spool directory is also wiped at the *start* of
//! every new `transfer_stream` session, so a stale file from a prior
//! crashed session never outlives the next session's start.

use std::collections::VecDeque;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Mutex;

use serde_json::{json, Value};

use ac_core::shared::calibration::Calibration;
use ac_core::snapshot::{ChannelMeta, SessionMeta, SnapshotMeta};

use crate::server::ServerState;

/// Max bytes returned per `snapshot_fetch` chunk (pre-base64; base64
/// inflates by ~4/3, so the JSON reply payload is ≈341 KB at this cap).
/// Chosen for CTRL sanity (deliverable 3) — small enough that one chunk
/// is a fast REQ/REP round-trip even over a slow remote link (D6).
pub const MAX_FETCH_CHUNK_BYTES: usize = 256 * 1024;

/// Live, growing state for one `transfer_stream` session's snapshot ring.
/// Lives behind `ServerState::snapshot_ring`; the worker thread mutates
/// it every capture tick, the `snapshot` CTRL handler reads it on demand.
pub struct SnapshotRingState {
    pub sr: u32,
    /// Session input-channel index per ring position (matches the order
    /// `bufs` arrives from `capture_multi`/`unique_ports`).
    pub unique_chans: Vec<u32>,
    pub channels: Vec<VecDeque<f32>>,
    /// Cap per channel, in samples (`snapshot_ring_s × sr`).
    cap_samples: usize,
    pub pairs: Vec<(u32, u32)>,
    /// Mirrors the worker's own `pair_delays` — `None` until the
    /// per-pair delay is estimated on warm-up.
    pub delay_samples: Vec<Option<i64>>,
    pub weighting_tag: String,
    pub integration_tag: String,
    /// Per-`unique_chans`-position calibration, loaded once at session
    /// start (same staleness caveat as every other cal snapshot in this
    /// codebase — a live `calibrate*` call mid-session isn't reflected).
    pub unique_cals: Vec<Option<Calibration>>,
}

impl SnapshotRingState {
    pub fn new(
        sr: u32,
        unique_chans: Vec<u32>,
        cap_samples: usize,
        pairs: Vec<(u32, u32)>,
        weighting_tag: String,
        integration_tag: String,
        unique_cals: Vec<Option<Calibration>>,
    ) -> Self {
        let n = unique_chans.len();
        Self {
            sr,
            unique_chans,
            channels: (0..n)
                .map(|_| VecDeque::with_capacity(cap_samples))
                .collect(),
            cap_samples,
            pairs,
            delay_samples: Vec::new(),
            weighting_tag,
            integration_tag,
            unique_cals,
        }
    }

    /// Push one tick's captured samples (same shape as `capture_multi`'s
    /// return) into the ring, dropping from the front once over cap.
    pub fn push_tick(&mut self, bufs: &[Vec<f32>]) {
        for (i, buf) in bufs.iter().enumerate() {
            if i >= self.channels.len() {
                break;
            }
            let ring = &mut self.channels[i];
            ring.extend(buf.iter().copied());
            while ring.len() > self.cap_samples {
                ring.pop_front();
            }
        }
    }

    /// Snapshot the ring's *current* contents into an owned
    /// `(SnapshotMeta, channels)` pair — cheap (just clones out the
    /// already-in-memory samples and builds a small struct), so the
    /// caller should hold the ring's lock for only as long as this call
    /// takes, not for the FLAC-encoding step that follows (that step is
    /// [`build_acsnap`], deliberately a free function taking owned data
    /// rather than a method on `&self`, so it's impossible to call it
    /// while still holding the ring's mutex).
    ///
    /// The live worker thread needs this same mutex on every capture
    /// tick (`push_tick`, `delay_samples` sync) — holding it across a
    /// FLAC encode of up to `snapshot_ring_s` seconds of multichannel
    /// audio would stall that tick loop and glitch the live
    /// `transfer_stream` cadence for the encode's whole duration.
    fn snapshot_meta_and_channels(&self, daemon_version: &str) -> (SnapshotMeta, Vec<Vec<f32>>) {
        let channels: Vec<Vec<f32>> = self
            .channels
            .iter()
            .map(|d| d.iter().copied().collect())
            .collect();
        let n_frames = channels.first().map(Vec::len).unwrap_or(0);
        let duration_s = n_frames as f64 / self.sr as f64;

        // Role naming: first occurrence of a channel as a pair's meas
        // leg is "meas_<pair index>"; a channel that's only ever a ref
        // leg is "ref". A channel used as meas in one pair and ref in
        // another (unusual but not forbidden) keeps its meas name — the
        // meas role is the more specific one to preserve.
        let mut roles = vec![None; self.unique_chans.len()];
        for (pair_idx, &(meas, refch)) in self.pairs.iter().enumerate() {
            if let Some(pos) = self.unique_chans.iter().position(|&c| c == meas) {
                roles[pos].get_or_insert(format!("meas_{pair_idx}"));
            }
            if let Some(pos) = self.unique_chans.iter().position(|&c| c == refch) {
                roles[pos].get_or_insert_with(|| "ref".to_string());
            }
        }
        let channel_map: Vec<String> = roles
            .into_iter()
            .enumerate()
            .map(|(i, r)| r.unwrap_or_else(|| format!("ch_{}", self.unique_chans[i])))
            .collect();

        let per_channel: Vec<ChannelMeta> = self
            .unique_chans
            .iter()
            .zip(channel_map.iter())
            .zip(self.unique_cals.iter())
            .map(|((&input_channel, role), cal)| ChannelMeta {
                role: role.clone(),
                input_channel,
                weighting: self.weighting_tag.clone(),
                integration: self.integration_tag.clone(),
                calibration: cal.clone(),
            })
            .collect();

        let delay_samples: Vec<i64> = self.delay_samples.iter().map(|d| d.unwrap_or(0)).collect();

        let meta = SnapshotMeta {
            format_version: ac_core::snapshot::FORMAT_VERSION,
            sr: self.sr,
            channel_map,
            per_channel,
            session: SessionMeta {
                pairs: self.pairs.clone(),
                delay_samples,
                nperseg: self.sr as usize,
            },
            captured_at_utc: chrono::Utc::now().to_rfc3339(),
            daemon_version: daemon_version.to_string(),
            ring_duration_s: duration_s,
        };
        (meta, channels)
    }
}

/// FLAC-encode + zip `meta`/`channels` into a `.acsnap`. Deliberately a
/// free function (not a `SnapshotRingState` method) taking owned data —
/// see [`SnapshotRingState::snapshot_meta_and_channels`]'s doc comment
/// for why this must never run while the ring's mutex is held. Returns
/// `(bytes, sha256, duration_s, channel_roles)`.
fn build_acsnap(
    meta: &SnapshotMeta,
    channels: &[Vec<f32>],
) -> anyhow::Result<(Vec<u8>, String, f64, Vec<String>)> {
    let (bytes, sha256) = ac_core::snapshot::write_acsnap(meta, channels)?;
    Ok((
        bytes,
        sha256,
        meta.ring_duration_s,
        meta.channel_map.clone(),
    ))
}

pub struct SpoolEntry {
    pub path: PathBuf,
    pub bytes: u64,
    pub sha256: String,
    pub duration_s: f64,
    pub channels: Vec<String>,
}

fn spool_dir(state: &ServerState) -> PathBuf {
    let cfg = state.cfg.lock().unwrap().clone();
    ac_core::config::snapshot_spool_dir(&cfg)
}

/// Wipe and recreate the spool directory. Called at the start of every
/// `transfer_stream` session (crash-safety fallback — see module doc).
/// Takes the resolved directory and spool map directly (not
/// `&ServerState`) so it's callable from a `'static` worker closure,
/// which only ever holds cloned `Arc`s / owned values, never a
/// `&ServerState` reference (same discipline every other worker in this
/// codebase already follows).
pub fn reset_spool_dir(
    dir: &std::path::Path,
    spool: &Mutex<std::collections::HashMap<String, SpoolEntry>>,
) {
    let _ = fs::remove_dir_all(dir);
    let _ = fs::create_dir_all(dir);
    spool.lock().unwrap().clear();
}

/// Delete every spooled file from this session. Called when the
/// `transfer_stream` worker stops.
pub fn clear_spool(spool: &Mutex<std::collections::HashMap<String, SpoolEntry>>) {
    let mut spool = spool.lock().unwrap();
    for entry in spool.values() {
        let _ = fs::remove_file(&entry.path);
    }
    spool.clear();
}

pub fn snapshot(state: &ServerState, _cmd: &Value) -> Value {
    let ring_handle = {
        let slot = state.snapshot_ring.lock().unwrap();
        match slot.as_ref() {
            Some(r) => r.clone(),
            None => return json!({"ok": false, "error": "no transfer_stream session running"}),
        }
    };
    // Hold the ring's lock only long enough to clone out its current
    // contents — never across the FLAC encode below, which would stall
    // the live worker's capture tick (holding this same mutex) for the
    // encode's whole duration. See `snapshot_meta_and_channels`'s doc.
    let daemon_version = env!("CARGO_PKG_VERSION");
    let (meta, channels) = {
        let ring = ring_handle.lock().unwrap();
        ring.snapshot_meta_and_channels(daemon_version)
    };
    let (bytes, sha256, duration_s, channels) = match build_acsnap(&meta, &channels) {
        Ok(v) => v,
        Err(e) => return json!({"ok": false, "error": format!("snapshot: {e}")}),
    };

    let dir = spool_dir(state);
    if let Err(e) = fs::create_dir_all(&dir) {
        return json!({"ok": false, "error": format!("snapshot: spool dir: {e}")});
    }
    let id = sha256.clone();
    let path = dir.join(format!("{id}.acsnap"));
    if let Err(e) = fs::write(&path, &bytes) {
        return json!({"ok": false, "error": format!("snapshot: write: {e}")});
    }

    let entry = SpoolEntry {
        path,
        bytes: bytes.len() as u64,
        sha256: sha256.clone(),
        duration_s,
        channels: channels.clone(),
    };
    state
        .snapshot_spool
        .lock()
        .unwrap()
        .insert(id.clone(), entry);

    json!({
        "ok": true,
        "id": id,
        "bytes": bytes.len(),
        "duration_s": duration_s,
        "channels": channels,
        "sha256": sha256,
    })
}

pub fn snapshot_fetch(state: &ServerState, cmd: &Value) -> Value {
    let id = match cmd.get("id").and_then(Value::as_str) {
        Some(s) => s.to_string(),
        None => return json!({"ok": false, "error": "id required"}),
    };
    let offset = cmd.get("offset").and_then(Value::as_u64).unwrap_or(0);
    let len = cmd
        .get("len")
        .and_then(Value::as_u64)
        .unwrap_or(MAX_FETCH_CHUNK_BYTES as u64)
        .min(MAX_FETCH_CHUNK_BYTES as u64);

    let entry_path = {
        let spool = state.snapshot_spool.lock().unwrap();
        match spool.get(&id) {
            Some(e) => (e.path.clone(), e.bytes),
            None => return json!({"ok": false, "error": format!("unknown snapshot id '{id}'")}),
        }
    };
    let (path, total_bytes) = entry_path;

    let mut file = match fs::File::open(&path) {
        Ok(f) => f,
        Err(e) => return json!({"ok": false, "error": format!("snapshot_fetch: open: {e}")}),
    };
    if let Err(e) = file.seek(SeekFrom::Start(offset)) {
        return json!({"ok": false, "error": format!("snapshot_fetch: seek: {e}")});
    }
    let mut buf = vec![0u8; len as usize];
    let n = match file.read(&mut buf) {
        Ok(n) => n,
        Err(e) => return json!({"ok": false, "error": format!("snapshot_fetch: read: {e}")}),
    };
    buf.truncate(n);

    json!({
        "ok": true,
        "id": id,
        "offset": offset,
        "chunk_b64": base64_encode(&buf),
        "chunk_len": n,
        "total_bytes": total_bytes,
    })
}

pub fn snapshot_list(state: &ServerState, _cmd: &Value) -> Value {
    let spool = state.snapshot_spool.lock().unwrap();
    let items: Vec<Value> = spool
        .iter()
        .map(|(id, e)| {
            json!({
                "id": id,
                "bytes": e.bytes,
                "duration_s": e.duration_s,
                "channels": e.channels,
                "sha256": e.sha256,
            })
        })
        .collect();
    json!({"ok": true, "snapshots": items})
}

pub fn snapshot_delete(state: &ServerState, cmd: &Value) -> Value {
    let id = match cmd.get("id").and_then(Value::as_str) {
        Some(s) => s.to_string(),
        None => return json!({"ok": false, "error": "id required"}),
    };
    let mut spool = state.snapshot_spool.lock().unwrap();
    match spool.remove(&id) {
        Some(entry) => {
            let _ = fs::remove_file(&entry.path);
            json!({"ok": true})
        }
        None => json!({"ok": false, "error": format!("unknown snapshot id '{id}'")}),
    }
}

/// Minimal base64 (standard alphabet, padded) — avoids pulling in the
/// `base64` crate for one call site. `snapshot_fetch` is the only
/// producer; `MAX_FETCH_CHUNK_BYTES` bounds the input size.
fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        out.push(ALPHABET[(b0 >> 2) as usize] as char);
        out.push(ALPHABET[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(b2 & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips_via_standard_decoder_shape() {
        // No decoder written daemon-side (clients decode), so verify
        // against known-answer vectors (RFC 4648 test vectors) instead.
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn ring_push_tick_caps_at_configured_length() {
        let mut ring = SnapshotRingState::new(
            48_000,
            vec![0, 1],
            10, // cap 10 samples for the test
            vec![(0, 1)],
            "Z".to_string(),
            "fast".to_string(),
            vec![None, None],
        );
        for _ in 0..5 {
            ring.push_tick(&[vec![1.0; 4], vec![2.0; 4]]);
        }
        assert_eq!(ring.channels[0].len(), 10, "ring must cap at 10 samples");
        assert_eq!(ring.channels[1].len(), 10);
    }

    /// AC #5 (ring correctness, wraparound): push distinguishable,
    /// monotonically-increasing sample values well past the cap and
    /// confirm the ring holds exactly the *newest* `cap` samples in
    /// order — not just the right length (the length-only test above),
    /// which would pass even if wraparound dropped from the wrong end
    /// or reordered samples.
    #[test]
    fn ring_wraparound_keeps_newest_samples_in_order() {
        let cap = 20;
        let mut ring = SnapshotRingState::new(
            48_000,
            vec![0],
            cap,
            vec![(0, 0)],
            "Z".to_string(),
            "fast".to_string(),
            vec![None],
        );
        // Push 0..100 in ticks of 7 (uneven tick size, like real capture
        // blocks) — well past `cap`, so this exercises wraparound
        // multiple times over, not just once.
        let total = 100;
        let mut pushed = 0usize;
        while pushed < total {
            let tick_len = 7.min(total - pushed);
            let tick: Vec<f32> = (pushed..pushed + tick_len).map(|v| v as f32).collect();
            ring.push_tick(&[tick]);
            pushed += tick_len;
        }
        let got: Vec<f32> = ring.channels[0].iter().copied().collect();
        let expected: Vec<f32> = ((total - cap)..total).map(|v| v as f32).collect();
        assert_eq!(
            got, expected,
            "ring must hold exactly the newest {cap} samples, in order"
        );
    }
}
