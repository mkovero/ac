# handoff-ir-integration-iso-amendment — ISO 18233 / 3382-1 / 3382-2 now held

**Status:** EXECUTED 2026-08-11. Issue bodies edited, A1 filed as #291, `work/handoff/handoff-ir-integration.md`
§Standards rewritten. GitHub holds the authoritative text — this is the provenance record
for *why* those edits were made, not the tracker. §5's `.agents/qa.md` change is on its
own branch awaiting human ratification and is the one item not landed.

**Expiry:** delete when #276 closes, or when §5's `.agents/qa.md` rows have landed and
the issue bodies below have been revised on GitHub past what this copy says. Do not edit
the issue text here to keep it in sync; edit the issues.

**Amends:** `work/handoff/handoff-ir-integration.md` and issues #276–#287.
**Trigger:** ISO 18233:2006, ISO 3382-1:2009 and ISO 3382-2:2008 added to
`stddocs/iso-full/`.
**Nature:** edits to filed issue bodies + two doc changes + one new issue. No new
sub-issues in the epic. Nothing here widens the epic's scope; two items narrow it.

---

## 0. Correction carried into this document

An earlier draft of this analysis claimed `filterbank.rs` uses `G = 2` and that the
IEC 61260-1 base-10 ratio fix was outstanding, making it a blocker for any ISO 18233
§6.3.2 conformance claim.

**That is false against the tree.** `ac-rs/crates/ac-core/src/shared/constants.rs:23` defines
`G_OCTAVE = 1.995_262_314_968_88` — that is 10^(3/10) — with the §5.2.1 reference in
its doc comment. Both `measurement/filterbank.rs` and `visualize/fractional_octave.rs`
import that constant; there is no second definition and no local override. The fix
landed, in lockstep, and `Filterbank::citation()`'s `verified: true` is sound.

The claim came from a stale summary rather than a grep. It is recorded here rather than
silently dropped because the epic's provenance discipline applies to its own amendments,
and because it is a direct instance of the corollary added in `agents-cite-precision-rule`
("added precision must come from a lookup") — the wrong claim read as more rigorous than
the vaguer true one it replaced.

**No action follows from it.** No blocker to add, no issue to file, no citation to
revisit.

---

## 1. What the three documents actually change

Four findings. One is a scope boundary that should be settled before #280 is designed.

### 1.1 ISO 18233 Annex B is normative

The annex is headed "Annex B (normative) Swept-sine method". This is a stronger footing
than the epic currently assumes — it rests on AES17-2015 Annex A.4 and IEC 61260-1
Annex G, both informative. The swept-sine method now has a normative standard behind it.

`sweep.rs::citation()` today names only the Farina AES 108 preprint #5093. A preprint
and a normative annex are different kinds of authority; the report should carry both.

### 1.2 ISO 18233 is a substitution standard — it never stands alone

§1: it specifies methods "to be used as substitutes for measurement methods specified in
standards covering classical methods, such as ISO 140 (all parts), ISO 3382 (all parts)
and ISO 17497-1." §9(c) then requires the test report to name "the number and title of
the International Standard of the applicable classical method."

That splits `ac plot ir` in two, by what is being measured rather than by which command
ran:

| use case | classical method | ISO 18233 applies? | citation |
|---|---|---|---|
| room measurement | ISO 3382-2 (or 3382-1) | yes | ISO 18233 + the classical standard |
| quasi-anechoic loudspeaker / PA | none in §1's list | **no** | AES17-2015 A.4 + Farina |

The loudspeaker case falls to IEC 60268-21, which is **not** held. So acquiring these
three documents did not make the PA use case citable — it made the room use case citable
and left the PA case exactly where it was.

**Design consequence:** the standards string is a property of the measurement, not of
the method enum. That decides whether the citation attaches to `MeasurementMethod` or to
the payload, and it must be settled in #280 before the schema is cut.

### 1.3 Linear deconvolution has a documented misreading

`deconvolve_full` calls `fft_linear_convolve` — `ac` does linear, not circular,
deconvolution. ISO 18233 B.5 states that linear deconvolution yields a decaying noise
tail, increasingly low-pass filtered toward its end, because that part of the result
originates from steady noise convolved with the sweep in reverse order. It then requires
that "the user shall be aware of this effect so as not to confuse the decreasing noise
floor with the reverberant tail of the room."

A standards-documented misreading of the exact artefact this implementation produces,
landing squarely on the epic's stated bar — that a user can tell what part of the result
is real. This is the highest-value single addition in this amendment.

### 1.4 Acquisition length has a criterion, not a default

ISO 18233 §6.3.2: for non-repetitive excitation and measurement of level, the recorded
response "shall cover the time from the start of excitation to the time where the
response in each fractional octave band has decayed by more than 30 dB." B.2 adds that
acquisition shall outlast the sweep so the reverberation is collected until it falls
under the noise floor, and that — unlike periodic excitation — there is no requirement
relating sweep duration to the expected reverberation time.

`handlers/audio/sweep.rs:157` defaults `tail_s = 0.5`. That is now checkable rather
than a guess.

---

## 2. Issue edits

Body edits to filed issues. No label changes. No new sub-issues.

| issue | was | edit |
|---|---|---|
| #276 | EPIC | rewrite §Standards — it is now false |
| #280 | S1 schema v4 | add ISO 18233 §9 + the knowable ISO 3382 §9.2 subset; settle where the citation attaches |
| #282 | S3 CLI | `tail_s` default gets a criterion |
| #283 | S4 print+persist | name the linear-deconvolution tail |
| #284 | S5 gated FR | mark the linear-deconvolution tail on the result |
| #285 | S6 mic/SPL | corroboration note only; **no criteria change** |

### #276 — rewrite the Standards section

The current text states these documents are "not held, and therefore not claimable" and
that acquiring them "is Markus's action and is deliberately not filed as an issue."
Both sentences are stale.

Replacement must carry: the three documents are held; ISO 18233 Annex B is normative;
the substitution structure and the two-case table from §1.2 above; and that IEC 60268-21
is now the **only** missing document, needed for the loudspeaker case alone.

Retain unchanged: the AES17 A.4 and IEC 61260-1 Annex G citations (still valid, still
the correct citation for the PA case), and the reason room parameters stay out of scope
— now with the quantitative backing in §4 below rather than as caution.

### #280 — schema fields and citation placement

Two additions.

**Report fields.** ISO 18233 §9 requires, beyond the classical method's own
requirements: reference to ISO 18233:2006; a short description of the applied method
(signal type, signal duration, number of averages); and the number and title of the
applicable classical standard. Small, concrete, fully in scope.

From ISO 3382-1 §9.2 and ISO 3382-2 §9.2, the subset a daemon can actually know:
temperature and relative humidity; source and receiver positions **with heights**;
description of the measuring apparatus and microphones; description of the sound signal;
date. Note that temperature already exists in the system — `ac setup temp` feeds the
`c = 331.3 + 0.606·T` distance calculation — and should flow into the report rather than
being collected twice.

Everything else those clauses require is operator metadata about the room: sketch plan,
volume, seating, occupancy, curtain state, stage furnishing. It confirms the existing
decision not to model a room. Do not add fields for it.

**Citation placement.** Per §1.2, the standards string depends on what was measured, not
on which stimulus method produced it. `plot.rs`'s existing
`SteppedSine { standard: Filterbank::citation() }` already shows the method slot being
used for something it does not fit. Decide in the design comment whether the citation
attaches to the method or to the payload; the multi-payload change makes the second
possible for the first time.

### #282 — `tail_s` default

Add: the shipped default must satisfy ISO 18233 §6.3.2 — the capture covers until each
fractional-octave band has decayed more than 30 dB — rather than remaining a fixed
0.5 s. Whether that is a computed default, a documented minimum, or a post-hoc check
that the capture was long enough is the implementer's call; a bare constant with no
stated basis is not one of the three.

### #283 and #284 — the noise tail must not read as reverberation

Add to both, phrased for each context:

- **#283 (print+persist):** the printed summary and the report state that the tail is a
  linear-deconvolution artefact where it is one, per ISO 18233 B.5. A user reading a
  decaying tail in a text summary has no other way to know.
- **#284 (gated FR):** the boundary is marked on the result, not left to the reader.
  Where it falls follows from the sweep and capture lengths and is derivable rather than
  estimated.

This is the item most directly tied to the epic's definition of done. If only one thing
from this amendment lands, it should be this.

### #285 — corroboration, no change

ISO 3382-1 A.3.4 records that direct and early-arriving low-frequency sound can be
significantly attenuated, and that it may be necessary to determine the start time from
the broadband or high-frequency response together with the measured filter delay.

That is independent support for the ordering already settled in #285: arrival estimation
and gating on the broadband uncorrected IR, correction applied afterward in the
frequency domain. Add as a note. **Do not add an acceptance criterion** — the criteria
are already right and the standard changes nothing about them.

---

## 3. New issue

### A1 — cite ISO 18233:2006 Annex B in `sweep.rs::citation()` — filed as #291

**type:** measurement-accuracy · **ready-to-implement** · **human gate: `verified` flip**

**problem statement**
`ac-core/src/measurement/sweep.rs::citation()` names the Farina AES 108 preprint #5093
alone. With ISO 18233:2006 in `stddocs/`, the swept-sine method has a normative annex
behind it, and the report should say so. The preprint remains the correct citation for
the theoretical basis — the harmonic offsets and inverse-filter construction — while
Annex B is the normative method.

**acceptance criteria**
- [ ] Citation names ISO 18233:2006 Annex B (normative) alongside the existing preprint,
      or the `StandardsCitation` type grows to hold both. Do not drop the preprint.
- [ ] `verified: true` only after cross-check against the document text. Human gate.
- [ ] `ARCHITECTURE.md` "Standards tracked" table updated in the same PR — its `sweep.rs`
      row currently names the preprint alone, and the table and the `citation()` fn must
      not disagree.
- [ ] `every_measurement_module_emits_populated_citation` stays green.

**out of scope**
- The classical-method citation from §1.2, which is per-measurement and belongs to #280.

**estimated complexity** small

**Not filed and deliberately so:** nothing about the base-10 `G` ratio. See §0.

---

## 4. Out of scope, now with numbers

Room parameters stay out of the epic. The documents make the reason quantitative rather
than cautionary:

- **ISO 3382-2 §7.3 / 3382-1 §7.3** — forward analysis requires `BT > 16` and
  `T > T_det`, relaxing to `BT > 4` and `T > T_det/4` with time reversal. This is the
  specific form of the filter-decay concern IEC 61260-1 Annex H raises.
- **ISO 3382-1 §5.3.3 and Eq. (3)** — backward integration from a noise-aware start
  point `t₁` with the optional correction `C`. With `C = 0` the background noise must sit
  at least the evaluation range plus 15 dB below the impulse peak: 45 dB down for T30.
- **ISO 3382-2 §6** — T20 evaluated over −5 to −25 dB, T30 over −5 to −35 dB,
  least-squares fit, with a linearity check on the decay curve before a result may be
  stated at all.

None of that is hard. All of it has its own falsifiable criteria and is exactly the kind
of work that gets done badly when bolted onto an integration epic.

**Carry forward when it is filed:** ISO 18233 B.6 and §6.3.6 both favour a single longer
sweep over averaged or repeated sweeps — doubling sweep duration buys 3 dB of effective
SNR, and averaging buys the same 3 dB while increasing sensitivity to environmental
drift. B.8 recommends against periodic sweeps for the same reason. If IR averaging is
proposed later, the standard already argues against it.

---

## 5. Doc updates

Two files, **different routing** — do not put them on one branch.

### Deferred to #291's implementation PR

**`ARCHITECTURE.md`** — "Standards tracked" table, `sweep.rs` row (line 333), which today
names the Farina preprint alone. It gains ISO 18233 Annex B **in #291's PR, not on the docs
branch**: editing it here would leave the table claiming a normative annex that
`sweep.rs::citation()` does not yet name, which is the precise disagreement #291's third
acceptance criterion exists to prevent. Nothing else in that table changes; in
particular the `filterbank.rs` row is correct as written (see §0).

### Ordinary docs branch

**`work/handoff/handoff-ir-integration.md`** — the §Standards section is now false in the
tree, and the tree copy is the one a future reader trusts. Amend in place to match the
rewritten #276 body. Record the amendment in the provenance section with a pointer to
this document, following the precedent set when the `transfer.rs` line-number drift was
recorded there rather than marked inline.

### Spec branch — human ratification required

**`.agents/qa.md`** — the "normative standards" table lists seven documents and is what
the QA agent consults before reviewing any measurement PR. Three are missing:

| standard | file | applies to |
|---|---|---|
| ISO 18233:2006 | `stddocs/iso-full/ISO18233.pdf` | deterministic-signal (swept-sine) substitution for classical room and building acoustics methods; IR acquisition, SNR, time-invariance, test report |
| ISO 3382-1:2009 | `stddocs/iso-full/ISO3382-1.pdf` | room acoustic parameters, performance spaces — reverberation time, early/late measures, positions, test report |
| ISO 3382-2:2008 | `stddocs/iso-full/ISO3382-2.pdf` | reverberation time in ordinary rooms — survey / engineering / precision grades, decay evaluation, uncertainty |

The three documents are under `stddocs/iso-full/`, not `stddocs/` root — an earlier draft
of this section said `stddocs/` and would have produced rows resolving to nothing. The
table's `file` column matches the format of the seven rows already in `.agents/qa.md`.

This is a `.agents/` change and goes through the same gate as
`agents-cite-precision-rule`: its own branch, commit body opening with the ratification
notice, not merged by an agent. Do not let it ride along with the `ARCHITECTURE.md`
edit — that is the failure the earlier split was made to avoid.

---

## 6. What this amendment does not do

- Does not add sub-issues to #276. The epic's shape is unchanged.
- Does not change any label or routing on #276–#288.
- Does not touch #277, #278, #279, #281, #286, #287, #288 — nothing in the three
  documents bears on the rig run, the harmonic-window contract, the calibrate clobber,
  τ, the scene/view work, the doc sweep, or the label defect.
- Does not make the PA / loudspeaker case more citable than it was. That still wants
  IEC 60268-21.
