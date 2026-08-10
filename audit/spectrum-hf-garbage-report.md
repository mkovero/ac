# Report: spectrum HF garbage — units-mismatch in the log-column aggregator

**status:** hypothesis in handoff.md **falsified**. Root cause found and confirmed
with a from-scratch reproduction. **NO PATCH APPLIED** — per handoff's hard fence,
this is evidence + a fix sketch only.

## Resampler located, single-path confirmed

`ac_core::visualize::aggregate::spectrum_to_columns` (called via
`spectrum_to_columns_wire` / `spectrum_to_columns_multiband_wire`,
`ac-rs/crates/ac-core/src/visualize/aggregate.rs`) is the only column
aggregator. Call sites, exhaustively grepped:

- `ac-daemon/src/handlers/audio/monitor.rs:1210,1220,1267,1277` — the live
  `monitor_spectrum` wire frame (feeds spectrum/waterfall/ember views) **and**
  the CSV export (`ac monitor` writes the same wire frames to CSV — single
  shared path, confirms handoff's assumption).
- `ac-ui/src/app/render_pipeline.rs` uses the sibling
  `samples_on_axis_to_columns` only for the **transfer** display, a separate
  path per the handoff's own out-of-scope note. Not implicated here.

## Invariant check: mapping is clean

Reproduced the exact geometry from handoff.md (sr=96 000, HF FFT N=8192 →
Δf=11.72 Hz, `DEFAULT_WIRE_COLUMNS`=4096, f_min=20/f_max=48000) with a
temporary test harness driving the real `thd::analyze` +
`spectrum_to_columns_wire` (harness deleted after capture; not a permanent
addition). Replicated the internal loop read-only to expose
`(src_start, src_end, n_src_bins)` per column and checked the invariant
from handoff.md over all 4096 columns:

```
invariant_violations = 0
```

`src_start[i+1] == src_end[i]` held for every column — **monotonic, gapless,
non-overlapping, non-empty past the crossover.** The handoff's index-mapping
hypothesis (log/linear domain mixup, off-by-one) is **falsified**. Do not
spend more effort on the range-mapping arithmetic; it's correct.

## Actual defect: `spectrum_db` is not in dB

`db_to_power`/`power_to_db` were added in commit cbb4225 (#162, "aggregation
uses band-power, not peak"). They assume their input is already expressed in
dB: `db_to_power(x) = 10^(x/10)`, and the doc comment on
`spectrum_to_columns` is explicit ("`spectrum_db` holds ... magnitudes in
dBFS").

But every production caller feeds it **linear amplitude**, never dB:

- `ac-core/src/shared/types.rs:40` — `AnalysisResult.spectrum` is documented
  as "One-sided **amplitude** spectrum (magnitude, windowed + normalised)."
- `thd::analyze` (`ac-core/src/measurement/thd.rs:84`) computes
  `spec = win_spectrum.iter().map(|c| c.norm() / norm)` — a linear ratio,
  never passed through `amplitude_to_dbfs` (`shared/reference_levels.rs:58`,
  the function that exists in this exact codebase for precisely this
  conversion and is never called on this path).
- `visualize::spectrum::spectrum_only` (`ac-core/src/visualize/spectrum.rs:112`)
  — same thing, same lack of conversion.
- `monitor.rs` passes both straight into `spectrum_to_columns_wire` /
  `spectrum_to_columns_multiband_wire` with zero conversion in between.

Before #162, `spectrum_to_columns` just took `max()` per column — unit-blind,
so feeding it linear amplitude "worked" by accident (order-preserving, wrong
absolute number but nobody multiplied anything). #162 switched the per-column
statistic to `10·log10(Σ 10^(bin/10))`, which is **only correct if `bin` is
already dB**. The caller was never updated. This is the bug: a contract
change in the aggregator that silently broke its only callers.

### Why this produces exactly the reported symptom

For a linear amplitude `a` near the noise floor (e.g. `1e-4`–`1e-14`),
`10^(a/10) ≈ 1.0` in `f32` — indistinguishable from unity regardless of the
real signal level. So every populated bin contributes ≈1 to the column's
power sum, and:

```
out_dbfs = 10·log10(n_src_bins)
```

— a number determined **purely by how many raw FFT bins land in the
column**, not by the audio at all. This explains every observed symptom at
once:

- **Deterministic, identical across captures 26 s apart and across all 10
  channels including silent ones** — because the value no longer depends on
  signal content, only on bin-count geometry, which is fixed per channel/FFT
  config.
- **Onset exactly at the interpolation→aggregation crossover** — before that
  point `n_src_bins == 0` (interpolation branch runs instead); the moment
  `n_src_bins` becomes ≥1 this effect switches on.
- **Reproduced onset frequency matches exactly**: my synthetic replay (same
  sr/N/columns) puts the first `n_src_bins == 1` column, and the first
  `n_src_bins == 2` column (first `out_dbfs > 0`, at `3.010 dB` =
  `10·log10(2)`), at **col 3046, f = 6533.5 Hz** — bit-for-bit the onset
  frequency reported in the field CSVs. This is strong independent
  confirmation the mechanism (not just the general theory) is right.
- **"LF is clean"** — the LF leg hands off at 750 Hz, below its own
  crossover (~770 Hz), so its aggregation branch (`n_src_bins ≥ 1`) never
  engages; it always uses the (also technically mislabeled, but at least not
  power-summed) interpolation branch, which is visually much less broken.

### Where I can't fully close the loop

My clean-room reproduction (near-silent floor, `amp ≈ 1e-14`) tops out at
`n_src_bins ≤ 8` near Nyquist (`col width / Δf` grows to ~8 at f_max=48 kHz),
and since amplitude is bounded to `[0,1]`, `db_to_power(amp) ≤ 10^0.1 = 1.259`,
giving a **hard ceiling of ~10.03 dB** under this mechanism alone — not
+19.115 dBFS as the field data shows. The extra ~9 dB gap is very likely
real per-bin amplitude values in the field capture that are *not*
vanishingly small (actual hardware noise floor/hum, or possibly the
dual-resolution multiband blend compounding LF+HF columns that are each
independently subject to the same bug) pushing `db_to_power` further above
1.0 per bin. I did not chase this further — it's a magnitude detail on top
of an already-confirmed mechanism, and chasing it risks wandering into the
"806 Hz peak" / "meters-vs-spectrum" investigations explicitly marked
out-of-scope in the handoff.

### The −240 dBFS floor bins are not from this aggregator

`spectrum_to_columns`'s own empty-column floor sentinel is `f32::NEG_INFINITY`,
not −240. `−240` matches `20·log10(MIN_AMPLITUDE=1e-12)` in
`shared/reference_levels.rs` — a different module's clamp. The interleaved
−240 bins almost certainly come from a downstream stage (CSV serialization
or mic-curve correction) re-clamping a `NaN`/`-inf` wire value it receives
from this aggregator. I did not trace this fully (out of the "no touching
renderers/export formatting" instinct, though the handoff doesn't explicitly
fence off the CSV writer) — flagging as a loose end for whoever picks up the
fix, not a second root cause.

## Acceptance criteria status

- [x] Resampler located; single shared path for spectrum views **and** CSV
      export confirmed (`monitor.rs`, both `Ok`/`Err` branches of the THD
      analysis funnel through the identical two aggregator calls).
- [x] Instrumentation dump produced (see below) from a real `--fake-audio`-
      style capture through the actual production functions
      (`thd::analyze` → `spectrum_to_columns_wire`), not the daemon binary
      directly — sufficient to isolate the bug to library code, independent
      of ZMQ/CLI plumbing.
- [x] Invariant evaluated with concrete numbers: **0 violations**, mapping
      falsified as the cause; **accumulation math is the confirmed cause**
      (units mismatch on `db_to_power`/`power_to_db` input).
- [ ] **Known-bad fixture NOT preserved** — the five 2026-07-04T13:31 CSVs
      referenced in handoff.md are not present anywhere in this environment
      (not committed, not under any scratchpad I can find). I cannot archive
      files I don't have. I've included my own synthetic reproduction's dump
      below as a substitute; if the original CSVs still exist wherever the
      prior session ran, they should be committed to `tests/fixtures/`
      separately.

## Dump (excerpt): out_idx, out_freq_hz, src_start, src_end, n_src_bins, accumulated_power, out_dbfs

sr=96000, N=8192 (Δf=11.72 Hz), n_columns=4096, f_min=20, f_max=48000,
signal = 1 kHz fundamental (amp 0.1) + 1% 2nd harmonic (fake.rs's exact
`make_samples_at` shape):

```
col=3044 f=6508.7  src_start=555 src_end=556 n_src_bins=1 power_sum=1.000000 out_dbfs=0.000
col=3045 f=6521.1  src_start=556 src_end=557 n_src_bins=1 power_sum=1.000000 out_dbfs=0.000
col=3046 f=6533.5  src_start=557 src_end=559 n_src_bins=2 power_sum=2.000000 out_dbfs=3.010   <- onset, matches field 6533 Hz exactly
col=3047 f=6545.9  src_start=559 src_end=560 n_src_bins=1 power_sum=1.000000 out_dbfs=0.000
...
col=3500 f=15481.3 src_start=1320 src_end=1323 n_src_bins=3 power_sum=3.000000 out_dbfs=4.771
col=3600 f=18721.1 src_start=1597 src_end=1600 n_src_bins=3 power_sum=3.000000 out_dbfs=4.771

invariant_violations = 0
max out_dbfs over all columns = 9.031  (10*log10(8), the Nyquist-edge count ceiling)
```

Raw input at the onset region (`k=557..559`, `f≈6533 Hz`) is genuine
noise-floor amplitude on the order of `1e-14` — i.e. the aggregator is being
handed values that are correctly near-silent, and manufacturing `+3 dB`
purely from `n_src_bins=2`.

## Proposed fix sketch (NOT applied — proposal only)

Two options, both localized:

1. **Convert at the call site** (smaller diff, matches handoff's fencing
   which forbids touching the aggregator/renderers): in `monitor.rs`, map
   `r.spectrum` / `spectrum_only()`'s output through
   `amplitude_to_dbfs` (or a `Vec`-mapped equivalent) before calling
   `spectrum_to_columns_wire` / `_multiband_wire`. Same for `lf_spec_cache`.
   Touches: `ac-daemon/src/handlers/audio/monitor.rs` only.
2. **Push the conversion into `aggregate.rs`** so the function's contract
   matches what callers actually have (accept linear amplitude, convert
   internally) — larger diff, changes the public contract of a function
   with its own doc comments and dedicated test suite (`#[cfg(test)] mod
   tests` in `aggregate.rs`), and would need those tests re-derived since
   they currently construct dB fixtures directly. Touches:
   `ac-core/src/visualize/aggregate.rs` + its tests +
   `ac-ui/src/app/render_pipeline.rs`'s `samples_on_axis_to_columns` caller
   (transfer path — check whether *that* caller already converts to dB
   correctly, since it wasn't in scope here, before assuming it needs the
   same fix).

Option 1 is lower-risk and matches the handoff's fencing exactly. Option 2 is
more architecturally honest (the aggregator's contract should be
self-enforcing) but is a bigger surface and needs architect sign-off since
`aggregate.rs`'s tests encode the current (broken-in-practice) contract as
intentional behavior.

Either way: **before shipping a fix**, resolve the ~9 dB magnitude gap noted
above (my repro caps at +9-10 dB, field data shows +19 dB) — otherwise the
fix might correct the visible symptom while leaving a second, smaller
contributor unaddressed. QA's conformance test ("no output bin exceeds 0
dBFS for any bounded input") will catch this regardless, so it's not
blocking, just worth understanding before signing off.
