# qa-signoff-m1 — snapshot backend (branch `m1-snapshot-backend`)

Reviewed against `handoff-snapshot-backend.md` (8 acceptance criteria)
and its architect addendum (6 decisions). `.agents/qa.md`'s module map
is stale (pre-`ac-rs` workspace), same caveat as M0/M1's architect
reviews — reviewed against actual current crates instead.

Branch: `m1-snapshot-backend`, 1 commit ahead of `main` (not merged, not
pushed) plus this review's fixes on top, not yet committed.

## spec coverage

| criterion | covered | notes |
|---|---|---|
| AC1 I-B parity (the gate) | ✓ | `snapshot_reprocessing_matches_live_frame_within_tolerance` — meas_spectrum peak level, live vs. snapshot-reprocessed, within a derived tolerance. H1 magnitude/coherence comparison was attempted, found unstable under the passive fake stimulus's uncorrelated meas/ref tones (measured, not assumed — see "correctness issues" below), dropped in favor of the stable invariant |
| AC2 fetch integrity | ✓ | `snapshot_fetch_reassembles_byte_identical_across_chunk_sizes_and_reply_has_no_fs_path` — 2 chunk sizes, sha256-verified, reply schema asserted field-for-field (no path leak) |
| AC3 self-containment | ✓ | Checked-in `tests/fixtures/snapshot-fixture-v1.acsnap`, reprocessed in a pure `ac-core` test with no daemon/audio backend/config file; H1 magnitude checked against a hand-derived expected value (-4.08 dB), not just "doesn't crash" |
| AC4 FLAC round-trip | ✓ | `flac::tests` — bit-exact on-grid (4ch/24-bit), ≤1 LSB off-grid, empirically measured not just asserted |
| AC5 ring correctness | ✓ | `ring_wraparound_keeps_newest_samples_in_order` — content-correctness (FIFO order under wraparound), not just length |
| AC6 edges | ✓ | No-session, unknown-id (fetch + delete), format_version reject — all exercised |
| AC7 docs | ✓ | `SNAPSHOT.md` (new) + `ZMQ.md` (4 commands + `setup` fields) |
| AC8 suite integrity | ✓ | 533 passed, 2 ignored (JACK runbook, fixture regenerator), 0 failed — 3 consecutive full-workspace runs. Zero deletions in any pre-existing file except one additive derive-attribute line (`Calibration` gains `Serialize`/`Deserialize`/`PartialEq` — capability addition, not a behavior change) |

## correctness issues

1. **Found and fixed this pass**: `snapshot()` held the ring's `Mutex` across the entire FLAC encode (`ring.to_acsnap(...)` ran with the lock held). The live worker's capture tick needs that same mutex every ~50 ms (`push_tick`, `delay_samples` sync) — a `snapshot` call on a near-full 30 s ring would stall live capture for the encode's whole duration, a real live-cadence glitch, not just a latency nit. Confirmed by reading the code, not assumed. **Fix**: split into `snapshot_meta_and_channels` (cheap clone-out, held under the lock briefly) and `build_acsnap` (the slow FLAC/zip step, a free function taking owned data — structurally impossible to call while still holding the ring's mutex, not just disciplined-not-to). Regression test added: `snapshot_ctrl_call_returns_promptly_even_with_a_near_full_ring`.
2. **Found and fixed during original implementation** (carried over from the M1 build, listed here for completeness): a too-short ring (`< 32` frames) produced an unreadable FLAC stream instead of a clean error. Guarded in `flac::encode`; tested via `encode_rejects_below_minimum_block_size`.
3. **Found during this pass, corrected the test not the code**: the I-B parity test's first attempt compared H1 magnitude and then coherence between live and snapshot-reprocessed data; both were wildly unstable (magnitude differed ~7 dB even with matched sample windows; coherence read exactly 1.0, not the expected "near zero for uncorrelated signals"). Root cause, confirmed by inspection: the passive default fake stimulus puts two *clean, deterministic* tones at different frequencies on meas vs. ref (`audio/fake.rs`'s channel-index-dependent offset) — "coherence near zero for uncorrelated signals" is a stochastic-process intuition that doesn't hold for two noiseless deterministic tones, which instead have a fixed, window-sensitive leakage relationship. Not a reprocessing defect. Replaced with the stable `meas_spectrum`-based invariant (depends only on meas's own signal, unaffected by the meas/ref correlation problem) plus a documented explanation in the test itself.
4. No other correctness issues found on a fresh line-by-line read of the diff. Specifically checked and confirmed safe:
   - Lock ordering: `snapshot()` never holds two locks simultaneously (outer `snapshot_ring` slot lock is dropped before the inner ring lock is taken); worker thread only ever takes the inner ring lock directly. No deadlock path.
   - `Arc` lifetime: if the worker clears `ServerState::snapshot_ring` to `None` while a `snapshot()` call still holds its own cloned `Arc<Mutex<SnapshotRingState>>`, the clone keeps the ring alive until that call finishes — ordinary `Arc` refcounting, no crash, no UB.
   - `fs::remove_file` racing an in-flight `snapshot_fetch` read (session-end spool clear vs. a concurrent fetch): safe on Linux/POSIX by construction — `unlink` only removes the directory entry, an already-open file descriptor keeps working until closed. The daemon targets Linux (`ac-rs/CLAUDE.md`: "Required on Linux").
   - `id` is content-addressed (the file's own sha256) — no separate ID generator, no collision risk beyond genuine content collision, no extra dependency.

## test coverage gaps

- **Found and filled this pass**: no test previously verified the retention policy actually works — added `snapshot_spool_cleared_on_session_stop` (clean-stop path) and `snapshot_spool_wiped_at_next_session_start_after_a_crash` (crash-safety fallback — genuinely kills the daemon process via `SIGKILL` mid-session, skips its `Drop`-based cleanup via `mem::forget` to simulate a real crash rather than a clean exit, then confirms a second daemon instance against the same `HOME` wipes the leftover file). Both pass.
- **Found and filled this pass**: `setup`'s two new fields (`snapshot_ring_s`, `snapshot_spool_dir`) were wired into the handler but had no test and — more importantly — were referenced in `ZMQ.md` as settable before they were actually wired into `setup`'s allowlist at all (a real doc/code mismatch, not just a missing test). Fixed both; `setup_updates_snapshot_ring_and_spool_dir` covers write, persistence-across-a-second-read, non-positive-value rejection, and null-clears-to-default.
- Not filled, not blocking: no test exercises `derive_pair` reprocessing a snapshot under a *different* weighting than the one recorded at capture time (D10/D11's "edit-time freedom" — the API supports it, `WeightingCurve` is a plain caller-supplied argument, but nothing currently proves reprocessing under `A` when capture used `Z` produces the expected offset). Low risk (the underlying `weighted_broadband_dbfs` function is already unit-tested against A/C/Z independently in M0), but a real, named gap for a fast follow rather than silently omitted.

## scope issues

None beyond what the fixes above required. `git diff main --stat` (excluding this review's own additions) matches the files the handoff/architect addendum named. No changes to `monitor.rs`, no UI/`ac-scene`/`ac-view` code (none exists yet).

## gate check — A3 pause (qa.md pending-gate clauses)

Both `[PENDING A3]` clauses checked against this diff, same reasoning as M0's sign-off: **inapplicable**. Nothing here is rendered or printed (no UI/`ac-scene`/`ac-view` crate exists — M1 is explicitly daemon+core only), and `handlers/audio/monitor.rs` has zero diff lines. Noted explicitly per qa.md's instruction not to silently skip a check.

## standards conformance

Not applicable — this milestone adds no new measurement/weighting/display logic (it reuses M0's already-conformance-checked `weighted_broadband_dbfs`/`WeightingCurve` unchanged) and introduces no new unit-labelled output.

## verdict

**approve**, with two fixes landed during this review (not rubber-stamped): the ring-lock-held-across-encode stall (a real live-cadence bug, not a style nit) and the `setup`/`ZMQ.md` mismatch for the two new config fields. Both are fixed, tested, and re-verified. Full workspace suite green across 3 consecutive runs (533 passed, 2 ignored, 0 failed), clippy and fmt clean, zero edits to any pre-existing assertion. One test-coverage gap (cross-weighting reprocessing) documented as a fast-follow, not blocking.

Fixes from this review are staged but **not yet committed** — recommend one commit for the QA-pass fixes (mirroring the M0 pattern: `feat` commit, then a separate `test:`-prefixed QA-pass commit), then the same merge/push decision as M0.
