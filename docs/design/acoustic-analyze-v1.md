<!-- agent: architect -->

# design-acoustic-analyze — grounded annotation of a Tier 1 measurement

**Path:** `docs/design/acoustic-analyze-v1.md` (name unchanged deliberately —
it is cited elsewhere; see the note in §11).
**Status: revision 2, 2026-08-11. PARKED, not ratified.** Nothing implements
it. It is parked behind `ac plot` and `ac sweep` getting focus first, for the
reason in §10, which is a data problem rather than a design one.
**Tier: none.** This is not a measurement. It annotates Tier 1 output and never
enters a conformance artifact.
**Routing when it wakes:** architect (advisory, complete) → developer → QA. QA
sign-off required: it renders measurement values. UX gate required in revision
2 and *not* in revision 1 — the annotation now has a visual surface in the
report.
**Ratification:** `.agents/*` additions require Markus's explicit approval in PR
review.

---

## 0. What changed in revision 2, and why

Revision 1 was a Tier 2 feature: send a decimated curve derived from a
`.acsnap` steady-state capture to an LLM, get a markdown report back. Its §7
then forbade the model from saying anything that depends on gating — no
reflection distances from comb spacing, no "that dip is a floor bounce."

**That prohibition was a symptom, not a design.** It existed because the
feature had been pointed at the wrong artefact. A steady-state snapshot cannot
resolve arrival times, so the prohibition was doing the work that choosing the
right input should have done.

Two changes follow, and everything else in this document is consequence:

1. **The native input is a Tier 1 `MeasurementReport`**, not a snapshot.
   `ac sweep` is Farina IR-based, so reflection arrival times are *measured*.
   The snapshot case survives as the degraded path, not the native one.
2. **The output is anchored annotation, not prose.** Each statement names the
   band range or report field it describes. An unanchored statement is rejected
   mechanically, without reading it.

The name for the shape is the operator's: **subtitles for the report.** The
report is the artefact of record. The annotation is an accessibility layer over
it, regenerable and disposable, and its correctness is a property of what it
points at rather than of what it says.

Recorded so nobody re-derives it: the old §7 was diagnosed as conflating three
distinct moves, only one of which deserved a prohibition. §7 of this revision
carries the analysis.

---

## 1. Intent

Produce plain-language annotation of a reference measurement, anchored to the
numbers it describes, so that an operator reading a report is told what the
figures mean rather than left to recognise it.

This remains a **cheap experiment**, not a committed subsystem. It answers one
question: *is grounded annotation of a measurement useful enough to justify
building a deterministic feature extractor?* Use it on a dozen real
measurements, then decide.

**Subject-matter focus is acoustic:** room modes, speaker response, placement
and boundary effects. Bench/electronics analysis (pad mismatch, clipping,
ground loops) is deferred — different heuristics, different vocabulary.

---

## 2. Core constraints (non-negotiable)

Three, and the third is new in revision 2.

### 2.1 The model never originates a number

Every frequency, level, slope and delay must be present in, or arithmetically
derivable from, the payload. If a figure in an annotation cannot be traced to a
payload line, that is a defect, not a wording preference.

**Revision 2 sharpens the failure this guards against.** "Directly derivable"
is a wider door than it looks. Given room geometry, an enormous class of
fabrications becomes derivable — the model can compute a floor-bounce frequency
from heights and match it to any dip within a band or two, producing a figure
that is arithmetically correct and completely unmeasured. That is worse than a
hallucination, because it is self-consistent and survives a numeric-token
check.

The guard is §4's anchoring, not the wording of this clause.

### 2.2 It never enters a conformance artifact

`MeasurementReport` is schema-versioned, self-describing and archival, and
Tier 1's property is determinism given the same input and calibration state. An
LLM annotation is non-deterministic, model-versioned, prompt-versioned and
network-dependent.

So the annotation is a **sidecar**, keyed on a hash of the report plus the model
id plus the prompt version. It is rendered *beside* the report, never merged
into it.

Specifically, and because it will be proposed: **not in `MeasurementReport.notes`.**
That field is inside the versioned artefact.

### 2.3 Deleting every annotation ever generated must lose nothing

The falsifiable form of 2.2, and the acceptance criterion that distinguishes a
sidekick from a second source of truth. If any fact exists only in an
annotation, the boundary has failed and the feature is no longer optional.

---

## 3. Scope

### In

1. **Decimator** in `ac-core` — fractional-octave complex aggregation of a
   derived transfer function.
2. **Payload emitter** — deterministic, human-readable, a *view over a
   `MeasurementReport`*.
3. **Anchored-annotation schema and validator** — §4.
4. **CLI**: `ac analyze <report.json>` with `--dump` (payload only, no
   network). `.acsnap` accepted as the degraded input per §6.5.
5. **`ds/analyze.py`** — prompt assembly, API call, sidecar write.
6. **Tests** — synthetic curves with hand-derivable expected values.

### Out

- Streaming or continuous analysis. Operates on completed artefacts.
- `--compare A B`. The highest-value follow-up and the intended next version —
  do not build it now, do not architect against it.
- Deterministic rule-based checks (clipping, polarity, hum comb). These belong
  to the feature-extractor milestone this experiment exists to justify or kill.
- Target-curve / house-curve comparison.
- **Room description as an input.** See §8 — it is the single most tempting
  addition and the one that breaks §2.1 hardest.
- Any wire-protocol change. Any `StandardsCitation` change.

### Resolved from revision 1

Revision 1 flagged "live measurement" as an ambiguity for Markus. Revision 2
resolves it by construction: the input is a completed reference measurement.
Streaming stays excluded on determinism, cost per call, and no benefit at bench
speed.

---

## 4. The anchored annotation

An annotation is a list of `(anchor, status, text)` records. Not markdown prose.

| field | content |
|---|---|
| `anchor` | a band range (`250–600 Hz`), a named report field (`calibration.spl_ref`), or a named IR feature (`arrival[1]`) |
| `status` | `observation` or `hypothesis` |
| `text` | one or two sentences |
| `falsifier` | required when `status: hypothesis` — the measurement that would settle it |

Four things this buys that revision 1's freeform report did not:

**The ask to the model changes.** Not "write a report on this curve" but
"annotate these rows." A constrained annotation task is markedly less prone to
fluent invention than an essay, and when it fails it fails visibly rather than
plausibly.

**The grounding check gets teeth.** Revision 1's `--strict-numbers` was a
stretch goal precisely because scanning prose for numeric tokens is weak. An
unanchored record is rejected without reading it — no text analysis, no
tolerance window. `--strict-numbers` is therefore promoted out of stretch in
§9.

**Epistemic status becomes structural.** The real failure mode of generated
prose is not falsehood; it is that an observation and a hypothesis are
formatted identically. *"The dip is 6 dB deep"* and *"the dip is a floor
bounce"* read as the same kind of sentence. A `status` field is not a wording
convention that can erode.

**It renders.** Annotations sit beside the curve in the HTML report, visually
and structurally demarcated, absent from the archival JSON and absent from any
PDF carrying a standards claim.

---

## 5. Payload

Deterministic plain text, generated from the report. Header block then table.

```
# ac analyze payload v2
source:         sweep-2026-08-14-L.json   (MeasurementReport schema v3)
method:         swept_sine 20–20000 Hz, 1.0 s
captured:       2026-08-14T14:02:11Z
notes:          "L speaker, mic at LP, 1.2 m"
smoothing:      1/6 octave (display-side; not IEC 61260-1)
calibration:    spl_ref 74.2 dB SPL (calibrated), mic_correction applied
processing:     weighting off, time_integration off
delay_removed:  3480 us (1.19 m at 345.8 m/s)
comb_visible:   delays 0.9–14 ms readable below 626 Hz at this smoothing
gate:           4.62 ms (216 Hz) | corrected: no
```

Header fields state calibration status explicitly. `spl_ref: uncalibrated` when
there is none — the model must not be left to assume absolute level.

`ProcessingChain` and `CalibrationSnapshot` are structured fields on the report,
so the header is a projection rather than a hand-assembled string. That is one
of the reasons revision 2 is cheaper than revision 1 despite being larger.

**`delay_removed`'s path length is currently wrong by the instrument constant.**
Until the reference cable change lands, it carries roughly 0.41 m of the
instrument's own latency, which is why a taped 1.000 m reads 1.40 m. Do not
ship the "stated distance versus measured time-of-flight" annotation before
that collapses, or the model will be handed a genuine discrepancy and will
invent an explanation for it. Track it as a precondition, not a caveat.

---

## 6. Technical specification

### 6.1 Band definition

- Fractional-octave, default **1/6**, exposed as `--smoothing {3,6,12}`.
- Octave ratio is base-10, `G = 10^(3/10)`. Consume
  `ac_core::shared::constants::G_OCTAVE`. **Revision 1's interlock is
  discharged** — that constant now exists in the tree at its final value and
  the filterbank consumes it. Do not introduce a local `2.0`.
- **Display-side smoother, not a conformant filterbank.** It must not be
  described, named or documented as IEC 61260-1 anything. No citation attaches
  to it.
- Range clamped to available bins; nominal 20 Hz – 20 kHz. 1/6 octave over that
  span yields ~60 rows.

### 6.2 Bulk delay removal (first — see §7.4)

1. Estimate bulk delay from the phase slope over a mid-band fit region.
2. Rotate it out of the complex spectrum **before** any aggregation.
3. Report it as a scalar in µs and as equivalent path length, using the
   report's own speed of sound where one is recorded rather than a hardcoded
   343.

**Retired for the IR path, not implemented here.** #284 lands a
`gated_frequency_response` payload derived from the linear IR, which
carries a measured arrival directly (the IR's peak sample, corrected by
`interface_latency` when resolved) rather than a value estimated from a
phase-slope fit. When the native input is that payload, step 1's estimator
has a measured value to defer to and is retired for that path — noted so
the next reader does not re-derive the estimator where a direct
measurement already exists. This document does not implement that
substitution; §10 still parks the whole feature on preconditions the IR
path has not yet met.

### 6.3 Per-band aggregation

From the delay-rotated complex values:

| output | definition |
|---|---|
| `mag_db` | power (RMS) average of magnitude, in dB. Energy-preserving; standard for room curves. |
| `phase_deg` | argument of the magnitude-weighted complex vector mean |
| `vector_ratio` | \|complex mean\| ÷ power-mean magnitude, range 0–1 |

`vector_ratio` drops in bands where phase varies rapidly across the band — that
is, reflection- and comb-dominated regions. It gives the model an honest basis
for saying a region is reflection-dominated instead of inventing a reason for a
ragged curve.

**Name it `vector_ratio`, never coherence.** It is not statistical coherence and
must not be confused with it in code, output or prompt.

### 6.4 Anchor resolution

The validator resolves each anchor against the payload before the annotation is
written. Unresolvable anchor → the record is dropped and the drop is reported.
A run in which records were dropped exits non-zero.

### 6.5 Degraded input: `.acsnap`

A snapshot carries steady-state spectra and no impulse response. Accepted, with
the payload header stating `method: snapshot (no IR)` and the §7 rules
tightening accordingly — no arrival-time observations exist, so all
reflection-related records are `hypothesis` at best.

This is the degraded path deliberately, not the native one. It exists so the
feature has something to run against before the Tier 1 IR path has captures
(§10), and so the design does not have to change when it does.

### 6.6 `ds/analyze.py`

- Rust gains **no** HTTP and **no** LLM dependency. `ac-core`/`ac-cli` produce
  text and validate records; Python owns the call.
- API key from env or `~/.config/ac/`. Never from the repo, never from an
  artefact.
- Sidecar cached next to the report, keyed on payload hash **and** prompt
  version **and** model id. Re-running is free, and two prompt revisions diff
  against byte-identical input — the whole workflow during prompt iteration.
- Prompt is a versioned file, not a string literal.

### 6.7 Degraded operation

No network, no key, or API failure → print the payload, state plainly that no
annotation was generated, exit non-zero **from `analyze` only**. Analysis
failure must never affect capture, and the rig must remain useful without
egress.

---

## 7. What the annotation may claim

This section replaces revision 1's §7 prohibition. The prohibition banned a
class of statement; this bans a class of *unsupported* statement and requires
the rest to carry their status.

### 7.1 The three moves

Revision 1 treated reflection-talk as one thing. It is three, and they have
different warrant.

| move | example | warrant |
|---|---|---|
| **characterisation** | "this region is reflection-dominated" | `vector_ratio`. Permitted as `observation`. |
| **localisation in time** | "a reflection about 5 ms out" | Measured directly from an IR → `observation`. Read from comb spacing → `hypothesis`, subject to §7.2. |
| **attribution to a surface** | "that's your floor bounce" | **Prohibited.** Not permitted even as hypothesis. |

Attribution stays prohibited on grounds that it is unanswerable from the data
and fails by producing a confident wrong answer rather than a hedged one — the
same class as a parametric correction misfitting into a plausible curve. A
measured 4.62 ms says 1.6 m of excess path and says nothing about which
boundary produced it.

### 7.2 Reading periodicity: when it is warranted

Comb spacing is `1/τ`. A 1/6-octave band is 11.5% wide. Requiring three bands
per ripple period before a ripple counts as visible:

| reflection delay | comb spacing | visible up to, 1/6 oct | 1/12 oct |
|---|---|---|---|
| 2.45 ms | 408 Hz | 1181 Hz | 2363 Hz |
| 4.62 ms | 217 Hz | 626 Hz | 1253 Hz |
| 10 ms | 100 Hz | 289 Hz | 579 Hz |
| 23 ms | 43 Hz | 126 Hz | 252 Hz |

So the first reflection in a normal measurement is readable across most of the
band an operator cares about. **Revision 1's blanket ban was discarding real,
available information** — that is the substantive reason this section changed,
not a preference for richer prose.

The guard is not resolvability, which the model can see for itself, but
fabrication from too little: **a periodicity claim requires at least three
complete visible periods.** That is a statable criterion and it is what
`comb_visible` in the header exists to make checkable.

### 7.3 Hypotheses carry their falsifier

Any `hypothesis` record names the measurement that would settle it. This is not
a hedging convention; it is the project's standing rule applied to prose. The
recurring failure across this project has been a plausible mechanism asserted
without the measurement that would distinguish it. The rule does not forbid the
mechanism — it requires the discriminator.

Worked example of the target register:

> **250–600 Hz** · *hypothesis* — Periodic ripple of about 200 Hz spacing,
> four periods visible. A single reflection near 5 ms would produce this, which
> is 1.7 m of excess path. *Falsifier: a gated IR measurement resolves the
> arrival directly.*

That is a natural explanation, grounded, status-legible, self-testing. It is
also strictly more useful than "this region appears reflection-dominated,"
which is all revision 1 permitted and which tells an operator nothing they
could not see.

### 7.4 Gotcha — aggregation order

Aggregate the complex spectrum per band with bulk delay removed **first**. A
1/6-octave band at 20 kHz is ~2.3 kHz wide; with a few ms of acoustic
time-of-flight the phase rotates through many turns inside one band, the vector
mean collapses, and — if magnitude comes from that mean — you get a fabricated
HF rolloff. Power-averaging magnitude makes this less catastrophic, but
`phase_deg` and `vector_ratio` are meaningless without prior delay removal.
Criterion 4 exists to catch exactly this.

---

## 8. Relationship to parametric reflection removal

`docs/design/design-parametric-reflection-removal.md` is the other unratified
proposal in this area. Under revision 2 the two stop being neighbours and
become stages of one pipeline: **measure → optionally correct → report →
annotate.**

Three consequences worth writing down before either is built.

**The annotation is where the correction discloses itself.** That proposal
requires a corrected curve to be distinguishable from a gated one and requires
the fit residual to be a published output. A subtitle is the natural place that
becomes plain language — *"corrected using the described room; fit residual
0.7 dB, within criterion"* — rather than a field nobody reads. The payload
header's `corrected:` flag exists for this.

**Coupling them creates a circularity, and the residual is what breaks it.**
If the fit corrects a curve using an operator's room description and the
annotation then narrates the corrected curve using the same description, the
model is confirming its own input. A misdescribed room yields a plausible
corrected curve — that is the exact failure mode that proposal's §6 exists to
catch — and then a plausible narrative explaining it. Two layers of
confident-wrong, each making the other look verified. **So if they are ever
coupled: the residual is a payload field, and a residual above the misfit
criterion invalidates every geometry-derived record in the annotation.**

**Room description stays out of scope here, and the ordering is the reason.**
A room description only becomes trustworthy when something measures against it.
Shipped into an annotation first, it produces a model narrating a room model
nobody has checked — worse than no description. The order that works is:
description becomes a first-class object with an image-source predictor →
predicted arrivals are checked against tape and against an IR → only then does
it earn influence over prose. Description earns trust from measurement before
it earns influence.

---

## 9. Acceptance criteria (falsifiable)

All expected values hand-derived and independently re-derived by QA — not
re-read from code comments. Every regression test mutation-verified at birth:
demonstrate it fails against the specific broken implementation it guards.

1. **Flat.** Synthetic unity transfer → all `mag_db` within ±0.05 dB of 0, all
   `vector_ratio` > 0.99.
2. **Single pole.** Known LF corner → recovered within half a band at 1/6
   octave.
3. **Notch.** Known f and Q → present in the correct band, depth matching the
   **hand-derived smoothed** value. Smoothing reduces apparent depth; asserting
   the unsmoothed depth is a wrong test.
4. **Delay mutation.** Inject 500 µs into a flat synthetic. `mag_db` stays flat
   within ±0.05 dB and `delay_removed` reports 500 µs ±2%. *Fails loudly if
   delay removal is ever reordered after aggregation — the regression this
   feature is most likely to ship silently.*
5. **Vector ratio.** Synthetic direct + single delayed reflection →
   `vector_ratio` depressed in the comb region relative to a reflection-free
   control, by a hand-derived margin.
6. **Determinism.** Same report, same flags → byte-identical payload across
   runs and machines.
7. **Offline.** With network disabled, `--dump` succeeds and plain `analyze`
   degrades per §6.7.
8. **No wire change.** Full existing suite green. No daemon, ZMQ, or
   frame-format diff in the PR.

New in revision 2:

9. **Anchor validation.** An annotation record whose anchor does not resolve
   against the payload is dropped, the drop is reported, and the run exits
   non-zero. Mutation: a record anchored to a band range outside the payload's
   frequency span.
10. **Numeric grounding.** `--strict-numbers` extracts numeric tokens from each
    record and warns on any not present in that record's anchored rows within
    tolerance. Promoted out of stretch: anchoring narrows the search from the
    whole payload to a few rows, which is what makes it worth running.
11. **Status discipline.** Every `hypothesis` record carries a non-empty
    `falsifier`. A record with `status: observation` whose figures are not
    present in its anchored rows is a failure, not a warning.
12. **Attribution prohibition.** A held-out prompt-response fixture containing
    a surface attribution is rejected. *This is the one criterion here that
    tests the prompt rather than the code, and it is the one most likely to be
    dropped for awkwardness. It should not be.*
13. **The deletion test (§2.3).** Removing every sidecar leaves the report set
    complete. Mechanically: no report field, HTML section, or test fixture
    references sidecar content.

---

## 10. Why this is parked, and what would unpark it

**The native input has no captures.** Nothing acoustic from `sweep_ir` is
committed anywhere in the tree; the rig archive holds per-frame delay scalars,
not impulse responses. Revision 1's virtue was that it ran on artefacts that
existed. Revision 2 is pointed at a path that is better in every way except
that it currently has nothing to narrate.

Related and worth fixing while passing: `sweep_ir`'s own header claims only the
fake backend implements `play_and_capture`. Both the JACK and CPAL backends
implement it; only the default trait impl bails. The claim is stale and would
mislead anyone scoping this.

So the preconditions, in order:

1. **`ac plot` and `ac sweep` get focus** — this is the operator's call and the
   reason for the park. A narration layer over a measurement path that is still
   moving is premature.
2. **At least one real acoustic sweep IR captured and committed.** This is the
   same capture the parametric proposal needs for its first question, so one
   rig block serves both.
3. **The reference cable change lands**, so `delay_removed`'s path length stops
   carrying the instrument constant (§5).

Sequencing once unparked, unchanged from revision 1 in spirit:

1. Decimator, emitter, anchor validator. **Stop and read the payload yourself.**
   If a room curve does not read sensibly to you as text, the model has no
   chance and the experiment ends there for free.
2. CLI wiring. `parse` is a known god-object — add the subcommand, do not
   refactor it in this PR.
3. `ds/analyze.py` with a hardcoded prompt.
4. Run on three measurements whose answers you already know: a decent room
   curve, one with an obvious LF mode, one with a bad mic position.
5. **Timebox prompt iteration to two hours.** Unbounded otherwise. If the
   annotations are not better than a glance at the plot after two hours, the
   conclusion is that the feature extractor was the right answer, and the cost
   of learning that was one day.

A note on that stopping rule, because revision 1's §7 quietly corrupted it: gag
the model on every statement an acoustician would actually make and a null
result is uninterpretable. You would have shown that a constrained narrative is
not useful, not that a narrative is not. §7 of this revision exists so the
timebox tests the thing it was meant to test.

---

## 11. Open, for Markus

1. **The name.** This is no longer "analyze a snapshot," and the filename says
   `acoustic-analyze-v1`. Left unchanged on purpose: renaming a document that
   other files cite by name is the failure the standing rule about grepping for
   cited names exists to prevent. If it is renamed, grep for the name and not
   only for the subject.
2. **Where annotations render.** Sidecar file only, or a demarcated
   non-normative section in the HTML report with the archival JSON and any
   standards-claiming PDF excluded. Revision 2 assumes the latter is wanted and
   specifies the exclusions, but the safe first shipment is the former.
3. **Criterion 12** tests a prompt, not code. QA has no precedent for that in
   this repo. Either it gets a fixture-based form, or the prohibition is
   enforced by the validator rejecting records containing a surface vocabulary
   list — which is cruder and would also reject legitimate quotation of the
   operator's own notes.
4. **Whether the degraded `.acsnap` path is worth carrying at all**, or whether
   the feature should simply wait for precondition 2 and drop §6.5. Carrying it
   costs a branch in the emitter and a second set of §7 rules.

## Definition of done

- `ac analyze report.json` writes a validated sidecar of anchored records.
- `ac analyze --dump report.json` prints the payload, offline, deterministically.
- All criteria in §9 green, mutation-verified, QA-re-derived.
- No wire-protocol or `StandardsCitation` diff.
- Prompt file versioned in-repo; model id and prompt version in the sidecar
  header.
- One paragraph in the PR from Markus: keep, extend to the feature extractor,
  or delete.
