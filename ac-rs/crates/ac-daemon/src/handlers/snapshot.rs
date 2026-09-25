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
//! crashed session never outlives the next session's start. The spool is
//! always a daemon-owned leaf beneath `~/.local/state/ac/snapshots` (#433,
//! `ac_core::config::reset_snapshot_spool`); nothing else is ever emptied.

use std::collections::VecDeque;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Mutex;

use serde_json::{json, Value};

use ac_core::shared::calibration::Calibration;
use ac_core::snapshot::{ChannelMeta, SessionMeta, SnapshotMeta};

use crate::server::ServerState;

use super::wire::Problem;
use super::{MAX_SNAPSHOT_RING_BYTES, MAX_SNAPSHOT_RING_S};

/// A `snapshot_ring_s` that has passed [`RingSeconds::new`]: finite, > 0 and
/// at most [`MAX_SNAPSHOT_RING_S`] (#635). The only way to size a ring, so
/// no path — `setup`, a hand-edited `config.json`, a future caller — can
/// build one from an unchecked value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct RingSeconds(f64);

impl RingSeconds {
    /// The one validator. `NotPositive` for `0`, a negative or a non-finite
    /// value; `AtMost` above the ceiling. Refuses, never clamps: a ring
    /// holding less than the config says is a value quietly doing
    /// something other than what it states.
    pub(crate) fn new(x: f64) -> Result<Self, Problem> {
        if !(x.is_finite() && x > 0.0) {
            return Err(Problem::NotPositive);
        }
        if x > f64::from(MAX_SNAPSHOT_RING_S) {
            return Err(Problem::AtMost {
                limit: MAX_SNAPSHOT_RING_S,
                unit: "s",
            });
        }
        Ok(Self(x))
    }

    pub(crate) fn get(self) -> f64 {
        self.0
    }
}

/// Ring capacity per channel, in samples: `ring_s × sr`, rounded. Converted
/// with an explicit range check, not `as usize` saturation. With the
/// ceiling the product is at most 300 × `u32::MAX` ≈ 1.3e12 samples
/// (≈ 5.2e12 B of `f32`), which fits `usize` on the 64-bit hosts the
/// daemon runs on; the assertion states that rather than assuming it.
pub(crate) fn ring_cap_samples(ring_s: RingSeconds, sr: u32) -> usize {
    let samples = (ring_s.get() * f64::from(sr)).round();
    assert!(
        samples >= 0.0 && samples <= usize::MAX as f64,
        "ring of {} s at {sr} Hz does not fit usize",
        ring_s.get()
    );
    let samples = samples as usize;
    assert!(
        samples.checked_mul(std::mem::size_of::<f32>()).is_some(),
        "ring of {samples} samples overflows its byte count"
    );
    samples
}

/// Bytes the ring holds per sample per channel, as measured from the real
/// allocation (`VecDeque::capacity` after steady state), not from the wire
/// format. See [`MAX_SNAPSHOT_RING_BYTES`] for the measurement.
pub(crate) const RING_BYTES_PER_SAMPLE: u128 = 4;

/// A snapshot ring whose byte count is above [`MAX_SNAPSHOT_RING_BYTES`]
/// (#642): the three factors, and the product they reach.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RingTooLarge {
    pub(crate) needs_bytes: u128,
    pub(crate) channels: usize,
    pub(crate) ring_s: RingSeconds,
    pub(crate) sr: u32,
}

impl RingTooLarge {
    /// The refusal text under `headline` — `transfer not started` before the
    /// CTRL reply, `transfer stopped` after it. What arrived and the bound;
    /// it does not rank the factors, since the daemon cannot know which one
    /// the operator can change.
    pub(crate) fn refusal(&self, headline: &str) -> String {
        let needs = format_mb(self.needs_bytes);
        let ceiling = format_mb(u128::from(MAX_SNAPSHOT_RING_BYTES));
        let w = needs.len().max(ceiling.len());
        let needs = format!("{needs:>w$} MB");
        let ceiling = format!("{ceiling:>w$} MB");
        let channels = self.channels.to_string();
        let ring_s = format!("{} s", self.ring_s.get());
        let rate = format!("{} Hz", self.sr);
        super::wire::trailer_refusal(
            headline,
            "snapshot ring exceeds the memory ceiling",
            &[
                ("needs", &needs),
                ("ceiling", &ceiling),
                ("channels", &channels),
                ("snapshot_ring_s", &ring_s),
                ("rate", &rate),
                ("source", "config.json"),
            ],
        )
    }
}

/// `b` bytes as MB (10⁶ B) with six decimals, from integers: exact to the
/// byte, so no over-ceiling count can print equal to the ceiling.
fn format_mb(b: u128) -> String {
    format!("{}.{:06}", b / 1_000_000, b % 1_000_000)
}

/// The bytes a ring of `channels` channels, `ring_s` long at `sr`, reserves:
/// `channels × ring_cap_samples × RING_BYTES_PER_SAMPLE`. Takes its sample
/// count from [`ring_cap_samples`], the same function [`SnapshotRingState::start`]
/// sizes the allocation with, so the check and the allocation cannot drift
/// apart. `u128`: `usize × usize × 4` cannot overflow it.
pub(crate) fn ring_bytes(channels: usize, ring_s: RingSeconds, sr: u32) -> u128 {
    channels as u128 * ring_cap_samples(ring_s, sr) as u128 * RING_BYTES_PER_SAMPLE
}

/// `Ok(bytes)` when the ring fits [`MAX_SNAPSHOT_RING_BYTES`] — a ring
/// exactly at the ceiling is accepted — else the refusal's inputs (#642).
pub(crate) fn check_ring_bytes(
    channels: usize,
    ring_s: RingSeconds,
    sr: u32,
) -> Result<u128, RingTooLarge> {
    let needs_bytes = ring_bytes(channels, ring_s, sr);
    if needs_bytes > u128::from(MAX_SNAPSHOT_RING_BYTES) {
        return Err(RingTooLarge {
            needs_bytes,
            channels,
            ring_s,
            sr,
        });
    }
    Ok(needs_bytes)
}

/// Max bytes returned per `snapshot_fetch` chunk (pre-base64; base64
/// inflates by ~4/3, so the JSON reply payload is ≈341 KB at this cap).
/// Chosen for CTRL sanity (deliverable 3) — small enough that one chunk
/// is a fast REQ/REP round-trip even over a slow remote link (D6).
pub const MAX_FETCH_CHUNK_BYTES: usize = 256 * 1024;

/// `snapshot` refusal while a `transfer_stream` session exists but its ring
/// is still pending — between the CTRL ok reply and the audio engine's start
/// completing (#188). Distinct from the no-session refusal on purpose: the
/// session is there, it just has no sample rate to encode yet.
pub const SESSION_STARTING_ERROR: &str =
    "transfer_stream session starting — audio engine start pending";

/// Live, growing state for one `transfer_stream` session's snapshot ring.
/// Lives behind `ServerState::snapshot_ring`; the worker thread mutates
/// it every capture tick, the `snapshot` CTRL handler reads it on demand.
///
/// Two states (#188). The handler publishes a *pending* ring
/// ([`SnapshotRingState::pending`]) before the worker exists, because the
/// sample rate — and so the cap in samples — is only known once the engine
/// has started; the worker then calls [`SnapshotRingState::start`]. A
/// pending ring holds no samples and cannot produce a snapshot.
pub struct SnapshotRingState {
    /// `0` while pending.
    pub sr: u32,
    /// Session input-channel index per ring position (matches the order
    /// `bufs` arrives from `capture_multi`/`unique_ports`).
    pub unique_chans: Vec<u32>,
    pub channels: Vec<VecDeque<f32>>,
    /// Cap per channel, in samples (`snapshot_ring_s × sr`); `0` while
    /// pending.
    cap_samples: usize,
    /// `Some(snapshot_ring_s)` while pending, `None` once started.
    pending_ring_s: Option<RingSeconds>,
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
    /// An already-started ring with a known `sr` and cap.
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
        // One entry per pair from the start: the worker pushes samples
        // before it first syncs delays, and a `snapshot` landing between
        // the two must still write one delay per pair (#435).
        let delay_samples = vec![None; pairs.len()];
        Self {
            sr,
            unique_chans,
            channels: (0..n)
                .map(|_| VecDeque::with_capacity(cap_samples))
                .collect(),
            cap_samples,
            pending_ring_s: None,
            pairs,
            delay_samples,
            weighting_tag,
            integration_tag,
            unique_cals,
        }
    }

    /// A ring published before the engine has reported its sample rate
    /// (#188): everything the session plan knows, with the retention held
    /// in seconds until [`start`](Self::start) turns it into samples.
    pub(crate) fn pending(
        ring_s: RingSeconds,
        unique_chans: Vec<u32>,
        pairs: Vec<(u32, u32)>,
        weighting_tag: String,
        integration_tag: String,
        unique_cals: Vec<Option<Calibration>>,
    ) -> Self {
        let mut ring = Self::new(
            0,
            unique_chans,
            0,
            pairs,
            weighting_tag,
            integration_tag,
            unique_cals,
        );
        ring.pending_ring_s = Some(ring_s);
        ring
    }

    /// Fix the sample rate and allocate the cap; from here on `snapshot`
    /// can encode this ring. A no-op on a ring that is already started.
    ///
    /// `Err` when the ring at this `sr` would exceed
    /// [`MAX_SNAPSHOT_RING_BYTES`] (#642): nothing is allocated and the ring
    /// stays pending. The launch checked the same product against the rate
    /// the engine was probed at; this check holds whichever rate the engine
    /// actually started at.
    pub(crate) fn start(&mut self, sr: u32) -> Result<(), RingTooLarge> {
        let Some(ring_s) = self.pending_ring_s else {
            return Ok(());
        };
        check_ring_bytes(self.channels.len(), ring_s, sr)?;
        self.pending_ring_s = None;
        self.sr = sr;
        self.cap_samples = ring_cap_samples(ring_s, sr);
        for ch in &mut self.channels {
            ch.reserve_exact(self.cap_samples);
        }
        Ok(())
    }

    /// `false` between [`pending`](Self::pending) and [`start`](Self::start).
    pub fn is_started(&self) -> bool {
        self.pending_ring_s.is_none()
    }

    /// Push one tick's captured samples (same shape as `capture_multi`'s
    /// return) into the ring, keeping the newest `cap_samples`.
    ///
    /// Drops from the front *before* extending, and keeps only the newest
    /// `cap_samples` of an oversized tick, so `len` never passes the cap and
    /// the allocation never grows past what `start` reserved (#642). The
    /// earlier extend-then-pop order pushed `len` over the cap on the first
    /// tick after the ring filled, and `VecDeque` then doubled its capacity
    /// for the rest of the session.
    pub fn push_tick(&mut self, bufs: &[Vec<f32>]) {
        let cap = self.cap_samples;
        for (i, buf) in bufs.iter().enumerate() {
            if i >= self.channels.len() {
                break;
            }
            let ring = &mut self.channels[i];
            let tail = &buf[buf.len() - buf.len().min(cap)..];
            let overflow = (ring.len() + tail.len()).saturating_sub(cap);
            ring.drain(..overflow);
            ring.extend(tail.iter().copied());
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
    ///
    /// `None` on a pending ring: with no sample rate there is nothing to
    /// encode (#188).
    fn snapshot_meta_and_channels(
        &self,
        daemon_version: &str,
    ) -> Option<(SnapshotMeta, Vec<Vec<f32>>)> {
        if !self.is_started() {
            return None;
        }
        let channels: Vec<Vec<f32>> = self
            .channels
            .iter()
            .map(|d| d.iter().copied().collect())
            .collect();
        let n_frames = channels.first().map(Vec::len).unwrap_or(0);
        let duration_s = n_frames as f64 / self.sr as f64;

        // Role naming: walking pairs in order, a channel takes the role of
        // its first occurrence — "meas_<pair index>" if that occurrence is
        // a meas leg, "ref" if it is a ref leg — and later occurrences do
        // not rename it. A channel used as meas in one pair and ref in
        // another (unusual but not forbidden) therefore keeps whichever
        // role comes first: pairs [[0,1],[1,2]] name channel 1 "ref".
        // Roles are labels, not unique keys; `input_channel` is the key.
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
                // Filled by the `snapshot` handler from the verdicts the
                // session applied at start (#466); `cal` is already gated.
                voltage_check: None,
                // Computed by `write_acsnap` from the audio it encodes.
                stream_sha256: None,
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
        Some((meta, channels))
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

/// The configured spool leaf, confined and prepared (#433): created with
/// its ownership marker if absent, refused if it is not daemon-owned.
fn spool_dir(state: &ServerState) -> Result<PathBuf, ac_core::config::SpoolRejection> {
    let cfg = state.cfg.lock().unwrap().clone();
    let leaf = ac_core::config::snapshot_spool_dir(&cfg)?;
    ac_core::config::prepare_snapshot_spool(&ac_core::config::snapshot_spool_root(), &leaf)?;
    Ok(leaf)
}

/// Empty the spool leaf. Called at the start of every `transfer_stream`
/// session (crash-safety fallback — see module doc). Only a daemon-owned
/// leaf beneath the spool root is ever emptied; ownership is re-checked
/// here, immediately before removal, and a refusal removes nothing (#433).
/// Takes the resolved directory and spool map directly (not
/// `&ServerState`) so it's callable from a `'static` worker closure,
/// which only ever holds cloned `Arc`s / owned values, never a
/// `&ServerState` reference (same discipline every other worker in this
/// codebase already follows).
pub fn reset_spool_dir(
    dir: &std::path::Path,
    spool: &Mutex<std::collections::HashMap<String, SpoolEntry>>,
) -> Result<(), ac_core::config::SpoolRejection> {
    spool.lock().unwrap().clear();
    ac_core::config::reset_snapshot_spool(&ac_core::config::snapshot_spool_root(), dir)
}

/// Delete every spooled file from this session. Called when the
/// `transfer_stream` worker stops — including when it panicked, so a
/// poisoned lock is accepted rather than unwrapped (a panic here, during
/// unwinding, would abort the daemon; #432).
pub fn clear_spool(spool: &Mutex<std::collections::HashMap<String, SpoolEntry>>) {
    let mut spool = spool
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
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
    // A pending ring (#188) means the session exists but the engine has not
    // reported its sample rate yet; checked under the same lock `start`
    // takes, so there is no window between the check and the read.
    let (meta, channels) = {
        let ring = ring_handle.lock().unwrap();
        match ring.snapshot_meta_and_channels(daemon_version) {
            Some(v) => v,
            None => return json!({"ok": false, "error": SESSION_STARTING_ERROR}),
        }
    };
    let mut meta = meta;
    for ch in meta.per_channel.iter_mut() {
        ch.voltage_check = crate::handlers::checks::transfer_applied(state, ch.input_channel);
    }
    let (bytes, sha256, duration_s, channels) = match build_acsnap(&meta, &channels) {
        Ok(v) => v,
        Err(e) => return json!({"ok": false, "error": format!("snapshot: {e}")}),
    };

    let dir = match spool_dir(state) {
        Ok(dir) => dir,
        Err(r) => return json!({"ok": false, "error": r.message()}),
    };
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

    /// #435: the worker pushes samples before it first syncs delays. A
    /// `snapshot` taken in that gap must still carry one delay per pair,
    /// or `write_acsnap` refuses the metadata.
    #[test]
    fn snapshot_before_first_delay_sync_has_one_delay_per_pair() {
        let mut ring = SnapshotRingState::new(
            48_000,
            vec![0, 1, 2],
            1_000,
            vec![(0, 1), (0, 2)],
            "Z".to_string(),
            "fast".to_string(),
            vec![None, None, None],
        );
        ring.push_tick(&[vec![0.1; 64], vec![0.2; 64], vec![0.3; 64]]);
        let (meta, channels) = ring
            .snapshot_meta_and_channels("test")
            .expect("a ring built with `new` is started");
        assert_eq!(
            meta.session.delay_samples.len(),
            meta.session.pairs.len(),
            "delay_samples must have one entry per pair before the first sync"
        );
        build_acsnap(&meta, &channels).expect("first-tick snapshot must write");
    }

    /// #188: a pending ring refuses to produce snapshot metadata, tolerates
    /// a tick without panicking, and once started carries the `sr` it was
    /// given and caps at `ring_s × sr`.
    #[test]
    fn pending_ring_yields_no_snapshot_until_started() {
        let mut ring = SnapshotRingState::pending(
            RingSeconds::new(0.5).unwrap(),
            vec![0, 1],
            vec![(0, 1)],
            "Z".to_string(),
            "fast".to_string(),
            vec![None, None],
        );
        assert!(!ring.is_started());
        ring.push_tick(&[vec![0.1; 8], vec![0.2; 8]]);
        assert!(ring.snapshot_meta_and_channels("test").is_none());

        ring.start(100).expect("a 50-sample ring fits the ceiling");
        assert!(ring.is_started());
        for _ in 0..20 {
            ring.push_tick(&[vec![0.1; 8], vec![0.2; 8]]);
        }
        assert_eq!(ring.channels[0].len(), 50, "cap must be ring_s × sr");
        let (meta, channels) = ring
            .snapshot_meta_and_channels("test")
            .expect("a started ring yields a snapshot");
        assert_eq!(meta.sr, 100);
        build_acsnap(&meta, &channels).expect("started ring snapshot must write");
    }

    /// #635: the ceiling is accepted, anything above it or outside
    /// `(0, ∞)` is refused, and nothing is clamped.
    #[test]
    fn ring_seconds_bounds() {
        let max = f64::from(MAX_SNAPSHOT_RING_S);
        assert_eq!(RingSeconds::new(max).map(RingSeconds::get), Ok(max));
        assert_eq!(RingSeconds::new(30.0).map(RingSeconds::get), Ok(30.0));
        let at_most = Problem::AtMost {
            limit: MAX_SNAPSHOT_RING_S,
            unit: "s",
        };
        for x in [max + 0.001, 1e6, 1e300, f64::MAX] {
            assert_eq!(RingSeconds::new(x), Err(at_most.clone()), "{x}");
        }
        for x in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(RingSeconds::new(x), Err(Problem::NotPositive), "{x}");
        }
    }

    /// #635: the widest accepted ring at the widest representable rate
    /// converts without saturating — the same arithmetic the raw
    /// `as usize` cast used to hide.
    #[test]
    fn ring_cap_samples_at_ceiling_and_max_rate() {
        let ceiling = RingSeconds::new(f64::from(MAX_SNAPSHOT_RING_S)).unwrap();
        let got = ring_cap_samples(ceiling, u32::MAX);
        assert_eq!(
            got as u64,
            u64::from(MAX_SNAPSHOT_RING_S) * u64::from(u32::MAX)
        );
        assert_eq!(ring_cap_samples(RingSeconds::new(0.5).unwrap(), 100), 50);
        assert_eq!(
            ring_cap_samples(ceiling, 192_000),
            MAX_SNAPSHOT_RING_S as usize * 192_000
        );
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

    /// The pre-#642 `push_tick` body, kept here only to measure what it
    /// did to the allocation: extend first, then pop down to the cap.
    fn rejected_push_tick(ring: &mut VecDeque<f32>, buf: &[f32], cap: usize) {
        ring.extend(buf.iter().copied());
        while ring.len() > cap {
            ring.pop_front();
        }
    }

    /// Uneven ticks (4099 samples, like real capture blocks of no fixed
    /// size) totalling three times `cap`, so the ring wraps repeatedly.
    fn ticks(cap: usize) -> Vec<Vec<f32>> {
        let total = 3 * cap;
        (0..total)
            .step_by(4099)
            .map(|start| {
                (start..(start + 4099).min(total))
                    .map(|v| v as f32)
                    .collect()
            })
            .collect()
    }

    /// #642 criterion 1: bytes per channel-second of the ring, measured from
    /// the real allocation (`VecDeque::capacity` after steady state) at 48 kHz
    /// and 192 kHz, against the rejected extend-then-pop order computed in
    /// the same test. Run with `--nocapture` to print the figures quoted on
    /// `MAX_SNAPSHOT_RING_BYTES`.
    #[test]
    fn ring_footprint_measured_at_48k_and_192k() {
        let ring_s = RingSeconds::new(1.0).unwrap();
        for sr in [48_000u32, 192_000] {
            let mut ring = SnapshotRingState::pending(
                ring_s,
                vec![0, 1],
                vec![(0, 1)],
                "Z".to_string(),
                "fast".to_string(),
                vec![None, None],
            );
            ring.start(sr).expect("a 1 s ring fits the ceiling");
            let cap = ring_cap_samples(ring_s, sr);
            let reserved = ring.channels[0].capacity();

            let mut rejected: VecDeque<f32> = VecDeque::new();
            rejected.reserve(cap);
            for t in ticks(cap) {
                ring.push_tick(&[t.clone(), t.clone()]);
                rejected_push_tick(&mut rejected, &t, cap);
            }

            let size = std::mem::size_of::<f32>();
            let per_ch_s = |capacity: usize| (capacity * size) as f64 / ring_s.get();
            eprintln!(
                "sr {sr}: reserved {} B/ch·s, steady {} B/ch·s; rejected order steady {} B/ch·s",
                per_ch_s(reserved),
                per_ch_s(ring.channels[0].capacity()),
                per_ch_s(rejected.capacity()),
            );
            for ch in &ring.channels {
                assert_eq!(ch.len(), cap);
                assert_eq!(
                    ch.capacity(),
                    reserved,
                    "the ring must not grow past what start reserved"
                );
            }
            assert_eq!(
                (reserved * size) as u128,
                cap as u128 * RING_BYTES_PER_SAMPLE,
                "RING_BYTES_PER_SAMPLE must match the measured allocation"
            );
            assert!(
                rejected.capacity() > cap,
                "the rejected order is expected to grow past the cap"
            );
            assert_eq!(
                ring.channels[0].iter().copied().collect::<Vec<_>>(),
                rejected.iter().copied().collect::<Vec<_>>(),
                "the fix must keep exactly what the rejected order kept"
            );
        }
    }

    /// #642 coupled constants: the bytes `start` reserves across every
    /// channel equal the count the ceiling check compares.
    #[test]
    fn start_reserves_exactly_the_checked_bytes() {
        let ring_s = RingSeconds::new(0.37).unwrap();
        let chans = vec![0, 3, 5];
        let mut ring = SnapshotRingState::pending(
            ring_s,
            chans.clone(),
            vec![(0, 3), (5, 3)],
            "Z".to_string(),
            "fast".to_string(),
            vec![None, None, None],
        );
        let checked = check_ring_bytes(chans.len(), ring_s, 96_000).unwrap();
        ring.start(96_000).unwrap();
        let reserved: u128 = ring
            .channels
            .iter()
            .map(|c| (c.capacity() * std::mem::size_of::<f32>()) as u128)
            .sum();
        assert_eq!(reserved, checked);
    }

    /// #642: an oversized tick keeps its newest `cap` samples, in order, and
    /// does not grow the allocation.
    #[test]
    fn oversized_tick_keeps_newest_cap_samples() {
        let mut ring = SnapshotRingState::pending(
            RingSeconds::new(0.1).unwrap(),
            vec![0],
            vec![(0, 0)],
            "Z".to_string(),
            "fast".to_string(),
            vec![None],
        );
        ring.start(100).unwrap();
        let reserved = ring.channels[0].capacity();
        ring.push_tick(&[vec![-1.0; 4]]);
        ring.push_tick(&[(0..25).map(|v| v as f32).collect()]);
        let got: Vec<f32> = ring.channels[0].iter().copied().collect();
        assert_eq!(got, (15..25).map(|v| v as f32).collect::<Vec<_>>());
        assert_eq!(ring.channels[0].capacity(), reserved);
    }

    /// #642: a ring over the ceiling is refused by `start` before anything is
    /// allocated, and stays pending.
    #[test]
    fn start_over_ceiling_allocates_nothing() {
        let ring_s = RingSeconds::new(289.3519).unwrap();
        let mut ring = SnapshotRingState::pending(
            ring_s,
            (0..18).collect(),
            vec![(0, 1)],
            "Z".to_string(),
            "fast".to_string(),
            vec![None; 18],
        );
        let err = ring.start(48_000).unwrap_err();
        assert_eq!(err.needs_bytes, 1_000_000_152);
        assert_eq!((err.channels, err.sr), (18, 48_000));
        assert!(!ring.is_started(), "a refused ring stays pending");
        for ch in &ring.channels {
            assert_eq!(ch.capacity(), 0, "refused before allocation");
        }
        ring.push_tick(&vec![vec![0.5; 64]; 18]);
        assert_eq!(ring.channels[0].capacity(), 0, "a pending ring never grows");
    }

    /// #642 criterion 6: the bound is reachable from both sides — a ring of
    /// exactly `MAX_SNAPSHOT_RING_BYTES` is accepted, one sample per channel
    /// more is refused.
    #[test]
    fn ceiling_accepts_exactly_the_limit_and_refuses_one_sample_more() {
        // 20 ch × 12 500 000 samples × 4 B = 10⁹ B.
        let at = RingSeconds::new(12_500_000.0 / 48_000.0).unwrap();
        assert_eq!(ring_cap_samples(at, 48_000), 12_500_000);
        assert_eq!(
            check_ring_bytes(20, at, 48_000),
            Ok(u128::from(MAX_SNAPSHOT_RING_BYTES))
        );
        let over = RingSeconds::new(12_500_001.0 / 48_000.0).unwrap();
        assert_eq!(ring_cap_samples(over, 48_000), 12_500_001);
        let err = check_ring_bytes(20, over, 48_000).unwrap_err();
        assert_eq!(err.needs_bytes, 1_000_000_080);
    }

    /// #642: the refusal blocks as UX specified them, character for
    /// character — just over the ceiling, a typical over, and the widest.
    #[test]
    fn ring_refusal_text_matches_ux() {
        let refuse = |channels: usize, ring_s: f64, sr: u32| {
            check_ring_bytes(channels, RingSeconds::new(ring_s).unwrap(), sr).unwrap_err()
        };
        assert_eq!(
            refuse(18, 289.3519, 48_000).refusal("transfer not started"),
            "transfer not started \u{2014} snapshot ring exceeds the memory ceiling\n\
             \x20        needs            1000.000152 MB\n\
             \x20        ceiling          1000.000000 MB\n\
             \x20        channels         18\n\
             \x20        snapshot_ring_s  289.3519 s\n\
             \x20        rate             48000 Hz\n\
             \x20        source           config.json"
        );
        assert_eq!(
            refuse(6, 300.0, 192_000).refusal("transfer stopped"),
            "transfer stopped \u{2014} snapshot ring exceeds the memory ceiling\n\
             \x20        needs            1382.400000 MB\n\
             \x20        ceiling          1000.000000 MB\n\
             \x20        channels         6\n\
             \x20        snapshot_ring_s  300 s\n\
             \x20        rate             192000 Hz\n\
             \x20        source           config.json"
        );
        let widest = refuse(64, 300.0, 192_000).refusal("transfer not started");
        assert_eq!(
            widest,
            "transfer not started \u{2014} snapshot ring exceeds the memory ceiling\n\
             \x20        needs            14745.600000 MB\n\
             \x20        ceiling           1000.000000 MB\n\
             \x20        channels         64\n\
             \x20        snapshot_ring_s  300 s\n\
             \x20        rate             192000 Hz\n\
             \x20        source           config.json"
        );
        for line in widest.lines() {
            assert!(line.chars().count() <= 80, "{line}");
        }
    }
}
