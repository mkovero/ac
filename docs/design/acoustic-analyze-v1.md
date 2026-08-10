# Handoff: `ac analyze` V1 — decimated-curve AI report (acoustic focus)

**Suggested path:** `.agents/handoffs/acoustic-analyze-v1.md`
**Tier:** 2 (live analysis). Never enters a conformance artifact.
**Routing:** architect (advisory, complete) → developer → QA. UX gate **not** required (no UI surface). QA sign-off **is** required: this PR displays measurement values.
**Ratification:** `.agents/*` additions require Markus's explicit approval in PR review.

---

## 1. Intent

Produce a written, plain-language report on a room/loudspeaker in-situ measurement by sending a **decimated numeric curve** — not an image, not raw bins — to an LLM, and having it narrate features it can see in that curve.

This is a **cheap experiment**, not a committed subsystem. The purpose of V1 is to answer one question: *is an LLM narrative of a room curve useful enough to justify building the full feature extractor?* Ship it, use it on a dozen real measurements, then decide.

**Subject-matter focus is acoustic:** room modes, speaker response, placement and boundary effects. Bench/electronics analysis (pad mismatch, clipping, ground loops) is deliberately deferred — it needs different heuristics and a different vocabulary.

---

## 2. Core constraint (non-negotiable)

> **The model never originates a number.**

Every frequency, level, slope, and delay in the report must be present in, or directly derivable from, the payload sent to it. The payload is computed in `ac-core` off the same derived path that feeds `ac-scene`. If a figure in a report cannot be traced to a payload line, that is a defect, not a wording preference.

Rationale: an LLM handed a curve will produce fluent prose containing invented figures — a corner frequency that isn't there, a mode 4 dB deeper than reality — with no signal that it has done so. Grounding is the entire design.

---

## 3. Scope

### In

1. **Decimator** in `ac-core` — fractional-octave complex aggregation of the derived transfer function.
2. **Text emitter** — deterministic, human-readable payload block.
3. **CLI**: `ac analyze <snapshot.acsnap>` with `--dump` (payload only, no network).
4. **`ds/analyze.py`** — prompt assembly, API call, markdown to stdout.
5. **Tests** — synthetic curves with hand-derivable expected values.

### Out (V1)

- All UI. No key binding, no panel, no `ac-view` or `ac-scene` changes.
- Streaming/continuous analysis. Operates on `.acsnap` only.
- `--compare A.acsnap B.acsnap`. This is the highest-value follow-up and the intended V2 — do not build it now, do not architect against it.
- Deterministic rule-based checks (clipping, polarity, hum comb). Deferred to the feature-extractor milestone.
- Target-curve / house-curve comparison.
- Reflection localisation from comb spacing. See §7.
- Any wire-protocol change. Any `StandardsCitation` change.

### Explicit ambiguity for Markus to resolve before work starts

"Live measurement" here is read as **snapshot-of-a-live-acoustic-session**, analysed offline. Streaming analysis is excluded on grounds of determinism, cost per call, and no obvious benefit at bench speed. If the intent was genuinely live/continuous, stop and re-scope — it is a different feature.

---

## 4. Technical specification

### 4.1 Band definition

- Fractional-octave, default **1/6**, exposed as `--smoothing {3,6,12}` (denominator).
- Octave ratio **must** be base-10: `G = 10^(3/10)`. Do not introduce a local `2.0`.
- **Interlock:** this depends on the pending `G_OCTAVE` constant from the IEC 61260-1 filterbank fix. If that constant does not yet exist, introduce it in this PR in its final form and let the filterbank fix consume it. Two competing octave-ratio constants in the tree is a fail condition.
- **This is a display-side smoother, not a conformant filterbank.** It must not be described, named, or documented as IEC 61260-1 anything. No citation is attached to it.
- Range clamped to available bins; nominal 20 Hz – 20 kHz. 1/6 octave over that span yields ~60 lines.

### 4.2 Bulk delay removal (do this first — see §7 gotcha)

1. Estimate bulk delay from the phase slope of the transfer function over a mid-band fit region.
2. Rotate it out of the complex spectrum **before** any aggregation.
3. Report the removed delay as its own scalar, in µs **and** as equivalent path length at 343 m/s. In an acoustic measurement this is time-of-flight, so the path length is directly meaningful to the operator and is a sanity check on mic distance.

### 4.3 Per-band aggregation

For each band, from the delay-rotated complex transfer values:

| Output | Definition |
|---|---|
| `mag_db` | power (RMS) average of magnitude, in dB. Energy-preserving; standard for room curves. |
| `phase_deg` | argument of the magnitude-weighted complex vector mean |
| `vector_ratio` | \|complex mean\| ÷ power-mean magnitude, range 0–1 |

`vector_ratio` is the useful one for acoustics: it drops in bands where phase varies rapidly across the band — i.e. reflection- and comb-dominated regions. It gives the model an honest basis for saying "this region is reflection-dominated" instead of inventing a reason for a ragged curve.

**Name it `vector_ratio`, not coherence.** It is not statistical coherence and must not be confused with it in code, output, or prompt.

### 4.4 Payload format

Deterministic plain text. Header block then table. Sketch:

```
# ac analyze payload v1
source:        living-room-L.acsnap
captured:      2026-07-29T14:02:11Z
tag:           "L speaker, mic at LP, 1.2 m"
smoothing:     1/6 octave
spl_ref:       74.2 dB SPL (calibrated)
delay_removed: 3480 us (1.19 m at 343 m/s)

freq_hz  mag_db  phase_deg  vector_ratio
20.0     -4.1    -31        0.98
22.4     -1.9    -24        0.97
...
```

Header fields must state calibration status explicitly. `spl_ref: uncalibrated` when there is no SPL calibration — the model must not be left to assume absolute level.

Carry the snapshot `--tag` into the header. Free at capture time, and it measurably improves report quality: "L speaker, mic at LP, 1.2 m" lets the model reason about geometry it would otherwise guess at.

### 4.5 `ds/analyze.py`

- Rust gains **no** HTTP and **no** LLM dependency. `ac-core`/`ac-cli` produce text; Python owns the call.
- API key from env or `~/.config/ac/`. Never from the repo, never from a snapshot.
- Cache the report next to the snapshot, keyed on a hash of the payload **and** the prompt version. Re-running is then free, and two prompt revisions can be diffed against byte-identical input — which is the whole workflow during prompt iteration.
- Prompt is a versioned file, not a string literal.

### 4.6 Degraded operation

No network, no key, or API failure → print the payload, state plainly that no report was generated, exit non-zero **from `analyze` only**. Analysis failure must never affect capture, and the rig must remain useful without egress.

---

## 5. Acceptance criteria (falsifiable)

All expected values hand-derived and independently re-derived by QA — not re-read from code comments. Every regression test mutation-verified at birth: demonstrate it fails against the specific broken implementation it guards.

1. **Flat.** Synthetic unity transfer → all `mag_db` within ±0.05 dB of 0, all `vector_ratio` > 0.99.
2. **Single pole.** Known LF corner → recovered corner within half a band at 1/6 octave.
3. **Notch.** Known f and Q → notch present in the correct band, depth matching the **hand-derived smoothed** value. Smoothing reduces apparent depth; asserting the unsmoothed depth is a wrong test.
4. **Delay mutation.** Inject 500 µs into a flat synthetic. `mag_db` must remain flat within ±0.05 dB, and `delay_removed` must report 500 µs ±2%. *This test fails loudly if delay removal is ever reordered after aggregation — the regression this feature is most likely to ship silently.*
5. **Vector ratio.** Synthetic direct + single delayed reflection → `vector_ratio` depressed in the comb region relative to a reflection-free control, by a hand-derived margin.
6. **Determinism.** Same snapshot, same flags → byte-identical payload across runs and machines.
7. **Offline.** With network disabled, `--dump` succeeds and plain `analyze` degrades per §4.6.
8. **No wire change.** Full existing suite green: `cargo test --workspace`, `pytest tests/ -q`. No daemon, ZMQ, or frame-format diff in the PR.

### Stretch (optional, drop if it costs time)

9. **Numeric grounding check.** `--strict-numbers` extracts numeric tokens from the generated report and warns on any that do not appear in the payload within tolerance. Cheap, imperfect, and the only automatable guard on §2.

---

## 6. Sequencing

1. Decimator + emitter. **Stop and read the payload yourself.** If a room curve does not read sensibly to you as text, the model has no chance and the experiment ends here for free.
2. CLI wiring. `parse.rs` is a known god-object — add the subcommand, do not refactor it in this PR.
3. `ds/analyze.py` with a hardcoded prompt.
4. Run on three snapshots whose answers you already know: a decent room curve, one with an obvious LF mode, one with a bad mic position.
5. **Timebox prompt iteration to two hours.** Unbounded otherwise. If the reports are not better than a glance at the plot after two hours, the conclusion is that the feature extractor was the right answer, and the cost of learning that was one day.

---

## 7. Known gotcha, and a limitation the prompt must state

**Gotcha — aggregation order.** Aggregate the complex spectrum per band, with bulk delay removed first. A 1/6-octave band at 20 kHz is ~2.3 kHz wide; with a few ms of acoustic time-of-flight the phase rotates through many turns inside a single band, the vector mean collapses, and — if magnitude is taken from that mean — you get a fabricated HF rolloff. Power-averaging magnitude (§4.3) makes this less catastrophic, but `phase_deg` and `vector_ratio` are still meaningless without prior delay removal. Criterion 4 exists to catch exactly this.

**Limitation — no gating.** The snapshot carries steady-state spectra, not an impulse response. The analysis therefore cannot separate direct sound from room reflections, and cannot resolve reflection arrival times. The prompt must forbid claims that depend on gating: no "that dip is a floor bounce at 2.1 m", no reflection-distance estimates from comb spacing. `vector_ratio` supports *"this region appears reflection-dominated"* and nothing stronger. A model confidently localising a boundary reflection it cannot see is the exact failure this design exists to prevent.

---

## 8. Definition of done

- `ac analyze room.acsnap` prints a grounded markdown report.
- `ac analyze --dump room.acsnap` prints the payload, offline, deterministically.
- All criteria in §5 green, mutation-verified, QA-re-derived.
- No wire-protocol, UI, or `StandardsCitation` diff.
- Prompt file versioned in-repo; model id and prompt version recorded in the cached report header.
- One paragraph in the PR from Markus: keep, extend to the feature extractor, or delete.
