# handoff-mtw-live-spectrum — a usable live transfer display

**REVISION 3 — ratified. Check you are reading this one before starting work.**
Revision 2 specified exponential averaging with a uniform wall-clock time
constant and a 3000 Hz bottom stage. **That was rejected** — it settles in
4.9 s at low frequency, which is unusable for live tuning. If a plan mentions
`τ = 1 s`, `α = 0.3401`, or a 3000 Hz bottom stage, it is built from the stale
revision. Stop and re-read.

Base: `main`. Tier: **2 (live display only).** Rig: 192.168.9.25 (96 kHz).

---

## Goal

A live transfer display good enough to tune a sound system with — the job
Smaart's live side does. **Transfer magnitude, phase and coherence only.**

Every displayed point backed by real bins. High frequencies fast enough to
hunt buzzes. No fabricated values. No repeats on transients. One mode. No new
reported numbers.

**Out of scope by decision, not deferral: the per-channel level curves**
(`meas_spectrum`, `ref_spectrum`). They are not displayed by this slice, so
there is nothing to decide about where they come from. SPL keeps its existing
full-rate path, untouched and undrawn.

This matters because the repeats the reporter saw and the transfer magnitude
come out of the same computation. Fixing the transfer path fixes what was
being watched. When #208 is re-checked on hardware, it is checked on transfer
magnitude.

## Why the current one isn't

1. **86 columns are invented.** `TRANSFER_SPECTRUM_COLS_PER_OCTAVE = 48.0` is
   asserted independently of resolution. With `nperseg = sr` (Δf = 1 Hz) that
   density is real only above ~69 Hz; below it the aggregator's interpolation
   branch fills in. For subwoofer and fault work those columns are fabricated.
2. **One window for the whole band.** `nperseg = sr` forces the LF window
   length onto HF, so a 15 kHz rattle is as slow to appear as a 20 Hz
   reading.
3. **Transients ripple.** A sliding re-segmented Welch window re-analyses an
   impulse for the whole window, with `n_averages` weight maxima at 6 dB —
   this is #208, confirmed by the reporter's `n_averages = 2` experiment.

---

## Design

### Ladder

Stage 0 is always full rate. Lower stages target **fixed decimated rates**,
so their windows and validity are identical at every sample rate. NFFT 4096
throughout:

| stage | decimated rate | Δf | window | 1/48 oct valid above |
|---|---|---|---|---|
| 0 | `sr` | `sr`/4096 | 4096/`sr` | scales with `sr` |
| 1 | 12000 Hz | 2.93 Hz | 0.341 s | 203 Hz |
| 2 | 4000 Hz | 0.98 Hz | 1.024 s | 67.6 Hz |

Decimation factors are **derived**, not tabulated: 48 kHz → 1/4/12;
96 kHz → 1/8/24; 192 kHz → 1/16/48. 44.1 kHz gives 1/4/11 (4009 Hz) — a few
percent off, recorded as accepted variance rather than discovered in testing.

Stage 2 reproduces today's LF resolution and reach (0.98 Hz, honest to
67.6 Hz). Finer LF resolution is bench mode's job and bench mode is deferred;
buying it here would cost settling time in the one mode that can't afford it.

Specifying by decimated rate rather than by factor is what makes the ladder
behave identically at 48 and 96 kHz. A fixed 4× step does not.

### Honest density — the load-bearing rule

**No column on the transfer display is ever synthesised from neighbours.**
Where density exceeds resolution, density drops. Points-per-octave becomes a
parameter, not a constant. Each column ships the Δf and window that produced
it; without that a screenshot is not interpretable.

**Do not touch `aggregate.rs`'s interpolation branch.** The peak-picker tests
(`close_tones_resolve`, `close_tones_resolve_multiband`) depend on it to
produce strict local maxima, and criterion 9 forbids editing pre-existing
assertions. The transfer display's grid widens rather than reaching that
branch, so the rule above holds without removing it. The only thing that
branch still fabricates is the per-channel level curves below 69 Hz — which
this slice does not display.

### Averaging

**Fixed block boundaries, one analysis per block, plain average of the last
N = 4 completed blocks, N uniform across all three stages.**

Fixed boundaries are what fixes #208. Today's code cuts analysis blocks from
the head of a buffer that slides by a variable amount each tick, so a
transient's position inside the block layout shifts and it is re-analysed at a
different weighting each time — that is the `n_averages` weight maxima the
reporter confirmed. Analyse each block of audio exactly once and the artifact
cannot form, whatever the averaging.

- Averaging applies to **Sxx, Syy, Sxy** — never to `|H1|`, never in dB.
  Averaging after the division biases magnitude and destroys coherence's
  meaning. Same class as the `aggregate.rs` double-conversion incident.
- **N uniform, not the time constant uniform.** A uniform wall-clock time
  constant gives different effective averages per stage (47 / 5.9 / 1.5 at
  96 kHz), hence coherence bias 0.02 / 0.17 / 0.68 and a 0.5 step at a fixed
  frequency that reads as a DUT property. Uniform N = 4 gives 0.25 bias
  everywhere — identical to today, and no step.
- Exponential averaging was considered and rejected: it settles in ~7 blocks
  against 4 for the same statistical quality, and it is not needed once block
  boundaries are fixed.
- Settling is `W + hop × (N−1)`. At 96 kHz, N = 4: top stage 0.11 s, middle
  0.85 s, bottom **2.56 s**. Today's bottom is 2.5 s — so low frequency is
  unchanged, and the top improves roughly twelvefold. Do not claim the bottom
  gets faster; it does not.
- N = 4 chosen deliberately. N = 3 would give 2.05 s and N = 2 1.54 s, but N
  is uniform across stages, so lowering it raises the coherence floor over the
  whole display (0.33 and 0.50 respectively). At 0.50 a coherence reading
  stops meaning anything. The bottom stage cannot settle faster than ~1 s at
  0.98 Hz resolution in any case — that is arithmetic, not implementation.
- N reported in the frame: coherence from uncorrelated signals floats near
  1/N, so a coherence reading cannot be judged without it.

### Reference alignment

Not optional, and not a refinement. Coherence follows `γ² = ((W − D)/W)²` for
window `W` and DUT delay `D` — measured 0.6441 against 0.64 predicted in #216.
Stage 0's window is short, so at 96 kHz it reaches **zero coherence by
D = 50 ms**. Post-hoc phase rotation yields no usable HF at all on any delayed
DUT.

Per `docs/design/design-mtw-alignment.md`, alignment is a **single signed integer offset
per pair applied at full rate before decimation**. Decimation latency is
common-mode and cancels in H1 provided both channels traverse identical
chains, so there is no per-band offset. The offset is signed and negative on
today's hardware (≈ −19200 at 96 kHz while #216 is live).

Retention: `W_deepest + |offset|_max + tick + transient` ≈ **2 s**, worker-side.
`REF_RING_CAPACITY` is not touched.

### Absolute levels stay off the ladder

`Gxy/Gxx` cancels `|Hdec|²`. `Sxx` does not — it is multiplied by it. So the
cancellation argument covers H1 and **not** `meas_spectrum`, `ref_spectrum`
(`transfer.rs:786,794`) or `spl` (`:749`).

`spl` is the only frame value with a direct standards claim (IEC 61672
weighting, BS.1770 integration) and it is a broadband integral that has no
need of log-frequency columns. **It stays on the full-rate path exactly as it
is today.** This is a rule about what not to do, not work to be done: do not
wire SPL or any calibrated absolute level through the stages for convenience.

The same applies to `meas_spectrum` and `ref_spectrum`, which this slice does
not display at all. Leave them where they are.

### Wire contract

Additive only. Same log-column array, plus per-column Δf/window and N. No
field removed, no semantics changed, no CLI or report consumer affected.

---

## Deliverables

1. Multirate ladder in `ac-core`, stages specified by decimated rate, factors
   derived from `sr`. Polyphase linear-phase FIR decimators, stopband ≥ 90 dB,
   **identical filter instances and phase lock on both channels** — the
   cancellation argument fails if the chains differ, and two decimators with
   independent sample counters can drift by up to `M−1`.
2. Splice and blend across band boundaries, variance matched.
3. Interpolation removed; density follows resolution; PPO parameterised.
4. Per-column Δf/window and N in the frame.
5. Fixed-phase block segmentation; plain average of the last N = 4 completed
   blocks, uniform across stages; N reported in the frame.
6. Single signed per-pair alignment offset at full rate, plus retention
   sizing.
7. Octave-convention note: MTW decimates by 2, IEC 61260-1 uses
   G = 10^(3/10) = 1.99526. They differ by 0.24% and **must not be unified.**
   Record the divergence and its reason where both are visible — the
   `G_OCTAVE` work is open and touching these files.

---

## Acceptance criteria

1. **No synthesised columns on the transfer display.** Every emitted transfer
   magnitude, phase and coherence column maps to ≥ 1 source bin.
   Mutation-verified. This is asserted on the transfer path only —
   `aggregate.rs`'s interpolation branch stays as it is, per the density
   section.
2. **Ladder derives from `sr`.** Passes at 44.1/48/96/192 kHz with no
   rate-specific constants in the layout.
3. **Splice continuity in magnitude.** No step greater than a stated
   tolerance across any crossover.

   **Coherence continuity is accepted-and-documented, not met.** A step is
   structural and cannot be removed by tuning. Crossovers sit at the
   reference-density validity edge, which pins the upper side at exactly one
   bin per column, so its bias is the full 1/N while the lower side is deeper
   by the decimation ratio. Measured 0.0502 at 1623 Hz, 96 kHz, γ² = 0.5.

   Moving crossovers does not help — the step is set by the decimation ratio,
   and it saturates: the spread across the whole ratio range 2.75→16 is 0.088,
   so a fourth stage buys a fraction of that. Ruled out on those grounds.

   Verify the step is **present and of the documented magnitude**, and that it
   does not move with warmup (criterion 10). Do not fail this criterion for
   the step's existence, and do not close it by widening a tolerance.

   Any test for this needs a **partially coherent** stimulus — correlated
   source plus uncorrelated noise. A flat reference is fully coherent, so
   γ² = 1 everywhere; worse, summing K bins of a flat spectrum scales Sxx,
   Syy and Sxy alike, making coherence bin-count-invariant by construction.
   Such a test verifies blend weights and is blind to estimator bias.
4. **Averaging is upstream of the division.** Structural: no averaging state
   on `|H1|` or any dB quantity.
5. **N** present in the frame, reporting **blocks actually averaged**, not the
   configured target. Coherence bias on uncorrelated inputs is **1/N per
   column at one bin**.

   Measure per column, not per stage. A stage average is not 1/N and must not
   be treated as a failure: columns hold more than one bin where the column
   is wider than Δf, and depth grows with bin count, so measured stage
   averages run below 1/N (0.216 / 0.139 / 0.095 at 96 kHz against mean bins
   1.84 / 4.38 / 8.95). That is the bin effect, not an estimator defect.

   **Do not apply an overlap correction to this quantity.** ρ = 1/6 is the
   Welch correction for the *variance of a power-spectrum estimate*; MSC bias
   is a different functional and 50% overlap costs it far less — measured
   N_eff 1.98 / 3.92 / 7.64 / 14.68 against nominal 2 / 4 / 8 / 16. An earlier
   revision of this handoff and its QA brief both instructed otherwise, that
   instruction reached the code, and the "corrected" figure was further from
   truth than the uncorrected one. See `docs/design/design-mtw-ladder.md`, "coherence
   depth — measured, not modelled".
5b. **Each block of audio is analysed exactly once.** Block boundaries do not
   move with the drain. Mutation-verified: reintroduce head-relative
   segmentation and criterion 5b must fail. This is the #208 fix.
6. **Coherence is delay-invariant across all bands** up to D_max = 100 ms,
   including stage 0. Mutation-verified: disable the offset and stage 0's
   coherence must collapse toward `((W − D)/W)²`. A test exercising only the
   deepest band cannot distinguish alignment from rotation.

   *This criterion cannot verify #216* — the offset derives from
   `estimate_delay`, which returns `D − skew`, so alignment absorbs the skew
   and passes by accident. Verify the skew from per-ring occupancy
   (`AC_DRAIN_TELEMETRY`, PR #215), never from coherence, magnitude or delay.
7. **`spl` bit-identical** before and after. This is the conformance guard.
8. **Tier 1 bit-identical** — `ac plot`, RTA, SPL outputs unchanged.
9. Workspace green, zero edits to pre-existing assertions, display-truth
   discipline holds (no `log10`, no measurement formatting in `ac-view`).
10. **#208 does not recur.** *Post-merge, owner Markus* — needs a finger snap
    at the rig, cannot run until this lands, so no agent or CI can hold it.
    Record on the merge PR that #208 shipped closed on a prediction rather
    than a measurement.

---

## Out of scope — deferred, not cancelled

- **Bench mode.** Revisit once the live spectrum works.
- **Snapshot parity implementation.** Option (a) stands as a ratified
  decision; its implementation is a follow-up slice. **Interim consequence,
  recorded deliberately: snapshots derive via Welch and will not match the
  live view.** File it so the divergence is known rather than discovered.
- Tier 1, IEC 61260-1 filterbank, `ac plot`.
- **#216** — both halves. The cheap half (warmup clearing meas and refs
  together) is its own PR; the general coherence loss is addressed by
  deliverable 6 here.
- `SpectrumEmber` merge, grid layout, sweep extraction.
- Error budgets and per-band uncertainty. Tier 1 concern.

## Routing

- **Architect:** ladder crossover placement; the decimator phase-lock
  mechanism (deliverable 1) — this is where the alignment guarantee is
  actually won or lost.
- **QA:** criteria 1–8. Independently re-derive Δf/window per band, analytic
  1/N coherence bias, and the `((W − D)/W)²` ceilings rather than reading them from
  implementation comments. QA chooses the partially-coherent stimulus for
  criterion 3 and the delay sweep for criterion 6. Criterion 7 and 8 are
  verified by comparison against pre-change output, not by inspection.
- **UX:** density change is visible in a value display, so QA sign-off
  precedes `ux-approved`.
