# ac — Architecture

This document records the architectural decision that shapes how features,
commands, and modules in `ac` are organized. It is the reference that
answers "where does this feature belong?" so the question does not have to
be re-argued per feature.

## Core principle: two tiers, one measurement stack

`ac` is a measurement tool. Every number it produces is a measurement. The
split below is **not** "accurate vs. inaccurate" — it is **what each tier
optimizes for when constraints conflict**.

### The seam between tiers

The split is not "Tier 1 = trustworthy, Tier 2 = best-effort." Both
tiers produce trustworthy numbers; the difference is what *claim* the
number can carry.

A Tier 2 value is a real, calibrated measurement at the analysis
technique's bin — exactly as accurate as f64 / dBFS / SPL math allows
within that technique's bin-leakage envelope. What a user cannot do
is **cite that number as a standards-compliant measurement** if the
technique itself is not the one a standard names. The CWT waterfall's
2 kHz reading is a real dB SPL value at that wavelet scale — it is not
"the 1/3-octave band level per IEC 61260-1 Class 1," because the
filter shape isn't IEC's.

This is the correct boundary: if a report needs a citable per-band
level, the user runs `ac plot` (or the future `ac noise`) with the
Tier 1 filterbank. If they want to see what's happening in a signal
right now, they use `ac monitor cwt`. Both are measurement. Only one
carries a standards citation.

## General
Level reference = scalar dBu offset

## Tier documentation

Tier 1 -> docs/architecture/tier1.md
Tier 2 -> docs/architecture/tier2.md
Standards & Citations -> docs/architecture/standards.md

## Module organization

```
ac-core/src/
  measurement/             # Tier 1
    mod.rs
    filterbank.rs          # IEC 61260-1 fractional-octave filterbank
    weighting.rs           # A, C, Z weighting filters
    thd.rs                 # IEC 60268-3 THD / THD+N
    stepped_sine.rs        # ac plot primitives
    sweep/                 # Farina log-sweep IR deconvolution
      mod.rs               #   SweepParams, citations, re-exports
      deconv.rs            #   sweep + inverse filter + FFT convolution
      harmonics.rs         #   linear/harmonic IR gating
      tail_decay.rs        #   ISO 18233 §6.3.2 capture-adequacy check
      gated.rs             #   time-gated quasi-anechoic response
    noise.rs               # AES17 idle-channel noise measurement
    report.rs              # MeasurementReport type, serialization
    report_layout/         # what each section says — shared by both renderers
      sections.rs          #   header, method, stimulus, calibration, environment
      payload.rs           #   per-payload rows, table columns, plot series
      axis.rs              #   log-f / dB domains, gridline steps, tick labels
    report_html/           # self-contained HTML renderer (inline CSS + SVG)
      plot.rs              #   one SVG plot: magnitude and phase
      emit.rs              #   <dl> and <table> emission, escaping
    report_pdf/            # pure-Rust printpdf renderer, paginated A4
      cursor.rs            #   page geometry, pt->mm, pagination
      metrics.rs           #   core-font advance widths; wrap in the drawing face
      plot.rs              #   plot frame, grids, trace

  visualize/               # Tier 2
    mod.rs
    spectrum.rs            # Live FFT spectrum (moved from analysis.rs)
    cwt.rs                 # Morlet CWT
    cqt.rs                 # Constant-Q transform (Brown 1991 / Schörkhuber-Klapuri)
    reassigned.rs          # Auger-Flandrin reassigned spectrogram
    aggregate.rs           # Display-column binning
    fractional_octave.rs   # CWT-band aggregator + 1/N-octave grid
    weighting_curves.rs    # IEC 61672-1 A/C/Z dB-offset table
    time_integration.rs    # EMA fast/slow + Leq accumulator

  shared/                  # Tier 0 — used by both tiers
    mod.rs
    calibration/           # one file per layer — they never compose
      mod.rs               #   the entry + the voltage/SPL derivations
      tau.rs               #   interface latency: conditions, history, #347
      mic_response.rs      #   frequency-response curve
      store.rs             #   cal.json read/modify/write
    conversions.rs
    constants.rs
    generator.rs
    types.rs

  tuner.rs                 # Tier 2 (stays at root for now, can move later)
  visualize/transfer.rs    # Tier 2 — live H1 estimator, display-first
  config.rs                # orthogonal
```

### The display-truth boundary — `ac-scene` vs `ac-view`

The tier split above decides where a *measurement* belongs. A second split
decides where a *displayed* number belongs, and it is the one to apply when
adding anything the operator reads off a screen.

```
ac-scene/     # every number and string the view shows, as plain data:
              # trace geometry, axis ticks, readout strings, fault state.
              # No egui, no wgpu, no ZMQ — enforced by its dependency list.
ac-view/      # egui/eframe shell. Paints ac-scene's output and handles keys.
              # No numeric computation of its own.
```

The rule: **if a value can be wrong, it belongs in `ac-scene`, where a test
can assert it without a window.** `ac-view` may decide where a glyph lands,
never what it says. A layout constant that has to clear real glyphs is tested
by composition in `ac-view` (`tests/it_banner_clearance.rs`) rather than by
asserting the constant, because asserting the constant passes on any value.

## Calibration — three orthogonal layers

Per-channel `CalibrationEntry` (in `shared/calibration/`, persisted to
`~/.config/ac/cal.json`) carries three independently-applicable
corrections. They compose: a fully-cal'd channel reads an absolute
SPL value with mic-curve compensation and the analog-domain Vrms /
dBu / dBV alongside.

| Layer | Stored field | Source | Affects |
|-------|--------------|--------|---------|
| Voltage | `vrms_at_0dbfs_in` / `vrms_at_0dbfs_out` | `ac calibrate` (sine + DMM) | dBu / dBV readouts |
| Absolute SPL | `mic_sensitivity_dbfs_at_94db_spl` | `ac calibrate spl` (94 dB pistonphone) | adds `spl_offset_db = 94 − captured_dbfs` to dBFS readouts so they read as **dB SPL** |
| Mic curve | `mic_response { freqs_hz, gain_db, … }` | `ac calibrate mic-curve <path>` (.frd / .txt) | per-bin frequency-response correction subtracted from spectrum before frame emission |

The voltage and SPL layers are pure dB offsets; the mic-curve layer is
a per-frequency dB array applied by the daemon to every monitor frame
type (`visualize/spectrum` / `cwt` / `cqt` / `reassigned` and the
`fractional_octave[_leq]` sidecars). All three load from the same
`(output_channel, input_channel)`-keyed entry; saving any one layer
preserves the others via `Calibration::load_or_new`.

Loudness (BS.1770-5 / R128) composes on top: when SPL cal is set, the
M / S / I LKFS readouts surface as K-weighted dB SPL (`Mk` / `Sk` /
`Ik` with `dB SPL` unit) while the R128 PASS / WARN / FAIL badge
stays anchored on the raw integrated LKFS (the `-23 LKFS` target is
independent of the absolute reference).