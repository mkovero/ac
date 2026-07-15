# handoff-snapshot-backend — M1: ring, `.acsnap`, offline derivation

Parent plan: `ui-plan.md` (D4–D8, D11, D17; invariant I-B).
Base: `m0-transfer-frame-v2` post-QA-merge. One PR. Daemon + core only —
no UI, no scene.

## Goal

A running transfer session can be frozen on command into a single
self-contained `.acsnap` file, fetched over CTRL from anywhere, and
re-derived offline by ac-core into the **same numbers the daemon shipped
live** — using the identical functions, proven by parity test.

## Deliverables

### 1. Capture ring (daemon)

- Per-session ring buffer of **raw pre-processing f32 samples**, all
  session channels (meas + ref), as delivered by the audio backend —
  before gain, calibration, weighting, or any DSP touches them (D3/D4).
- Default length 30 s, config key (`snapshot_ring_s` or similar in
  config.jsonc), bounded allocation at session start.
- Lock-free or coarse-locked such that the audio callback path is never
  blocked (same discipline as existing ring usage in the engine).

### 2. `snapshot` CTRL command

- Valid only while a transfer session runs; otherwise `{"ok": false}`.
- Dumps ring contents + provenance to a daemon-local `.acsnap` file
  (temp/spool dir, config-overridable), returns
  `{"ok": true, "id", "bytes", "duration_s", "channels", "sha256"}`.
- The returned `id` is the fetch handle. **No filesystem path in the
  reply** — remote is first-class (D6); the UI must never learn or need
  a daemon-side path.

### 3. Chunked fetch over CTRL

- `snapshot_fetch {id, offset, len}` → base64 chunk + `total_bytes` on
  REQ-REP (CTRL). DATA socket stays pure frames.
- `snapshot_list` / `snapshot_delete {id}` for hygiene; daemon prunes
  spool on session end or bounded retention — pick one, document it.
- Chunk size chosen for CTRL sanity (≤ 256 KB per rep); client
  reassembles and verifies `sha256`.

### 4. `.acsnap` format (ac-core owns read/write)

Single zip: `meta.json` + `audio.flac`.

- **FLAC:** one multichannel stream, 24-bit. Conversion f32 → i24 by
  scale 2²³ with saturation; samples originating in 24-bit converters
  round-trip bit-exact. Synthetic/fake-audio f32 that doesn't sit on the
  i24 grid quantizes at ≈ −138 dBFS — the I-B tolerance derivation must
  account for exactly this floor and nothing more.
- **meta.json:** `format_version` (start at 1; future 32-bit FLAC bumps
  it), `sr`, channel map (FLAC channel → session role: meas_N / ref),
  per-channel `weighting`/`integration` tags at capture (**string-
  identical vocabulary to M0 frame tags**), full 3-layer `Calibration`
  snapshot per channel, session config (pairs, delay, nperseg in effect),
  capture UTC timestamp, daemon version, ring duration.
- Self-containment is a hard requirement: reprocessing must need **zero
  external state** (D5).

### 5. Offline derivation (ac-core)

- New module (proposal: `ac_core::snapshot`): open `.acsnap`, expose raw
  channels + provenance, and derive on demand: calibrated spectra
  (band-power columns, same aggregate path as M0), SPL (via
  `spl_level` from M0), H1/coherence
  (`h1_estimate_with_delay`), under caller-chosen weighting /
  integration / FFT params (D10/D11 — edit-time freedom).
- **No reimplementation**: derivation calls the same functions the
  daemon's live path calls. Any new function needed by both sides gets
  lifted, not duplicated.

## Acceptance criteria (falsifiable)

1. **I-B parity (the gate):** under `--fake-audio`, run a transfer
   session, record live frames, trigger `snapshot`, fetch, reprocess via
   `ac_core::snapshot` with the unchanged capture-time params. Derived
   `meas_spectrum`, `ref_spectrum`, `spl`, `|H|`, phase, coherence match
   the live frames covering the same time window within a tolerance
   **derived from** (a) the −138 dB i24 quantization floor, (b) Welch
   segment-boundary alignment between live emission and ring extraction.
   State both terms in the test.
2. **Fetch integrity:** reassembled file is byte-identical to the
   daemon-side file (sha256), across ≥ 2 chunk sizes; protocol carries no
   daemon filesystem paths (assert on reply schema).
3. **Self-containment:** a checked-in `.acsnap` fixture reprocesses in an
   ac-core unit test with no daemon, no audio backend, no config file.
   (This fixture is deliberately also M2's display-truth substrate.)
4. **FLAC round-trip:** i24-grid input → bit-exact after write/read;
   off-grid f32 → error bounded by 1 LSB.
5. **Ring correctness:** with a time-marked fake stimulus (e.g. frequency
   step at a known instant), a snapshot taken T seconds after the step
   places the step at the correct offset; wraparound covered by taking a
   snapshot after > ring-length of runtime.
6. **Edges:** `snapshot` with no session → `{"ok": false}`; fetch of
   unknown id → error; `format_version` present and = 1.
7. **Docs:** ZMQ.md gains `snapshot` / `snapshot_fetch` / `snapshot_list`
   / `snapshot_delete`; `.acsnap` schema documented (new `SNAPSHOT.md` or
   ZMQ.md section — implementer's choice, architect confirms placement).
8. Workspace test/clippy/fmt clean; zero edits to existing assertions.

## Out of scope (hard fence)

- UI, `ac-scene`, `ac-view`, any rendering or viewer (M2/M3).
- Snapshot *editing* or re-export — read + derive only.
- CLI snapshot commands (D17).
- Overlays, Tier 1 interaction of any kind (D15).
- Compression tuning, binary wire framing, streaming the ring over DATA.
- Any change to M0 frame fields or monitor path.

## Routing

Architect: CTRL protocol additions + `.acsnap` schema (contract
surfaces). QA: criteria 1, 4, 5 (measurement invariants; tolerance
derivations). No UX gate.

---

## Architect review (design-approved, one verify-first spike)

Reviewed against actual current code (`.agents/architect.md`'s module map
is stale, same caveat as M0's addendum). Grounded in:
`ac-daemon/src/audio/jack_backend.rs` (existing ring precedent),
`ac-daemon/src/server.rs` (`ServerState` shared-state pattern),
`ac-daemon/src/workers.rs` (`cmd_group`/busy-guard table),
`ac-core/src/shared/calibration.rs` (already `Serialize`/`Deserialize`),
`Cargo.lock` (zero FLAC/zip/sha256 deps anywhere in the workspace today),
`tests/fixtures/` (repo-root convention, established by M0's
`fixtures-spectrum-hf-garbage`).

### decision 1 — the ring is simpler than the spec's phrasing suggests

Deliverable 1's "lock-free or coarse-locked such that the audio callback
path is never blocked" reads as if the new 30 s snapshot ring sits in the
JACK RT callback. It shouldn't, and doesn't need to: `transfer_stream`'s
worker already receives raw samples via `eng.capture_multi(chunk_secs)`
**on the worker thread**, one step removed from the RT callback — the
RT-safety problem is already solved at a lower layer
(`jack_backend.rs`'s existing `HeapRb<f32>` SPSC rings, producer in the
RT callback, consumer on the worker thread). The snapshot ring only
needs to consume from the same point the existing 2.5 s H1 sliding
window already does (`transfer.rs`'s `rings: Vec<Vec<f32>>`,
`r.extend_from_slice(buf)` per tick) — a **second, larger** bounded
buffer fed at the same call site, entirely on the worker thread.

**Decision:** a plain `VecDeque<f32>` per channel (same pattern
`monitor.rs` already uses for its CWT/CQT/reassigned rings), capped at
`snapshot_ring_s × sr` samples, wrapped in `Arc<Mutex<...>>` for
cross-thread read access (decision 2) — not `ringbuf`'s SPSC machinery,
which solves a problem this layer doesn't have. Simpler code, same
correctness.

### decision 2 — cross-thread handle: `ServerState` field, existing pattern

`snapshot` runs on the CTRL/REP thread; the ring lives inside
`transfer_stream`'s worker closure. `ServerState` already has the exact
shape needed for this (`dut_reply_tx: Arc<Mutex<Option<Sender<()>>>>`,
`cal_reply_tx: Arc<Mutex<Option<Sender<Option<f64>>>>>` — both
"populated when a worker starts, read/cleared elsewhere" fields).

**Decision:** add `snapshot_ring: Arc<Mutex<Option<Arc<SnapshotRing>>>>`
(name TBD) to `ServerState`, populated by `transfer_stream` at worker
start, left `None` otherwise. The `snapshot` handler's "no session
running" check (AC #6) is then just `.lock().unwrap().is_none()` — no
new state-tracking mechanism needed.

### decision 3 — concurrency: no busy-guard entry, by design

`workers.rs::cmd_group` is a closed match; anything absent returns
`None`, and `check_busy` short-circuits to "not gated" for `None` groups
(confirmed in code — this is exactly how `get_calibration`/`status`/
`devices` already work, zero special-casing). `snapshot` /
`snapshot_fetch` / `snapshot_list` / `snapshot_delete` do not spawn
workers and don't touch audio I/O — they read/write shared state and a
spool file.

**Decision:** do not add these four commands to `cmd_group`'s match
table. They run ungated, exactly like `get_calibration` today.
`snapshot`'s own "only while a transfer session runs" rule (deliverable
2) is enforced by decision 2's state check, not the busy-guard.

### decision 4 — FLAC/zip/sha256 crates: one verify-first spike, not a human decision

Not contestable in the architect-review sense (no two viable designs to
weigh) but genuinely unresolved — **zero** FLAC/zip/sha256 crates exist
anywhere in the workspace today (`Cargo.lock` confirms), so this handoff
is implicitly asking for 2-3 new dependencies the spec never names.
Per D8, `ac-core` will be linked directly by `ac-view` (M3) — any FLAC
crate needing a system C library (`libflac` via an `-sys` crate) burdens
that future build on whatever platform `ac-view` ships on. **Constraint:
pure-Rust only for the FLAC path, no system lib.**

**Decision:**
- `zip` crate for the container — uncontested, standard choice, pure
  Rust, read+write.
- `sha2` crate for the `sha256` field — uncontested, standard.
- FLAC: **developer must verify before writing any snapshot code**
  whether a current pure-Rust crate combination provides both 24-bit
  multichannel *encode* (daemon side) and *decode* (ac-core side, needed
  for AC #3's "no daemon, no audio backend" fixture reprocessing).
  `flacenc` (encode-only, last known pure-Rust) + `claxon` (decode-only,
  pure Rust) is the working hypothesis, but crate state moves — spike
  first, confirm 24-bit multichannel actually round-trips (not just
  16-bit mono, the common-path test most crates optimize for), *then*
  proceed. If no pure-Rust story holds up, stop and flag back — that's
  a `needs-discussion`-worthy finding, not something to silently route
  around with a system dependency.

### decision 5 — I-B parity test needs a live-frame↔ring-window correlation, without touching M0's frame shape

AC #1 requires derived values to "match the live frames covering the
same time window" — but `transfer_stream` frames carry no timestamp or
sample-position field (confirmed against M0's frozen `ZMQ.md` shape),
and this handoff's own out-of-scope fence forbids changing M0 frame
fields. Without *some* correlation mechanism, "the same time window" is
unverifiable — the test would be comparing two arbitrary, unrelated
captures.

**Decision:** correlate via wall-clock, done entirely in the **test
harness**, not the wire protocol. Record system time when each live
frame arrives (client-side, already possible — nothing new needed) and
system time when `snapshot` is triggered; `meta.json`'s `capture UTC
timestamp` + ring duration then let the test compute how many seconds
before the snapshot-trigger moment a given live frame's capture
happened, and extract the matching sample range from the ring
(`elapsed_s × sr`) for reprocessing. No new field on `transfer_stream`,
no fence violation.

### decision 6 — `.acsnap` schema: confirmed complete, one simplification

`meta.json`'s field list (deliverable 4) is otherwise sufficient. One
simplification worth stating explicitly: M0's spectrum column grid
(`spec_freqs`, 48 cols/octave 20 Hz-Nyquist) is a **pure function of
`sr`** (`transfer_spectrum_n_columns`, no other inputs) — `sr` is
already a required `meta.json` field, so no separate grid-parameter
field is needed for AC #1's capture-time reproduction. Don't add one.

### affected modules

- `ac-daemon/src/handlers/snapshot.rs` (new) — `snapshot`,
  `snapshot_fetch`, `snapshot_list`, `snapshot_delete`. Register in
  `handlers/mod.rs` alongside the existing `mod transfer;` /
  `pub use transfer::{...}` pattern.
- `ac-daemon/src/handlers/transfer.rs` — worker gains the snapshot ring
  (decision 1) and populates `ServerState.snapshot_ring` (decision 2) at
  start, clears at stop.
- `ac-daemon/src/server.rs` — new `ServerState` field (decision 2).
- `ac-daemon/src/workers.rs` — **no change** (decision 3 is "don't add
  an entry," not a code change).
- `ac-core/src/snapshot/` (new, as proposed) — `.acsnap` read/write,
  offline derivation. Depends on `zip`, `sha2`, and the FLAC crate(s)
  from decision 4 as new `ac-core` dependencies (`ac-daemon` doesn't
  need its own copies — it calls into `ac-core::snapshot` for writing
  too, per "ac-core owns read/write" in deliverable 4).
- `ac-core/Cargo.toml`, `ac-daemon/Cargo.toml` — new deps.
- `ZMQ.md` or new `SNAPSHOT.md` — per AC #7; recommend `SNAPSHOT.md` for
  the `.acsnap` binary schema (mirrors nothing else in `ZMQ.md`, which
  is JSON-wire-only) with `ZMQ.md` linking to it, and the 4 new CTRL
  commands documented in `ZMQ.md` itself (consistent with every other
  command).
- `tests/fixtures/` (repo root) — new checked-in `.acsnap`, matching the
  `fixtures-spectrum-hf-garbage` precedent's location.

### interface changes

4 new CTRL commands (`snapshot`, `snapshot_fetch`, `snapshot_list`,
`snapshot_delete`), ungated by busy-guard. New binary file format
(`.acsnap`). Two new `ac-core` public modules/deps. No change to any
existing CTRL command or DATA frame shape.

### ZMQ protocol impact

Yes — 4 new CTRL commands, additive. No DATA socket changes (chunked
fetch stays on CTRL per D6, confirmed consistent with the existing
"CTRL is JSON REQ/REP" transport contract — base64-in-JSON is the only
option consistent with that contract, not a real two-way choice).

### implementation notes for developer

- Start the FLAC spike (decision 4) **before** anything else — it's the
  one piece of this handoff with real uncertainty outside this
  codebase's control.
- Model the snapshot ring's insertion point on `transfer.rs`'s existing
  `rings[i].extend_from_slice(buf)` call (same tick, same raw `bufs`
  from `capture_multi`) — same data, second consumer.
- Model `ServerState.snapshot_ring`'s lifecycle on `cal_reply_tx`'s
  existing set-at-start/clear-at-stop pattern in the same file.
- `Calibration` and `MicResponse` already derive `Serialize`/
  `Deserialize` — embed directly in `meta.json`, no adapter needed.
- For AC #3 (self-contained fixture), follow `aggregate.rs`'s
  `t6_known_bad_fixtures_violate_t2_invariant` test as the model for
  locating a repo-root fixture via `CARGO_MANIFEST_DIR`.
- `ac_core::snapshot`'s derivation functions should be thin call-throughs
  to `visualize::aggregate::spectrum_to_columns_wire`,
  `visualize::spl_level::weighted_broadband_dbfs`,
  `visualize::transfer::h1_estimate_with_delay` — if any of these need a
  signature tweak to be callable offline, that's a lift, not a fork
  (same discipline as M0's own reuse of existing functions).

### risks

- FLAC crate spike (decision 4) is the real schedule risk — surface
  early, don't discover it mid-implementation.
- 30 s × N-channel raw ring is a real memory commitment (~5.76 MB/channel
  at 48 kHz f32) allocated at every `transfer_stream` start regardless of
  whether a snapshot is ever taken — confirm this is acceptable (it's
  bounded and one-time per session, likely fine, but worth a line in the
  PR description since it's new baseline memory use for every transfer
  session, not just ones that use snapshots).
- Wall-clock correlation (decision 5) is only as precise as client/daemon
  clock skew if the client is remote (D6) — for the same-machine
  `--fake-audio` I-B test this doesn't matter, but note it as a known
  limitation for the real-remote case rather than silently assuming
  synchronized clocks.

