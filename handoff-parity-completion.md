# handoff-parity-completion — M1.5: correlated stimulus, full I-B, cross-weighting

Parent plan: `ui-plan.md` (invariant I-B, D10). Closes the three items
left open by `qa-signoff-m1.md`. Base: `main` post M0+M1 merge. One
small PR — mostly test work plus one fake-engine addition. Must land
**before M2**, because it regenerates the fixture M2's display-truth
tests will be written against.

## Goal

Discharge I-B as originally written: live-vs-snapshot parity for **all**
frame quantities (spectra, SPL, H1 magnitude, phase, coherence), made
possible by a fake stimulus whose H1 actually converges; and prove the
D10 promise (reprocess a snapshot under a different weighting) with a
test.

## Deliverables

### 1. Correlated-pair stimulus mode (`audio/fake.rs`)

- New selectable mode: a **seeded deterministic broadband source** on the
  ref channel; the paired meas channel carries the *same* source through
  a known gain `G` and integer delay `D` samples (a fake DUT).
- Expected ground truth, band-wide: `|H1| = G` (flat), phase after the
  transfer path's delay compensation ≈ 0, coherence ≈ 1.
- **Additive and default-unchanged**: existing fake behavior stays the
  default; the mode is selected explicitly (config key or `setup` field —
  implementer's choice, one-line architect ack since it's a CTRL/config
  surface). Zero edits to existing assertions, as always.
- Deterministic seed fixed in code so fixture regeneration is
  reproducible.

### 2. Full I-B parity test

Extend (or sibling) the existing
`snapshot_reprocessing_matches_live_frame_within_tolerance`, under the
correlated stimulus:

- live-vs-snapshot: `meas_spectrum`, `ref_spectrum`, `spl`,
  `|H1|`, phase, coherence over the same time window.
- Additionally against **ground truth**: `|H1| = G` and coherence
  ≥ 0.99 on both the live and the reprocessed side (catches the case
  where both paths agree on a wrong value).
- Tolerance derivations stated per term, same discipline as before:
  i24 quantization floor, Welch segment alignment, estimator variance at
  the coherence achieved — and nothing else.

### 3. Cross-weighting reprocessing test (QA's named gap)

- Capture (or reuse) a snapshot recorded at `Z` weighting; reprocess via
  `derive_pair` under `A`.
- Assert the A-vs-Z broadband offset against the value computed
  independently from the known stimulus spectrum and the IEC 61672 A
  curve (hand-derived in a comment, like the fixture's −4.08 dB) — not
  against the same function under test.
- One edge: reprocessing under a weighting ≠ capture-time weighting must
  be reflected in the derived output's tags (the snapshot's *stored*
  tags are capture provenance and stay untouched).

### 4. Fixture regeneration

- Regenerate `tests/fixtures/snapshot-fixture-v1.acsnap` via the existing
  `#[ignore]`'d regenerator, now containing the correlated-pair stimulus
  (keep a sine + broadband component on meas so M2's amplitude-truth and
  band-power test classes still have their substrate).
- Update the hand-derived expected values in the self-containment test
  (the −4.08 dB class of check) for the new content — hand-derived
  again, not read back from the code under test.
- SNAPSHOT.md: soften the M1 honesty paragraph to its final form —
  exact per-frame H1 parity is tested under correlated stimulus;
  under uncorrelated/low-coherence signals, snapshot-derived H1 matches
  live statistically, not per-frame.

## Acceptance criteria (falsifiable)

1. Full workspace green; zero edits to pre-existing assertions
   (fixture-expectation updates in the self-containment test are the one
   sanctioned exception — they are fixture data, not invariants; call
   them out explicitly in the PR).
2. I-B test asserts all six quantities live-vs-snapshot **and** the two
   ground-truth checks, tolerances derived per term.
3. Cross-weighting test passes with a hand-derived expected offset.
4. Regenerated fixture reprocesses in the pure ac-core test with zero
   external state; regenerator re-run twice produces identical sha256
   (determinism proof).
5. New stimulus mode is opt-in; default fake behavior byte-identical
   (existing suite proves it).

## Out of scope (hard fence)

- `ac-scene`, `ac-view`, any UI code (M2/M3).
- Wire-frame changes of any kind — this slice adds no fields.
- Window-alignment machinery for live emission vs. ring extraction
  (the correlated stimulus makes it unnecessary for parity; alignment
  stays a documented non-goal).
- Stimulus realism beyond gain+delay (no simulated noise floor, no IR
  convolution — YAGNI until an H-view needs it).

## Routing

QA: criteria 2–4 (tolerance derivations, hand-derived expectations).
Architect: one-line ack on the mode-selection surface only. No UX gate.

---

## Architect ack (design-approved)

**Mode selection: request param on `transfer_stream`, guarded by
`state.fake_audio`, not a config key.** Existing precedent —
`monitor_spectrum`'s `fake_tones`/`fake_noise_dbfs` (`handlers/audio/
monitor.rs:300-313`) — is exactly this shape: optional request fields,
read only inside `if fake { ... }`, real backends never see them, no
persistence. A config key would be wrong here (global/sticky state for
what's inherently a single test's stimulus choice); the request-param
pattern this codebase already has for fake-only knobs is the right one
to extend, not reinvent. Suggested field: `"fake_correlated_pair":
{"gain_db": <f64>, "delay_samples": <u64>}` on the `transfer_stream`
request, mirroring `fake_tones`'s shape.

**Implementation note, not a blocking decision:** `fake.rs`'s
`make_samples_for(port, duration)` currently derives everything from a
per-channel-offset key (`CHANNEL_OFFSET_HZ`-based), generic across
ports. A correlated pair needs asymmetric roles — ref is the source,
meas is a delayed+scaled *copy of that same source* — which the offset
trick can't express. `FakeEngine` already tracks `input_port` (meas
role) and `ref_port` (ref role) as distinct fields (set by
`transfer_stream`'s `eng.start(..., Some(&main_port))` /
`add_ref_input`), so role dispatch inside `make_samples_for` can key off
`Some(port) == self.ref_port.as_deref()` vs. `input_port` directly,
without inventing new plumbing. The existing `Noise` variant's
persistent per-channel LCG (`noise_state: HashMap<u64,u64>`) is the
right model to extend — give the pair one shared deterministic stream
(keyed by the new mode's fixed seed, not the channel offset) and let
meas read `D` samples behind ref, scaled by `G`.

**Pre-existing limitation, not this deliverable's to fix:**
`FakeEngine::ref_port` is a single `Option<String>` —
`add_ref_input` overwrites it, so multi-pair sessions with more than one
distinct ref channel have never been fully representable on the fake
backend. The handoff's own scope (one ref, one meas) doesn't hit this;
worth a one-line comment in the new code so a future multi-pair fake
test doesn't rediscover it the hard way, not worth fixing here.

No objection to any of the four deliverables' own scoping (D10
cross-weighting test, fixture regeneration, the two `#[ignore]`
determinism re-run, the honesty-paragraph softening) — all straightforward
extensions of patterns M0/M1 already established. Ready for developer.
