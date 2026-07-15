# qa-signoff-m0 — transfer frame v2 (branch `m0-transfer-frame-v2`)

Blocking gate per `ui-plan.md` §3: this sign-off precedes merge. QA owns
whether the values are true. Every item is pass/fail against the **diff
and the tests as written** — not the implementation summary.

Verdict rule: any FAIL blocks merge. Any item resolved by *editing an
existing assertion* is an automatic FAIL on item 8.

## 1. Amplitude truth — derived tolerance (criterion 2)

- [x] Test pins **absolute** linear amplitude of a known-dBFS fake-audio
      sine in the `meas_spectrum` column containing f₀ — not
      self-consistency between two code paths.
      `transfer_stream_meas_spectrum_amplitude_truth` (it_protocol.rs):
      fake ch0 default (1 kHz @ 0.1 peak = -20 dBFS, a fixed known value)
      vs the actual `meas_spectrum` peak. Not compared to monitor at all
      — that's item 4.
- [x] The tolerance is **derived in a comment or doc-test**: window ENBW,
      overlap correction, band-power aggregation accounting, spectral
      leakage into adjacent columns. A bare `assert!(x < 0.1)` with no
      derivation is FAIL.
      Derived from the Hann window's 3-tap frequency kernel
      (`[-0.25, 0.5, -0.25]`): a K=491-column, 1 kHz grid spans ~14
      Welch bins, wide enough to include the tone's own leakage into its
      ±1 Hz neighbours. `sqrt(0.5²+0.25²+0.25²)/0.5 ≈ 1.2247` → +1.76 dB,
      real signal energy correctly summed by band-power aggregation, not
      an artifact. 2.2 dB tolerance clears it with margin. First attempt
      used a naive 1.5 dB bound and failed at the predicted value — that
      failure is what surfaced and confirmed the derivation.
- [x] The dB conversion in the test lives in the test (receiver side);
      no dB values on the wire. Confirmed: `meas_spectrum` is linear
      amplitude on the wire (verified in the diff — no `.log10()`
      anywhere in the daemon's field construction); the test does
      `20.0 * peak_amp.log10()` itself.

## 2. Normalization regression (falsification-first)

- [x] A test exists that the **pre-fix Welch normalization would fail** —
      i.e. it measures absolute scale, not shape. Verify by inspection
      that reverting the normalization fix flips this test red (state in
      sign-off notes how this was checked: revert-and-run or arithmetic
      argument).
      **Revert-and-run, not just argued**: temporarily set `norm = 1.0`
      in `h1_estimate_core`, reran
      `meas_and_ref_amp_match_spectrum_only_convention_on_bin_tone` —
      failed with `meas_amp[1000] = 11999.75` (expected ~1.0), matching
      the ~12000 predicted by hand (`norm = (nperseg/2)·wc ≈ 48000/2·0.5
      = 12000`). Reverted immediately after; `git diff` against the
      committed version is empty, confirming a clean revert.
- [x] `ref_amp`/`meas_amp` normalization is asserted in ac-core unit
      tests, not only end-to-end through the daemon.
      `meas_and_ref_amp_match_spectrum_only_convention_on_bin_tone`,
      `amp_off_tone_bins_are_near_silence`, `amp_arrays_parallel_to_freqs`
      — all in `ac-core/.../transfer.rs`'s test module, no daemon
      involved.

## 3. N-independence — band-power semantics (criterion 3)

- [x] Test varies nperseg / segment count under broadband fake stimulus.
      `nperseg` is pinned to `sr` in `h1_estimate_core` (not a session
      parameter) — the only lever actually variable in this estimator is
      **Welch segment count**, which varies with capture length. Added
      `broadband_level_invariant_to_welch_segment_count` (ac-core): same
      seeded broadband noise truncated to K=2 segments (1.5 s) vs K=8
      segments (4.5 s), fed through the real `h1_estimate` →
      `weighted_broadband_dbfs` pipeline.
- [x] Assertion is on **band level integrated across columns** of a
      fractional-octave region, not per-column equality (bin→column
      assignment shifts with N; per-column comparison is either flaky or
      vacuous — FAIL if per-column).
      Integrated over ~1800 bins in the 200-2000 Hz sub-band via
      `weighted_broadband_dbfs` (Z weighting) — one scalar per run, no
      per-column comparison anywhere.
- [x] Tolerance stated with rationale (noise-of-noise variance for the
      segment counts used).
      Derived: single-bin power estimate has relative variance ≈1/K
      (chi²(2K)/2K); summing over ~1800 roughly-independent bins reduces
      the *total*'s relative variance by a further ≈1/√1800. Combined
      ≈1/√(K·1800) ≈1.3% (K=2) → ≈0.1 dB. Measured actual delta:
      **0.095 dB** (via temporary tight-bound probe, then reverted to the
      real 1.0 dB threshold) — matches the derivation almost exactly.

## 4. Cross-path parity — I-C extension (criterion 4)

- [x] Extends the #99 parity fixture; does **not** fork a parallel
      fixture. All 4 new tests appended to the existing
      `it_cross_tier_parity.rs`, reusing its `Daemon`/`Client`/
      `capture_one_monitor_frame`/`synthetic_curve_flat` helpers.
- [x] Identical cal chain (voltage + SPL + mic curve) applied on both
      paths in the test setup.
      Split across 3 tests rather than one, deliberately: voltage cal
      (`parity_transfer_meas_spectrum_matches_monitor_after_voltage_cal_scale`),
      SPL cal (`parity_transfer_spl_matches_monitor_derived_spl_on_first_frame`),
      mic curve (`cal_tags_mic_curve_matches_monitor_mic_correction_tag`)
      — each loaded on channel 0 for **both** paths' shared underlying
      capture, not independently. Not combined into one mega-test
      because voltage cal changes `meas_spectrum`'s *units*
      (Vrms-domain, since monitor's plain `spectrum` field is never
      voltage-scaled — a pre-existing, unrelated-to-this-PR asymmetry)
      and mic curve interacts with a monitor.rs quirk noted below —
      conflating all three into one assertion would have papered over
      both.
- [x] Level from `meas_spectrum` matches monitor-path calibrated level
      within tolerance; `spl` matches monitor-path SPL under identical
      `spl_weighting`/`spl_integration` config.
      **meas_spectrum**: monitor's dBFS-domain peak amplitude, scaled by
      the *exact* `vrms_at_0dbfs_in` fetched via `get_calibration` (not
      re-derived), predicts transfer's voltage-scaled peak within the
      same 3.0 dB bound as the uncalibrated cross-path test. Passes.
      **spl**: monitor has no equivalent broadband-weighted-integrated
      scalar to compare against (M0 adds no field to `monitor_spectrum`
      by design). Reconstructed via monitor's `fundamental_dbfs` +
      `spl_offset_db` (not by summing monitor's *displayed* `spectrum`
      array — first attempt did that and failed by ~5 dB; traced to
      `spectrum_to_columns`'s empty-column interpolation repeating the
      same noise-floor value across many low-frequency display columns,
      which inflates a naive re-summed "total power" — a real property
      of a *display* aggregate, not a bug, but the wrong basis for this
      reconstruction). Compared against transfer's `spl` on its first
      frame specifically, where `EmaIntegrator`'s `primed == false`
      branch returns the raw input unsmoothed. Predicted delta ≈2-2.5 dB
      (Hann 3-tap leakage transfer's full-band raw sum picks up that a
      peak reading doesn't, plus small cross-FFT-length effects);
      measured **2.39 dB**. 4.0 dB tolerance. Passes.
- [x] `cal_tags` vocabulary is **string-identical** to monitor-path tags
      (assert equality of tag strings, not semantic equivalence).
      `cal_tags_mic_curve_matches_monitor_mic_correction_tag`: literal
      `assert_eq!` on the tag strings themselves (both monitor's
      `mic_correction` and transfer's `cal_tags.meas.mic_curve` against
      each other, plus transfer's own top-level `mic_correction`).

  **Out-of-scope finding, not blocking**: while designing the `spl`
  comparison, `monitor.rs:1341-1345` appears to apply
  `apply_mic_curve_inplace_f64` (documented as a dB-domain subtraction)
  to `spec`, an array that traces back to `AnalysisResult.spectrum`
  (documented as linear amplitude). If real, this is a pre-existing
  dimensional mismatch, untouched by this PR (`monitor.rs` has zero
  diff lines) and not caught by any existing test (the existing mic-curve
  parity tests only check the `mic_correction` tag and `plot`'s
  `fundamental_dbfs`, never `monitor_spectrum`'s `spectrum` array
  values numerically). All tests in this PR were deliberately routed
  around it (no mic curve loaded where a numeric monitor-spectrum value
  was asserted). Recommend a follow-up issue; not filed here (no issue
  tracker wired into this session).

## 5. Edges

- [x] Channel without SPL cal layer → `spl: null`, `cal_tags` still
      present and coherent, spectra unaffected.
      `transfer_stream_frame_v2_fields_present_and_spec_freqs_stable`:
      uncalibrated session, `spl.is_null()`, `cal_tags` present with all
      sub-tags `"none"`, `meas_spectrum`/`ref_spectrum` present and
      correctly sized.
- [x] `spec_freqs` identical across consecutive frames (fixed grid).
      Same test: `assert_eq!(sf0, sf1)` across frames 0 and 1.
- [x] Rejected `weighting`/`integration` params → `{"ok": false}` before
      worker spawn; no partial session.
      `transfer_stream_rejects_invalid_spl_session_params` — strengthened
      during this review to also assert `status.busy == false`
      immediately after a rejection (not just inferred from the trailing
      valid call succeeding).

## 6. Wire

- [x] Measured frame cost (~31 KB JSON) recorded in ZMQ.md with
      precision-rounding named as the designated first lever (per
      architect note). Not a code change — a documented decision.
      Added during this review: ZMQ.md's `transfer_stream` section now
      states K=491, ≈31 KB/frame/pair for the new fields, explains the
      binary-vs-JSON-text estimate correction, and names precision
      reduction (fewer significant digits, or f32) as the first lever —
      explicitly *not* shrinking K or reverting to per-column dB, both
      of which would undo D18.
- [x] ZMQ.md states the linear-amplitude contract: "dB conversion happens
      in the receiver, nowhere else."
      That exact phrase is now in ZMQ.md (tightened from a paraphrase
      during this review).
- [x] New fields additive only: no rename, removal, or re-decimation of
      existing fields (verify from diff of the `json!` block).
      Verified directly from `git diff` on `handlers/transfer.rs`: all
      12 pre-existing fields (`type` through `mic_correction`) appear
      unchanged in the new `json!` block; 7 new fields appended after.

## 7. Session params

- [x] Defaults Z / fast when unspecified, reflected in frame tags.
      `transfer_stream_frame_v2_fields_present_and_spec_freqs_stable`
      asserts `spl_weighting == "Z"` and `spl_integration == "fast"`
      with no params sent.

## 8. Suite integrity

- [x] `cargo test --workspace` + clippy `-D warnings` + fmt clean —
      re-run, not taken from summary.
      Re-ran fresh after all QA-phase additions (3 new ac-core tests, 5
      new/strengthened daemon tests, ZMQ.md edits): 82+295+67+8+53 = 505
      passed, 1 ignored (JACK runbook), 0 failed — twice in a row.
      `cargo clippy --workspace --all-targets -- -D warnings`: clean.
      `cargo fmt --check`: clean. (One transient failure in an unrelated
      pre-existing test, `test_software::tests::
      handler_returns_results_array_and_all_pass_true`, occurred once
      earlier under full-workspace parallel load and passed on 3
      immediate reruns both isolated and full-workspace — confirmed
      pre-existing flakiness unrelated to this diff, not this PR's code.)
- [x] **Zero edits to existing assertions**, verified from the diff.
      Test-file diffs contain additions only.
      `git diff main -- <each touched file>` — zero `-` lines (deletions)
      in any touched source or test file. Confirmed on
      `it_protocol.rs`, `it_cross_tier_parity.rs`, `transfer.rs`
      (ac-core), `aggregate.rs`. Pure addition in every file.

## Sign-off

| Item | Pass/Fail | Notes |
|------|-----------|-------|
| 1. Amplitude truth | PASS | Derived tolerance, confirmed by an initial too-tight-bound failure at the predicted value |
| 2. Normalization falsification | PASS | Revert-and-run performed, not just argued; matched hand-derived prediction |
| 3. N-independence | PASS | Real Welch-segment-count test added this pass; measured 0.095 dB vs ~0.1 dB predicted |
| 4. Cross-path parity | PASS | 3 new calibrated tests added this pass (voltage, SPL, mic-curve-tag); one pre-existing out-of-scope finding noted, not blocking |
| 5. Edges | PASS | "No partial session" check strengthened this pass |
| 6. Wire | PASS | ZMQ.md wire-cost + exact-phrase + lever note added this pass |
| 7. Session params | PASS | |
| 8. Suite integrity | PASS | 505 passed / 1 ignored / 0 failed, twice; zero deletions in any touched file |

**Verdict: all-pass. Merging `m0-transfer-frame-v2` to `main` (local; not
pushed — pushing to the remote was not part of this instruction).**

QA: Claude (Sonnet 5)  Date: 2026-07-15
