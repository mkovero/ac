## Standards tracked

Tier 1 modules cite the edition each implementation has been verified
against:

Paths to the held documents are in the **document map** at the end of this
file. That map used to live in `.agents/qa.md`; it was removed there on
2026-08-29 while these pointers to it survived, so for two days every row
below pointed at nothing. Keep the map and the rows in one file.

| Module | Standard | Clause | Verified against |
|--------|----------|--------|------------------|
| `thd.rs` | IEC 60268-3:2018 | §15.12.3 Total harmonic distortion under standard measuring conditions | see document map |
| `filterbank.rs` | IEC 61260-1:2014 | §5.2.1 base-10 G; §5.10 Class 1 relative-attenuation | see document map |
| `weighting.rs` | IEC 61672-1:2013 | §5.5 Frequency weightings; Annex E eqs. (E.1)–(E.8) | see document map |
| `noise.rs` | AES17-2020 | §6.4.2 Idle channel noise level | see document map |
| `reference_levels.rs` | AES17-2020 | §3.12.1 Full-scale level; §3.12.3 Decibels full scale | see document map |
| `ccir468.rs` | ITU-R BS.468-4 | §1 Weighting network; §2 Measuring-device characteristics | see document map |
| `loudness.rs` | ITU-R BS.1770-5 / EBU Tech 3342 | BS.1770 Annex 1 + Annex 2; Tech 3342 §2.2 LRA | see document map; EBU Tech 3341/3342 conformance cases (not a `stddocs/` file) |
| `sweep.rs::citation()` | Farina, AES 108th Conv. preprint #5093 (2000); ISO 18233:2006 Annex B (normative) | §2 Theoretical basis; Annex B (normative) Swept-sine method | see document map (Annex B pending human cross-check, `verified: false`) |
| `sweep.rs::farina_citation()` | Farina, AES 108th Conv. preprint #5093 (2000) | §2 Theoretical basis (log sweep, inverse filter, harmonic offsets) | see document map; `verified: false` |
| `sweep.rs::gated_response_citation()` | AES17-2020 | Annex A.4.5 (informative) quasi-anechoic frequency response via time-gated impulse response | see document map; pending human cross-check, `verified: false` |

When a standard is revised and the revision changes a computation, the
old computation stays available behind a version flag so historical
reports remain reproducible. The default follows the most recent
revision the implementation has been verified against.

### Citation audit workflow

Every Tier 1 module exposes a `citation()` (or `Type::citation()`) fn
returning a `StandardsCitation { standard, clause, verified }`. Handler
code (e.g. `plot.rs`, `plot_ir`) should always call that fn rather than
inlining the citation — that keeps the source-of-truth in one place and
makes audits trivial to roll out.

Flipping `verified: true` requires a cross-check of both `standard` and
`clause` strings against the **published text of the named standard**,
not against secondary sources. As of the #72 audit pass every Tier 1
module ships `verified: true`; a regression test
(`every_measurement_module_emits_populated_citation`) asserts the
non-empty invariant. When adding a new Tier 1 module, place the full
text of the cited standard under `stddocs/iec-full/` and land the
module with `verified: true` from the start — do not reintroduce
`verified: false` placeholders.



## applicable standards

Source docs in `stddocs/` at (main) repo root. Read relevant standard before reviewing any PR touching measurement values, output formatting, or display units. No memory — consult document.

**Take the path from the `file` column, never from the standard's issuing body.** The three subdirectories are historical, not semantic: `iec-full/` holds AES17-2020 and one paper alongside the IEC documents, `iso-full/` holds the ISO ones, and several documents sit at `stddocs/` root. `stddocs/Fundamentals_of_modern_audio_measurement.pdf` (root) is the Cabot paper. 

### normative standards

| standard | file | applies to |
|---|---|---|
| AES-17-2020 | `stddocs/iec-full/aes17_2020_aes_standard_method_for_digital_audio_engineering_measurement.pdf` | THD+N methodology, notch filter specs, measurement conditions, result expression — digital audio |
| IEC 60268-3:2018 | `stddocs/iec-full/IEC60268-3.pdf` | Sound system equipment — amplifiers: frequency response, S/N, dynamic range |
| IEC 61260-1:2014 | `stddocs/iec-full/IEC61260-1.pdf` | Octave and fractional-octave band filters: bandwidth, ripple, attenuation |
| IEC 61672-1:2013 | `stddocs/iec-full/IEC61672-1.pdf` | Sound level meters: frequency weighting, time weighting, level linearity |
| ITU-R BS.468-4 | `stddocs/ITU-R BS.468-4.pdf` | Noise measurement: quasi-peak detector, 468 weighting curve |
| ITU-R BS.1770-5 | `stddocs/ITU-R BS.1770-5.pdf` | Loudness measurement: K-weighting, integrated loudness (LUFS), true-peak |
| ISO 18233:2006 | `stddocs/iso-full/ISO18233.pdf` | Deterministic-signal (swept-sine) substitution for classical room and building acoustics methods; IR acquisition, SNR, time-invariance, test report |
| ISO 3382-1:2009 | `stddocs/iso-full/ISO3382-1.pdf` | Room acoustic parameters, performance spaces — reverberation time, early/late measures, source/receiver positions, test report |
| ISO 3382-2:2008 | `stddocs/iso-full/ISO3382-2.pdf` | Reverberation time in ordinary rooms — survey / engineering / precision grades, decay evaluation, uncertainty |

### reference reading (non-normative)

Not standards, but hold authoritative derivations + worked examples. Consult when standard text ambiguous or when checking numerical results.

| document | file | useful for |
|---|---|---|
| Metzler — Audio Measurement Handbook 2nd ed. | `stddocs/pdfcoffee.com_audio-measurement-handbook-2nd-ed-2005-bob-metzler-pdf-free.pdf` | Practical measurement procedures, expected value ranges, instrument behaviour |
| Fundamentals of Modern Audio Measurement | `stddocs/Fundamentals_of_modern_audio_measurement.pdf` | Estimator theory, windowing, FFT measurement fundamentals |
| Müller & Massarani 2001 | `stddocs/iec-full/Simultaneous_Measurement_of_Impulse_Response_and_D.pdf` | H1 estimator derivation — primary reference for `ac-core/visualize/transfer.rs` |

### how to use them during review

**AES-17** = primary normative reference for `ac-core/measurement/thd.rs`. Read relevant clause — no paraphrase. Check:
- THD+N residual computed after fundamental removal, not as ratio to total RMS
- Measurement bandwidth explicitly stated or match standard default
- Notch filter attenuation at fundamental sufficient before residual capture
- Results labelled unambiguous as `%` or `dB re fundamental` — never bare numbers

**AES-17-2020** supersede 2015 for any digital signal path. PR touch digital I/O, sampling, or dithering → use 2020 doc.

**IEC 60268-3** govern frequency response + S/N display in `ac-cli`. Check:
- Frequency response referenced to 1 kHz level unless otherwise stated (§12)
- S/N expressed as dB relative to rated output, weighting stated (§14)
- Measurement conditions (source impedance, load impedance) present in output if logged

**IEC 61260-1** apply to any fractional-octave band analysis. Check:
- Filter class (1 or 2) stated in output
- Bandwidth designator follow standard notation (e.g. `1/3-octave`, not `third-octave`)
- Attenuation at band edges meet class requirements

**IEC 61672-1** apply when A-, C-, or Z-weighting used. Check:
- Weighting designator explicit in output label (`dBA`, `dBC`, `dBZ`)
- Time constant stated when time-weighted levels displayed (`F`, `S`, or `I`)

**ITU-R BS.468-4** apply to noise measurements using quasi-peak detection or 468-weighted noise figures. Check:
- Detector type stated (`quasi-peak` vs `RMS`)
- Weighting curve identified in output if not unweighted

**ITU-R BS.1770-5** apply if integrated loudness or true-peak values appear. Check:
- Integrated loudness expressed as `LUFS` (not `LKFS` — both used in wild, LUFS is current preferred term per BS.1770-5 §3)
- True-peak expressed as `dBTP`, not `dBFS`
- Gating behaviour (absolute + relative gates) match §2.7 if implemented

**ISO 18233** apply to swept-sine / deterministic-signal measurement. It is a *substitution* standard — §1 gives methods used "as substitutes for measurement methods specified in standards covering classical methods", and §9(c) require the report name the applicable classical standard. It never stands alone. Check:
- A room measurement cite ISO 18233 **and** the classical standard it substitutes for (ISO 3382-1 or 3382-2). One without the other is incomplete.
- A quasi-anechoic loudspeaker / PA measurement cite **neither** — no classical method in §1's list covers it. That case want IEC 60268-21, which is not held. AES17-2020 A.4.5 + Farina remain its citation.
- Annex B is normative. Clause strings for it must not say "(informative)"; that qualifier belongs to AES17 A.4 and IEC 61260-1 Annex G.

### standards check procedure

Every PR touching output formatting, unit display, or measurement computation:

1. Identify which standard(s) apply to changed code (use table above)
2. Read relevant clause in actual PDF — no memory, no summary above; summaries are orientation, not authoritative
3. Answer: does implementation match standard's requirements for both value computation AND display/labelling format?
4. Cite standard + clause number in review comment, e.g.:
   `AES-17-2015 §6.3: THD+N must be referenced to fundamental level, not total RMS`
5. PR output format differ from standard → flag as correctness issue even if math right — display conformance is part of correctness here

No applicable standard covers changed behaviour → write
`standards check: not applicable — {reason}` in review comment, not omit section.


- PR diff
- PR body (written by dev agent — files touched, test output, open questions)
- Original issue + triage spec comment (acceptance criteria)
- Architect design comment (if present)

Corollary for citations: **cite a section by name, not by line number.** A
  line range is invalidated by the next edit to the file, including the edit
  that adds the citation.
- **Added precision must come from a lookup, not from an inference.** The
  citation corollary above says where a cite should point; this says where the
  precision may come from. A bare `plot.rs:430` is ambiguous but true, and
  `ac-cli/src/commands/plot.rs:430` — inferred from the command name — is
  specific and false, because the code is in
  `ac-daemon/src/handlers/audio/plot.rs` and the CLI file is 212 lines long.
  Resolving an ambiguous cite is worth doing; resolving it from memory of the
  layout is not, and the result is *harder* to catch than the ambiguity it
  replaced, because the failure reads as diligence. Open the file, name the
  section, or leave the cite as it was.
- **Report the sign of an unscored gap.** Where a gap is left unscored — a
  check not run, a case not tested, a value not verified — say which direction
  its error would push a result, or say that the direction is unknown. An
  unscored gap with no stated direction reads as harmless; most are not.
- **A cite that was added is not a cite that was verified.** In a final
  artifact the two look identical: a reference resolved cleanly against the
  tree and one that was wrong and got corrected both appear as correct
  references. Only the second says anything about the draft's reliability, so
  counting additions as corrections inflates the apparent verification rate of
  the source document. When reporting what a verification pass found, separate
  *corrected*, *added*, and *checked and unchanged* — this is the project's own
  harm-statistic discipline turned on the verification process itself.

## document map

Source docs in `stddocs/` at repo root. Read relevant standard before reviewing any PR touching measurement values, output formatting, or display units. No memory — consult document.

**Take the path from the `file` column, never from the standard's issuing body.** The three subdirectories are historical, not semantic: `iec-full/` holds AES17-2020 and one paper alongside the IEC documents, `iso-full/` holds the ISO ones, and several documents sit at `stddocs/` root. `stddocs/Fundamentals_of_modern_audio_measurement.pdf` (root) is the Cabot paper. A file of the same name previously sat at `stddocs/iec-full/` too, but it was a mislabelled copy of IEC 60268-3 — not an edition or variant of the Cabot paper — and has been deleted. If a same-named file ever reappears under `iec-full/`, treat it as suspect and verify against its first page before citing it; don't assume it's the fuller copy. Copy the cell.

### normative standards

| standard | file | applies to |
|---|---|---|
| AES-17-2020 | `stddocs/iec-full/aes17_2020_aes_standard_method_for_digital_audio_engineering_measurement.pdf` | THD+N methodology, notch filter specs, measurement conditions, result expression — digital audio |
| IEC 60268-3:2018 | `stddocs/iec-full/IEC60268-3.pdf` | Sound system equipment — amplifiers: frequency response, S/N, dynamic range |
| IEC 61260-1:2014 | `stddocs/iec-full/IEC61260-1.pdf` | Octave and fractional-octave band filters: bandwidth, ripple, attenuation |
| IEC 61672-1:2013 | `stddocs/iec-full/IEC61672-1.pdf` | Sound level meters: frequency weighting, time weighting, level linearity |
| ITU-R BS.468-4 | `stddocs/ITU-R BS.468-4.pdf` | Noise measurement: quasi-peak detector, 468 weighting curve |
| ITU-R BS.1770-5 | `stddocs/ITU-R BS.1770-5.pdf` | Loudness measurement: K-weighting, integrated loudness (LUFS), true-peak |
| ISO 18233:2006 | `stddocs/iso-full/ISO18233.pdf` | Deterministic-signal (swept-sine) substitution for classical room and building acoustics methods; IR acquisition, SNR, time-invariance, test report |
| ISO 3382-1:2009 | `stddocs/iso-full/ISO3382-1.pdf` | Room acoustic parameters, performance spaces — reverberation time, early/late measures, source/receiver positions, test report |
| ISO 3382-2:2008 | `stddocs/iso-full/ISO3382-2.pdf` | Reverberation time in ordinary rooms — survey / engineering / precision grades, decay evaluation, uncertainty |

### reference reading (non-normative)

Not standards, but hold authoritative derivations + worked examples. Consult when standard text ambiguous or when checking numerical results.

| document | file | useful for |
|---|---|---|
| Metzler — Audio Measurement Handbook 2nd ed. | `stddocs/pdfcoffee.com_audio-measurement-handbook-2nd-ed-2005-bob-metzler-pdf-free.pdf` | Practical measurement procedures, expected value ranges, instrument behaviour |
| Fundamentals of Modern Audio Measurement | `stddocs/Fundamentals_of_modern_audio_measurement.pdf` | Estimator theory, windowing, FFT measurement fundamentals |
| Müller & Massarani 2001 | `stddocs/iec-full/Simultaneous_Measurement_of_Impulse_Response_and_D.pdf` | H1 estimator derivation — primary reference for `ac-core/visualize/transfer.rs` |
