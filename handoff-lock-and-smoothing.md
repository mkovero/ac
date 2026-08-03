# handoff-lock-and-smoothing — decisions, and the issues that follow

Covers the design decisions taken after the 2026-07-28 rig session, and what
they change in the issues already filed from it (#225–#228). Written to be
read cold after a gap.

---

## Decisions, ratified by Markus

### 1. The delay lock is a maintained quantity, not a cached one

A lock held for the life of the session is wrong. Required behaviour:

- **A key to re-lock on demand.** Finnish layout constraint applies — `[`,
  `]`, `+`, `-` are prohibited. Check against existing `ac-view` bindings.
- **Automatic refresh at regular intervals once coherence is lost.**
- **Flushing is acceptable on both paths.** A re-lock invalidates the running
  block averages (blocks accumulated against different alignments cannot be
  mixed), so it flushes and re-settles — 2.56 s at the bottom stage, display
  filling top-down again. Markus accepts this cost on both the manual and the
  automatic path.
- **Two UI states: `LOST LOCK` and `LOCK ACQUIRED`.**

### 2. The re-lock trigger must distinguish three cases

Low coherence is not by itself evidence of a bad lock, and the situation where
re-locking is most tempting is the one where it is most dangerous — #226
exists because the estimator locked against silence.

Signal presence gates on `meas_peak_dbfs` and `ref_peak_dbfs`, both already in
the frame. `LOST LOCK` must not fire merely because the drive is off, or it
cries wolf every time the operator stops.

The full state set is in **#228** below — the same gates feed the indicator, so
the two are one piece of work.

### 3. Points-per-octave is **not** a user control

PPO stays fixed at 48. Internal parameterisation for tests is fine; what is
decided is that it is not exposed.

Reason: the coupling runs opposite to intuition. Coarser display means wider
columns, more phase rotation across each column, and a *tighter* delay
requirement:

| density | delay tolerance at 20 kHz |
|---|---|
| 1/96 oct | 1233 µs |
| 1/48 oct | 616 µs |
| 1/24 oct | 308 µs |
| 1/12 oct | 154 µs |
| 1/6 oct | **77 µs** |

Someone smoothing the trace for readability would silently make their
measurement eight times more delay-fragile — at 77 µs a 50 ppm clock offset
between two devices eats the whole budget in 1.5 s. Keeping PPO fixed removes
the trap, leaves one control instead of two, and means #228's threshold does
not need a density input.

The aggregation that produces this coupling is a deliberate, documented choice
(`splice.rs:8` — a column's coherence is a statement about the bins it
contains). It is not being reopened.

### 4. Readability is served by fractional-octave smoothing instead

Smoothing averages the already-computed curve across neighbouring columns.
Because it happens *after* coherence is formed, it cannot reintroduce delay
sensitivity however heavy it is. PPO 48 with 1/6-octave smoothing gives the
same picture as PPO 6 while keeping the 616 µs tolerance rather than 77 µs.

Standard designators: 1/1, 1/3, 1/6, 1/12, 1/24 octave, plus off.

**Rules that are not negotiable:**

- **Smooth magnitude in dB.** Smoothing complex H1 reintroduces the same
  problem — real and imaginary parts cancel where phase rotates.
- **Phase must be unwrapped before smoothing**, or the average crosses wraps.
- **Coherence is not smoothed**, or only with a visible label. It is the trust
  indicator; smoothing makes a bad measurement look good, which is the one
  direction a measurement instrument must never fail in.

**Half of this already exists.** `ProcessingChain.smoothing_bpo: Option<u32>`
(`report.rs:60`) is carried through daemon provenance
(`handlers/mod.rs:363` — its own comment reads *"reserved; daemon doesn't
smooth today"*), serialised into the frame, and rendered by both report
writers as `"1/6 octave"` or `"off"`. Every producer sets it to `None` and
nothing smooths. The vocabulary, units, naming and provenance plumbing are
already decided and consistent — this fills a declared hole rather than adding
a concept.

Band geometry helpers exist too: `ioct_band_centers` and `ioct_band_edges` in
`fractional_octave.rs`.

**Caveat before reusing them.** Those helpers sit in the path of the open
`G_OCTAVE` work — base-2 against IEC 61260-1's 10^(3/10). Display smoothing is
Tier 2 and does not need conformant band edges, but it must not silently share
a constant that is about to change meaning underneath it. Decide up front
whether smoothing uses its own base-2 geometry or the conformant helper, and
write down which. This is the two-conventions problem in a third place; the
ladder's version is already recorded in `design-mtw-ladder.md`.

### 5. Delay accuracy: about 12 cycles at the top of the band

The coherence loss at high frequency is dominated by **phase rotation across a
column's bandwidth**, not by loss of window overlap. A column at 20 kHz spans
289 Hz at 1/48 octave, `Sxy` is summed across its bins, and a residual delay
rotates phase across that span — so the coherent sum collapses by roughly
`|sinc(τ·BW)|` while `Sxx` and `Syy` do not.

| highest frequency needed coherent | tolerance |
|---|---|
| 200 Hz | 62 ms |
| 2 kHz | 6.2 ms |
| 10 kHz | 1.2 ms |
| 20 kHz | **616 µs** |

Sub-millisecond is a 20 kHz requirement, not a general one. Subwoofer work is
trivially tolerant.

Precision is not the constraint — one sample is 10.4 µs at 96 kHz, 22.7 µs at
44.1 kHz. The threats are all discrete or drifting:

- **Two clock domains.** Same interface for both legs, one clock, no drift.
  Separate devices means two crystals: at 20 ppm, 600 µs of drift in 30 s; at
  50 ppm, 12 s; at 100 ppm, 6 s. Consumer crystals are ±50–100 ppm. This sets
  the automatic re-lock interval — seconds to tens of seconds, not minutes.
- **Xruns.** A dropped buffer shifts alignment permanently by that amount.
- **Wrong peak** (#227) — milliseconds, not microseconds.

---

## Existing issues — what changes

### #226 — scope grows

Was "validate the lock at warmup". Becomes **"the lock is a maintained
quantity"**: manual re-lock key, automatic refresh on lost coherence, flush and
re-settle on both paths, `LOST LOCK` / `LOCK ACQUIRED` states, and the
three-case trigger table above including the signal-presence gate.

The re-lock interval derives from decision 5 — seconds to tens of seconds,
sized against clock drift.

### #227 — acceptance tightens

Acceptance is **sub-millisecond**, not "inside the window". A fix landing in
the right ballpark is not good enough.

**Confirm before this becomes a criterion:** the sinc relation is derived, not
measured. Sweep a synthetic residual 0–2 ms in 100 µs steps and measure γ² at
20 kHz. If it tracks `sinc²(τ·BW)` the figure holds; if not, the sub-ms
conclusion probably survives but the number needs re-deriving.

Still open on the fix itself: which prominence measure (peak-to-median,
peak-to-second-peak, peak-to-RMS), and earliest-prominent-peak versus global
maximum. Every existing headless test feeds one unambiguous peak, so a fix
needs new fixtures — direct peak plus reflections, and no correlated content at
all (Run 5's pair 1 locked confidently to 494 ms on an uncorrelated input).

Falsify any fix against Run 1's data shape: it must turn the 22.8 / 30.3 /
30.4 ms locks into either a correct lock or a refusal.

### #228 — becomes load-bearing, and gains the full state set

> **Built as PR #234, 2026-08-03.** Two things below changed in the building.
> The `LOST LOCK` row's discriminator is **superseded**: it reads #227's
> `delay_locked` rather than "HF collapsed, LF fine", so the 0.715/0.05
> figures are no longer a threshold (they remain the evidence that motivated
> the issue). And the drive gate turned out to cover the two level rows only —
> a refusal on a `drivable: false` session is still a fault, and less
> recoverable than one on a driving session. Everything else in this section
> was implemented as ratified. Full record: `state-live-spectrum.md`, "The
> fault indicator (#228), as built".

It now drives the indicator rather than being advisory, so its thresholds
matter more. It **no longer needs a PPO input**, because PPO is fixed
(decision 3).

**It must distinguish a dead reference from every other fault.** The original
table collapsed three situations into "not `LOST LOCK`": everything fine, drive
deliberately off, and reference leg dead. The third is #225's symptom exactly,
and the operator had no indication of it for a whole session. The information
is already required — the re-lock gate needs signal presence on both legs, and
the daemon knows its own drive state — so surfacing it costs only the label.

| condition | state | cause |
|---|---|---|
| not driving | *(nothing)* | idle, expected |
| driving, reference leg at floor | **`NO REFERENCE`** | #225, misrouted or unpatched |
| driving, measurement leg at floor | **`NO SIGNAL`** | mic unplugged, DUT off, wrong input |
| both legs live, coherence low everywhere | **`CHECK ROUTING`** | legs carry different sources |
| both legs live, HF collapsed, LF fine | **`LOST LOCK`** | delay fault |
| after a successful re-lock | **`LOCK ACQUIRED`** | transient confirmation |

Each row maps to a distinct cause and a distinct action — "something is wrong"
would leave the operator guessing between four of them. `CHECK ROUTING` is the
row previously written as "warn, do not re-lock"; it is named because it is
genuinely different from a delay fault and has a different fix.

Only the `LOST LOCK` row triggers a re-lock. The rest are indications.

**Thresholds.** "At the floor" should be generous and absolute — around
−80 dBFS is far below any usable measurement, so it will not fire on a quiet
but valid session. A *relative* test against the other leg would misfire
whenever levels legitimately differ, which they did by 15 dB on this rig
(mic at −30 dBFS peak against a reference at −14.5 dBFS).

Coherence threshold caveat unchanged: stage 0 at 0.755 is
reverberation-limited — flat to 0.006 across 20 dB of input gain (Run 7) — so
a healthy acoustic measurement in a live room legitimately sits well below 1.0
and must not be flagged. A threshold set from an electrical loopback would
flag it.

### #225 — unchanged

Reference output leg resolves from `reference_channel`, an input index. Still
a blocker, still independent.

---

## New issues to file — **done 2026-07-28**

> **A** → **#229** (fractional-octave smoothing).
> **B** → **#230** (correct the `((W−D)/W)²` model). One correction: the model
> is **not** in `design-mtw-alignment.md` — zero occurrences, checked. It is in
> `handoff-mtw-live-spectrum.md:239` and `qa-brief-218-222.md:51` only.
> **C** → written up as *design decision 3* in `design-mtw-ladder.md`, not an
> issue.
> Scope changes for **#226**, **#227**, **#228** posted as issue comments.
>
> **Decision 5 is now measured, not derived.** Stage-0 path simulated exactly
> (sr 96000, nperseg 4096, 8 blocks, bins summed per column before coherence),
> 1/48 octave at 20 kHz, BW 288.8 Hz: γ² tracks `sinc²(τ·BW)` within ±6% out to
> τ·BW ≈ 0.45, γ² = 0.9 crossing at **625 µs = 12.5 cycles at 20 kHz**. The
> derived 616 µs holds to 1.5%, so #227's sub-millisecond acceptance stands as
> written.

**A — fractional-octave smoothing control.** Implements the already-declared
`smoothing_bpo`. Designators 1/1, 1/3, 1/6, 1/12, 1/24 and off; magnitude in
dB; phase unwrapped first; coherence unsmoothed or labelled. Records the
base-2 versus `G_OCTAVE` decision explicitly rather than inheriting it.

**B — correct the `((W−D)/W)²` model where it is documented.** It appears in
`design-mtw-alignment.md`, `handoff-mtw-live-spectrum.md`, and QA's criterion
6, which tells a reviewer the top stage should "collapse toward" that ceiling
under mutation. It collapses much further — the mutation test still passes but
against a wrong expected value, and anyone reading it would badly
under-estimate high-frequency delay sensitivity. The model is correct for the
window-overlap term at low frequency; keep that form, add the dispersion term.

**C — record that PPO is fixed, and why.** A decision note in
`design-mtw-ladder.md` rather than an issue, if that fits better. The point is
that the next person to propose exposing PPO finds the reasoning rather than
re-deriving it.

---

## Carried forward, unchanged

- **#224** — per-band Δf and settling labels. Still open, still wants to land
  before the ladder is used to tune a real system.
- **#221** — snapshot parity. Real now that the live view runs the ladder.
- **#219 Part B** — injection seam, mixed-stream requirement recorded.
- **Set the interface clock to Internal** before the next rig session.
- **`conn_tags` absent** from this daemon's frames. The follow-up that stood
  here — "confirm the field reads as *unknown* rather than healthy" — is not
  actionable against main as of 2026-08-03: `conn_tags` has zero occurrences
  anywhere in `ac-rs/`, and the reader that maps absent to *unknown* ships
  only on #214 (issue #205), still open. The six-state indicator (#228, merged
  as `ab3d236`) does not consult it. Reinstate this check when #214 lands.
- **Stage 0's 0.755 is reverberation-limited.** Record durably; it is not a
  defect and gain cannot improve it.
- **#208's positive control was never obtained.** The A/B used a 6 s level
  step, longer than the analysis window, so its edge produces a monotone ramp
  on both builds — the stimulus could not excite the symptom. #208 is closed on
  other evidence; the gap is recorded rather than assumed discharged.
- **Issue F closed.** The lower-crossover drift did not survive a repeat, the
  sign flips between sessions, and the metric is three columns wide at 258 Hz.
  Anyone revisiting needs a wider-band metric first.
