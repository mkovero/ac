# qa-signoff-m2 — ac-scene (branch `m2-ac-scene`)

Reviewed against `handoff-ac-scene.md`'s 7 acceptance criteria, the
architect review's 7 decisions + addendum (3a/3b), and the two QA
follow-up items raised on the developer's first pass. `.agents/qa.md`'s
module map is stale, same caveat as every prior review in this stack.

Branch: `m2-ac-scene`, not merged, not pushed.

## Follow-up item 1 — SPL layer topology (was: undocumented, unverified
under a non-trivial voltage cal)

Confirmed, not assumed:

- **Live daemon parity.** `transfer.rs:552-561` computes `spl_raw` from
  `mc_meas_amp` (mic-curve only) — the voltage-cal scale is applied
  afterward into the *separate* `meas_amp_wire` variable
  (`transfer.rs:567-577`), never fed back into `spl_raw`. Read directly,
  not taken on report.
- **monitor.rs parity.** Grepped every `vrms_at_0dbfs_in` /
  `spl_offset_db` site in `handlers/audio/monitor.rs`: `spl_offset_db`
  (lines 810/864/914/1003/1098/1361/1412) comes straight from
  `Calibration::spl_offset_db`; `dbu_offset_db` (1270/1369) comes
  straight from `vrms_at_0dbfs_in`. Emitted as two independent sibling
  fields — nothing multiplies one into the other's input. Spot-checked
  directly rather than trusting the investigating agent's report.
- **New regression test**, `it_cross_tier_parity.rs`'s
  `parity_transfer_spl_is_independent_of_voltage_cal_scale`: asserts
  `spl` is unchanged (Δ<2.0 dB) between no-voltage-cal and
  `vrms=5.0`. **Mutation-tested in this review**: temporarily rewired
  `transfer.rs`'s `spl_raw` to read the voltage-scaled spectrum instead
  of the mic-curve-only one (simulating the "composed" bug this test
  exists to catch), re-ran — failed with Δ=26.99 dB (test's own
  predicted order-of-magnitude for the injected bug was ~14 dB; higher
  here because run-to-run capture-window jitter on the plain default
  tone stimulus adds on top — still an unambiguous, order-of-magnitude
  correct failure, not a near-miss). Reverted (`git diff` on
  `transfer.rs` is empty). This is not a vacuously-passing test.
- **Topology documented** in `shared/calibration.rs`'s new module doc
  (`# Layer topology: voltage cal and SPL cal are parallel, not
  composed`), cross-linked from `spl_offset_db`,
  `pair_derivation.rs`'s `spl` field, and `ac-scene`'s `readout.rs`.
  States the "why" (the pistonphone reference tone is itself a raw
  digital reading; composing voltage cal would rescale one side of the
  dBFS→dBSPL equation and not the other) so a future recalibration
  change has something to violate on purpose, not by accident.

**Verdict on item 1: closed.** The parallel topology is confirmed to be
the actual live-path behavior (not just the offline `derive_pair`
path), the gap in test coverage (no non-trivial-vrms SPL test existed)
is filled and mutation-verified, and the semantics are now written down
in one place.

## Follow-up item 2 — AC4 fixture circularity (was: only a derive_pair-
generated fixture, WireFrame's real counterparty never exercised)

`tests/fixtures/transfer-frame-v2-live.json` is now a second fixture —
verbatim bytes off a real `ac-daemon --fake-audio` session's ZMQ PUB
socket (`it_scene_fixture::generate_live_captured_frame_fixture`), full
cal stack loaded (voltage `vrms=2.0`, SPL, mic curve) so `cal_tags`
exercises all three "on" branches, not just the "none" defaults.
`ac-scene/tests/it_live_frame.rs` checks the full raw key set (catches
a field rename `WireFrame`'s own optional-field-tolerant deserialize
wouldn't), the `cal_tags` vocabulary as literal strings, then
`WireFrame::deserialize` + `Scene::from_wire_frame` end to end. Ran
clean on the first real capture — regenerator's own inline asserts
(`spl` numeric, `cal_tags.meas.*` all `"on"`) caught nothing wrong, and
this review re-inspected the fixture's raw JSON directly (not just the
regenerator's own passing assertions) to confirm the same.

The original `transfer-frame-v2.json` (from `derive_pair` on the
checked-in `.acsnap`) is kept for AC4's numeric-equivalence test — the
two fixtures now serve their two genuinely different purposes, per the
follow-up's own resolution.

**Verdict on item 2: closed.**

## spec coverage (handoff's 7 acceptance criteria)

| criterion | covered | notes |
|---|---|---|
| AC1 amplitude/readout truth, character-for-character | ✓ | `it_fixtures.rs`'s two tests. **Independently re-derived from scratch in this review** (fresh Python script, not the developer's Rust comment re-read): cursor/tone-column value −6.7585 dB dB predicted vs. −6.7518 dB actual fixture value (Δ=0.0067 dB); SPL value 110.6539 dB predicted vs. 110.6585 dB actual (Δ=0.0046 dB). Both inside the tests' own 0.05 dB assertion bounds, confirmed by a second, independent calculation path, not just by the tests passing. |
| AC2 orientation invariant, pure code | ✓ | `scene::tests::orientation_higher_level_yields_larger_y_higher_freq_yields_larger_x` — present, exercised, not missing (flagged explicitly since the developer's own hand-off summary named AC1/AC4/AC5/AC6 but not this one). |
| AC3 tick truth, positions + labels, ≥2 range cases | ✓ | `ticks::tests::freq_axis_ac3_case_a_100_to_10k`, `freq_axis_ac3_case_b_20_to_20k`, `db_axis_ac3_case_minus80_to_0` — three cases, not two; positions and label strings both asserted; the 1 kHz-at-exactly-0.5 log-mapping check is a real correctness assertion, not just a label check. Also flagged as missing from the developer's summary; present in the code. |
| AC4 wire/snapshot scene equivalence | ✓ | `ac4_wire_and_snapshot_scenes_are_equivalent_except_the_integration_tag` — trace coordinates, axis ticks exact; SPL value asserted equal (3b), integration clause asserted to differ by construction (3a), both directions checked explicitly (not just "readouts differ, skip"). |
| AC5 reference-label correctness, both directions | ✓ | `ac5_reference_label_decided_only_by_spl_cal_presence` — calibrated→`dB SPL`, uncalibrated→`dBFS`, both live and snapshot paths. |
| AC6 no forbidden deps | ✓ | `cargo tree -p ac-scene` re-run in this review: `ac-core`, `serde`, `serde_json` and their transitive deps only. No egui/wgpu/zmq. (One dependency, `zmij` under `serde_json`, looked like a possible supply-chain anomaly on first glance — checked the registry-cached `Cargo.toml` directly: real crate, `dtolnay/zmij`, same author as `ryu`. Not a finding, noted so a future reviewer doesn't re-spend the time.) |
| AC7 headless CI, workspace green, clippy/fmt clean, zero pre-existing assertion edits | ✓ | `cargo test --workspace` run 3× in this review: 2 clean (487 passed, 0 failed, 4 ignored — regenerators + the pre-existing JACK runbook), 1 hit the pre-existing `handlers::test_software::tests` flake (see `issues.md` § Known flaky tests, updated in this review with this pass's specific failing test name) — isolated and retried per that entry's own policy, passed both times, no new investigation warranted. `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --check` both clean across all runs. `git status`/`git diff --stat` confirm the only touched pre-existing files are `Cargo.toml`/`Cargo.lock` (crate registration), `calibration.rs`/`pair_derivation.rs` (doc comments only, zero code/assertion changes), and `it_cross_tier_parity.rs` (one new test function appended, zero existing assertions edited). |

## correctness issues

None found beyond the two follow-up items above, which are process/
coverage gaps rather than logic bugs in `ac-scene` itself — the
`Scene`/`ticks`/`readout`/`dbfs` code was read line by line in this
review and matches the architect-approved design exactly (`SceneInput`
funnel, `MIN_DBFS` reuse, caller-supplied ranges, `Option<&'static
str>` integration tag).

## test coverage gaps

- Not filled, not blocking: `Scene::cursor_readout` only exposes the
  meas-channel column; a ref-channel cursor readout isn't part of this
  milestone's scope (handoff names only "the nearest column['s]... "
  singular, and M4+ is where H/phase/coherence traces — the natural
  place a ref-focused readout would matter — land).
- Not filled, not blocking: the crate doesn't yet have a test asserting
  `freq_axis`/`db_axis` behavior on a *degenerate* range (`f_min ==
  f_max`, or `db_min > db_max`) — current code would produce an
  empty-or-nonsensical tick set silently rather than erroring. Low risk
  (caller-supplied ranges are expected to come from sane UI state in
  M3), worth a defensive test before M3 wires real user-adjustable
  zoom, not before.

## scope issues

None. Diff touches exactly what the follow-up review specified: the new
`ac-scene` crate, workspace registration, two new fixtures + their
regenerators, and the three files needed to close items 1/2 (doc
comments + one new daemon-side regression test). No `ac-view`, no
rendering, no wire/schema changes.

## gate check — A3 pause (qa.md pending-gate clauses)

Per the architect review's decision 6: this PR *is* the re-homing of
A3's blocked display-truth harness (headless, fixture-backed,
character-for-character), not an instance of "gate doesn't apply
because no UI exists." Evaluated AC1-AC7 directly, as the architect
review directed. Gate stays live for M3 (the actual `ac-view` renderer),
where pixel-level truth still has no harness — noted explicitly here so
a future M3 review doesn't have to re-derive this reasoning.

## standards conformance

No new IEC/ITU-R measurement logic in this slice — `ac-scene` is
formatting and geometry over numbers `ac-core` already produces and
already standards-tests. N/A here by scope, not by omission.

## recurring-constant appendix

The 1.5× / 1.76 dB Hann coherent-vs-noise-gain ratio now has a written
first-principles explanation in `ac-scene/src/lib.rs`'s doc comment
(both the tone-leakage and broadband-noise-gain faces of the same
constant, unified). Re-derived independently once more in this review
(the Python script above) rather than just checked for internal
consistency with the developer's own comment — matches to <0.01 dB.

## verdict

**approve.** Both items raised on the developer's first pass are
substantively closed, not just annotated: the SPL layer topology is
confirmed live-path-consistent (read directly, not trusted from a
report) and now covered by a test this review mutation-verified to
actually fail on the bug it targets; the AC4 fixture's circularity is
resolved by adding a second, genuinely daemon-emitted fixture with its
own parse-and-construct test. AC1's hand-derived expectations were
re-derived from scratch in this review via an independent calculation,
not re-read from the developer's comments, and match to <0.01 dB. AC2
and AC3 — absent from the developer's own hand-off summary — are
confirmed present and correctly scoped. Full workspace green, clippy/
fmt clean, zero edits to any pre-existing assertion.
