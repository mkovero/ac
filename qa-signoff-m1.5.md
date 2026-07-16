# qa-signoff-m1.5 — parity completion (branch `m1_5-parity-completion`)

Reviewed against `handoff-parity-completion.md`'s 5 acceptance criteria
and the architect ack (mode-selection surface + two implementation
notes). `.agents/qa.md`'s module map is stale, same caveat as every
prior review in this stack.

Branch: `m1_5-parity-completion`, 1 commit ahead of `main` (not merged,
not pushed).

## spec coverage

| criterion | covered | notes |
|---|---|---|
| AC1 Full workspace green, zero edits to pre-existing assertions (fixture-expectation update sanctioned) | ✓ | 543 passed, 2 ignored (JACK runbook, fixture regenerator), 0 failed — 3 consecutive full-workspace runs (one transient flake in the unrelated, untouched `test_software` module reproduced as green on isolation + 3 retries — third consecutive QA pass to see it; now filed in `issues.md` § Known flaky tests with its symptom, so this is the last sign-off that re-justifies the retry from scratch). `git diff main` confirms the *only* deletions are inside `generate_snapshot_fixture`/`t3_checked_in_fixture_reprocesses_with_no_daemon` (fixture content + its hand-derived expectation) plus two small non-assertion edits in `fake.rs` (a doc comment and an or-pattern match arm extending exhaustiveness for the new enum variant) — no other test assertion touched anywhere. |
| AC2 I-B test asserts all 6 quantities + 2 ground-truth checks, tolerances derived per term | ✓ | `full_ib_parity_under_correlated_stimulus` checks `meas_spectrum`, `ref_spectrum`, `spl`, `\|H1\|`, phase, coherence live-vs-derived, plus `\|H1\|=gain` and coherence≥0.99 independently on both sides. Tolerances are stated per-term (i24 floor, Welch alignment, estimator variance at achieved coherence) and then verified against real measured deltas (~0.1 dB, ~0.0001 coherence) printed by the test's own diagnostic `eprintln!` — not asserted blind. |
| AC3 Cross-weighting test, hand-derived expected offset | ✓ | `derive_pair_reprocesses_correctly_under_a_different_weighting_than_capture_time` — A-vs-Z offset at a bin-exact 100 Hz tone, compared to IEC 61672-1 Table 2's `A(100 Hz) = -19.1 dB` (cited from an already-standards-verified, already-passing test — `weighting_curves::tests::a_weighting_standard_table_values` — re-confirmed passing in this review, not re-derived). Measured 0.042 dB off. |
| AC4 Fixture reprocesses standalone; regenerator run twice → identical sha256 | ✓ | `t3_checked_in_fixture_reprocesses_with_no_daemon` (no daemon/audio backend/config). Determinism independently re-verified in this review: regenerated the fixture a second time outside the test suite, byte-for-byte identical sha256 (`a10688c7…`), `git status` reports the file unchanged after regeneration. |
| AC5 New stimulus mode opt-in, default fake behavior byte-identical | ✓ | `fake_correlated_pair` request param, read unconditionally but applied only inside `if fake { ... }`, and `Stimulus::default()` unchanged — the entire pre-existing fake-audio test suite (Tones/Noise paths, `it_protocol.rs`, `it_cross_tier_parity.rs`, all of M0/M1's tests) passes unmodified, which is the only real proof "byte-identical" holds. |

## correctness issues

1. **Found and fixed during the M1.5 build itself** (not this review — listed for completeness since it's the headline finding of this slice): the warmup flush's single-channel `capture_block` call advanced the fake engine's meas-role position counter with no matching ref-role advance, desyncing `CorrelatedPair`'s `gain`/`delay_samples` relationship before the main loop started. Symptom was a corrupted FLAC stream and a 7x slower encode on the first test run. Fixed at the call site (`capture_multi` instead of `capture_block` for the warmup, scoped to exactly this stimulus). Verified in this review: re-read the fix, confirmed `probe`'s own separate `capture_block(0.05)` call (a different handler entirely, `transfer.rs:892`) is unreachable from `transfer_stream`'s `fake_correlated_pair` state — `probe` builds its own fresh `FakeEngine` and never calls `set_correlated_pair`, so no cross-contamination risk there.
2. Checked and confirmed correct, not assumed:
   - Role dispatch (`port == self.ref_port.as_deref()`) is `None`-safe — before `add_ref_input` is called, `ref_port` is `None`, so no port can spuriously match it; every call correctly falls through to the meas branch.
   - `PairDerivation`'s new `spl_weighting` field is additive only — the struct isn't `Serialize` (checked), so nothing wire-facing changes; the one existing call site (`Snapshot::derive_pair`) was the only constructor and is updated.
   - Fixture regeneration is genuinely deterministic (independently re-verified in this review, not just re-trusted from the M1.5 commit's own claim).
   - `qa-signoff-m1.md`'s original English claim ("H1/coherence parity not asserted") is now literally false, and `SNAPSHOT.md`'s M1 honesty paragraph is updated to say so accurately, with the new stimulus and gate named — checked side by side with the current test, no stale claims left (grepped for the old "not asserted"/"unverified" phrasing — zero hits).
3. No new correctness issues found on a fresh line-by-line read of the `fake.rs`/`transfer.rs` diff.

## fixture expectation — independent re-derivation

The original sign-off confirmed the self-containment test passes and
the fixture regenerates deterministically, but not that the corrected
hand-derived number itself had been checked by anyone other than the
person who wrote the fix. Redone here from scratch, not re-read from
the code comment:

**Setup** (`generate_snapshot_fixture`): meas = `gain·ref[i-delay] +
tone`, `tone_freq=1000 Hz`, `tone_amp=0.25`; `meas_cal.vrms_at_0dbfs_in
= 1.5`; `nperseg = sr = 48000` (1 Hz/bin); K≈480 log columns,
48/octave.

1. **Raw tone amplitude.** `h1.meas_amp` at the 1000 Hz bin reads the
   tone's own peak amplitude under `spectrum_only`'s convention
   (established and independently re-tested in the M0 review) ≈ 0.25.
2. **Voltage-cal scale.** `meas_spectrum` applies `vrms_at_0dbfs_in`
   linearly: `0.25 × 1.5 = 0.375` → `20·log10(0.375) = -8.519 dB`.
3. **Hann 3-tap band-power leakage.** Column width at 1 kHz with
   K≈480/48-per-octave: `ln(24000/20)/480 ≈ 0.014771` in log-space →
   `Δf ≈ 1000 × 0.014771 ≈ 14.8 Hz`, i.e. ~15 Welch bins (1 Hz/bin) —
   wide enough to include the tone's Hann leakage into its immediate
   ±1 Hz neighbours. Raised-cosine window kernel `[-0.25, 0.5, -0.25]`
   ⇒ column sums `sqrt(0.5² + 0.25² + 0.25²) = 0.61237` against an
   ideal single-bin `0.5` → ratio `1.22474` → `20·log10(1.22474) =
   +1.759 dB`. (Independently reproduces the same 1.76 dB M0's own
   derivation reports for this exact kernel — not copied, re-derived
   from the raised-cosine coefficients directly.)
4. **Broadband contribution to this column — order-of-magnitude
   negligibility check.** Broadband amplitude parameter 0.3, uniform
   LCG ⇒ variance `0.3²/3 = 0.03`; after `gain=0.5`: `0.0075`. Spread
   over ~24000 Hz ⇒ per-bin power `≈ 3.1×10⁻⁷`, per-bin amplitude
   `≈5.6×10⁻⁴`, ×1.5 voltage scale `≈8.4×10⁻⁴`. The ~12 non-tone bins
   in the column contribute combined power `≈12×(8.4×10⁻⁴)² ≈
   8.4×10⁻⁶`, against the tone's own column power
   `(0.61237×0.375)² ≈ 0.0527` — a power fraction of `≈1.6×10⁻⁴`,
   i.e. `≈0.0007 dB`. Confirmed negligible independently, not assumed.
5. **Predicted total:** `-8.519 + 1.759 ≈ -6.76 dB` (broadband term
   dropped as negligible per step 4).

**Result:** matches the code's own stated prediction (`-6.76 dB`) and
the test's measured value (`-6.75 dB`) to within the arithmetic
rounding already present in the original derivation. Independently
confirmed correct, not merely re-read.

## test coverage gaps

- Not filled, not blocking (already flagged as pre-existing and out of scope by the architect ack, re-confirmed here rather than re-litigated): `FakeEngine::ref_port` is a single `Option<String>`, so a multi-pair session combining `fake_correlated_pair` with more than one distinct ref channel isn't representable on the fake backend. This slice's own scope (one ref, one meas) never exercises that combination, and no test claims otherwise.
- Not filled, low priority: no test drives `fake_correlated_pair` through the legacy single-pair (`meas_channel`/`ref_channel`) request form specifically — only the modern shape is exercised. Both forms parse into the same internal `pairs` list upstream (`parse_transfer_pairs`), so this is a thin risk, not a real gap in the parsing logic itself.

## scope issues

None. `git diff main --stat` (excluding `Cargo.lock` and the fixture binary) touches exactly: `fake.rs`, `audio/mod.rs` (trait method), `handlers/transfer.rs`, `ac-core`'s `snapshot/mod.rs` and `visualize/pair_derivation.rs`, `SNAPSHOT.md`, and the new `it_snapshot.rs` tests — matches the handoff and architect ack's named surfaces exactly. No `monitor.rs`, no UI/`ac-scene`/`ac-view` code (none exists yet).

## gate check — A3 pause (qa.md pending-gate clauses)

Both `[PENDING A3]` clauses inapplicable, same reasoning as every prior sign-off in this stack: nothing here is rendered or printed (no UI crate exists), and `handlers/audio/monitor.rs` has zero diff lines. Noted explicitly per qa.md's own instruction not to silently skip a check.

## standards conformance

| standard | clause | check | result |
|---|---|---|---|
| IEC 61672-1:2013 | Table 2, A(100 Hz) | Cross-weighting test's expected offset is cited from `weighting_curves::tests::a_weighting_standard_table_values`, an already-standards-verified test — re-run in this review, still passes (`-19.1 dB ± 0.1`, matches Table 2 directly). This PR reuses the existing A-curve implementation unmodified; it doesn't re-derive or re-verify the curve itself, only that reprocessing under it produces the expected relative offset. | ✓ |

No other new measurement/weighting/display logic in this slice.

## verdict

**approve.** This review found no new correctness issues — the one real bug in this slice (the warmup-flush position desync) was already found and fixed during the M1.5 build itself, and this pass independently re-verified the fix's reasoning, re-confirmed the fixture's determinism claim from scratch (not by re-reading the commit message), and re-ran the cited standards test rather than trusting the citation. Full workspace suite green across 3 consecutive runs (one transient, unrelated, pre-existing flake reproduced clean on retry), clippy and fmt clean, zero edits to any pre-existing test assertion outside the one sanctioned fixture-expectation update. Two minor, non-blocking gaps documented for a future pass, not this one.
