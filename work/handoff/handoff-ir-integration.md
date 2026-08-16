# handoff-ir-integration — EPIC: make the non-live IR path a usable, reportable measurement

**Status:** FILED 2026-08-11 as epic #276 with sub-issues #277–#287. The `S1`…`S11`
identifiers below are the drafting placeholders; the mapping is in §Filed issue numbers.
GitHub now holds the authoritative spec text and routing labels — this document is the
provenance record, not the tracker.

**Expiry:** delete when #276 closes, or when any sub-issue's spec has been revised on
GitHub such that this copy no longer matches. Do not edit the specs here to keep them in
sync; edit the issues.

**Scope discipline:** this epic integrates capability that already exists in the tree.
It adds no new measurement science. Room-acoustic parameters (T20/T30/EDT/C50) are
**out of scope** and depend on documents not yet in `stddocs/` — see §Standards below.

---

## Why

`ac-core/src/measurement/sweep.rs` is a complete, cited, eight-test Farina
implementation. Both real backends implement `play_and_capture`. The daemon runs the
measurement end to end and publishes `measurement/impulse_response` +
`measurement/report`.

Then `ac-cli/src/commands/sweep.rs::run_ir` calls `wait_for_stop`, which watches only
for `done` and `error`, and the result is discarded. Nothing is printed. Nothing is
saved. Unlike `plot`, `sweep_ir` never writes to `report_dir`. The IR is gone when the
command returns.

Separately, `handlers/transfer.rs` computes a `visualize/ir` sidecar for every transfer
frame and `ac-view/src/session.rs:145` filters it out, because no consumer exists.

Two working IR producers, zero consumers. That is the whole problem this epic solves.

## What "useful" means here

A user must be able to read the output and know **why the number is worth having**.
For an IR-derived frequency response that reduces to one thing: the gate's validity
floor must be stated next to the curve. A gated response that runs to 20 Hz without
saying that everything below `f_low = 1 / t_gate` is the room rather than the device is
a lie by omission.

The pitch, and the acceptance bar for the epic as a whole: *this part of the curve is
the loudspeaker, that part is the room, and here is the frequency where one becomes the
other.* Neither `ac plot` nor `transfer_stream` can say that today.

---

## Settled decisions (do not relitigate in sub-issues)

1. **Farina moves under `ac plot ir`.** `ac plot` becomes "give me a frequency
   response", with the stimulus method as the noun. This is the shape
   `MeasurementMethod::{SteppedSine, SweptSine}` was already written for.
2. **`ac sweep` becomes a pure generator verb.** `sweep level` and `sweep frequency`
   capture nothing and analyse nothing today; they are `generate` variants wearing a
   measurement verb.
3. **No live loopback reference for the Farina path.** The stimulus is analytic —
   `inverse_sweep()` regenerates it exactly — so the reference is a property of the
   signal chain, not of the run. It belongs in `cal.json`.
4. **τ (interface round-trip latency) is a new, fourth, *parallel* calibration layer,
   captured inside the existing `ac calibrate` flow. No new subcommand.**
   `handlers/calibrate.rs` already opens both directions, plays a tone, captures the
   input, and detects a patched loopback (`is_loopback`, line 110). τ is derived at that
   point when the loopback is detected, and skipped with a stated reason when it is not.
   Parallel per the `.agents/architect.md` invariant: τ is a time quantity and composes
   with nothing. It is keyed on the conditions it was measured under and is refused,
   never interpolated, on mismatch.
5. **Mic correction on the IR path is frequency-domain and happens after gating.**
   `MicCurveFir` is linear-phase with group delay `(n_taps−1)/2` — 255 samples at the
   512-tap default, 5.3 ms at 48 kHz, **1.8 m of path**. Convolving the IR with it
   before gating moves the arrival and displaces the gate. Arrival time is a property
   of the physical path; the mic curve is a property of the transducer's magnitude
   response.
6. **`f_low = 1 / t_gate`.** Consistent with the 216 Hz figure already derived in
   `docs/design/design-parametric-reflection-removal.md` §2 for a 4.62 ms gate.

---

## Filed issue numbers

| placeholder | issue |
|---|---|
| epic | [#276](https://github.com/mkovero/ac/issues/276) |
| S9 | [#277](https://github.com/mkovero/ac/issues/277) |
| S7 | [#278](https://github.com/mkovero/ac/issues/278) |
| S11 | [#279](https://github.com/mkovero/ac/issues/279) |
| S1 | [#280](https://github.com/mkovero/ac/issues/280) |
| S2 | [#281](https://github.com/mkovero/ac/issues/281) |
| S3 | [#282](https://github.com/mkovero/ac/issues/282) |
| S4 | [#283](https://github.com/mkovero/ac/issues/283) |
| S5 | [#284](https://github.com/mkovero/ac/issues/284) |
| S6 | [#285](https://github.com/mkovero/ac/issues/285) |
| S8 | [#286](https://github.com/mkovero/ac/issues/286) |
| S10 | [#287](https://github.com/mkovero/ac/issues/287) |

### What was corrected at filing, and what was only added

**One cite drifted.** The `visualize/ir` sidecar is computed at
`handlers/transfer.rs:1238`, not `:1253`. Corrected in #286. This document's own S8
section carried the stale number until after filing, so a reader who checked the tree
copy rather than the issue would have found the wrong line; S8 now cites the file and
the `"type": "visualize/ir"` `json!` block by name and carries no line number at all.
The drift is recorded here, once, rather than pinned to a number in the body that will
move again — the same reason the calibration invariant is documented by section name in
`calibration.rs` after moving twice.

**Three cites were added, not corrected.** `handlers/calibrate.rs:155–156`,
`measurement/sweep.rs:253` and `it_loopback_ir.rs:205` have no line numbers in the draft
below; they were resolved against the tree while filing and appear only in the issue
text. Reported as "corrections" in the filing summary, which was wrong: nothing about
them changed, they were absent.

**Two cites were mis-resolved after filing and then repaired.** `plot.rs:430` (the
`SteppedSine { standard: Filterbank::citation() }` emitter) and `plot.rs:170`
(`mic_correction_applied` reported honestly) are both in
`ac-daemon/src/handlers/audio/plot.rs`, not `ac-cli/src/commands/plot.rs` — the CLI file
is 212 lines and has neither. The draft's bare `plot.rs:` was right; the first pass at
#280 and #285 named the wrong file in *files likely affected*. Both bodies fixed.

**Everything else holds.** `session.rs:145`, `sweep.rs:306`, `report_html.rs:285`,
`report_pdf.rs:85`, `ZMQ.md:1939`, `it_protocol.rs:1451`, `jack_backend.rs:375`,
`cpal_backend.rs:363` all check out against the tree at filing time.

### Amended after filing

**2026-08-11 — ISO 18233:2006, ISO 3382-1:2009, ISO 3382-2:2008 acquired.** §Standards
above said those documents were not held and that acquiring them was unfiled; both
sentences went false the moment they landed in `stddocs/iso-full/`. The section is
rewritten in place rather than annotated, because the tree copy is what a future reader
trusts. Full analysis, and the routing of every consequence to an issue, is in
`work/handoff/handoff-ir-integration-iso-amendment.md`. Nothing there widens the epic:
no sub-issue was added, no label changed, and #277, #278, #279, #281, #286, #287 and #288
are untouched. Two items narrow it. One new issue was filed for the `sweep.rs::citation()`
addition; the rest are body edits to #276, #280, #282, #283, #284 and #285.

The amendment's own citations, counted the three ways `AGENTS.md` §evidence discipline
now requires:

- **Corrected.** Two. The constants file is `ac-rs/crates/ac-core/src/shared/constants.rs`
  — the amendment draft dropped the `crates/` segment, in a repo where every crate sits
  under it. And the three ISO documents are filed under `stddocs/iso-full/`, not
  `stddocs/` root; the draft's `.agents/qa.md` rows would have carried paths that resolve
  to nothing.
- **Checked and unchanged.** Four. `G_OCTAVE = 1.995_262_314_968_88` with the §5.2.1
  reference in its doc comment; `deconvolve_full` calling `fft_linear_convolve`;
  `tail_s` defaulting to `0.5` at `handlers/audio/sweep.rs:157`; the three ISO PDFs
  present on disk.
- **Added.** None. No line number in the amendment was absent from its draft.

Recorded separately because a draft that had two paths wrong is not the same artifact as
one where two paths were merely written down for the first time, and in the finished
document the two are indistinguishable.

**A false claim in an earlier draft of that amendment, retracted before it reached any
issue.** The draft asserted `filterbank.rs` uses `G = 2` and that the IEC 61260-1
base-10 ratio fix was still outstanding, which would have blocked any ISO 18233 §6.3.2
conformance claim. It is false against the tree: `G_OCTAVE` is defined once, as
10^(3/10), and both `measurement/filterbank.rs` and `visualize/fractional_octave.rs`
import it — there is no second definition and no local override, so
`Filterbank::citation()`'s `verified: true` is sound. The claim came from a stale summary
rather than a grep, and read as *more* rigorous than the vaguer true statement it
replaced — the failure mode #290's first corollary names. No blocker, no issue, no
citation revisited.

### Routing exceptions

`output-format` has no label in this repo; #286 carries `feature` + `needs-ux` +
`needs-design` instead, with the type recorded in its spec body. The mismatch is a
`.agents/triage.md` defect — §1 lists a category §4 cannot apply — and is filed as
**#288**. #286's labels change if #288 chooses to create the label.

No `agent:triage` label was applied to any of these. That label means the triage agent
acted and pairs with a spec comment carrying the `<!-- agent: triage -->` marker in the
prescribed structure; these specs came out of architect-mode analysis transcribed from
this document, so the label would misdescribe how they were made. This document is the
provenance instead.

## Sub-issues

Ordered by dependency. `S9` and `S7` are cheap and unblock nothing — do them first
anyway, because `S9` validates the foundation the rest of the epic assumes.

| id | title | type | routing | blocks |
|---|---|---|---|---|
| S9 | Run `it_loopback_ir` on the rig | infrastructure | ready-to-implement | — |
| S7 | `extract_irs` harmonic-window contract is unenforced and the defaults violate it | bug | ready-to-implement | — |
| S11 | Skipping a `calibrate` prompt destroys the stored voltage calibration | bug | ready-to-implement | S2 |
| S1 | Report schema v4 — multi-payload, gate parameters, position provenance | feature | needs-design | S4, S5 |
| S2 | Interface latency (τ) captured during `ac calibrate`, scoped + history | feature | needs-design | S4 |
| S3 | Move Farina to `ac plot ir`; demote `ac sweep` to a generator verb | feature | needs-design | S4 |
| S4 | `ac plot ir` must print and persist its result | bug | ready-to-implement | — |
| S5 | Gated frequency response derived from the linear IR | feature | needs-design | — |
| S6 | Mic-curve and SPL correction on the IR path | measurement-accuracy | ready-to-implement | — |
| S8 | `ac-scene` IR type + `ac-view` IR mode, fed by both producers | output-format | needs-design | — |
| S10 | Documentation currency sweep | docs | ready-to-implement | — |

---

### S9 — Run `it_loopback_ir` on the rig

**type:** infrastructure

**problem statement**
`ac-daemon/tests/it_loopback_ir.rs` is the only test covering `sweep_ir` through real
audio hardware, and it is `#[ignore]`'d on JACK. `work/qa/qa-ignore-audit-2026-08-10.md`
already records the gap as "sweep maths yes; end-to-end audio path **no**". Eight
passing tests cover the Farina numerics; zero verified runs cover the audio path around
them. Everything else in this epic assumes that path works.

**acceptance criteria**
- [ ] Test run on the Babyface Pro at 96 kHz, per the runbook in `ARCHITECTURE.md` →
      "Loopback IR runbook". Not `jackd -d dummy` — the dummy driver exercises the
      ring, not the converter.
- [ ] Peak index, peak magnitude and pre-impulse SNR recorded in `work/rig/` with the
      period size and sample rate that produced them.
- [ ] The measured peak offset from window centre is recorded as a candidate τ for S2.
- [ ] If it fails, the failure is characterised before any other sub-issue starts.

**out of scope**
- Fixing anything it finds. That is a new issue.

**estimated complexity** small (rig time, not code)

---

### S7 — `extract_irs` harmonic-window contract is unenforced and the defaults violate it

**type:** bug

**problem statement**
`extract_irs`' doc states `window_len` must be ≤ the sample distance between adjacent
harmonic peaks, to avoid cross-contamination between orders. Nothing enforces it, and
the daemon's own defaults break it. At `T = 1 s`, 20 Hz → 20 kHz, `L = 144.76 ms`:

| pair | gap | samples @48k | samples @96k |
|---|---|---|---|
| H3→H4 | 41.65 ms | 1999 | 3998 |
| H4→H5 | 32.30 ms | 1551 | 3101 |

Default `window_len = 4096` exceeds three of those four. With default
`n_harmonics = 5` the H3/H4/H5 gates overlap at 48 kHz and H5 overlaps H4 at 96 kHz.
Latent today only because nothing consumes the harmonic IRs — which S4 and S5 change.

**acceptance criteria**
- [ ] `extract_irs` computes the minimum adjacent-harmonic spacing for the requested
      `n_harmonics` and either errors or clamps `window_len`, with the chosen value
      reported to the caller. Which of the two is a design call for the implementer;
      silently proceeding is not an option.
- [ ] Unit test with hand-derived spacing that fails against today's implementation.
      Mutation-verify at birth.
- [ ] Daemon defaults adjusted so the shipped configuration satisfies its own contract.

**out of scope**
- Anything about what the harmonic IRs are *used* for. See S5.

**estimated complexity** small

---

### S1 — Report schema v4: multi-payload, gate parameters, position provenance

**type:** feature · **needs-design**

**problem statement**
`MeasurementReport` carries one `method` and one `data`. A single Farina capture
naturally yields three payloads — the impulse response, the frequency response derived
from it, and the per-band levels — and today you must pick one or emit three sibling
reports that do not know they are siblings. `plot.rs:430` already shows the strain from
the other direction: it emits `SteppedSine { standard: Filterbank::citation() }`, a
*method* slot holding a *processing* citation, because there is nowhere else to put it.

Separately and more seriously, there is nowhere to record the gate. `IntegrationParams`
is `{ duration_s, window: String }`. A gated quasi-anechoic response is a **different
number** depending on gate start, gate length and window shape. Archive one into today's
schema and the result cannot be reproduced from its own contents — which is the single
thing `MeasurementReport` exists to prevent.

**acceptance criteria**
- [ ] `data` becomes a list. `SCHEMA_VERSION` → 4. Version history comment extended in
      the same style as v2/v3.
- [ ] A gate parameter block records at minimum: gate start, gate length, window kind,
      and the derived `f_low_hz`. `f_low_hz` is stored, not left to the reader to
      recompute.
- [ ] Optional source/receiver position and distance fields, free-form enough to be
      useful now and specific enough that a later ISO-3382-shaped requirement can
      tighten rather than replace them.
- [ ] v1/v2/v3 reports still decode. Round-trip test per variant.
- [ ] `to_csv` handles a multi-payload report without inventing a column schema that
      collapses two payloads into one table.
- [ ] `report_html.rs` and `report_pdf.rs` render every payload in the list.

**out of scope**
- ZMQ wire changes. This is the archival schema only.
- Any room-acoustic parameter field.

**needs architect review** yes — archival contract, and the multi-payload shape
determines how S5 emits.

**estimated complexity** medium

---

### S2 — Interface latency (τ) captured during `ac calibrate`, with scoped validity and history

**type:** feature · **needs-design**
**depends on:** S11 (skip-clobbers-voltage-cal), for the cheap-refresh criterion below.

**problem statement**
A Farina IR carries the converter round trip as an unmeasured additive offset on every
arrival time. The rig has measured that constant — 1.1931 ms — and there is nowhere in
`CalibrationEntry` to put it, so every distance readout derived from an IR is currently
wrong by an unknown amount.

τ is not a property of the interface. It is a property of *(device, backend, sample
rate, period size, port pair)*. Change `-p 1024` to `-p 256` and it moves by
milliseconds. A stale τ biases every report with no symptom: the IR still looks
perfect, the peak is simply in the wrong place. That is the same silent-wrongness shape
as the stale snapshot references.

**No new subcommand.** `handlers/calibrate.rs` already opens both directions
(`eng.start(&[out_port], Some(&in_port))`), plays a reference tone, captures the input
to scale the DMM reading, and computes `is_loopback` (line 110) — the state needed to
decide whether τ is measurable is already established at that point. τ is derived there.

**acceptance criteria**
- [ ] New calibration layer storing τ **with the conditions it was measured under**:
      device identity, backend (`jack` / `cpal`), sample rate, period/buffer size,
      output port, input port, `measured_at`, and the method that produced it.
- [ ] Entries are kept as a **history**, not overwritten. Selection is exact match on
      the condition tuple.
- [ ] No exact match → the measurement **refuses** and names the delta between the
      requested conditions and the nearest stored entry. It does not interpolate,
      does not fall back to "closest", does not proceed uncorrected with a warning.
- [ ] τ is measured inside the `calibrate` worker when `is_loopback` is true: a short
      ESS after the voltage steps, deconvolved with the same `inverse_sweep`, peak
      position is τ. The existing `cal_prompt` / `cal_done` frame sequence is extended,
      not replaced.
- [ ] When `is_loopback` is false, τ is **not** measured and `cal_done` says so
      explicitly. A user calibrating against a DMM with no loopback patched must not be
      left guessing whether τ was captured.
- [ ] **Cheap refresh:** running `ac calibrate` with both voltage prompts skipped
      refreshes τ and leaves every other layer untouched. This is the path a user takes
      after a period-size change and it must not cost a DMM session. (Requires S11.)
- [ ] The layer is **parallel**, not composed, per the `.agents/architect.md`
      calibration invariant. Test that asserts no call site derives a level from τ or
      a time from a voltage cal.
- [ ] The report records which route produced τ: measured this run, or loaded from cal
      under these conditions. A reader a year later must be able to tell.
- [ ] Test: a synthetic entry recorded at one period size is refused at another, with
      the delta in the message.

**stretch (drop if it costs time)**
- [ ] Outlier check against history: a newly measured τ that disagrees with every prior
      entry for the same device at any rate is flagged. Cheap, and it catches the case
      where the patch is wrong rather than the settings.

**out of scope**
- Interface magnitude/phase response (`H_if`). Genuinely small over 20 Hz–20 kHz at
  96 kHz; mostly top-octave droop at 48 kHz. Leave schema room, do not implement.
- Applying τ to the live transfer path, which derives its own delay per session.
- Any new `calibrate` noun.

**needs architect review** yes — new calibration layer, and the refuse-vs-degrade rule
is a policy decision.

**estimated complexity** medium

---

### S3 — Move Farina to `ac plot ir`; demote `ac sweep` to a generator verb

**type:** feature · **needs-design**

**problem statement**
`ac sweep level` and `ac sweep frequency` drive the tone generator and return. They
capture nothing and analyse nothing — `generate` variants wearing a measurement verb.
`ac sweep ir` is a full Tier-1 measurement stranded in that neighbourhood. Meanwhile
`ac plot` is already "give me a frequency response with a report", which is exactly what
the Farina path should produce.

**acceptance criteria**
- [ ] `ac plot ir` accepts `f1`, `f2`, duration, level, and exposes `n_harmonics`,
      `window_len`, `tail_s` and the gate parameters that the CLI currently hardcodes
      to daemon defaults.
- [ ] `ac sweep level` / `ac sweep frequency` reachable under `generate`. Whether
      `sweep` survives as an alias is the implementer's call; if it does, it is
      documented as one, not as a second spelling.
- [ ] `ac sweep ir` either removed or aliased with a deprecation note. Not left as a
      silent second path.
- [ ] Parser tests for the new forms, including abbreviations, and for the rejection of
      the old ones if removed.
- [ ] Daemon command name follows the CLI or is documented where it does not.

**out of scope**
- Refactoring `parse/mod.rs`. It is a known god-object; add the subcommand, do not
  rewrite it in this PR.

**needs architect review** yes — public CLI surface.

**estimated complexity** medium

---

### S4 — `ac plot ir` must print and persist its result

**type:** bug

**problem statement**
`commands/sweep.rs::run_ir` sends the command and calls `wait_for_stop`, which handles
only `done` and `error`. The `measurement/impulse_response` and `measurement/report`
frames are published by the daemon and dropped by the client. Nothing prints, nothing
saves, and `sweep_ir` — unlike `plot` — never writes to `report_dir`.

**acceptance criteria**
- [ ] The command consumes both frames and prints, at minimum: arrival time, arrival as
      distance (τ-corrected, with the τ provenance named), peak magnitude, pre-impulse
      SNR, the gate in use and its `f_low_hz`.
- [ ] Report JSON written to `report_dir` following the path and naming convention
      `plot` already uses, so `ac report <path.json>` renders it with no further work.
- [ ] CSV written alongside, via the existing `to_csv` IR branch.
- [ ] When τ is unavailable, distance is stated as unavailable — not computed from an
      uncorrected arrival and not silently omitted.
- [ ] Integration test under `--fake-audio` asserting the files exist and the printed
      arrival matches the fake backend's known 32-sample loopback delay.

**out of scope**
- Any graphical display. See S8.

**estimated complexity** small

---

### S5 — Gated frequency response derived from the linear IR

**type:** feature · **needs-design**

**problem statement**
Nothing derives a frequency response from the IR. `report_html.rs:285` and
`report_pdf.rs:85` print the sample rate, the sweep bounds, `linear_ir.len()` and the
count of harmonic orders — and stop. The harmonic IRs, whose stated purpose in the
module docstring is a frequency-resolved THD curve, have no consumer at all.

This is the sub-issue that produces the thing a user actually looks at, and the one
where the "understanding why it is useful" bar is met or missed.

**acceptance criteria**
- [ ] Gate applied to the linear IR with a stated window; magnitude and phase derived
      by FFT of the gated result.
- [ ] `f_low_hz = 1 / t_gate` computed, stored in the report per S1, and **printed
      next to the curve wherever the curve appears**. A response shown without its
      validity floor fails this criterion regardless of numerical correctness.
- [ ] Distortion-vs-frequency derived from the harmonic IRs, or an explicit statement
      in the issue that it is deferred and why. Silence is not an answer here — the
      extraction already runs on every measurement.
- [ ] Emitted as an additional payload in the S1 multi-payload report, not as a
      separate report.
- [ ] Synthetic tests with hand-derived expected values: flat system → flat gated
      response; known single pole → corner recovered; known gate length → `f_low_hz`
      matches by hand arithmetic.
- [ ] A test asserting the gated response of a synthetic direct-plus-reflection differs
      from the ungated one in the way the gate predicts. Mutation-verify.

**out of scope**
- Reflection localisation, parametric reflection removal, target curves.
- Room-acoustic parameters.

**needs architect review** yes — window choice and the `f_low` convention are display-
truth decisions with archival consequences.

**estimated complexity** large

---

### S6 — Mic-curve and SPL correction on the IR path

**type:** measurement-accuracy

**problem statement**
`handlers/audio/sweep.rs:306` hardcodes `mic_correction_applied: false`, deferring to a
follow-up of #97. Both calibration layers already exist (`mic_response`,
`mic_sensitivity_dbfs_at_94db_spl`) and `plot.rs:170` already reports the flag honestly.
This is wiring, not new capability — but it cannot reuse `plot`'s route naively.

`MicCurveFir` is linear-phase symmetric with group delay `(n_taps−1)/2`: 255 samples at
the 512-tap default, **5.3 ms at 48 kHz, 1.8 m of path**. Convolving the IR with it
before gating moves the arrival time by nearly two metres, corrupts the distance
readout, and displaces the gate relative to the reflection it was placed to exclude.

**acceptance criteria**
- [ ] Arrival estimation and gating operate on the **uncorrected** IR.
- [ ] Mic-curve correction applied in the frequency domain to the derived spectrum, via
      the route `plot` already uses — not the time-domain FIR.
- [ ] SPL offset applied to the derived spectrum where an SPL layer exists;
      `dbspl = dbfs − mic_sens + 94` computed from uncalibrated dBFS per the parallel-
      layers invariant.
- [ ] `mic_correction_applied` reports the truth instead of a literal `false`.
- [ ] **The load-bearing test:** inject a synthetic mic curve with known group delay
      into a synthetic IR at a known arrival; assert the IR peak index is **unchanged**
      and the derived response is corrected by the expected amount. This test must fail
      against the obvious wrong implementation — demonstrate that at birth.

**out of scope**
- Deep FIR-based IR correction. The frequency-domain route is sufficient and is the
  correct one for a derived spectrum.

**estimated complexity** medium

---

### S8 — `ac-scene` IR type + `ac-view` IR mode, fed by both producers

**type:** output-format · **needs-design**

**problem statement**
`handlers/transfer.rs` computes a `visualize/ir` sidecar — the `json!` block emitting
`"type": "visualize/ir"` — for every transfer frame.
`ZMQ.md:1939` documents it. `it_protocol.rs:1451` tests it. `ac-view/src/session.rs:145`
returns only frames where `type == "transfer_stream"` and the comment says outright that
`ac-view` has no consumer for anything else. It is produced, tested, documented, and
thrown away on arrival.

One scene type and one view mode serve both this sidecar and the S5 sweep result. This
is the single highest-leverage item in the epic because it retires an existing waste and
delivers the new capability in the same PR.

**acceptance criteria**
- [ ] `ac-scene` grows an IR scene type. All dB conversion happens there; `ac-view`
      computes nothing numeric — `computes_nothing` stays green.
- [ ] The mode renders both sources. **The two must be labelled distinctly**, because
      they are not equivalent: the sidecar is Welch-averaged H₁ at 1 Hz resolution,
      delay pre-compensated out, mic curve deliberately not applied, decimated to 2000
      samples (stride 24 at 48 kHz — 0.5 ms, ~17 cm of path per sample). It is a live
      arrival view. It is **not** a basis for a gated measurement, and the display must
      not let a user mistake it for one.
- [ ] Arrival marker placed from the frame's own `delay_ms` / `t_origin_ms`, not
      recomputed in the view.
- [ ] Gate bounds and `f_low_hz` shown when displaying a sweep-derived IR.
- [ ] Keybindings respect the Finnish layout constraint — no `[`, `]`, `+`, `-`.
- [ ] Display-truth invariants I-B and I-C hold: every shown value tested in pure code
      against checked-in fixtures, live/snapshot parity, cross-tier parity.

**out of scope**
- Snapshot format changes for IR. File separately if S1 does not cover it.

**needs architect review** yes. **UX gate also required** — display surface. QA sign-off
required before `ux-approved` per the value-display routing rule.

**estimated complexity** large

---

### S11 — Skipping a `calibrate` prompt destroys the stored voltage calibration

**type:** bug
**Pre-existing. File and fix regardless of whether this epic proceeds.**

**problem statement**
`handlers/calibrate.rs` prompts for the output and input Vrms readings, both offering
"press Enter to skip". A skipped prompt yields `None`, and the save path assigns it
unconditionally:

```
cal.vrms_at_0dbfs_out = vrms_at_0dbfs_out;
cal.vrms_at_0dbfs_in  = vrms_at_0dbfs_in;
```

So skipping does not skip — it erases. The adjacent comment ("only voltage fields are
overwritten here") is accurate and is describing the defect: SPL and mic-curve layers
are correctly preserved via `load_or_new`, voltage layers are not. A user who runs
`ac calibrate` to re-check one leg and presses Enter through the other loses it, with a
`cal_done` frame reporting `null` that reads as "not measured" rather than "deleted".

This blocks S2's cheap-refresh path: refreshing τ after a period-size change means
re-running `calibrate` with the voltage prompts skipped, which currently costs the
voltage calibration.

**acceptance criteria**
- [ ] A skipped prompt preserves the existing stored value. Only a supplied reading
      overwrites.
- [ ] An explicit clear remains possible, but requires an unambiguous action — not the
      same keystroke as "skip".
- [ ] `cal_done` distinguishes "unchanged" from "newly measured" from "absent". Three
      states, three words.
- [ ] Regression test: seed a cal entry, run `calibrate` with both prompts skipped,
      assert both voltage fields survive byte-identical. Mutation-verify against the
      current implementation.
- [ ] Check whether `calibrate_spl` and `calibrate_mic_curve` share the shape. They
      appear not to — confirm rather than assume.

**out of scope**
- The τ layer itself. See S2.

**estimated complexity** small

---

### S10 — Documentation currency sweep

**type:** docs

**problem statement**
Several documents describe a tree that no longer exists, and one describes a limitation
this epic removes.

**acceptance criteria**
- [ ] `docs/design/acoustic-analyze-v1.md` §7 — "**Limitation — no gating**" states that
      the snapshot carries steady-state spectra, that the analysis cannot separate
      direct sound from reflections, and that the prompt must forbid gating-dependent
      claims. Once S5 lands, that constraint is conditional on the payload source, not
      absolute. Amend §7 to say which payload the limitation applies to, and note that
      an IR-derived payload lifts it. Do **not** design the IR payload in that document
      — record the changed constraint so the next reader does not architect around a
      limitation that has gone.
- [ ] Same document §4.2 — bulk delay estimated from mid-band phase slope. An IR gives
      arrival time directly and retires that estimator for the IR path. Note it; do not
      implement it.
- [ ] `ARCHITECTURE.md:456` and the `sweep_ir` header comment both state that only the
      fake backend implements `play_and_capture` and that real JACK/CPAL is follow-up
      #78. Both backends implement it (`jack_backend.rs:375`, `cpal_backend.rs:363`).
      Same staleness class as the "set clock to Internal" repair.
- [ ] `ARCHITECTURE.md:16`, `:193` and `README.md:164` describe the `plot` / `sweep`
      split that S3 changes.
- [ ] `ZMQ.md` updated for any command rename or new frame arising from S3/S5.
- [ ] Grep for each edited document **as a cited name**, not only as a subject, before
      changing or moving anything.

**out of scope**
- Rewriting `acoustic-analyze-v1.md` for the IR era. It is a scoped experiment; a
  V2 that consumes an IR payload is a new document.

**estimated complexity** small

---

## Standards — what this epic can and cannot claim

**Amended 2026-08-11.** ISO 18233:2006, ISO 3382-1:2009 and ISO 3382-2:2008 are now held
as `stddocs/iso-full/ISO18233.pdf`, `ISO3382-1.pdf` and `ISO3382-2.pdf`. The section
below was written when they were not, and said so; it has been rewritten in place.
Analysis and the routing of every consequence:
`work/handoff/handoff-ir-integration-iso-amendment.md`. #276's body on GitHub is the
authoritative version — this is the provenance copy.

**Citable from documents already in `stddocs/`, after human cross-check:**

- ISO 18233:2006 **Annex B (normative)**, "Swept-sine method". The swept-sine method now
  has a normative standard behind it rather than two informative annexes alone.
- AES17-2015 Annex A.4 (informative), "Exponential sine sweep (chirp) analysis" — ESS
  is an AES17-recognised measurement method.
- IEC 61260-1:2014 Annex G (informative), "Filter response to exponentially swept
  sinusoidal signals", with Figure G.1 relating the logarithmic frequency scale to the
  linear time scale of the sweep. This is the hook for ESS → conformant filterbank →
  band levels.
- The existing Farina AES 108 #5093 citation, already `verified: true`, correctly
  labelled as a preprint rather than a standard. It stays: the preprint is the
  theoretical basis (harmonic offsets, inverse-filter construction), Annex B is the
  normative method. Both, not either.

Informative annexes do not block `verified: true` — verified means a human checked the
clause says what the code claims, not that the clause is normative. The clause string
must say "(informative)".

**ISO 18233 never stands alone.** §1 specifies methods "to be used as substitutes for
measurement methods specified in standards covering classical methods, such as ISO 140
(all parts), ISO 3382 (all parts) and ISO 17497-1", and §9(c) requires the test report to
name "the number and title of the International Standard of the applicable classical
method". The standards string therefore depends on *what was measured*, not on which
stimulus produced it:

| use case | classical method | ISO 18233 applies? | citation |
|---|---|---|---|
| room measurement | ISO 3382-2 (or 3382-1) | yes | ISO 18233 + the classical standard |
| quasi-anechoic loudspeaker / PA | none in §1's list | **no** | AES17-2015 A.4 + Farina |

That is a design constraint on #280, not prose: it decides whether the citation attaches
to `MeasurementMethod` or to the payload, and must be settled before the schema is cut.

**Still not held:** IEC 60268-21, the acoustical-output counterpart to the 60268-3
already held. It is now the *only* missing document, and it is missing for the
loudspeaker/PA case alone. Acquiring the three ISO documents did not make that case
citable — it made the room case citable and left the PA case where it was. Acquiring
60268-21 is Markus's action and is deliberately **not** filed as an issue.

**A documented misreading of what this code produces.** `deconvolve_full` calls
`fft_linear_convolve` — `ac` does linear, not circular, deconvolution. ISO 18233 B.5
states that linear deconvolution yields a decaying noise tail, increasingly low-pass
filtered toward its end, because that part of the result is steady noise convolved with
the sweep in reverse order; it then requires that "the user shall be aware of this effect
so as not to confuse the decreasing noise floor with the reverberant tail of the room."
This lands exactly on the epic's bar — a user can tell what part of the result is real.
Routed to #283 (print and persist) and #284 (mark it on the result).

**Acquisition length has a criterion.** ISO 18233 §6.3.2: for non-repetitive excitation
and measurement of level, the recording "shall cover the time from the start of
excitation to the time where the response in each fractional octave band has decayed by
more than 30 dB". B.2 adds that acquisition shall outlast the sweep until the
reverberation falls under the noise floor, and that — unlike periodic excitation — no
requirement relates sweep duration to expected reverberation time.
`handlers/audio/sweep.rs:157` defaults `tail_s = 0.5`; that default is now checkable
rather than a guess. Routed to #282.

**Room parameters stay out of scope, now for quantitative reasons.** IEC 61260-1:2014
§3.30 defines *filter decay time* — 60 dB drop after cessation of the input — and Annex H
covers measuring it, explicitly distinguishing instruments that can measure reverberation
time from those that cannot. Band-filtering an IR and integrating the decay contaminates
the result with the filter's own decay, worst at LF where the bands are narrowest. The
ISO documents give that concern its specific form and add their own:

- ISO 3382-1:2009 §7.3, Eq. (6)/(7) — forward analysis requires `BT > 16` and
  `T > 2·T_det`. Twice as strict as 3382-2's detector limit.
- ISO 3382-2:2008 §7.3, Eq. (4)/(5) — forward analysis requires `BT > 16` and
  `T > T_det`. A NOTE under this clause relaxes to `BT > 4` and `T > T_det/4` for the
  time-reversal technique, citing an undated `ISO 3382-1:—` (the then-forthcoming
  edition). The held ISO 3382-1:2009 §7.3 contains neither the relaxation nor a
  time-reversal clause, so the relaxation is not verifiable against the 2009 edition on
  disk — whoever files T20/T30 should treat it as 3382-2-only.
- ISO 3382-1 §5.3.3 and Eq. (3) — backward integration from a noise-aware start point
  `t₁` with optional correction `C`. With `C = 0` the background noise must sit at least
  the evaluation range plus 15 dB below the impulse peak: 45 dB down for T30.
- ISO 3382-2 §6 — T20 over −5 to −25 dB, T30 over −5 to −35 dB, least-squares fit, with a
  linearity check on the decay curve before a result may be stated at all.

None of that is hard, and all of it has its own falsifiable criteria — which is the
argument for filing it separately rather than bolting it onto an integration epic. Carry
forward when it is filed: ISO 18233 B.6, §6.3.6 and B.8 all favour a single longer sweep
over averaged or repeated sweeps (doubling duration buys 3 dB of effective SNR; averaging
buys the same 3 dB while increasing sensitivity to environmental drift). If IR averaging
is proposed later, the standard already argues against it.

Any parameter shipped before its document is on disk carries `verified: false` or no
citation at all.

---

## Sequencing

```
S9  ─┐                         (rig; validates the foundation)
S7  ─┤                         (small; independent)
S11 ─┤                         (pre-existing bug; lands standalone)
     │
S1  ─┴─→ S3 ─→ S4              (schema → CLI shape → visible output)
     │
S11 ────→ S2 ─→ S4             (skip-preserve → τ → distance readout)
     │
S1  ────→ S5 ─→ S6             (payload shape → derivation → correction)
     │
S8  ───────────────            (parallel track; consumes the existing sidecar
                                from day one, S5's result when it lands)
S10 ───────────────            (after S3 and S5 settle)
```

`S8` is deliberately not gated on anything. The transfer sidecar exists today and is
being discarded today; that is worth stopping regardless of how the sweep path
progresses.

## Definition of done for the epic

- `ac plot ir` runs on the rig, prints an arrival time and a distance that agree with
  the tape measure, and leaves a report on disk that `ac report` renders.
- The gated frequency response is displayed with its `f_low_hz` beside it, and a user
  who did not build this can tell which part of the curve is the device and which is
  the room.
- The `visualize/ir` sidecar reaches a screen.
- τ is captured by `ac calibrate` when a loopback is patched, stored with its
  conditions, refuses rather than degrades, and can be refreshed after a period-size
  change without touching a DMM.
- No room-acoustic parameter has been invented, and no `StandardsCitation` has been
  flipped to `verified: true` without a document in `stddocs/` behind it.
