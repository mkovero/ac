# Tier-framing follow-up — execution plan

Running execution plan for the source-code work that closes the gaps
between [`ARCHITECTURE.md`](ARCHITECTURE.md) (commit
[`7389f4e`](https://github.com/mkovero/ac/commit/7389f4e)) and the
implementation. The framing promises:

- Both Tier 1 and Tier 2 produce **trustworthy, calibration-aware
  numbers** — the split is which technique is in use, not numeric
  rigor.
- Every influence on a displayed number (voltage / SPL / mic-curve
  calibration, smoothing, fractional-octave aggregation, A/C/Z
  weighting, time integration, mic-correction enable) is **surfaced
  as a labelled tag** in the overlay AND on every wire frame.
- Calibration applies **identically across tiers**: a mic'd channel
  in `ac plot` reports the same physical level as the same mic'd
  channel in `ac monitor cwt`.

A code audit found 12 source-side gaps. They're filed as 12 issues
below, organised into 7 phases by dependency / risk.

## Phase 1 — data-model foundation

Must land first; everything that surfaces SPL / mic provenance
depends on the snapshot carrying those fields.

- [x] [#94](https://github.com/mkovero/ac/issues/94) (B) — extend
      `CalibrationSnapshot` with SPL + mic_response provenance

## Phase 2 — quick wins (parallel)

Both small (<100 LOC), independent.

- [x] [#95](https://github.com/mkovero/ac/issues/95) (C) —
      `get_calibration` / `list_calibrations` return all three cal layers
- [x] [#96](https://github.com/mkovero/ac/issues/96) (E) —
      `tier_badge` parameterises CQT + reassigned modes

## Phase 3 — headline correctness (parallel after Phase 1)

The actual "make Tier 1 match the doc" work. Substantial — ~600 LOC
across four handlers. Lift the existing `apply_mic_curve_inplace_*`
helpers from `monitor.rs` into a shared utility on first reuse.

- [x] [#97](https://github.com/mkovero/ac/issues/97) (A) — apply
      mic-curve in Tier 1 capture paths (`plot`, `plot_level` —
      `sweep_ir` IR-correction deferred to a follow-up; the snapshot
      records the curve provenance regardless)
- [x] [#98](https://github.com/mkovero/ac/issues/98) (D) — Tier 1
      frames carry the full processing-context envelope

## Phase 4 — validation gate

Filed as `blocker`. Will fail until Phase 3 lands — protects against
future cross-tier parity regressions.

- [x] [#99](https://github.com/mkovero/ac/issues/99) (F) — cross-tier
      numeric parity test

## Phase 5 — extensions

All depend on prior phases. Independent of each other.

- [x] [#100](https://github.com/mkovero/ac/issues/100) (G) — CSV export
      records cal + processing context
- [x] [#101](https://github.com/mkovero/ac/issues/101) (H) — apply
      calibration layers in transfer paths (refuse mic-curve on the
      reference leg)
- [x] [#102](https://github.com/mkovero/ac/issues/102) (J) —
      report_html / report_pdf surface SPL + mic-curve provenance
- [x] [#103](https://github.com/mkovero/ac/issues/103) (K) —
      `test_dut` / `test_hw` results carry mic + SPL context (envelope
      stamp; per-subtest mic-curve apply deferred as a follow-up)

## Phase 6 — deep loudness integration

Per-sample inverse-curve FIR before K-weighting. Substantial — ~400
LOC: FIR design + per-sample filter loop + parity tests at three
frequencies + a bench. Composes with SPL cal (#89) so a calibrated
channel reads K-weighted dB SPL with mic-correction.

- [x] [#104](https://github.com/mkovero/ac/issues/104) (I) — integrate
      mic-curve into BS.1770-5 path (per-sample inverse-curve FIR
      before K-weighting; 55× realtime in release)

## Phase 7 — backlog

Low priority, not on critical path.

- [ ] [#105](https://github.com/mkovero/ac/issues/105) (L) —
      `MeasurementReport` records the active processing chain

## Dependency graph

```
B ──┬── J
    ├── L
    └── C

A ──┬── F  ← parity gate; blocks until A+D pass
    ├── H
    ├── I
    └── K

D ── F
G ── (depends on D for context source)
E (independent)
```

## Verification

For each landed issue:

1. `cd ac-rs && cargo test` stays green (currently 516 + 1 ignored).
2. `cargo run --release --example bench_cqt` / `bench_reassigned`
   show no regression vs baseline (1.42 ms / 0.054 ms). For #104,
   `cargo run --release --example bench_loudness_mic` should land.
3. **#99 is the systemic gate** — any phase 1–3 issue that breaks
   cross-tier parity gets caught.
4. Manual smoke for each tier: with a mic-curve loaded on input 0,
   `ac plot 1khz 1khz 0dbu`, `ac monitor`, `ac monitor cwt`,
   `ac monitor cqt`, `ac monitor reassigned`, and `ac transfer` all
   apply the same correction (within bin-leakage tolerance).
5. `cal.json` round-trip: write all three layers, read back, every
   field present and unchanged.

## What this plan does NOT do

- No standards-mask testing for Tier 2 (CWT / CQT / reassigned aren't
  IEC 61260-1 — that boundary stays as documented).
- No deferral of LKFS × mic-curve to a tag-only stub. Issue #104
  ships the deep integration (per-sample inverse-curve FIR before
  K-weighting), as the user committed to during planning.
