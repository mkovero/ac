<!-- agent: architect -->

# design-mtw-ladder — crossover placement and decimator phase lock

**Updated for handoff revision 3.** The averaging section of the original
draft argued from a uniform wall-clock time constant, which revision 2
specified and revision 3 rejected. The *conclusion* — uniform N, not uniform
τ — was ratified; the mechanism is a plain mean of the last N blocks, not an
exponential one. Bottom stage is 4000 Hz, not 3000. If you find `τ = 1 s`,
`α = 0.3401` or a 3000 Hz bottom stage below, that is a section I missed —
stop and check against the handoff.

Scope: the two items `$AC_HOME/handoff/handoff-mtw-live-spectrum.md` routes to architect —
**ladder crossover placement**, and **the decimator phase-lock mechanism
(deliverable 1), "where the alignment guarantee is actually won or lost".**

Companion to `docs/design/design-mtw-alignment.md`, which settled alignment (option A:
one signed integer offset per pair, applied at full rate before decimation).
That decision is taken as given here and is load-bearing throughout.

---

## design decision 1 — crossover placement

### core question

**What sets the frequency where one ladder stage hands over to the next: the
display density, the decimator's passband, or a fixed ladder constant?**

The handoff fixes the stages (`sr` / 12000 / 4000 Hz, NFFT 4096) but not the
boundaries between them. Everything downstream depends on the answer: which
band each column is sourced from, where the blend sits, and whether changing
points-per-octave silently re-times the analysis.

### the quantity everything hangs on

For a log grid of `P` points per octave, a column at `f` has width
`f · (2^(1/2P) − 2^(−1/2P))`. It holds ≥ 1 bin only when

    f  ≥  Δf · κ(P),      κ(P) = 1 / (2^(1/2P) − 2^(−1/2P))

`κ(48) = 69.2488`. This is not a new constant — it is the number already
implicit in the handoff: at `Δf = 1 Hz` it puts the validity edge at 69.25 Hz,
and `48 · log₂(69.2488/20) = 86.01`, the handoff's "86 columns are invented".
Call `f_valid(b) = κ · Δf_b` a stage's **validity edge**.

### option A — crossover at the display density's validity edge

Stage `b` hands over to stage `b−1` at `f_valid(b−1)` computed from the
**live display PPO**. Each column is served by the shallowest (fastest) stage
that can support the requested density.

*tradeoffs:* optimal responsiveness by construction, and the crossover needs no
constant of its own. But deliverable 3 makes PPO a parameter — so under option A
a display-density knob moves the crossovers, which changes the window and
averaging behind every column in the mid-band. A display setting would silently re-time
the analysis, and two screenshots at different PPO would not be comparable.

### option B — crossover at a fixed fraction of the decimated Nyquist

`f_top(b) = α · R_b/2`, α a ladder constant (α = 0.27 reproduces option A at
48 kHz).

*tradeoffs:* PPO-independent and states the passband margin directly. But it is
blind to `sr`. At 192 kHz stage 0 resolves 1/48 octave down to 3246 Hz, while
α = 0.27 hands over at 811 Hz — so 811–3246 Hz is served by stage 0 at a density
it cannot support and the columns thin out, in a band where stage 1 was
available and idle. A visible density notch in the midrange that exists only
because the ladder ignored the rate it was given.

### option C — crossover at a *reference*-density validity edge  ← recommended

`f_top(b) = κ(P_ref) · Δf_{b−1}` with **`P_ref = 48` a ladder constant,
distinct from the display PPO.** The ladder is built to support 1/48 octave;
the display may ask for more or less, and where it asks for more than the
serving stage can deliver, density drops per deliverable 3 — but the crossover
does not move.

### recommendation

**Option C.** It keeps option A's `sr`-adaptivity (crossovers derive from
`Δf_b`, which derives from `sr`) and option B's PPO-independence, and it
reproduces the boundaries already tabulated in `docs/design/design-mtw-alignment.md`'s
phase-error table (811 / 203 Hz at 48 kHz) rather than inventing a
second set. `P_ref` is the ladder's *design* density and belongs next to
NFFT 4096 and the target rates; display PPO belongs in the frame.

The two must be named separately in code. A single `COLS_PER_OCTAVE` reused for
both is how option A gets reintroduced by accident.

### resulting layout

`M_b = round(sr / R_target_b)`, `R_target = [—, 12000, 4000]`, NFFT 4096:

| sr | stage | M | R_b | Δf | window | validity edge = band bottom | band top |
|---|---|---|---|---|---|---|---|
| 44100 | 0 | 1 | 44100 | 10.767 | 92.88 ms | 745.58 Hz | 22050 |
| | 1 | 4 | 11025 | 2.6917 | 371.5 ms | 186.40 Hz | 745.58 |
| | 2 | 11 | 4009.1 | 0.97878 | 1021.7 ms | 67.78 Hz | 186.40 |
| 48000 | 0 | 1 | 48000 | 11.719 | 85.33 ms | 811.51 Hz | 24000 |
| | 1 | 4 | 12000 | 2.9297 | 341.3 ms | 202.88 Hz | 811.51 |
| | 2 | 12 | 4000 | 0.97656 | 1024.0 ms | 67.63 Hz | 202.88 |
| 96000 | 0 | 1 | 96000 | 23.438 | 42.67 ms | 1623.0 Hz | 48000 |
| | 1 | 8 | 12000 | 2.9297 | 341.3 ms | 202.88 Hz | 1623.0 |
| | 2 | 24 | 4000 | 0.97656 | 1024.0 ms | 67.63 Hz | 202.88 |
| 192000 | 0 | 1 | 192000 | 46.875 | 21.33 ms | 3246.0 Hz | 96000 |
| | 1 | 16 | 12000 | 2.9297 | 341.3 ms | 202.88 Hz | 3246.0 |
| | 2 | 48 | 4000 | 0.97656 | 1024.0 ms | 67.63 Hz | 202.88 |

Below the deepest validity edge (67.6–67.8 Hz) no stage is valid at 1/48
octave; `48 · log₂(67.63/20) = 84.5` columns thin out. The ladder does not
shrink the honest-density gap much against today's 86 — the bottom rung
reproduces today's LF resolution deliberately, per revision 3. What it changes
is that those columns stop being **fabricated**: the grid widens to ~49 real
columns instead of asserting 84 interpolated ones.

### the 3-stage ladder has a rate ceiling — the layout must be fallible

`f_top(1) = κ · sr/4096 = 0.016906 · sr`, and it must stay inside stage 1's
passband. At 192 kHz `f_top(1)/Nyquist₁ = 3246/6000 = 0.541` — usable, but that
is the limit, reached exactly at the highest rate criterion 2 requires. (The
bottom rung is never the binding one: its top edge sits at 202.9 Hz against a
2000 Hz Nyquist.) Guard:

    f_top(1) ≤ 0.45 · R_1   ⇒   sr ≤ 319.4 kHz

Above that the ladder needs an intermediate stage (target `4·R_1 = 48000`).
So the layout function returns a **`Vec<Stage>` and is fallible** — not a
3-element array. A fixed `[Stage; 3]` passes criterion 2 today and breaks
silently the first time someone opens a 384 kHz device.

### blend placement (deliverable 2)

Put the overlap **above** the validity edge, not centred on it:

- below `f_x = f_top(b)`: deeper stage `b` only
- `[f_x, f_x · 2^(1/3)]`: cosine-in-log-f crossfade, weight of the shallower
  stage ramping 0 → 1 (16 columns at 48 PPO)
- above: shallower stage `b−1` only

Centring the blend on `f_x` would source columns from stage `b−1` *below* its
own validity edge, which is criterion 1 violated inside the mechanism built to
satisfy it. Placed above, no column is ever drawn from a stage that cannot
support it, structurally.

**Blend `H1` (complex) and `γ²`, not dB and not the cross-spectra.** dB
averaging is biased and is the `aggregate.rs` double-conversion class again.
The cross-spectra are the wrong domain for a different reason: `S_xy` from
stage `b` carries `|H_dec,b|²` and a stage-specific normalisation, so blending
them would require deconvolving `|H_dec|` — reintroducing synthesis at exactly
the frequencies the crossover exists to handle. `H1` and `γ²` are already
dimensionless and stage-independent (that is the whole cancellation argument),
so they blend directly. Criterion 4 is untouched: the averaging is still
upstream of the division; the blend is downstream of it and operates only on scale-free
quantities.

A complex-`H1` blend nulls if the two stages disagree in phase. They must not —
both estimate the same `H1` from the same aligned pair. Leave it unguarded: a
null at a crossover is a correct, loud symptom of a phase-lock or alignment bug,
and is worth more than a magnitude-blend that hides it.

### per-column metadata in the blend region

Report the **deeper** stage's Δf and window for blended columns (single-valued,
monotone in `f`, conservative — it bounds how stale the column can be), plus the
blend weight. Reporting the dominant stage's makes the window non-monotone
across the crossover, which reads as a data artifact.

---

## design decision 2 — decimator phase lock

### core question

**What structure makes it impossible for the two channels of a pair to
decimate out of phase?**

Per `docs/design/design-mtw-alignment.md`, `H1 = G_xy/G_xx` is transparent to the decimator
— *provided both channels traverse identical chains*. That proviso is the entire
justification for there being no per-band offset. Two decimators with
independent sample counters can drift by up to `M−1` samples: 20 ms at
`M = 64`/3 kHz. That is not a degradation, it is total HF coherence loss, and it
is silent.

### option A — two decimator instances, identical coefficients, reset together

*tradeoffs:* obvious and easy to write. But the invariant is a runtime property
maintained by convention. Any path that feeds the two instances different
sample counts — a short read, an error branch, a warmup discard on one leg —
desyncs them permanently with no error and no way to detect it from the output
except as the coherence collapse we are trying to prevent. `transfer.rs:501`
already documents this exact class of bug being found the hard way, where
`capture_block` advanced the fake engine's meas-side counter with no ref-side
advance.

### option B — one two-channel decimator, one shared phase counter  ← recommended

A `PairDecimator` owning one coefficient set, both channels' delay lines, and a
**single** phase counter. Its only input method takes `(&[f32] meas, &[f32] ref)`
and rejects unequal lengths. There is no API through which one channel can
advance without the other.

*tradeoffs:* the invariant becomes a property of the type rather than of the
call sites. Costs nothing — the delay lines were needed anyway.

### recommendation

**Option B.** The guarantee that removes per-band alignment is worth a type,
not a comment.

Scope it to the **pair**, not to unique channels. A shared reference feeding two
pairs cannot be decimated once, because alignment is applied at full rate and
each pair has its own offset; `docs/design/design-mtw-alignment.md` already accepted this
("one aligned read per band from a shared history"). Quantising the offset to a
common multiple to allow sharing was considered and is rejected: at 44.1 kHz
`lcm(4,15) = 60` samples leaves up to 0.68 ms of residual delay — harmless for
coherence against an 85 ms window, but a linear phase tilt of hundreds of
degrees across stage 0's band, and phase is displayed.

### independent chains per stage, not a cascaded tree

A tree (stage 2 fed from stage 1's output) is cheaper and inherits lockstep for
free, but **it does not exist at 44.1 kHz**: the handoff ratifies `M = 1/4/15`,
and 15 is not a multiple of 4. There is no nesting-preserving alternative worth
having — constraining `M_2` to a multiple of 4 gives 2756 Hz, 8.1% off target
against the ratified 15's 2.0%.

So: **each stage decimates independently from the aligned full-rate pair.**
Criterion 2 then passes at all four rates with no rate-specific branch, and a
class of "do the stages stay in phase with each other" bugs never exists.

Cost, Kaiser design at 90 dB stopband, passband to the stage's band top,
stopband from `R_b − f_top` (aliasing into the *unused* part of the decimated
band is harmless — it is served by the stage above):

| sr | stage | taps | MAC / input sample |
|---|---|---|---|
| 48 k | 1 | 27 | 6.7 |
| | 2 | 106 | 6.6 |
| 96 k | 1 | 63 | 7.9 |
| | 2 | 212 | 6.6 |
| 192 k | 1 | 199 | 12.4 |
| | 2 | 423 | 6.6 |

≤ 19 MAC per input sample per channel — ~7 MMAC/s per pair at 192 kHz, noise
against the 4096-point FFTs. The tree would save ~6 MAC/sample. Not worth a
special case at 44.1 kHz.

Longest transient is 423 taps = 2.2 ms at 192 kHz, well inside the ~0.15 s
transient allowance in the alignment doc's retention table.

### the pipeline must push, not re-segment

Defect 3 in the handoff (#208) is that a **sliding re-segmented Welch
re-analyses an impulse for the whole window**, giving `n_averages` weight maxima.
Feeding the ladder from the existing `rings` sliding buffer would reproduce it
one level down. The ladder must be a **push pipeline**: per tick, the aligner
emits synchronised `(meas, ref)` samples, the decimators consume them, each
stage emits a frame whenever NFFT samples have accumulated at hop `W_b/2`, and
the block average updates. Block `k` always covers decimated samples
`[k·HOP, k·HOP + NFFT)`, so the boundaries are a property of the stream rather
than of how the drain chunked it — that is criterion 5b, and it is the #208
fix.

Consequence for retention: streaming needs only `|offset| + taps` (~0.3 s), not
`W_deepest + |offset|` (~2 s). The 2 s figure in `docs/design/design-mtw-alignment.md`
still stands — it sizes the history the snapshot path re-reads — but the live
ladder is not what consumes it.

Aligner sign handling, since the offset is negative today: `estimate_delay`
returns `D` such that meas lags ref by `D`, so the pair is `(meas[n],
ref[n−D])`. For `D < 0` that is a future ref sample, so the aligner delays meas
instead and emits `(meas[n − max(0,−D)], ref[n − D − max(0,−D)])`. Both legs
buffer; neither branch is the "normal" one.

---

## averaging — where the crossover meets the statistics

This started as a flagged conflict and is now the ratified design, so it is
recorded here as reasoning rather than as an objection.

**A uniform wall-clock time constant cannot satisfy criterion 3.**
`E[γ̂²] ≈ γ² + (1−γ²)/N`, and `N ∝ 1/hop`. Uniform τ = 1 s at 96 kHz:

| stage | window | hop | N at τ = 1 s | coherence bias 1/N |
|---|---|---|---|---|
| 0 | 42.7 ms | 21.3 ms | ≈ 47 | 0.02 |
| 1 | 341 ms | 171 ms | ≈ 5.9 | 0.17 |
| 2 | 1024 ms | 512 ms | ≈ 2.0 | 0.50 |

A ~0.5 step in γ² at the 202.9 Hz crossover, fixed in frequency, reading as a
property of the DUT — precisely what criterion 3 exists to catch. Not a tuning
problem: four independent 1.024 s windows require 2.56 s of audio and cannot be
had from a 1 s τ.

**Deliverable 2's "variance matched" therefore means uniform N**, and revision
3 ratifies that. `N = 4` puts the bias at 0.25 everywhere — the same figure the
full-rate estimator has today — with no step at any crossover.

**Plain mean, not exponential.** The first draft of this document proposed an
EMA with a per-band τ derived from a uniform effective-average count. That
reaches the same
variance but settles in roughly seven blocks against four, and it buys nothing:
what actually fixes #208 is fixed block boundaries, not the shape of the
averaging window. An exponential window also makes "which blocks is this
column made of" unanswerable, which the per-column `n` is supposed to answer.

Settling is `W + hop·(N−1)`:

| stage (96 kHz) | settling at N = 4 |
|---|---|
| 0 | 0.107 s |
| 1 | 0.853 s |
| 2 | 2.560 s |

The bottom matches today's 2.5 s — **low frequency is not made slower**, which
was the whole objection to the 3000 Hz / exponential revision (4.9 s). The top
improves roughly twentyfold.

`N` is not free to lower. It is uniform across stages, so dropping it to speed
the bottom up raises the coherence floor across the *entire* display: 0.33 at
N = 3, 0.50 at N = 2. At 0.50 a coherence reading has stopped meaning anything.

One consequence for the reported figure: the blocks overlap 50%, so the
variance-equivalent count is `N/(1 + 2ρ(N−1)/N)` = 3.2 at N = 4, ρ = 1/6, and
the measured bias on uncorrelated inputs lands near 0.31 rather than exactly
0.25. The frame reports the nominal `N`, matching today's convention; the gap
is criterion 5's tolerance, and it is a derived number rather than slop.

---

## coherence depth — measured, not modelled

Recorded because two successive models of this were wrong, and the second
reached the code.

**What the variance-matching argument covers.** Holding `N` uniform across
stages equalises the *block* contribution to coherence bias. That part is
sound and is why the ~0.5 step a uniform wall-clock τ would produce does not
appear.

**What it does not cover.** A display column sums several FFT bins before the
division, so the effective averaging depth is set by blocks **and** bins. Bins
per column is `column_width / Δf`, and column width is fixed by the display
density while Δf jumps by the decimation ratio at a crossover — so the bin
count drops discontinuously there. Worse, it is guaranteed rather than
incidental: crossovers sit at the reference-density validity edge, which is
*defined* as the frequency where one bin fills one column, so the upper side is
pinned at one bin by construction.

A residual coherence step therefore remains. Measured on a partially coherent
stimulus (true γ² = 0.5), 24 runs, ~36 columns each side:

| crossover (96 kHz) | below | above | step |
|---|---|---|---|
| 1623.02 Hz | 0.5170 (6.2 bins) | 0.5671 (1.7 bins) | 0.050 |
| 202.88 Hz | 0.5561 (2.4 bins) | 0.5515 (1.7 bins) | 0.005 |

It scales with `(1 − γ²)`, so it is worst where coherence is already poor.
**Accepted and documented rather than engineered away:** moving crossovers does
not help, because the upper side is pinned at one bin whatever the ratio, and
the whole spread across ratios 2.75→16 is under 0.09. A fourth stage buys a
fraction of that.

### Two corrections, both measured

**1. Welch's ρ = 1/6 does not apply to coherence bias.** It corrects the
*variance of a power-spectrum estimate* for 50% overlapping Hann segments; the
magnitude-squared coherence bias is a different functional. Single-bin,
non-blend columns, 30 runs per point:

| N | 1/N | 1/N_var (ρ=1/6) | measured floor | implied N_eff |
|---|---|---|---|---|
| 2 | 0.5000 | 0.5833 | 0.5053 | 1.98 |
| 4 | 0.2500 | 0.3125 | 0.2548 | 3.92 |
| 8 | 0.1250 | 0.1615 | 0.1309 | 7.64 |
| 16 | 0.0625 | 0.0820 | 0.0681 | 14.68 |

The floor tracks nominal `1/N`; overlap costs under 10%, growing slowly with
N. The "corrected" figure was *further* from truth than the uncorrected one.
`variance_equivalent_blocks` has been removed rather than left available.

**2. Depth is sublinear in bins.** Pooled by bin count, depth runs 4.04 at one
bin to 16.28 at eight — 4x for 8x the bins. Adjacent bins of a Hann-windowed
segment are correlated, so summing K adjacent bins does not give K-fold
reduction.

### Rule

**Ship the inputs, not a derived depth.** The frame carries blocks actually
held (`mtw.n`) and bins per column (`mtw.bins`). Nothing derives an "effective
depth" from them, because no validated model exists — a `bins^0.67` fit is a
two-point fit, and Hann main-lobe correlation would predict something closer to
a fixed effective-bin count than a power law. Do not let either harden into a
constant. If a depth figure is wanted, fit it against the tables above first
and file it as its own change.

---

## design decision 3 — points-per-octave is fixed at 48, not a user control

Ratified 2026-07-28 after the rig session. Internal parameterisation for tests
is fine; what is decided is that **PPO is not exposed**.

The coupling runs opposite to intuition. A coarser display means wider
columns, more phase rotating across each column, and therefore a *tighter*
delay requirement — the opposite of the "smoother, more forgiving picture" a
user would expect to be asking for:

| density | delay tolerance at 20 kHz |
|---|---|
| 1/96 oct | 1233 µs |
| 1/48 oct | 616 µs |
| 1/24 oct | 308 µs |
| 1/12 oct | 154 µs |
| 1/6 oct | **77 µs** |

Someone smoothing the trace for readability would silently make their
measurement eight times more delay-fragile. At 77 µs a 50 ppm clock offset
between two devices consumes the entire budget in 1.5 s, so the display
control would quietly dictate the re-lock interval.

The mechanism is the aggregation this document already describes: a column
sums `Sxy` across its bins before the division (`splice.rs:8` — a column's
coherence is a statement about the bins it contains), so a residual delay `τ`
rotates phase across the column bandwidth and the coherent sum falls as
`sinc²(τ·BW)` while `Sxx` and `Syy` do not. Column bandwidth is proportional
to both centre frequency and column width, hence the table.

Measured, not derived — stage-0 path simulated exactly (sr 96000, nperseg
4096, 8 blocks, bins summed per column before coherence), 1/48 octave at
20 kHz, BW 288.8 Hz: γ² tracks `sinc²(τ·BW)` within ±6% out to τ·BW ≈ 0.45,
with the γ² = 0.9 crossing at **625 µs = 12.5 cycles at 20 kHz**.

Readability is served instead by fractional-octave smoothing of the
already-computed curve (issue #229). Because smoothing runs *after* coherence
is formed it cannot reintroduce delay sensitivity however heavy it is: PPO 48
smoothed to 1/6 octave gives the same picture as PPO 6 while keeping the
616 µs tolerance rather than 77 µs.

Two consequences worth keeping together with this: the aggregation that
produces the coupling is deliberate and is not being reopened, and the fault
indicator (#228) needs no density input because this number is fixed.

---

## design decision 4 — smoothing's band geometry is base-2

Recorded with #229's implementation, which the issue required to decide this
rather than inherit it.

**Display smoothing uses `2^(1/(2·bpo))` as its half-band factor, not IEC
61260-1's `G = 10^(3/10)`.** The reasons, in order of weight:

- the columns being averaged sit on this ladder's `2^(1/P)` grid (deliverable
  7, above). A window specified in base-2 octaves therefore spans a fixed
  number of columns across the axis; a base-ten window would breathe against
  the grid it slides over, for no gain;
- smoothing is Tier 2 and claims no conformance, so it has no call on the
  normative ratio. `visualize/fractional_octave.rs`'s `ioct_band_centers` /
  `ioct_band_edges` — which *do* use `G_OCTAVE`, because they aggregate into
  named bands that are reported as such — are deliberately **not** reused by
  `visualize/smoothing.rs`.

This is the two-conventions problem in its third place, and the answer is the
same as deliverable 7's: the constants are not two spellings of one thing, and
unifying them would look like a tidy-up while moving a window edge. The
difference is 0.24%, which is invisible in a smoothing window and is not the
point — the point is that the next person to unify them finds two written
refusals instead of one.

**Window shape, decided with it.** Hann in log frequency, not rectangular. A
boxcar average puts small ripples either side of a deep narrow notch — content
the measurement does not have, at the frequency the operator is looking hardest
at. Hann's taper costs width, so the full width carries the 1.5x ENBW
correction (`(∫w)²/∫w² = 2/3`) and the half-width is `2^(1.5/(2·bpo))`. Without
that correction "1/6 octave" would smooth like 1/9 and the caption would
overstate what was done.

**Where the code is.** `ac-core/src/visualize/smoothing.rs` holds the maths
(magnitude in dB, phase unwrapped then wrapped by the caller, coherence never
touched, masked columns excluded from every window);
`ac-scene::transfer::Smoothing` owns the designators and the on-screen label;
`ac-view` holds the operator's choice (`N`) and draws that label verbatim.

**What did not change.** The daemon still does not smooth, and
`ProcessingChain.smoothing_bpo` on the wire still reports that truthfully.
Display smoothing is applied after the frame, in the view, and is announced on
screen so a screenshot cannot claim a resolution the measurement did not have.

---

## affected modules

- `ac-core/src/visualize/mtw/ladder.rs` — stage layout from `sr`; fallible,
  returns `Vec<Stage>`; owns `P_REF = 48`, `NFFT = 4096`, target rates,
  validity edges, crossovers, blend ranges.
- `ac-core/src/visualize/mtw/decimate.rs` — `PairDecimator`: one coefficient
  set, both delay lines, one phase counter, equal-length-only input.
- `ac-core/src/visualize/mtw/align.rs` — per-pair signed-offset aligner, both
  sign branches, emits synchronised pairs.
- `ac-core/src/visualize/mtw/average.rs` — per-stage plain mean of the last
  `N` blocks of `Sxx`, `Syy`, `Sxy`; no state on `|H1|` or any dB quantity.
- `ac-core/src/visualize/mtw/splice.rs` — column assembly, crossfade over
  `H1`/`γ²`, per-column Δf/window/N.
- `ac-daemon/src/handlers/transfer.rs` — the ladder consumes the per-tick
  `bufs` (push). `rings` and the `nperseg = sr` Welch **stay** — see below.

## interface changes

`ac-core` gains the `mtw` module. No existing signature changes.

## ZMQ protocol impact

Additive only, as the handoff's wire contract specifies: per-column Δf/window,
N, blend weight. No field removed or renamed. `ds` consumes session state,
not transfer frames.

## implementation notes for developer

- **The ladder is additive; the full-rate Welch survives.** Criterion 7 pins
  `spl` bit-identical, and `spl` is derived from `result.meas_amp` ←
  `gyy` ← the `nperseg = sr` Welch over `target_total`
  (`transfer.rs:463–466`, `:774–783`). Any attempt to re-derive `spl` from the
  ladder fails criterion 7 by construction. Keep both consumers of `bufs`.
- **The ladder carries `H1`, `γ²` and `N` only.** `meas_spectrum` /
  `ref_spectrum` (`transfer.rs:811–826`) stay on the full-rate path with `spl`:
  `Sxx` is multiplied by `|H_dec|²` rather than cancelling it, so putting them
  on the ladder means deconvolving the decimator near the band edges. This is
  the handoff's SPL fence, applied to the other two absolute-level arrays for
  the same reason. Deliverable 3 (interpolation removed, PPO parameterised)
  still applies to them — that is a separate fix to the same defect class, at
  full rate.
- Deliverable 3 changes the *column count* of `meas_spectrum` below the
  validity edge, not just the values. `spec_freqs` already ships alongside, so
  consumers reading the paired arrays are fine — but anything assuming
  `transfer_spectrum_n_columns(20, sr/2)` is not.
- `TRANSFER_SPECTRUM_COLS_PER_OCTAVE` (`aggregate.rs:14`) must not become the
  ladder's `P_REF`. Same number today, different meaning, and option A above is
  what happens when they are unified.
- Interpolation branch to remove: `aggregate.rs:109–124` (the `count == 0`
  path). Its doc comment at `:53–57` documents the behaviour and must go with
  it.
- Model the `sr`-derived layout test on criterion 2's four rates from the
  start. 44.1 kHz is the one that finds tree-shaped assumptions.
- `measurement/loudness/truepeak.rs` has the BS.1770 4-phase 48-tap polyphase
  interpolator — a structural model for the polyphase indexing, not something
  to reuse (it interpolates, and its coefficients are normative).
- Deliverable 7: `κ` is built from `2^(1/96)`, the MTW convention. It must not
  be rebuilt on `G = 10^(3/10)`. Record the 0.24% divergence where both are
  visible.

## risks

- **Phase counters drift silently.** Mitigation: option B makes it
  unrepresentable. Test: feed a pair through, assert every stage's decimated
  outputs are equal-length and that a deliberate one-sample injection on one
  leg fails to compile / panics rather than being absorbed.
- **`P_REF` and display PPO get unified during review** as "the same 48".
  Mitigation: different names, different modules, and a test that changes
  display PPO and asserts the crossover frequencies are unchanged.
- **A fixed `[Stage; 3]` ladder.** Passes all four required rates and breaks
  above ~319 kHz. Mitigation: fallible `Vec<Stage>` and a test at 384 kHz
  asserting either a fourth stage or a clean error.
- **Blend region drifts below a stage's validity edge** during tuning.
  Mitigation: assert `blend_lo ≥ f_valid(shallower)` in the layout constructor,
  not at the call site.
- **Criterion 3 is signed off against a fully coherent stimulus.** γ² = 1
  everywhere hides exactly the N step described above. The handoff already
  requires a partially coherent stimulus; the risk is that it gets relaxed when
  the test is hard to stabilise. Mitigation: QA owns the stimulus choice, and
  the coherence-continuity assertion should fail if `N` differs across a
  crossover, independently of the measured γ².
- **Criterion 6 cannot detect #216** — carried over from
  `docs/design/design-mtw-alignment.md`. Verify skew from per-ring occupancy
  (`AC_DRAIN_TELEMETRY`, #215), never from coherence, magnitude or delay.
