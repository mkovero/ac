### Tier 1 — Reference measurement

### Tier 1 commands

Plain, claim-the-ground names. These are the tools a user reaches for
when they need a number that goes in a report.

- `ac plot` — stepped-sine frequency response
- `ac plot ir` — swept-sine IR measurement (Farina log-sweep, #282)
- `ac plot level` — single-point THD vs. level measurement (future)
- `ac noise` — noise floor per AES17 §6.4.2 (future CLI surface; core landed)
- `ac impedance` — impedance measurement
- `ac calibrate` — calibration workflow


Optimizes for: **reproducibility, standards alignment, report-grade output.**

Properties:
- Implements a published standard where one exists. Modules cite the
  clause (e.g. `// Per IEC 60268-3:2018 §15.12.3`).
- Deterministic given the same input and calibration state.
- Conservative about uncertainty: if a band cannot be resolved at the
  current settings, the report says so rather than interpolating it away.
- Results are structured,
  versioned, archivable, and contain the metadata needed to interpret
  them years later (stimulus parameters, calibration state, standards
  cited, timestamps, DUT notes, sample rate, signal chain).
- Heavy test coverage against reference implementations, published
  datasets, or derived analytic truths.

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
  
## `MeasurementReport` — Tier 1 output format

All Tier 1 commands produce a `MeasurementReport` on completion. This
type is the archival product — the thing you commit to a project
directory or attach to an email.

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
    pub standards:  Vec<StandardsCitation>, // e.g. IEC 60268-3:2018 §15.12.3
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
