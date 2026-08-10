<!-- agent: architect -->

# design-parametric-reflection-removal — buying two octaves of quasi-anechoic LF by modelling the floor bounce

**This is a proposal. Nothing in it is ratified**, no issue implements it, and
no part of it has been measured on this rig. It is written down because the
material existed only in conversation, and because the one genuinely binding
constraint in it — §6 — is the kind that is expensive to discover after an
implementation exists.

Read §1 first. The method this borrows from is famous for solving a problem
acoustics does not have, and every reader who recognises the name will assume
that is the pitch.

---

## 1. What this is not: it is not delay estimation

MEDLL and its relatives exist in GNSS because a GPS C/A chip is 1 µs wide.
Multipath inside ~30 m arrives while the direct correlation peak is still
open, so the reflection cannot be separated by looking — it has to be
estimated jointly. Broadband acoustics is the opposite regime:

| | bandwidth | correlation peak width | path equivalent |
|---|---|---|---|
| GPS C/A | 1 MHz | 1.0 µs | 300 m |
| acoustic, 20 Hz – 20 kHz | 20 kHz | 50 µs | **1.7 cm** |

Room reflections sit 15 cm to several metres out — 10 to 200 correlation
widths. They are **already resolved**. Rig session 3 shows exactly that: direct
arrival at lag 780, reflection cluster at 987–1020, cleanly separated, no
overlap to deconvolve (`work/rig/rig-session-3-results.md`).

So picking the direct arrival out of that cluster is a *selection* problem —
which peak is the direct one — and selection is a decision rule, not an
estimator. That work is already underway and is not this: the earliest-peak
rule and its admission floor (#246, #250), the disclosure gap when a second
comparable arrival is discarded (#255), and the near-wall case where the wrong
peak is genuinely more prominent than the right one. **Nothing in this document
improves delay estimation, and a version of it that claims to should be
rejected on that basis.**

## 2. What it is: the low-frequency ceiling on a gated measurement

A quasi-anechoic measurement gates the impulse response before the first
reflection arrives. The gate length sets the lowest trustworthy frequency, and
the first reflection is usually the floor.

Take source and microphone at 1.2 m height, 1 m apart:

- direct path: **1.0 m**
- image-source path via the floor: √(1² + 2.4²) = **2.6 m**
- excess: 1.6 m = **4.62 ms**

The gate must close before that arrival, which puts the usable LF limit at
roughly **216 Hz**. Everything below is the room, not the loudspeaker.

Remove the floor bounce parametrically and the binding constraint becomes the
*next* arrival — ceiling or side wall, typically 15–20 ms — which drops the
limit to **50–67 Hz**.

**Two octaves of valid quasi-anechoic response out of the same capture, with
no change of hardware and no change of room.** The alternatives are a
ground-plane measurement (changes the loading, half-space only), a physically
larger room, or a Klippel NFS.

Byproduct, and not a small one: the fit hands back a per-surface reflection
coefficient — the frequency-dependent absorption of one specific boundary,
from one measurement.

## 3. What this project has that the general method does not

Three of the general method's weak points are already paid for here.

**The delay is predicted, not fitted.** Session 3 validated
`arrival = const + d/c` against a tape measure across eight positions to within
5 cm, with the converter constant measured in-session rather than assumed
(`arrival(d) = 1.1931 ms + d/346 m/s`, and #243 is the change that should
collapse the constant). Mic height, source height and separation give the
image-source path by geometry. That removes the single most ill-conditioned
parameter from the fit before it starts.

**Model order comes from surfaces, not from an information criterion.** Floor,
ceiling, two side walls — enumerated from a room description the operator
already has to give. Order selection is the acknowledged soft spot of the
general method, and geometry dissolves it rather than deferring it to AIC/MDL
on a signal that violates their assumptions anyway.

**It is well-conditioned precisely where it looks hopeless.** This is the first
objection anyone will raise, so it is answered here rather than in a footnote:

> At 50 Hz the floor reflection sits 0.23 of a period behind the direct
> arrival. Frequency-domain separation at that frequency is hopeless — the two
> are not distinguishable as separate contributions in any single-bin sense.

That objection is correct and does not apply, because **the estimation does not
run at 50 Hz.** It runs in the time domain at full bandwidth, where the two
arrivals are 444 samples apart at 96 kHz — the same well-separated peaks §1
described. The correction is then *applied* at low frequency. Estimate where
the problem is easy, correct where the answer is needed. Any critique of this
proposal that argues from LF frequency-domain separability has mistaken which
domain the fit lives in.

## 4. Validation, arranged to fail in the right direction

The proposal is only worth building if it can be shown wrong cheaply.

**Primary: validate at HF, deploy at LF.** Gate conventionally above 500 Hz,
run the parametric fit on the same capture ungated, and compare the two in the
overlap region. Above 500 Hz the loudspeaker is directional, so the direct and
floor paths differ most and the model is under maximum stress. At LF the source
is closer to omnidirectional and the reflection is nearly a scaled copy, which
is the easy case. **Validating in the hard band and deploying in the easy one
is the conservative direction**; the reverse would prove nothing.

**Secondary, and independent of the first:** does the *fitted* delay match the
tape-measured image-source geometry? That check does not depend on the gated
comparison at all, so it does not inherit its assumptions. Given §3's measured
`arrival = const + d/c`, a fit that lands on the wrong path length is
detectable without any reference measurement.

Two independent checks that can each fail on their own is the bar; one check
that can only fail together with its own premise is not.

## 5. Where the literal model fights back

The GNSS formulation models each path as a scaled, delayed replica of the
direct one. For acoustics that is wrong twice:

- **Directivity.** The floor reflection leaves the source off-axis, so the
  loudspeaker's own directivity colours it. The reflected path is not the
  direct path times a constant.
- **Frequency-dependent absorption.** Boundaries absorb differently with
  frequency. Carpet is not a scalar.

A single scalar gain per path will not fit, and forcing one produces a
confident wrong answer rather than a poor one. The practical compromise:
**reflection magnitude per octave or third-octave band, delay as a single
scalar per path.** Delay stays scalar because geometry already predicts it
(§3); only the magnitude needs to breathe.

This is not unexplored territory — it overlaps sparse deconvolution of room
impulse responses and subspace methods such as matrix pencil applied to early
RIRs. What is unusual is not the mathematics but that no measurement tool
ships it.

## 6. The architectural requirement — binding, and the reason this file exists

**The fit residual must be an output, not an internal.**

A gated measurement is honest about its own limit by construction: state the
gate length, and the frequency floor follows arithmetically. Anyone reading the
number knows what it does not cover.

A parametrically corrected measurement asserts a room model. When that model
misfits — wrong surface, wrong height, an absorber that is not where the
description says — **the error appears as a plausible response curve.** There
is no gate to quote and nothing on screen that says the correction was applied
to a room it did not describe.

That is the confident-wrong-display class, and it is the specific failure this
project has spent three rig sessions removing: a −826 ms lock painted `LOCK
ACQUIRED`; a near-wall measurement accepted at prominence 24.15 and 52 cm
wrong, with successive estimates agreeing tightly around the wrong answer; a
composed calibration topology that would move an SPL reading by 27 dB while
re-calibration returns the same number. In every one of those, the instrument
was confident and wrong, and the fix was to make the failure visible rather
than to make the estimate cleverer.

So, as a gate on any implementation:

1. The residual is a **published output**, on the wire and in the report — not
   a debug print, not a log line, not a threshold applied internally.
2. There is a stated criterion for what residual means "the model did not
   describe this room", and the operator-facing text **names what to check**
   (mic height, source height, surface description) rather than asserting a
   cause the instrument cannot know.
3. A corrected curve and an uncorrected one are distinguishable in the output.
   A consumer must never be able to plot a corrected response believing it
   gated.

Point 3 is where this interacts with an existing decision: `ac-scene` is the
display-truth boundary, and a corrected response that reaches `ac-view` without
its provenance is exactly the kind of thing that boundary exists to prevent.

## 7. Where it would live, if it is ever built

Tier 1, `ac-core/src/measurement/`. This is reference-measurement processing on
a captured impulse response — the Farina sweep path — not live analysis, and it
must not take a dependency on `visualize/` (ARCHITECTURE.md's tier split).

- **No wire-schema change is implied by the fit itself**, but §6.1 and §6.3 do
  imply new fields: a residual, and a flag distinguishing corrected from gated.
  Those are additive to `ac-rs/ZMQ.md` and are a breaking-change question for
  every consumer, per the wire-schema invariant.
- **No change to the live delay estimator, the prominence gate, or the
  earliest-peak selection rule.** §1 is the reason; a patch that touches those
  in this proposal's name has misread it.
- Room description (surface enumeration, heights) is new configuration, and
  configuration that silently does nothing is its own defect class — if a
  surface list is accepted it must demonstrably reach the fit.

## 8. What would have to be true before this is worth ratifying

Open questions, in the order that would settle them cheapest-first:

1. **Does the HF overlap comparison in §4 actually agree, on this rig, in this
   room?** Offline on a captured sweep, no new hardware. If it does not, the
   proposal ends there.
2. **Does the fitted delay match tape geometry** (§4, secondary) across the
   positions session 3 already measured?
3. **How much residual is "misfit"?** §6.2 needs a number, and a number nobody
   has scored is not a criterion. Deriving it from the conjunction that causes
   harm — a misfit large enough to move the LF curve — is more defensible than
   a maximum observed on one room.
4. **Does per-octave reflection magnitude (§5) fit better than a scalar, by
   enough to justify the parameter count?** Scoreable on existing captures.
5. Only then: whether two octaves of LF is worth the implementation and the
   permanent explanatory burden of a corrected measurement.

Nothing here needs rig time before question 2.
