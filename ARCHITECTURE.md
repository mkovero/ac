# ac — Architecture

This document records the architectural decision that shapes how features,
commands, and modules in `ac` are organized. It is the reference that
answers "where does this feature belong?" so the question does not have to
be re-argued per feature.

## Core principle: two tiers, one measurement stack

`ac` is a measurement tool. Every number it produces is a measurement. The
split below is **not** "accurate vs. inaccurate" — it is **what each tier
optimizes for when constraints conflict**.

### Tier 1 — Reference measurement

Commands: `ac plot`, `ac sweep` (future Farina IR-based), `ac noise`
(future), `ac level` (future), `ac impedance`, `ac transfer`.

Optimizes for: **reproducibility, standards alignment, report-grade output.**

Properties:
- Implements a published standard where one exists. Modules cite the
  clause (e.g. `// Per IEC 60268-3:2018 §14.12`).
- Deterministic given the same input and calibration state.
- Conservative about uncertainty: if a band cannot be resolved at the
  current settings, the report says so rather than interpolating it away.
- Output is **data first, visualization second**. Results are structured,
  versioned, archivable, and contain the metadata needed to interpret
  them years later (stimulus parameters, calibration state, standards
  cited, timestamps, DUT notes, sample rate, signal chain).
- Heavy test coverage against reference implementations, published
  datasets, or derived analytic truths.

### Tier 2 — Live analysis

Commands: `ac monitor` (defaults to FFT spectrum), `ac monitor cwt`,
`ac monitor cqt` (future), `ac monitor reassigned` (future), `ac tuner`.

Optimizes for: **insight, responsiveness, smooth interactive experience.**

Properties:
- Values shown are real measurements, computed with the same numeric
  rigor (f64 internally, dBFS-calibrated) as Tier 1. A dB value on a
  Tier 2 waterfall is as accurate as the technique allows.
- Where performance or visual fluidity would be compromised by strict
  adherence to a standard's computation method, Tier 2 chooses the
  smoother user experience. Example: the existing CWT display uses
  Morlet wavelets (not IEC 61260 filter shapes) because Morlet gives
  better time-frequency resolution for a given CPU budget.
- Output is **visualization first, extractable data second**. Frames
  are ephemeral, optimized for display update rates.
- Tested for correctness against synthetic signals (a pure tone reads
  the expected dBFS within tolerance), not against compliance masks.

### The seam between tiers

A user reading a value off a Tier 2 display can trust the number for
what it represents. What they cannot do is **cite that number as a
standards-compliant measurement** if the technique itself is not
standards-defined. The CWT waterfall's 2 kHz reading is a real dBFS
value at that scale — it is not "the 1/3-octave band level per
IEC 61260-1 Class 1."

This is the correct boundary: if a report needs a citable per-band
level, the user runs `ac plot` (or the future `ac noise`) with the
Tier 1 filterbank. If they want to see what's happening in a signal
right now, they use `ac monitor cwt`. Both are measurement. Only one
carries a standards citation.

UI surfaces show a small badge on Tier 2 views labeling the technique
("Morlet CWT, σ=12") so the user knows what they are looking at. No
disclaimer about trustworthiness — just factual labeling.

## Module organization

```
ac-core/src/
  measurement/             # Tier 1
    mod.rs
    filterbank.rs          # IEC 61260-1 fractional-octave filterbank
    weighting.rs           # A, C, Z weighting filters
    thd.rs                 # IEC 60268-3 THD / THD+N
    stepped_sine.rs        # ac plot primitives
    sweep.rs               # Farina log-sweep IR deconvolution (future)
    noise.rs               # AES17 noise measurement (future)
    report.rs              # MeasurementReport type, serialization
    report_html.rs         # self-contained HTML renderer (inline CSS + SVG)

  visualize/               # Tier 2
    mod.rs
    spectrum.rs            # Live FFT spectrum (moved from analysis.rs)
    cwt.rs                 # Morlet CWT (moved from cwt.rs)
    cqt.rs                 # Constant-Q transform (future)
    aggregate.rs           # Display-column binning (moved from aggregate.rs)

  shared/                  # Tier 0 — used by both tiers
    mod.rs
    calibration.rs
    conversions.rs
    constants.rs
    generator.rs
    types.rs

  tuner.rs                 # Tier 2 (stays at root for now, can move later)
  visualize/transfer.rs    # Tier 2 — live H1 estimator, display-first
  config.rs                # orthogonal
```

### Transfer — Tier 2 today, Tier 1 TBD

`visualize/transfer.rs` is the live H1 estimator used by `ac transfer`
and `transfer_stream`. It was classified Tier 2 when the two-tier
migration finished (#70): the current implementation streams
magnitude/phase/coherence frames for display and does not produce a
`MeasurementReport`, so it does not meet Tier 1's archival criterion.

A future Tier 1 variant — dedicated stimulus, integration params,
`MeasurementData::TransferFunction`, archived JSON — is a separate
module that would reuse the core H1 math. There is no Tier 1 consumer
today, so that split is deferred; the file will be moved to
`measurement/transfer.rs` (or factored into a shared core) when one
appears.

The directory structure makes the tier of every module legible at the
file-tree level. A new contributor (or Claude Code) opening the repo
sees `measurement/` and `visualize/` and immediately knows where new
code goes.

Migration from the current flat layout is incremental — see the
migration checklist at the end.

## Command naming conventions

### Tier 1 commands

Plain, claim-the-ground names. These are the tools a user reaches for
when they need a number that goes in a report.

- `ac plot` — stepped-sine frequency response
- `ac sweep` — swept-sine IR measurement (future, Farina method)
- `ac noise` — noise floor per AES17 (future)
- `ac level` — single-point level measurement (future)
- `ac impedance` — impedance measurement
- `ac transfer` — transfer function
- `ac calibrate` — calibration workflow

### Tier 2 commands

Namespaced under `monitor`. Reading `ac monitor <something>` tells the
user this is a live, exploratory view.

- `ac monitor` — defaults to FFT spectrum
- `ac monitor spectrum` — explicit form
- `ac monitor cwt` — Morlet wavelet scalogram
- `ac monitor cqt` — constant-Q transform (future)
- `ac monitor reassigned` — reassigned spectrogram (future)
- `ac tuner` — pitch tracker (Tier 2 but pre-existing; keep the name)

The existing `monitor_spectrum` ZMQ command is retained as the transport
layer. The CLI surface adds the `ac monitor <mode>` parsing on top. An
`ac monitor cwt` call sends `monitor_spectrum` with a mode set to `cwt`
(the server already has `analysis_mode` supporting this).

## Wire message conventions

Every published frame carries a tier marker in the `type` field, using
a path-like prefix.

### Tier 1 frames

- `measurement/frequency_response/point`
- `measurement/frequency_response/complete`
- `measurement/impulse_response`
- `measurement/thd`
- `measurement/noise`
- `measurement/report` — the final `MeasurementReport` JSON

### Tier 2 frames

- `visualize/spectrum`
- `visualize/cwt`
- `visualize/cqt`
- `visualize/tuner`

Existing frames (`type: "spectrum"`, `type: "tuner"`, etc.) are aliased
during migration: the server emits both old and new types for one
release cycle, then drops the old. Python test clients and existing UI
code continue to work during the transition.

## `MeasurementReport` — Tier 1 output format

All Tier 1 commands produce a `MeasurementReport` on completion. This
type is the archival product — the thing you commit to a project
directory, attach to an email, or feed to `ds/` for AI diagnostics.

```rust
pub struct MeasurementReport {
    // Provenance
    pub schema_version: u32,          // report format version
    pub ac_version:     String,       // ac git describe output
    pub timestamp_utc:  String,       // ISO 8601
    pub operator:       Option<String>,
    pub dut_notes:      Option<String>,

    // Method
    pub method:     MeasurementMethod, // SteppedSine, SweptSine, Noise, ...
    pub standards:  Vec<StandardsCitation>, // e.g. IEC 60268-3:2018 §14.12
    pub stimulus:   StimulusParams,    // freqs, levels, durations, sweep params
    pub integration: IntegrationParams, // dwell time, cycles, window type

    // Signal chain
    pub sample_rate:  u32,
    pub input_port:   String,
    pub output_port:  String,
    pub calibration:  Option<CalibrationSnapshot>, // what was loaded

    // Results
    pub data:       MeasurementData,   // tagged enum per method
    pub warnings:   Vec<String>,       // e.g. "25 Hz band below minimum dwell"
}
```

Serialization: JSON is canonical. CSV export is provided for tabular
`data` variants (frequency responses, THD sweeps). A future HTML or PDF
report generator reads the JSON and produces presentation output — but
JSON is the source of truth.

Versioning: `schema_version` increments on any breaking change. Old
reports remain readable forever.

## Standards tracked

Tier 1 modules cite the current editions at time of implementation:

- IEC 60268-3 — Amplifier measurements (THD, frequency response)
- IEC 60268-21 — Acoustical / output-based measurements
- IEC 61260-1 — Octave and fractional-octave filters (Class 0/1/2)
- IEC 61672-1 — Sound level meters (weighting, time constants)
- AES17 — Digital audio equipment measurement

When a standard is revised and the revision changes a computation, the
old computation stays available behind a version flag so historical
reports remain reproducible. The default follows the most recent
revision the implementation has been verified against.

### Citation audit workflow

Every Tier 1 module exposes a `citation()` (or `Type::citation()`) fn
returning a `StandardsCitation { standard, clause, verified }`. Handler
code (e.g. `plot.rs`, `sweep_ir`) should always call that fn rather than
inlining the citation — that keeps the source-of-truth in one place and
makes audits trivial to roll out.

`verified: false` is the default for every citation today. Flipping a
citation to `verified: true` requires a cross-check of both `standard`
and `clause` strings against the **published text of the named
standard**, not against secondary sources. The audit workflow is tracked
in issue #72.

## Testing strategy

### Tier 1

- Unit tests against analytic truths (pure tone through a filterbank
  produces expected per-band energy to within tolerance).
- Integration tests against reference implementations where available
  (MATLAB's `octaveFilter`, `pyfilterbank`, published tolerance masks).
- End-to-end tests verify calibration propagates correctly from
  stimulus to report.
- Regression tests lock serialization: a `MeasurementReport` from a
  known input hashes to a known value.

### Tier 2

- Unit tests for correctness of the underlying transform (CWT of a
  tone lands at the right scale with the right magnitude).
- Property tests for robustness (random input does not panic, NaN is
  handled, edge-case sample rates work).
- Performance tests / benchmarks guard against regressions in the
  per-frame cost, since visual fluidity is the tier's objective.

## Decisions this architecture closes

- "Should this new feature be accurate or pretty?" — Wrong framing.
  Ask which tier it belongs to.
- "Should the CWT view use IEC-compliant filters?" — No; it's Tier 2,
  Morlet stays.
- "Can I put a CWT reading in a report?" — Not as a cited measurement.
  If the report needs that data, add a Tier 1 command that produces it.
- "Where does the new fractional-octave code go?" — Tier 1:
  `measurement/filterbank.rs`.
- "Where does the CWT-band-summed fractional-octave visualization go?" —
  Tier 2: `visualize/cwt.rs` as an additional output of that module.
  It does not replace or compete with the Tier 1 filterbank.
- "Do I need to change the existing `monitor_spectrum` wire command?" —
  No; it stays as transport. The tier language lives in `type` fields
  and CLI surface.

## Migration checklist

Each step is independently mergeable. The repo stays green between
steps — nothing is broken en route.

- [ ] Add `measurement/`, `visualize/`, and `shared/` submodules under
      `ac-core/src/` with empty `mod.rs` files. Add to `lib.rs`.
- [ ] Create `shared/` and move `calibration.rs`, `conversions.rs`,
      `constants.rs`, `generator.rs`, `types.rs` under it. Update
      imports. No behavior change.
- [ ] Create `visualize/cwt.rs` by moving existing `cwt.rs`. Update
      imports.
- [ ] Create `visualize/aggregate.rs` by moving existing `aggregate.rs`.
      Update imports.
- [ ] Split `analysis.rs`: the `analyze` function (THD/THDN) moves to
      `measurement/thd.rs`. The `spectrum_only` function moves to
      `visualize/spectrum.rs`. Shared helpers stay in a private module
      accessible to both.
- [ ] Create `measurement/report.rs` with `MeasurementReport` type.
      Wire `ac plot` to produce one on `done`.
- [ ] Add `ac monitor <mode>` CLI parsing. Keep `ac monitor spectrum`
      and `ac monitor cwt` as subcommands; default is spectrum.
- [ ] Add tiered `type` prefixes to wire frames, emitting both old and
      new types during transition. Note a deprecation date in `ZMQ.md`.
- [x] Build `measurement/filterbank.rs` (IEC 61260-1). Class 1 via
      6th-order Butterworth BP SOS, bpo ∈ {1, 3, 6, 12, 24}; emits
      per-band dBFS suitable for `MeasurementData::SpectrumBands`.
- [ ] Integrate the filterbank into `ac plot` as an optional
      per-band-summary output. Tracked in issue #74.
- [x] Build `measurement/sweep.rs` — Farina exponential log-sweep IR
      measurement. Generator + inverse filter + FFT deconvolution + time-
      gated harmonic IR extraction. Populates
      `MeasurementData::ImpulseResponse`.
- [x] Wire `ac sweep ir` to run a Farina measurement end-to-end and emit
      `measurement/impulse_response` + `measurement/report` frames. Fake
      backend supported end-to-end (including integration test); real
      JACK/CPAL `play_and_capture` is follow-up #78.
- [x] Build `measurement/weighting.rs` — IEC 61672-1 A / C / Z
      frequency weighting. Bilinear-mapped biquad cascade, unity gain at
      1 kHz, Class 1 tolerance verified in tests.
- [x] Build `measurement/noise.rs` — AES17 §6.4 idle-channel noise.
      Reports unweighted and A-weighted dBFS over a provided buffer;
      populates `MeasurementData::NoiseResult`. CCIR-468 quasi-peak is
      a follow-up (#76).
- [x] Build `measurement/report_html.rs` — self-contained HTML renderer
      for `MeasurementReport` (inline CSS + inline SVG plot, no external
      assets). Wired to CLI as `ac report <path.json>`; writes sibling
      `.html`. PDF output is a follow-up (#77).
- [ ] Build `visualize/cqt.rs` if/when desired. Purely additive.
- [ ] After one release cycle, drop the legacy `type` names.

## Non-goals of this document

- Does not specify the implementation of any individual measurement
  method. Those get their own design notes as needed.
- Does not mandate a specific report file format beyond "JSON is
  canonical." HTML / PDF generation is a separate concern.
- Does not require immediate migration of all existing code. The
  directory move is the first step; new code gets placed correctly
  from day one; old code moves as it is touched.
