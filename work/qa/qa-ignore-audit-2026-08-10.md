# qa audit — the `#[ignore]`d population, 2026-08-10

Reconciliation item 4. Read-only: no code changed, no issues filed, nothing
run. Enumerated from the tree at `ea83f33`, not from the snapshot-era table in
`handoff-flush-reconciliation.md` — which said "roughly 11 real attributes
across 8 files" and is now **14 across 8 files**, matching the 14 ignored that
`cargo test --workspace` reports.

The question asked of each, from the item: **what would notice if the thing
this covers broke?** A row reading *nothing does* is the output, not a failure.

The audit exists because three times a test has been unable to **see** a
defect rather than merely asserting weakly: the vacuous equality assertions;
`FakeEngine` inheriting an empty `last_drain_occupancy`, so ring defects were
invisible in the one mode built to reproduce them; and the stale `ac-view`
snapshots, which cannot run in the environment where they would fail.

---

## Finding 1 — three of the fourteen are not tests, and should stop sharing an attribute with the ones that are

| file | what it is |
|---|---|
| `ac-core/src/snapshot/mod.rs:565` | regenerates `tests/fixtures/snapshot-fixture-v1.acsnap` |
| `ac-scene/tests/regenerate_fixture.rs:32` | regenerates `tests/fixtures/transfer-frame-v2.json` |
| `ac-daemon/tests/it_scene_fixture.rs:204` | regenerates `transfer-frame-v2-live.json`, needs a live daemon |

These are **producers, not tests**. They assert nothing about the system; they
write files that other tests consume. Asking "what would notice if this broke"
is malformed for them — they cover nothing, so nothing needs to notice.

Their consumers *do* run in the default suite: `ac-scene/tests/it_fixtures.rs`
reads both `snapshot-fixture-v1.acsnap` and `transfer-frame-v2.json` on every
`cargo test`.

**The real hazard is the opposite one, and it is not covered.** Nothing checks
that a checked-in fixture still matches what the current code would generate.
A format change that updates both the writer and the reader leaves the frozen
fixture describing a format neither side speaks any more, and every consuming
test keeps passing against it. That is the same shape as the stale `ac-view`
snapshots in finding 2 — a fixture that is silently no longer a reference —
and it is worth knowing that the fixture category has the same failure mode as
the category it looks unrelated to.

**Recommendation (not filed):** name this category distinctly. Sharing
`#[ignore]` with genuinely-unrun coverage means a count of ignored tests reads
as a coverage gap three larger than it is, and it hides that the regeneration
entries need a *currency* check rather than an execution environment.

---

## Finding 2 — the wgpu snapshot five: substance covered offline, rasterisation not, and they are currently stale

`ac-view/tests/it_transfer_snapshots.rs`, five attributes, all
*"real-adapter only (wgpu); run on 192.168.9.25 per A3 policy"*:
`snapshot_transfer_live_masked_gap`, `snapshot_transfer_armed_banner`,
`snapshot_transfer_driving_banner`, `snapshot_spectrum_ref_trace_on`,
`snapshot_spectrum_ref_trace_off`.

| defect | what notices offline |
|---|---|
| a view's text overlapping the connection banner | `it_banner_clearance.rs` — asserts no painted text overlaps the banner rect, headless |
| meas and ref traces indistinguishable | `it_trace_distinction.rs` — asserts different stroke colours |
| snapshot traces not visually distinct from live ones | `it_trace_distinction.rs` — dashed multi-segment vs one solid path |
| axis/tick geometry wrong | `it_transfer_geometry.rs`, `it_geometry.rs` |
| **any defect visible only as pixels** | **nothing does** |

The layout and colour *invariants* are covered headless. What is not: the
rasterisation itself. A trace painted with the right colour at the right
coordinates and then not visible — clipped, alpha-zero, occluded, font
fallback substituting a glyph — is exactly what a pixel comparison catches and
what geometry assertions cannot.

**These five are known stale.** #245's fix (`d569907`, PR #252) reserves half a
text line at the top of every view, shifting layout by ~7 px, and the
references were last regenerated at `de4b658` (#194). The next run pixel-diffs
all five at once. That is recorded in `work/rig/rig-verify-queue.md` as a block
with the instruction not to read the first failure as a regression — the
correct handling, and worth restating here because **a stale reference is a
test that has stopped being able to fail for the right reason**: it will now
fail for a reason that has nothing to do with the build under test, which is
indistinguishable from failing for a real one until someone looks.

---

## Finding 3 — the JACK four: the fake models the mechanism; only these confirm the model matches hardware

| attribute | what it does |
|---|---|
| `jack_backend.rs:758` `jack_contiguous_drain_discards_nothing_and_keeps_up` | #207 fix on real JACK; capture only, emits nothing |
| `jack_backend.rs:810` `jack_capture_multi_discards_live_audio_between_ticks` | the defect the fix addresses, on real JACK |
| `contiguity.rs:555` `jack_hf_sweep_replica_read` | **emits**; per-run consent |
| `contiguity.rs:727` `jack_stimulus_then_silence_recurrence_probe` | **emits**; per-run consent |
| `it_loopback_ir.rs:205` | `sweep_ir` through real JACK port-to-port loopback |

What notices offline: a lot, and deliberately so. `contiguity.rs` carries
around a dozen ring-mode tests that reproduce the splice, the replica spacing,
the alias, the discard count and the backlog against `FakeEngine`'s ring mode —
which exists precisely so ring-shaped defects are reproducible without
hardware. `rings.rs` adds nine more. `ac-core`'s `sweep.rs` covers the Farina
sweep numerically with eight tests.

**What nothing covers: whether the fake's model of the hardware is right.**
`period_quantisation_decides_which_frequencies_expose_the_splice` encodes a
specific claim — a JACK callback pushes whole periods, so the discarded gap is
always `k·period` and the phase discontinuity is exactly zero for tones at
multiples of `sr/period`. That claim was established on the Babyface Pro and is
the reason the fake predicts hardware rather than merely resembling it. The
only things that would notice it drifting out of true are the four JACK
attributes above.

This is a **legitimate** nothing-else-covers-it, not a gap to close: the model
can only be checked against the thing it models. Worth stating because the
offline suite around it is dense enough to look like coverage of the same
question, and it is not.

Two of the four **emit**, which puts them behind per-run operator consent and
the −40 dBFS standing cap. That is a policy gate, not an environment gate, and
it should not be conflated with the other two — capture-only JACK tests need a
server, not a decision.

---

## Finding 4 — `it_stimulus_live`: the most-covered of the ignored, and the residue is real

`ac-view/tests/it_stimulus_live.rs:92`,
`stimulus_arm_fire_keepalive_panic_and_deadman_over_real_zmq`. Its own header
claims four properties, "none observable headless".

That claim is **too strong as of today**. `ac-daemon/tests/it_set_drive.rs`
spawns a real daemon over real ZMQ and covers the dead-man directly, in the
default suite:

- `dead_man_drops_drive_after_keepalive_silence_but_keeps_the_session`
- `drive_survives_up_to_the_dead_man_window`
- `idempotent_resends_hold_drive_past_the_dead_man_window`
- `legacy_launch_time_drive_is_not_killed_by_the_dead_man`
- `the_published_drive_state_follows_the_dead_man_not_the_last_request`

So properties 1 and 4 — drive reaches the daemon; the dead-man is a real
backstop — are covered without the rig.

**The residue is property 2**, and it is the interesting one: that `ac-view`'s
own 250 ms keepalive, driven by the real `StimulusMachine` under real
scheduling, never gaps past the 1.5 s window. That is a claim about the
*client's* timing under a real scheduler, and nothing offline covers it —
`it_set_drive` exercises the daemon's side of the contract with a test
harness's timing, not the app's.

**Recommendation (not filed):** the header should say which property the test
is now uniquely for. A test whose stated justification has been overtaken by
other coverage is one nobody will re-examine, and its real value gets
discarded along with the stale part of its claim.

---

## Summary

| category | count | covered elsewhere? |
|---|---|---|
| fixture regeneration | 3 | n/a — not tests; but fixture *currency* is unchecked |
| wgpu snapshots | 5 | invariants yes, pixels **no** — and currently stale |
| JACK, capture-only | 2 | mechanism yes; fake-model fidelity **no** |
| JACK, emitting (consent-gated) | 2 | mechanism yes; hardware recurrence **no** |
| JACK loopback IR | 1 | sweep maths yes; end-to-end audio path **no** |
| real-daemon stimulus | 1 | 3 of 4 properties yes; client keepalive timing **no** |

**Nothing here is a defect and nothing needs filing.** Every attribute carries
a stated reason string, which is the convention that made this audit cheap —
14 attributes, all reasons legible, no archaeology.

Three things are worth acting on eventually, in descending order of value:

1. **Fixture currency is unchecked** (finding 1). The failure is silent and it
   is the same shape as the stale snapshots.
2. **The five snapshot references are stale** (finding 2), already queued.
3. **Two stale justifications** — `it_stimulus_live`'s header (finding 4) and
   the fixture-regeneration entries sharing an attribute with real coverage
   (finding 1).
