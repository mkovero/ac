//! `MeasurementReport` — the Tier 1 archival artifact emitted by
//! reproducible measurement commands (`ac plot`, `ac plot ir`, future
//! `ac noise`). Serialises to self-describing JSON for archiving and
//! to CSV for spreadsheet tools.
//!
//! Schema is explicitly versioned via [`SCHEMA_VERSION`]; readers
//! that see an unknown version must refuse to decode. See
//! `ARCHITECTURE.md` for the tiered model.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Current schema version. Bumped on any breaking field change.
///
/// History:
/// - v1: original schema (pre-#94).
/// - v2: SPL field + mic-curve provenance on `CalibrationSnapshot` (#94).
/// - v3: `processing_chain` records the active overlay state at
///   capture time (#105). Field defaults to "all-off" so v1/v2
///   reports still decode under the current struct.
/// - v4: `data` becomes `Vec<MeasurementPayload>` — a single capture
///   (e.g. a Farina sweep) can yield an impulse response, a gated
///   frequency response, and gated band levels, and each is now its
///   own payload with its own `standard` citation(s) and optional
///   `gate` block, instead of one `data` object per report.
///   `MeasurementMethod` drops `standard`: it describes the stimulus
///   shape (what was played), not what a derived payload means — that
///   citation now lives on the payload it applies to. New optional
///   `position: PositionSnapshot` records temperature, relative
///   humidity, source/receiver height and distance. `IntegrationParams`
///   gains optional `n_averages` (ISO 18233 §9 method-description
///   item), unpopulated until a call site averages repeat captures.
///   Legacy v1/v2/v3 reports (where `data` is a bare object) still
///   decode: the object is wrapped into a single-element payload vec
///   with no citation and no gate (#280).
/// - v5: new optional `interface_latency: InterfaceLatency` records the
///   τ resolved for the capture — or the named reason none applied.
///   Without it an archived arrival can never be converted to a path
///   length, since τ is a property of the *(device, backend, sample
///   rate, period size, port pair)* tuple and is not recoverable from
///   the report otherwise (#283, consuming #281). Reports written at
///   v1-v4 decode unchanged: the field defaults to `None`, which
///   `ir_arrival_distance` reports as unavailable rather than deriving
///   a distance from an uncorrected arrival.
pub const SCHEMA_VERSION: u32 = 5;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct MeasurementReport {
    pub schema_version: u32,
    pub ac_version: String,
    pub timestamp_utc: String,
    pub method: MeasurementMethod,
    pub stimulus: StimulusParams,
    pub integration: IntegrationParams,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub calibration: Option<CalibrationSnapshot>,
    /// Environment + geometry captured with the report (#280): the
    /// knowable subset of ISO 3382-1 §9.2 / 3382-2 §9.2 a daemon can
    /// record without modelling the room. `None` when nothing was
    /// captured (no temperature configured, no operator-entered
    /// geometry).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<PositionSnapshot>,
    /// Interface round-trip latency (τ) resolved for the capture that
    /// produced this report (#281 measures it, #283 is its first
    /// consumer). Carried in the archive because it is what turns a
    /// recorded arrival into a path length: without it, a reader a year
    /// later has an arrival that can never be converted to a distance.
    /// `None` on reports written before v5 and on captures where τ was
    /// never looked up at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface_latency: Option<InterfaceLatency>,
    #[serde(deserialize_with = "deserialize_data_payloads")]
    pub data: Vec<MeasurementPayload>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// Active overlay / processing state at capture time (#105). Lets
    /// a year-later reader tell whether the values reflect smoothing,
    /// weighting, time integration, or mic-correction. Defaults to
    /// "all-off" so legacy `schema_version: 1`/`2` reports still
    /// decode without the field present.
    #[serde(default)]
    pub processing_chain: ProcessingChain,
}

/// Accepts either the v4 shape (`data` is a JSON array of
/// `MeasurementPayload`) or the v1/v2/v3 shape (`data` is a single
/// tagged `MeasurementData` object). A legacy object is wrapped into a
/// one-element vec with no citation and no gate — the citation that
/// used to live on `method.standard` is not migrated, since it was
/// already misplaced there (see #280); a year-later reader of a v1-v3
/// archive gets the payload back, just without a moved-over citation.
fn deserialize_data_payloads<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<MeasurementPayload>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    if value.is_array() {
        serde_json::from_value(value).map_err(serde::de::Error::custom)
    } else {
        let data: MeasurementData =
            serde_json::from_value(value).map_err(serde::de::Error::custom)?;
        Ok(vec![MeasurementPayload {
            data,
            standard: Vec::new(),
            gate: None,
        }])
    }
}

/// One derived result from a single capture, paired with the
/// citation(s) and gate parameters (if any) that describe *that
/// payload* specifically — not the whole report. A Farina capture
/// naturally yields an impulse response, a gated frequency response,
/// and gated band levels from one run; each is its own payload with
/// its own provenance (#280).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct MeasurementPayload {
    pub data: MeasurementData,
    /// Citation(s) this payload is measured against, in
    /// citation-relevance order (foundational method first — e.g.
    /// ISO 18233 before a classical room standard). Empty when no
    /// standard applies (e.g. a raw quasi-anechoic capture) — omitted
    /// from JSON entirely rather than serialised as `[]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub standard: Vec<StandardsCitation>,
    /// Present only when this payload was derived by gating an
    /// impulse response into a quasi-anechoic result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<GateParams>,
}

/// Gate parameters applied when a payload is derived by windowing an
/// IR into a quasi-anechoic frequency response or band levels. A
/// gated result is a *different number* depending on gate start,
/// length, and window shape — recording all three (plus the value
/// they imply) is what makes the payload reproducible from the
/// archive alone (#280).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct GateParams {
    pub gate_start_s: f64,
    pub gate_length_s: f64,
    pub window_kind: String,
    /// Lower frequency limit implied by the gate length
    /// (`f_low_hz = 1 / gate_length_s`). Stored, not left for the
    /// reader to recompute.
    pub f_low_hz: f64,
}

/// Interface round-trip latency (τ) as resolved for one capture. Either
/// a τ measured under this run's exact conditions, or a statement of why
/// none applies — never a nearest-match or interpolated value, matching
/// [`crate::shared::calibration::Calibration::tau_for`]'s refusal rule
/// (#281). Both cases are archived: "no τ" is itself provenance a reader
/// needs, and is what stops a distance being derived downstream (#283).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum InterfaceLatency {
    Measured(MeasuredLatency),
    /// `reason` is the operator-facing text from
    /// [`crate::shared::calibration::TauRefusal::message`], which names
    /// the differing condition fields rather than asserting a cause.
    Unavailable {
        reason: String,
    },
}

/// A τ that matched this capture's conditions exactly. The conditions are
/// flattened in rather than referenced, so the report stays readable
/// without `cal.json` beside it — the same self-containment rule
/// [`CalibrationSnapshot`] follows.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct MeasuredLatency {
    pub tau_s: f64,
    /// RFC3339 timestamp of the τ measurement — not of this capture.
    pub measured_at: String,
    /// How τ was measured, e.g. `"farina_short_ess"`.
    pub method: String,
    pub backend: String,
    pub sample_rate_hz: u32,
    /// `None` means the backend cannot report one, not "unknown" — see
    /// [`crate::shared::calibration::TauConditions::period_size`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period_size: Option<u32>,
    pub output_port: String,
    pub input_port: String,
}

/// Environment + geometry captured with a report (#280): the knowable
/// subset of ISO 3382-1 §9.2 / 3382-2 §9.2 a daemon can record on its
/// own or via free-form operator entry — no room-acoustic parameter,
/// sketch, volume, seating, occupancy, curtain state, or stage
/// furnishing (a daemon cannot know any of that; see #280 out of
/// scope). All fields optional and independently present/absent.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct PositionSnapshot {
    /// Ambient temperature in °C — the same value that feeds
    /// `speed_of_sound_from_config` (`Config.temperature_c`), flowed
    /// into the report rather than captured a second time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature_c: Option<f64>,
    /// Relative humidity in percent. No sensor path exists in `ac`
    /// today — this is a free-form operator-entered value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relative_humidity_pct: Option<f64>,
    /// Source (loudspeaker) height above the floor, metres.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_height_m: Option<f64>,
    /// Receiver (microphone) height above the floor, metres.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver_height_m: Option<f64>,
    /// Source-to-receiver distance, metres.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distance_m: Option<f64>,
}

/// Overlay / processing state recorded with a `MeasurementReport` so a
/// re-loaded report can tell which corrections were active during
/// capture. Matches the keys Tier 1 wire frames carry under #98 — the
/// snapshot is the archival counterpart of that envelope.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ProcessingChain {
    /// Active band-weighting curve: `"off"`, `"a"`, `"c"`, or `"z"`.
    pub weighting: String,
    /// Fractional-octave smoothing in bins per octave when active;
    /// `None` means no smoothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smoothing_bpo: Option<u32>,
    /// Active time-integration mode: `"off"`, `"fast"`, `"slow"`,
    /// `"leq"`.
    pub time_integration: String,
    /// Was the per-channel mic-curve correction applied to the data
    /// in this report? When `true`, callers can interpret the
    /// values as the true acoustic level the mic was capturing.
    pub mic_correction_applied: bool,
}

impl Default for ProcessingChain {
    fn default() -> Self {
        Self {
            weighting: "off".into(),
            smoothing_bpo: None,
            time_integration: "off".into(),
            mic_correction_applied: false,
        }
    }
}

/// The measurement technique. `kind` is a discriminant so new methods
/// (Farina sweep, pink-noise, etc.) extend the enum without breaking
/// existing readers. Describes only the *stimulus shape* — what was
/// played — never what a derived result means; a citation for what a
/// payload was measured against lives on [`MeasurementPayload::standard`]
/// instead (#280).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MeasurementMethod {
    /// Discrete-frequency stepped-sine sweep — one tone per bin, fundamental
    /// analyzed in isolation (`measurement::thd::analyze`). Used by `plot`.
    SteppedSine { n_points: usize },
    /// Continuous log-swept sine (Farina ESS) — stimulus is a single
    /// exponential sweep from `f1_hz` to `f2_hz` over `duration_s`; the
    /// captured response is processed by deconvolution or a fractional-
    /// octave filterbank. Used by `plot_ir`.
    SweptSine {
        f1_hz: f64,
        f2_hz: f64,
        duration_s: f64,
    },
}

/// Pointer to a published standard clause.
///
/// `verified: false` is the default: the citation is declarative, not
/// audited. Downstream readers that care about provenance (lab reports,
/// archival tools) should display "unverified" or equivalent unless the
/// field is `true`.
///
/// Flipping `verified: true` requires a human cross-check of both
/// `standard` and `clause` against the **published text** of the named
/// standard — not against secondary sources. Once verified against a
/// specific edition, the clause number and field names are expected to
/// remain stable for the lifetime of that edition. See issue #72 for the
/// audit workflow.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct StandardsCitation {
    pub standard: String,
    pub clause: String,
    #[serde(default)]
    pub verified: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct StimulusParams {
    pub sample_rate_hz: u32,
    pub f_start_hz: f64,
    pub f_stop_hz: f64,
    pub level_dbfs: f64,
    pub n_points: usize,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct IntegrationParams {
    pub duration_s: f64,
    pub window: String,
    /// Number of averages taken to produce the reported result — the
    /// third ISO 18233 §9 method-description item (signal type, signal
    /// duration, number of averages). No call site sets this today (no
    /// `ac` command averages repeat captures yet); `None` rather than a
    /// misleading `1` when the capture path doesn't exist (#280).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n_averages: Option<u32>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct CalibrationSnapshot {
    pub output_channel: u32,
    pub input_channel: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vrms_at_0dbfs_out: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vrms_at_0dbfs_in: Option<f64>,
    pub ref_freq_hz: f64,
    pub ref_level_dbfs: f64,
    /// Pistonphone SPL cal (94 dB ref) when present at capture time.
    /// `None` on uncalibrated channels and on legacy `schema_version: 1`
    /// reports (the field defaults to absent). When set, downstream
    /// readers can convert any dBFS value in the report to dB SPL via
    /// `dbspl = dbfs - mic_sens_dbfs + 94.0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mic_sensitivity_dbfs_at_94db_spl: Option<f64>,
    /// Mic frequency-response correction provenance — NOT the full
    /// curve. The curve itself stays in `cal.json`; the report records
    /// enough to identify which curve was active when the measurement
    /// was taken (so a year-later reader can tell whether the points
    /// they're looking at were mic-corrected, and against which file).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mic_response: Option<MicResponseRef>,
}

/// Pointer-style record of a mic-response curve attached to a channel
/// when a measurement was captured. Keeps reports small (the full
/// curve is many KB of `(freq, gain)` pairs) while preserving the
/// information a reader needs: how many points it had, where it came
/// from, and when it was imported.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct MicResponseRef {
    /// Number of `(freq, gain)` points in the curve at capture time.
    pub n_points: usize,
    /// Original `.frd` / `.txt` path the curve was imported from, when
    /// the user provided one. Informational only — the curve itself is
    /// in `cal.json`, not at this path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    /// RFC3339 timestamp the curve was imported into `cal.json`.
    pub imported_at: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MeasurementData {
    FrequencyResponse {
        points: Vec<FrequencyResponsePoint>,
    },
    /// IEC 61260-1 fractional-octave band levels — output of the Tier 1
    /// filterbank in `measurement/filterbank.rs`.
    SpectrumBands {
        bpo: u32,
        class: String,
        centres_hz: Vec<f64>,
        levels_dbfs: Vec<f64>,
    },
    /// Farina exponential-sweep impulse response — output of
    /// `measurement/sweep.rs`. The `linear_ir` is the deconvolved linear
    /// IR with the peak placed at `linear_ir.len() / 2`; each entry of
    /// `harmonics` is a pre-impulse-gated k-th-order harmonic IR.
    ImpulseResponse {
        sample_rate_hz: u32,
        f1_hz: f64,
        f2_hz: f64,
        duration_s: f64,
        linear_ir: Vec<f64>,
        harmonics: Vec<crate::measurement::sweep::HarmonicIr>,
        /// The instant, seconds after the linear-IR peak, past which the
        /// captured deconvolution can only carry linear-deconvolution
        /// noise, never real system response — see
        /// [`crate::measurement::sweep::noise_tail_start_s`] (#284).
        /// `None` on reports written before this field existed and on
        /// captures where it was never computed (e.g. no tail captured).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        noise_tail_start_s: Option<f64>,
    },
    /// AES17 idle-channel noise — output of `measurement/noise.rs`.
    /// `ccir_weighted_dbfs` is the ITU-R BS.468-4 weighted quasi-peak
    /// level (see `measurement/ccir468.rs`); the field is kept `Option`
    /// for backward compatibility with reports produced before the CCIR
    /// detector landed.
    NoiseResult {
        sample_rate_hz: u32,
        duration_s: f64,
        unweighted_dbfs: f64,
        a_weighted_dbfs: f64,
        #[serde(skip_serializing_if = "Option::is_none")]
        ccir_weighted_dbfs: Option<f64>,
    },
    /// Quasi-anechoic frequency response derived by time-gating and FFTing
    /// the linear IR from a Farina sweep — output of
    /// `measurement::sweep::gated_frequency_response` (#284). A distinct
    /// producer from the stepped-sine `FrequencyResponse` above: no
    /// per-point THD (the gate+FFT path carries no harmonic analysis) and
    /// a `phase_deg` the stepped-sine path never measures.
    GatedFrequencyResponse {
        points: Vec<GatedFrequencyResponsePoint>,
    },
}

impl MeasurementData {
    /// Short machine-stable label for the variant — matches the
    /// serde `kind` tag. Used by `to_csv`'s per-payload comment header
    /// and by the HTML/PDF renderers.
    pub fn kind_label(&self) -> &'static str {
        match self {
            MeasurementData::FrequencyResponse { .. } => "frequency_response",
            MeasurementData::SpectrumBands { .. } => "spectrum_bands",
            MeasurementData::ImpulseResponse { .. } => "impulse_response",
            MeasurementData::NoiseResult { .. } => "noise_result",
            MeasurementData::GatedFrequencyResponse { .. } => "gated_frequency_response",
        }
    }

    fn write_csv(&self, s: &mut String) -> Result<()> {
        match self {
            MeasurementData::FrequencyResponse { points } => {
                writeln!(
                    s,
                    "freq_hz,fundamental_dbfs,thd_pct,thdn_pct,noise_floor_dbfs,linear_rms,clipping,ac_coupled"
                )?;
                for p in points {
                    writeln!(
                        s,
                        "{:.6},{:.6},{:.6},{:.6},{:.6},{:.9},{},{}",
                        p.freq_hz,
                        p.fundamental_dbfs,
                        p.thd_pct,
                        p.thdn_pct,
                        p.noise_floor_dbfs,
                        p.linear_rms,
                        p.clipping,
                        p.ac_coupled,
                    )?;
                }
            }
            MeasurementData::SpectrumBands {
                bpo,
                class,
                centres_hz,
                levels_dbfs,
            } => {
                writeln!(s, "centre_hz,level_dbfs,bpo,class")?;
                for (c, l) in centres_hz.iter().zip(levels_dbfs.iter()) {
                    writeln!(s, "{:.6},{:.6},{},{}", c, l, bpo, class)?;
                }
            }
            MeasurementData::ImpulseResponse {
                sample_rate_hz,
                linear_ir,
                harmonics,
                ..
            } => {
                writeln!(s, "sample_idx,time_s,order,amplitude")?;
                let fs = *sample_rate_hz as f64;
                for (i, v) in linear_ir.iter().enumerate() {
                    writeln!(s, "{},{:.9},1,{:.9}", i, i as f64 / fs, v)?;
                }
                for h in harmonics {
                    for (i, v) in h.samples.iter().enumerate() {
                        writeln!(s, "{},{:.9},{},{:.9}", i, i as f64 / fs, h.order, v)?;
                    }
                }
            }
            MeasurementData::NoiseResult {
                sample_rate_hz,
                duration_s,
                unweighted_dbfs,
                a_weighted_dbfs,
                ccir_weighted_dbfs,
            } => {
                writeln!(
                    s,
                    "sample_rate_hz,duration_s,unweighted_dbfs,a_weighted_dbfs,ccir_weighted_dbfs"
                )?;
                let ccir = ccir_weighted_dbfs
                    .map(|v| format!("{v:.6}"))
                    .unwrap_or_default();
                writeln!(
                    s,
                    "{},{:.6},{:.6},{:.6},{}",
                    sample_rate_hz, duration_s, unweighted_dbfs, a_weighted_dbfs, ccir,
                )?;
            }
            MeasurementData::GatedFrequencyResponse { points } => {
                writeln!(s, "freq_hz,magnitude_db,phase_deg")?;
                for p in points {
                    writeln!(
                        s,
                        "{:.6},{:.6},{:.4}",
                        p.freq_hz, p.magnitude_db, p.phase_deg
                    )?;
                }
            }
        }
        Ok(())
    }
}

/// One point of a [`MeasurementData::GatedFrequencyResponse`] payload —
/// see `measurement::sweep::GatedResponsePoint`, which this mirrors for
/// archival (#284).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct GatedFrequencyResponsePoint {
    pub freq_hz: f64,
    pub magnitude_db: f64,
    /// Wrapped `atan2` phase in degrees — see
    /// `measurement::sweep::GatedResponsePoint::phase_deg`.
    pub phase_deg: f64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct FrequencyResponsePoint {
    pub freq_hz: f64,
    pub fundamental_dbfs: f64,
    pub thd_pct: f64,
    pub thdn_pct: f64,
    pub noise_floor_dbfs: f64,
    pub linear_rms: f64,
    #[serde(default)]
    pub clipping: bool,
    #[serde(default)]
    pub ac_coupled: bool,
}

impl MeasurementReport {
    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(self).context("encode MeasurementReport as JSON")
    }

    /// Flat CSV of the report's data payloads. One block per payload,
    /// separated by a blank line and a `# payload N: <kind>` comment
    /// row (with the gate summary appended when the payload carries
    /// one) — two payloads never collapse into one table, and tooling
    /// that wants a single clean table can `grep -v '^#'` or split on
    /// the blank line. The header and column set within a block depend
    /// on that payload's `MeasurementData` variant.
    pub fn to_csv(&self) -> Result<String> {
        let mut s = String::new();
        for (i, payload) in self.data.iter().enumerate() {
            if i > 0 {
                writeln!(s)?;
            }
            match &payload.gate {
                Some(g) => writeln!(
                    s,
                    "# payload {}: {}  gate={:.1}ms+{:.1}ms {} f_low={:.1}Hz",
                    i + 1,
                    payload.data.kind_label(),
                    g.gate_start_s * 1000.0,
                    g.gate_length_s * 1000.0,
                    g.window_kind,
                    g.f_low_hz,
                )?,
                None => writeln!(s, "# payload {}: {}", i + 1, payload.data.kind_label())?,
            }
            payload.data.write_csv(&mut s)?;
        }
        Ok(s)
    }

    pub fn write_to(&self, path: &Path) -> Result<()> {
        let json = self.to_json()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
        fs::write(path, json).with_context(|| format!("write {}", path.display()))
    }

    /// Derived read-out quantities for the report's first
    /// `ImpulseResponse` payload: arrival timing, peak magnitude,
    /// pre-impulse SNR, and the time gate's low-frequency limit. `None`
    /// when no payload carries an impulse response, or its linear IR is
    /// empty (see issue #283).
    ///
    /// Arrival (`delay_samples`/`arrival_s`) is derived from an onset
    /// estimate ([`crate::measurement::sweep::estimate_onset`]), not from
    /// the IR's magnitude peak — see `IrStats::onset_index`'s doc for why
    /// (#346). When this report carries both a measured interface latency
    /// and a recorded `position.distance_m`, the onset estimate is bound
    /// to reject any candidate earlier than pure flight time allows.
    pub fn ir_stats(&self) -> Option<IrStats> {
        let payload = self
            .data
            .iter()
            .find(|p| matches!(p.data, MeasurementData::ImpulseResponse { .. }))?;
        let MeasurementData::ImpulseResponse {
            sample_rate_hz,
            linear_ir,
            ..
        } = &payload.data
        else {
            return None;
        };
        if linear_ir.is_empty() || *sample_rate_hz == 0 {
            return None;
        }
        let window_len = linear_ir.len();
        let (peak_index, peak_magnitude) =
            linear_ir
                .iter()
                .enumerate()
                .fold((0usize, 0.0_f64), |acc, (i, &v)| {
                    let m = v.abs();
                    if m > acc.1 {
                        (i, m)
                    } else {
                        acc
                    }
                });

        // `extract_irs` (`measurement::sweep`) centres the gate at the
        // sweep endpoint — the position an identity (zero-delay) system
        // would peak at.
        let centre = window_len / 2;

        // Pre-impulse noise floor: everything strictly before the peak,
        // minus a small guard band so the peak's own skirt doesn't bias
        // the floor estimate upward. Computed before the onset estimate
        // below, which is scaled off this same floor rather than a
        // percent of the peak (#346).
        let guard = (window_len / 32).max(8);
        let pre_end = peak_index.saturating_sub(guard);
        let pre_region = &linear_ir[..pre_end];
        let floor_rms = if pre_region.is_empty() {
            0.0
        } else {
            let mean_sq = pre_region.iter().map(|v| v * v).sum::<f64>() / pre_region.len() as f64;
            mean_sq.sqrt()
        };
        let pre_impulse_snr_db = if pre_region.is_empty() {
            f64::INFINITY
        } else if floor_rms > 0.0 {
            20.0 * (peak_magnitude / floor_rms).log10()
        } else {
            f64::INFINITY
        };

        // The earliest sample known geometry admits as an onset (pure
        // flight time, converted to a sample index) — only computable
        // when both a measured τ and a recorded distance are present for
        // this capture. `None` otherwise, and `estimate_onset`'s `rule`
        // says so (#346 architect review, option A).
        let min_admissible_index = match (
            &self.interface_latency,
            self.position.as_ref().and_then(|p| p.distance_m),
        ) {
            (Some(InterfaceLatency::Measured(tau)), Some(distance_m)) => {
                let c = crate::shared::conversions::speed_of_sound_from_config(
                    self.position.as_ref().and_then(|p| p.temperature_c),
                );
                let bound_offset = (tau.tau_s + distance_m / c) * *sample_rate_hz as f64;
                Some((centre as f64 + bound_offset).round().max(0.0) as usize)
            }
            _ => None,
        };

        let onset = crate::measurement::sweep::estimate_onset(
            linear_ir,
            peak_index,
            floor_rms,
            min_admissible_index,
        );
        let onset_index = onset.index;
        let onset_rule = onset.rule;

        // The onset's offset from the window centre is the arrival —
        // not `peak_index`'s (#346): on a multi-way loudspeaker the
        // largest sample sits at a fixed group-delay offset past the
        // wavefront that actually left the baffle first.
        let delay_samples = onset_index as i64 - centre as i64;
        let arrival_s = delay_samples as f64 / *sample_rate_hz as f64;

        // Prefer the gate the producer actually applied. #280 stores
        // `f_low_hz` on the payload precisely so a reader does not
        // recompute it; falling back to `window_len / sample_rate_hz`
        // only covers legacy (v1-v3) reports, where no gate was recorded
        // and the rectangular `extract_irs` window is the only gate that
        // could have produced this payload.
        let (gate_window_s, gate_f_low_hz, gate_window_kind) = match &payload.gate {
            Some(g) => (g.gate_length_s, g.f_low_hz, g.window_kind.clone()),
            None => {
                let len_s = window_len as f64 / *sample_rate_hz as f64;
                (len_s, 1.0 / len_s, "rectangular (not recorded)".to_string())
            }
        };

        Some(IrStats {
            sample_rate_hz: *sample_rate_hz,
            window_len,
            peak_index,
            peak_magnitude,
            onset_index,
            onset_rule,
            delay_samples,
            arrival_s,
            pre_impulse_snr_db,
            gate_window_s,
            gate_f_low_hz,
            gate_window_kind,
        })
    }

    /// Arrival converted to an acoustic path length, in metres, or the
    /// reason it cannot be.
    ///
    /// The arrival [`IrStats::arrival_s`] reports is a *round-trip* figure:
    /// it still contains the interface's own latency (τ). Subtracting a τ
    /// measured under this run's exact conditions is the only thing that
    /// turns it into a path length, so this returns
    /// [`ArrivalDistance::Unavailable`] — never a number — when the report
    /// carries no τ. Speed of sound comes from `position.temperature_c`
    /// when the capture recorded one, else the 20 °C default (see
    /// [`crate::shared::conversions::speed_of_sound_from_config`]).
    pub fn ir_arrival_distance(&self) -> ArrivalDistance {
        let Some(stats) = self.ir_stats() else {
            return ArrivalDistance::Unavailable {
                reason: "report carries no impulse-response payload".to_string(),
            };
        };
        let tau = match &self.interface_latency {
            Some(InterfaceLatency::Measured(m)) => m,
            Some(InterfaceLatency::Unavailable { reason }) => {
                return ArrivalDistance::Unavailable {
                    reason: reason.clone(),
                }
            }
            None => {
                return ArrivalDistance::Unavailable {
                    reason: "no interface latency (\u{3c4}) recorded with this measurement"
                        .to_string(),
                }
            }
        };
        let c = crate::shared::conversions::speed_of_sound_from_config(
            self.position.as_ref().and_then(|p| p.temperature_c),
        );
        ArrivalDistance::Known {
            distance_m: (stats.arrival_s - tau.tau_s) * c,
            speed_of_sound_m_s: c,
            tau_s: tau.tau_s,
            provenance: m_provenance(tau),
        }
    }
}

/// One-line provenance for a τ used in a distance derivation: when it was
/// measured, by what method, and under which conditions. Named in full
/// because a distance derived from someone else's τ is a different number
/// — see [`crate::shared::calibration::TauConditions`].
fn m_provenance(m: &MeasuredLatency) -> String {
    format!(
        "\u{3c4} = {:.4} ms, measured {} by {} on {} backend @ {} Hz, period {}, {} \u{2192} {}",
        m.tau_s * 1000.0,
        m.measured_at,
        m.method,
        m.backend,
        m.sample_rate_hz,
        m.period_size
            .map(|p| p.to_string())
            .unwrap_or_else(|| "n/a".to_string()),
        m.output_port,
        m.input_port,
    )
}

/// Result of converting an IR arrival into an acoustic path length. A
/// distance is either derived from a named τ or explicitly unavailable —
/// there is deliberately no third case that returns an uncorrected
/// arrival as if it were a distance (#283).
#[derive(Debug, Clone, PartialEq)]
pub enum ArrivalDistance {
    Known {
        distance_m: f64,
        speed_of_sound_m_s: f64,
        tau_s: f64,
        /// Human-readable τ provenance, for printing next to the number.
        provenance: String,
    },
    Unavailable {
        reason: String,
    },
}

/// See [`MeasurementReport::ir_stats`].
#[derive(Debug, Clone, PartialEq)]
pub struct IrStats {
    pub sample_rate_hz: u32,
    /// Length of the gated linear IR, in samples.
    pub window_len: usize,
    /// Index of the peak-magnitude sample within the gated IR. Kept as a
    /// diagnostic — since #346 this is **not** what `delay_samples` /
    /// `arrival_s` are derived from; on a multi-way loudspeaker the
    /// largest sample sits at a fixed group-delay offset past the actual
    /// wavefront (see [`crate::measurement::sweep::estimate_onset`]).
    pub peak_index: usize,
    /// `|linear_ir[peak_index]|`.
    pub peak_magnitude: f64,
    /// Index of the estimated onset within the gated IR — see
    /// [`crate::measurement::sweep::estimate_onset`]. `delay_samples` and
    /// `arrival_s` are derived from this, not from `peak_index` (#346).
    pub onset_index: usize,
    /// The rule that produced `onset_index`, from
    /// [`crate::measurement::sweep::OnsetEstimate::rule`] — states
    /// whether a causal bound (known geometry) was enforced. Travels with
    /// `arrival_s` so a persisted number can be told apart from a bare
    /// peak read a year later (#346 acceptance criterion 4).
    pub onset_rule: String,
    /// `onset_index - window_len / 2` — signed offset of the estimated
    /// onset from the gate centre, in samples. Positive means the
    /// response arrived after the zero-delay reference position.
    pub delay_samples: i64,
    /// `delay_samples / sample_rate_hz` — arrival time relative to the
    /// gate's zero-delay reference. This is **not** acoustic path delay:
    /// it still contains any uncorrected interface latency, which is why
    /// it must not be converted to a distance without a calibrated τ.
    pub arrival_s: f64,
    /// `20·log10(peak_magnitude / rms(pre-impulse region))`. `+inf` when
    /// no pre-impulse energy was measurable at all (silent floor).
    pub pre_impulse_snr_db: f64,
    /// Gate window duration, in seconds — the recorded
    /// [`GateParams::gate_length_s`] when the payload carries one.
    pub gate_window_s: f64,
    /// The lowest frequency for which one full period fits inside the
    /// gate window. Read from [`GateParams::f_low_hz`] when recorded;
    /// content below it is not reliably resolved by a gate this short.
    pub gate_f_low_hz: f64,
    /// Window shape the gate applied, from [`GateParams::window_kind`].
    /// `"rectangular (not recorded)"` for legacy reports that stored no
    /// gate — an inference from `extract_irs`, flagged as such so a
    /// reader does not mistake it for a recorded value.
    pub gate_window_kind: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_report() -> MeasurementReport {
        MeasurementReport {
            schema_version: SCHEMA_VERSION,
            ac_version: "0.1.0".into(),
            timestamp_utc: "2026-04-21T20:00:00Z".into(),
            method: MeasurementMethod::SteppedSine { n_points: 3 },
            stimulus: StimulusParams {
                sample_rate_hz: 48_000,
                f_start_hz: 100.0,
                f_stop_hz: 10_000.0,
                level_dbfs: -20.0,
                n_points: 3,
            },
            integration: IntegrationParams {
                duration_s: 1.0,
                window: "hann".into(),
                n_averages: None,
            },
            calibration: None,
            position: None,
            interface_latency: None,
            data: vec![MeasurementPayload {
                data: MeasurementData::FrequencyResponse {
                    points: vec![
                        FrequencyResponsePoint {
                            freq_hz: 100.0,
                            fundamental_dbfs: -20.1,
                            thd_pct: 0.005,
                            thdn_pct: 0.012,
                            noise_floor_dbfs: -120.0,
                            linear_rms: 0.0707,
                            clipping: false,
                            ac_coupled: false,
                        },
                        FrequencyResponsePoint {
                            freq_hz: 1_000.0,
                            fundamental_dbfs: -20.05,
                            thd_pct: 0.003,
                            thdn_pct: 0.009,
                            noise_floor_dbfs: -121.3,
                            linear_rms: 0.0707,
                            clipping: false,
                            ac_coupled: false,
                        },
                        FrequencyResponsePoint {
                            freq_hz: 10_000.0,
                            fundamental_dbfs: -20.2,
                            thd_pct: 0.008,
                            thdn_pct: 0.015,
                            noise_floor_dbfs: -119.5,
                            linear_rms: 0.0706,
                            clipping: false,
                            ac_coupled: false,
                        },
                    ],
                },
                standard: vec![crate::measurement::thd::citation()],
                gate: None,
            }],
            notes: None,
            processing_chain: ProcessingChain::default(),
        }
    }

    #[test]
    fn report_serializes_round_trip() {
        let r = sample_report();
        let json = r.to_json().unwrap();
        let r2: MeasurementReport = serde_json::from_str(&json).unwrap();
        assert_eq!(r, r2);
    }

    #[test]
    fn report_csv_is_stable() {
        let r = sample_report();
        let a = r.to_csv().unwrap();
        let b = r.to_csv().unwrap();
        assert_eq!(a, b);
        // Payload comment + header + 3 data lines.
        assert_eq!(a.lines().count(), 5);
        assert!(a.starts_with("# payload 1: frequency_response"));
        assert!(a.contains("freq_hz,fundamental_dbfs,"));
    }

    #[test]
    fn swept_sine_method_round_trip() {
        let mut r = sample_report();
        r.method = MeasurementMethod::SweptSine {
            f1_hz: 20.0,
            f2_hz: 20_000.0,
            duration_s: 3.0,
        };
        let json = r.to_json().unwrap();
        assert!(json.contains("\"kind\": \"swept_sine\""));
        assert!(json.contains("\"f1_hz\": 20.0"));
        assert!(json.contains("\"f2_hz\": 20000.0"));
        assert!(json.contains("\"duration_s\": 3.0"));
        let r2: MeasurementReport = serde_json::from_str(&json).unwrap();
        assert_eq!(r, r2);
    }

    #[test]
    fn method_json_no_longer_carries_standard() {
        // The bug #280 exists to fix: a citation describing a payload
        // must not be representable in the method slot at all.
        let r = sample_report();
        let json = r.to_json().unwrap();
        let method_obj =
            serde_json::from_str::<serde_json::Value>(&json).unwrap()["method"].clone();
        assert!(method_obj.get("standard").is_none(), "{method_obj}");
    }

    #[test]
    fn schema_version_present() {
        let r = sample_report();
        let json = r.to_json().unwrap();
        assert!(json.contains("\"schema_version\": 5"));
    }

    #[test]
    fn deserialize_rejects_wrong_discriminant() {
        // A future reader must see `kind` so it can branch; a payload
        // without `kind` should fail to decode.
        let malformed = r#"{
            "schema_version": 1,
            "ac_version": "0.1.0",
            "timestamp_utc": "2026-04-21T00:00:00Z",
            "method": { "n_points": 1 },
            "stimulus": {"sample_rate_hz":48000,"f_start_hz":100,"f_stop_hz":1000,"level_dbfs":-20,"n_points":1},
            "integration": {"duration_s":1.0,"window":"hann"},
            "data": {"points":[]}
        }"#;
        assert!(serde_json::from_str::<MeasurementReport>(malformed).is_err());
    }

    fn sample_spectrum_bands_report() -> MeasurementReport {
        MeasurementReport {
            schema_version: SCHEMA_VERSION,
            ac_version: "0.1.0".into(),
            timestamp_utc: "2026-04-22T12:00:00Z".into(),
            method: MeasurementMethod::SteppedSine { n_points: 0 },
            stimulus: StimulusParams {
                sample_rate_hz: 48_000,
                f_start_hz: 100.0,
                f_stop_hz: 1000.0,
                level_dbfs: -20.0,
                n_points: 0,
            },
            integration: IntegrationParams {
                duration_s: 1.0,
                window: "none".into(),
                n_averages: None,
            },
            calibration: None,
            position: None,
            interface_latency: None,
            data: vec![MeasurementPayload {
                data: MeasurementData::SpectrumBands {
                    bpo: 3,
                    class: "Class 1".into(),
                    centres_hz: vec![100.0, 125.893, 158.489],
                    levels_dbfs: vec![-30.0, -20.0, -40.0],
                },
                standard: vec![crate::measurement::filterbank::Filterbank::citation()],
                gate: None,
            }],
            notes: None,
            processing_chain: ProcessingChain::default(),
        }
    }

    #[test]
    fn spectrum_bands_round_trip() {
        let r = sample_spectrum_bands_report();
        let json = r.to_json().unwrap();
        let r2: MeasurementReport = serde_json::from_str(&json).unwrap();
        assert_eq!(r, r2);
    }

    #[test]
    fn spectrum_bands_csv_shape() {
        let r = sample_spectrum_bands_report();
        let csv = r.to_csv().unwrap();
        assert!(csv.starts_with("# payload 1: spectrum_bands"));
        assert!(csv.contains("centre_hz,level_dbfs,bpo,class"));
        assert_eq!(csv.lines().count(), 5);
    }

    fn sample_impulse_response_report() -> MeasurementReport {
        use crate::measurement::sweep::HarmonicIr;
        MeasurementReport {
            schema_version: SCHEMA_VERSION,
            ac_version: "0.1.0".into(),
            timestamp_utc: "2026-04-22T12:00:00Z".into(),
            method: MeasurementMethod::SweptSine {
                f1_hz: 20.0,
                f2_hz: 20_000.0,
                duration_s: 1.0,
            },
            stimulus: StimulusParams {
                sample_rate_hz: 48_000,
                f_start_hz: 20.0,
                f_stop_hz: 20_000.0,
                level_dbfs: -6.0,
                n_points: 0,
            },
            integration: IntegrationParams {
                duration_s: 1.0,
                window: "none".into(),
                n_averages: None,
            },
            calibration: None,
            position: None,
            interface_latency: None,
            data: vec![MeasurementPayload {
                data: MeasurementData::ImpulseResponse {
                    sample_rate_hz: 48_000,
                    f1_hz: 20.0,
                    f2_hz: 20_000.0,
                    duration_s: 1.0,
                    linear_ir: vec![0.0, 0.5, 1.0, 0.25, 0.0],
                    noise_tail_start_s: None,
                    harmonics: vec![HarmonicIr {
                        order: 2,
                        samples: vec![0.0, 0.1, 0.2, 0.05, 0.0],
                    }],
                },
                standard: vec![crate::measurement::sweep::citation()],
                gate: None,
            }],
            notes: None,
            processing_chain: ProcessingChain::default(),
        }
    }

    #[test]
    fn impulse_response_round_trip() {
        let r = sample_impulse_response_report();
        let json = r.to_json().unwrap();
        let r2: MeasurementReport = serde_json::from_str(&json).unwrap();
        assert_eq!(r, r2);
    }

    #[test]
    fn impulse_response_csv_shape() {
        let r = sample_impulse_response_report();
        let csv = r.to_csv().unwrap();
        assert!(csv.starts_with("# payload 1: impulse_response"));
        assert!(csv.contains("sample_idx,time_s,order,amplitude"));
        // Payload comment + header + 5 linear rows + 5 harmonic rows.
        assert_eq!(csv.lines().count(), 12);
    }

    // ─── `ir_stats` (#283) ────────────────────────────────────────────

    /// Build an IR report with `window_len` samples, an impulse of
    /// `peak_mag` at `peak_index`, and `noise` amplitude everywhere else
    /// — enough signal shape to exercise `ir_stats` deterministically.
    /// Carries no `gate`, so it also covers the legacy fallback path.
    fn ir_report_with_peak(
        window_len: usize,
        peak_index: usize,
        peak_mag: f64,
        noise: f64,
        sample_rate_hz: u32,
    ) -> MeasurementReport {
        let mut r = sample_impulse_response_report();
        let mut ir = vec![noise; window_len];
        ir[peak_index] = peak_mag;
        r.data = vec![MeasurementPayload {
            data: MeasurementData::ImpulseResponse {
                sample_rate_hz,
                f1_hz: 20.0,
                f2_hz: 20_000.0,
                duration_s: 1.0,
                linear_ir: ir,
                noise_tail_start_s: None,
                harmonics: vec![],
            },
            standard: Vec::new(),
            gate: None,
        }];
        r
    }

    /// Companion to [`ir_report_with_peak`] for tests (#346) that need a
    /// hand-built `linear_ir` shape rather than a single spike over flat
    /// noise — e.g. sustained onset energy distinct from the peak.
    fn ir_report_with_custom_ir(linear_ir: Vec<f64>, sample_rate_hz: u32) -> MeasurementReport {
        let mut r = sample_impulse_response_report();
        r.data = vec![MeasurementPayload {
            data: MeasurementData::ImpulseResponse {
                sample_rate_hz,
                f1_hz: 20.0,
                f2_hz: 20_000.0,
                duration_s: 1.0,
                linear_ir,
                noise_tail_start_s: None,
                harmonics: vec![],
            },
            standard: Vec::new(),
            gate: None,
        }];
        r
    }

    #[test]
    fn ir_stats_reports_delay_samples_relative_to_gate_centre() {
        // Peak 32 samples after the window centre — the fake backend's
        // fixed loopback delay (see `ac-daemon/src/audio/fake.rs`).
        let window_len = 1024;
        let centre = window_len / 2;
        let r = ir_report_with_peak(window_len, centre + 32, 1.0, 0.0, 48_000);
        let stats = r.ir_stats().expect("impulse response data present");
        assert_eq!(stats.delay_samples, 32);
        assert!((stats.arrival_s - 32.0 / 48_000.0).abs() < 1e-12);
        assert_eq!(stats.peak_index, centre + 32);
        assert_eq!(stats.peak_magnitude, 1.0);
    }

    #[test]
    fn ir_stats_delay_is_negative_when_peak_precedes_centre() {
        let window_len = 1024;
        let centre = window_len / 2;
        let r = ir_report_with_peak(window_len, centre - 10, 1.0, 0.0, 48_000);
        let stats = r.ir_stats().unwrap();
        assert_eq!(stats.delay_samples, -10);
        assert!(stats.arrival_s < 0.0);
    }

    #[test]
    fn ir_stats_pre_impulse_snr_reflects_noise_floor() {
        let window_len = 1024;
        let centre = window_len / 2;
        // Peak of 1.0 against a 0.01 floor -> 20*log10(100) = 40 dB.
        let r = ir_report_with_peak(window_len, centre, 1.0, 0.01, 48_000);
        let stats = r.ir_stats().unwrap();
        assert!(
            (stats.pre_impulse_snr_db - 40.0).abs() < 0.5,
            "pre_impulse_snr_db = {}",
            stats.pre_impulse_snr_db
        );
    }

    #[test]
    fn ir_stats_snr_is_infinite_over_true_silence() {
        let window_len = 1024;
        let centre = window_len / 2;
        let r = ir_report_with_peak(window_len, centre, 1.0, 0.0, 48_000);
        let stats = r.ir_stats().unwrap();
        assert!(stats.pre_impulse_snr_db.is_infinite());
    }

    #[test]
    fn ir_stats_falls_back_to_window_duration_when_no_gate_recorded() {
        let r = ir_report_with_peak(4_800, 2_400, 1.0, 0.0, 48_000);
        let stats = r.ir_stats().unwrap();
        // 4800 samples @ 48 kHz = 100 ms window -> f_low = 10 Hz.
        assert!((stats.gate_window_s - 0.1).abs() < 1e-12);
        assert!((stats.gate_f_low_hz - 10.0).abs() < 1e-9);
        // A legacy report's gate is inferred, and must say so — a reader
        // must not take "rectangular" here for a recorded fact.
        assert!(
            stats.gate_window_kind.contains("not recorded"),
            "inferred gate must be flagged: {}",
            stats.gate_window_kind
        );
    }

    /// The recorded gate wins over the `window_len / sample_rate` guess.
    /// This is the case that separates the two: a gate whose recorded
    /// `f_low_hz` and length disagree with what the IR length implies
    /// (a half-length gate on a zero-padded payload) — if `ir_stats`
    /// recomputed instead of reading, it would report 10 Hz, not 20.
    #[test]
    fn ir_stats_prefers_the_recorded_gate_over_the_ir_length() {
        let mut r = ir_report_with_peak(4_800, 2_400, 1.0, 0.0, 48_000);
        r.data[0].gate = Some(GateParams {
            gate_start_s: 0.0,
            gate_length_s: 0.05,
            window_kind: "half-hann".into(),
            f_low_hz: 20.0,
        });
        let stats = r.ir_stats().unwrap();
        assert!((stats.gate_window_s - 0.05).abs() < 1e-12);
        assert!((stats.gate_f_low_hz - 20.0).abs() < 1e-9);
        assert_eq!(stats.gate_window_kind, "half-hann");
    }

    #[test]
    fn ir_stats_none_for_non_impulse_response_report() {
        let r = sample_report(); // FrequencyResponse variant
        assert!(r.ir_stats().is_none());
    }

    /// A Farina capture emits several payloads; the impulse response is
    /// not necessarily first. `ir_stats` must find it rather than read
    /// `data[0]` and give up.
    #[test]
    fn ir_stats_finds_the_ir_payload_behind_another_payload() {
        let mut r = ir_report_with_peak(1_024, 1_024 / 2 + 32, 1.0, 0.0, 48_000);
        let ir_payload = r.data.remove(0);
        r.data = vec![
            MeasurementPayload {
                data: MeasurementData::FrequencyResponse { points: vec![] },
                standard: Vec::new(),
                gate: None,
            },
            ir_payload,
        ];
        assert_eq!(r.ir_stats().unwrap().delay_samples, 32);
    }

    #[test]
    fn ir_stats_none_for_empty_linear_ir() {
        let mut r = sample_impulse_response_report();
        r.data = vec![MeasurementPayload {
            data: MeasurementData::ImpulseResponse {
                sample_rate_hz: 48_000,
                f1_hz: 20.0,
                f2_hz: 20_000.0,
                duration_s: 1.0,
                linear_ir: vec![],
                noise_tail_start_s: None,
                harmonics: vec![],
            },
            standard: Vec::new(),
            gate: None,
        }];
        assert!(r.ir_stats().is_none());
    }

    /// #346: `delay_samples`/`arrival_s` must be onset-derived, not
    /// peak-derived. Test against the rejected implementation — a
    /// synthetic multi-way-like IR where sustained onset energy sits well
    /// before a group-delay-inflated peak, with the peak position
    /// computed here directly (not just asserted "close to the true
    /// arrival", which the old behaviour would also pass).
    #[test]
    fn ir_stats_arrival_is_onset_derived_not_peak_derived() {
        let window_len = 1024;
        let centre = window_len / 2;
        let noise = 0.001;
        let peak_true = centre + 100;
        let onset_true = peak_true - 20; // within the guard band before the peak
        let mut ir: Vec<f64> = vec![noise; window_len];
        for v in ir.iter_mut().take(peak_true + 1).skip(onset_true) {
            *v = 0.3;
        }
        ir[peak_true] = 1.0;

        let peak_index = ir
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
            .unwrap()
            .0;
        assert_eq!(
            peak_index, peak_true,
            "test setup: peak must be at peak_true"
        );

        let r = ir_report_with_custom_ir(ir, 48_000);
        let stats = r.ir_stats().unwrap();
        assert_eq!(stats.peak_index, peak_true, "peak_index stays diagnostic");
        assert_eq!(stats.onset_index, onset_true);
        assert_ne!(
            stats.delay_samples,
            peak_true as i64 - centre as i64,
            "arrival must not be the peak-derived delay — #346"
        );
        assert_eq!(stats.delay_samples, onset_true as i64 - centre as i64);
        assert!(stats.onset_rule.contains("no causal bound"));
    }

    /// #346: when a report carries both a measured interface latency and
    /// a recorded `position.distance_m`, `ir_stats` must convert them
    /// into a causal bound and enforce it — proven by picking a bound
    /// that actively clamps the estimate away from what the unbounded
    /// search alone would return.
    #[test]
    fn ir_stats_wires_a_causal_bound_from_position_and_interface_latency() {
        let window_len = 1024;
        let sr = 48_000u32;
        let centre = window_len / 2;
        let noise = 0.001;
        let peak_true = centre + 100;
        let onset_true = peak_true - 20;
        let mut ir = vec![noise; window_len];
        for v in ir.iter_mut().take(peak_true + 1).skip(onset_true) {
            *v = 0.3;
        }
        ir[peak_true] = 1.0;

        let mut r = ir_report_with_custom_ir(ir, sr);
        let unbounded = r.ir_stats().unwrap();
        assert_eq!(unbounded.onset_index, onset_true);

        // Pure-flight bound at `peak_true - 12` — inside the sustained
        // onset run, so it actively clamps the estimate rather than
        // agreeing with the unbounded answer by coincidence.
        let bound_index = peak_true - 12;
        let bound_offset_samples = (bound_index - centre) as f64;
        let c = crate::shared::conversions::speed_of_sound_from_config(Some(20.0));
        r.position = Some(PositionSnapshot {
            temperature_c: Some(20.0),
            distance_m: Some(bound_offset_samples / sr as f64 * c),
            ..Default::default()
        });
        r.interface_latency = Some(measured_tau(0.0));

        let bounded = r.ir_stats().unwrap();
        assert_eq!(bounded.onset_index, bound_index);
        assert_ne!(bounded.onset_index, unbounded.onset_index);
        assert!(bounded.onset_rule.contains("causal bound enforced"));
    }

    // ─── `ir_arrival_distance` (#283) ─────────────────────────────────

    fn measured_tau(tau_s: f64) -> InterfaceLatency {
        InterfaceLatency::Measured(MeasuredLatency {
            tau_s,
            measured_at: "2026-08-15T00:00:00Z".into(),
            method: "farina_short_ess".into(),
            backend: "fake".into(),
            sample_rate_hz: 48_000,
            period_size: Some(1024),
            output_port: "out1".into(),
            input_port: "in1".into(),
        })
    }

    /// The rig's own converter constant: arrival(d) = 1.1931 ms + d/c.
    /// An arrival of τ + 1 m/c must come back as 1 m — the τ subtraction
    /// is the whole content of the conversion.
    #[test]
    fn arrival_distance_subtracts_tau_before_converting() {
        let tau_s = 0.0011931;
        let sr = 48_000.0;
        let c = crate::shared::conversions::speed_of_sound_from_config(Some(20.0));
        let want_m = 1.0;
        // Round to whole samples, as a real peak index would be.
        let delay = ((tau_s + want_m / c) * sr).round() as usize;
        let window_len = 4_096;
        let mut r = ir_report_with_peak(window_len, window_len / 2 + delay, 1.0, 0.0, 48_000);
        r.position = Some(PositionSnapshot {
            temperature_c: Some(20.0),
            ..Default::default()
        });
        r.interface_latency = Some(measured_tau(tau_s));

        let ArrivalDistance::Known {
            distance_m,
            provenance,
            ..
        } = r.ir_arrival_distance()
        else {
            panic!("distance should be known when tau is recorded");
        };
        // One sample at 48 kHz is ~7 mm of path; allow that quantisation.
        assert!(
            (distance_m - want_m).abs() < 0.01,
            "distance_m = {distance_m}"
        );
        // The AC requires the provenance be *named*, not just applied.
        assert!(provenance.contains("farina_short_ess"), "{provenance}");
        assert!(provenance.contains("2026-08-15T00:00:00Z"), "{provenance}");
        assert!(provenance.contains("fake"), "{provenance}");
    }

    /// The load-bearing refusal: no τ must never fall through to a
    /// distance computed from the uncorrected arrival. At 48 kHz a
    /// 1.1931 ms τ is 57 samples — roughly 0.41 m of phantom path — so a
    /// fallthrough would be a plausible-looking wrong number, not an
    /// obvious one.
    #[test]
    fn arrival_distance_unavailable_without_tau() {
        let window_len = 4_096;
        let r = ir_report_with_peak(window_len, window_len / 2 + 57, 1.0, 0.0, 48_000);
        assert!(r.interface_latency.is_none());
        match r.ir_arrival_distance() {
            ArrivalDistance::Unavailable { reason } => {
                assert!(reason.contains("\u{3c4}"), "reason must name τ: {reason}")
            }
            ArrivalDistance::Known { distance_m, .. } => {
                panic!("derived {distance_m} m from an uncorrected arrival")
            }
        }
    }

    /// A recorded refusal carries its reason through to the reader
    /// instead of being flattened into a generic "no τ".
    #[test]
    fn arrival_distance_propagates_a_recorded_refusal_reason() {
        let window_len = 4_096;
        let mut r = ir_report_with_peak(window_len, window_len / 2 + 57, 1.0, 0.0, 48_000);
        r.interface_latency = Some(InterfaceLatency::Unavailable {
            reason: "nearest stored entry differs in period_size (requested 512, stored 1024)"
                .into(),
        });
        match r.ir_arrival_distance() {
            ArrivalDistance::Unavailable { reason } => assert!(reason.contains("period_size")),
            other => panic!("expected unavailable, got {other:?}"),
        }
    }

    /// v1-v4 archives decode with `interface_latency` absent, and must
    /// land on the refusal — not on a distance derived without a τ.
    #[test]
    fn legacy_report_without_interface_latency_decodes_and_refuses_distance() {
        let r = ir_report_with_peak(4_096, 4_096 / 2 + 57, 1.0, 0.0, 48_000);
        let mut v: serde_json::Value = serde_json::from_str(&r.to_json().unwrap()).unwrap();
        v["schema_version"] = serde_json::json!(4);
        assert!(v.get("interface_latency").is_none(), "None must not encode");
        let back: MeasurementReport = serde_json::from_value(v).unwrap();
        assert!(back.interface_latency.is_none());
        assert!(matches!(
            back.ir_arrival_distance(),
            ArrivalDistance::Unavailable { .. }
        ));
    }

    #[test]
    fn interface_latency_round_trips_through_json() {
        let mut r = ir_report_with_peak(1_024, 512, 1.0, 0.0, 48_000);
        r.interface_latency = Some(measured_tau(0.0011931));
        let back: MeasurementReport = serde_json::from_str(&r.to_json().unwrap()).unwrap();
        assert_eq!(back.interface_latency, r.interface_latency);
    }

    fn sample_noise_report() -> MeasurementReport {
        MeasurementReport {
            schema_version: SCHEMA_VERSION,
            ac_version: "0.1.0".into(),
            timestamp_utc: "2026-04-22T12:00:00Z".into(),
            method: MeasurementMethod::SteppedSine { n_points: 0 },
            stimulus: StimulusParams {
                sample_rate_hz: 48_000,
                f_start_hz: 0.0,
                f_stop_hz: 0.0,
                level_dbfs: 0.0,
                n_points: 0,
            },
            integration: IntegrationParams {
                duration_s: 1.0,
                window: "none".into(),
                n_averages: None,
            },
            calibration: None,
            position: None,
            interface_latency: None,
            data: vec![MeasurementPayload {
                data: MeasurementData::NoiseResult {
                    sample_rate_hz: 48_000,
                    duration_s: 0.9,
                    unweighted_dbfs: -98.4,
                    a_weighted_dbfs: -103.1,
                    ccir_weighted_dbfs: None,
                },
                standard: vec![crate::measurement::noise::citation()],
                gate: None,
            }],
            notes: None,
            processing_chain: ProcessingChain::default(),
        }
    }

    #[test]
    fn noise_result_round_trip() {
        let r = sample_noise_report();
        let json = r.to_json().unwrap();
        let r2: MeasurementReport = serde_json::from_str(&json).unwrap();
        assert_eq!(r, r2);
    }

    #[test]
    fn noise_result_csv_shape() {
        let r = sample_noise_report();
        let csv = r.to_csv().unwrap();
        assert!(csv.starts_with("# payload 1: noise_result"));
        assert!(csv.contains("sample_rate_hz,duration_s,unweighted_dbfs,"));
        assert_eq!(csv.lines().count(), 3);
    }

    #[test]
    fn write_to_round_trips_through_disk() {
        let r = sample_report();
        let tmp = std::env::temp_dir().join(format!("ac-report-{}.json", std::process::id()));
        r.write_to(&tmp).unwrap();
        let text = std::fs::read_to_string(&tmp).unwrap();
        let r2: MeasurementReport = serde_json::from_str(&text).unwrap();
        assert_eq!(r, r2);
        let _ = std::fs::remove_file(&tmp);
    }

    /// Every Tier 1 measurement module must emit a populated
    /// `StandardsCitation` — non-empty `standard` and `clause`. Serialising a
    /// report built from each `citation()` round-trips cleanly and survives
    /// at the current `SCHEMA_VERSION`. See #72 for the audit workflow.
    #[test]
    fn every_measurement_module_emits_populated_citation() {
        let citations = [
            crate::measurement::thd::citation(),
            crate::measurement::filterbank::Filterbank::citation(),
            crate::measurement::noise::citation(),
            crate::measurement::weighting::WeightingFilter::citation(),
            crate::measurement::sweep::citation(),
            crate::measurement::sweep::farina_citation(),
            crate::measurement::sweep::gated_response_citation(),
            crate::measurement::ccir468::citation(),
            crate::shared::reference_levels::citation(),
        ];
        for c in &citations {
            assert!(!c.standard.is_empty(), "empty standard in {c:?}");
            assert!(!c.clause.is_empty(), "empty clause in {c:?}");
        }

        // Round-trip each through a full MeasurementReport payload.
        for c in citations {
            let mut r = sample_report();
            r.data[0].standard = vec![c.clone()];
            let json = r.to_json().unwrap();
            assert!(json.contains("\"schema_version\": 5"));
            let r2: MeasurementReport = serde_json::from_str(&json).unwrap();
            assert_eq!(r, r2);
        }
    }

    /// Maps one edition string (as it appears in a `citation().standard`
    /// field, or one `; `-separated half of one) to the `stddocs/`-relative
    /// path of the document it names. Single place this mapping lives —
    /// see #313: it must not be re-derived per call site.
    ///
    /// Matches by prefix (not equality) because a `standard` field may
    /// carry trailing qualifiers the citation owns (e.g. sweep.rs's
    /// combined `citation()` appends "; ISO 18233:2006 Annex B
    /// (normative)"). Returns `None` for any edition this map does not
    /// recognise — that is the failure this guard exists to catch: a
    /// citation naming an edition nobody holds.
    fn standard_edition_to_stddocs_path(edition: &str) -> Option<&'static str> {
        const KNOWN: &[(&str, &str)] = &[
            ("IEC 61672-1:2013", "iec-full/IEC61672-1.pdf"),
            ("IEC 60268-3:2018", "iec-full/IEC60268-3.pdf"),
            ("IEC 61260-1:2014", "iec-full/IEC61260-1.pdf"),
            ("ITU-R BS.468-4", "ITU-R BS.468-4.pdf"),
            ("ITU-R BS.1770-5", "ITU-R BS.1770-5.pdf"),
            ("AES17-2020", "iec-full/aes17_2020_aes_standard_method_for_digital_audio_engineering_measurement.pdf"),
            (
                "Farina, AES 108th Convention preprint #5093 (2000)",
                "iec-full/Simultaneous_Measurement_of_Impulse_Response_and_D.pdf",
            ),
            ("ISO 18233:2006", "iso-full/ISO18233.pdf"),
            ("ISO 3382-1:2009", "iso-full/ISO3382-1.pdf"),
            ("ISO 3382-2:2008", "iso-full/ISO3382-2.pdf"),
        ];
        KNOWN
            .iter()
            .find(|(key, _)| edition.starts_with(key))
            .map(|(_, path)| *path)
    }

    /// A citation's `standard` field resolves only if every `; `-separated
    /// half of it maps to a file that actually exists under `stddocs_root`.
    fn citation_standard_resolves(standard: &str, stddocs_root: &Path) -> bool {
        standard.split("; ").all(|edition| {
            standard_edition_to_stddocs_path(edition)
                .map(|rel| stddocs_root.join(rel).is_file())
                .unwrap_or(false)
        })
    }

    /// Pulls every backtick-delimited `stddocs/...pdf` path reference out
    /// of a markdown document. Used to walk the normative-standards table
    /// path columns in `.agents/qa.md` and `ARCHITECTURE.md`.
    fn extract_stddocs_pdf_paths(markdown: &str) -> Vec<String> {
        let mut paths = Vec::new();
        let mut rest = markdown;
        while let Some(start) = rest.find("stddocs/") {
            let after = &rest[start..];
            let Some(end) = after.find('`') else {
                break;
            };
            let candidate = &after[..end];
            // Skip bare directory references like `stddocs/` or
            // `stddocs/iec-full/` — only file references matter here.
            if candidate.ends_with(".pdf") {
                paths.push(candidate.to_string());
            }
            rest = &after[end..];
        }
        paths
    }

    /// Regression guard for #313. `every_measurement_module_emits_populated_citation`
    /// only checks that `standard`/`clause` are non-empty — a well-formed
    /// lie passes it just as well as the truth. That is exactly what
    /// shipped: `gated_response_citation()` cited `AES17-2015` while only
    /// AES17-2020 was ever held in `stddocs/` (#312), and the existing
    /// guard could not go red for it. This test additionally resolves each
    /// citation's `standard` to a document actually present in `stddocs/`,
    /// and separately walks the `.agents/qa.md` and `ARCHITECTURE.md`
    /// standards tables for stale path references (#291's own acceptance
    /// criterion: the table and the emitting fns must never disagree).
    ///
    /// `stddocs/` is gitignored — held PDFs are licensed and exist only in
    /// the local main tree, not in worktrees (see repo `CLAUDE.md`). This
    /// test skips, visibly, rather than failing when the directory is
    /// absent, so the suite stays runnable where most agent work happens.
    #[test]
    fn every_citation_resolves_to_a_held_document() {
        let stddocs_root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../stddocs"));
        if !stddocs_root.is_dir() {
            eprintln!(
                "SKIP every_citation_resolves_to_a_held_document: {} not present \
                 (stddocs/ is gitignored, main-tree only — see CLAUDE.md)",
                stddocs_root.display()
            );
            return;
        }
        let repo_root = stddocs_root.parent().expect("stddocs_root has a parent");

        let citations = [
            crate::measurement::thd::citation(),
            crate::measurement::filterbank::Filterbank::citation(),
            crate::measurement::noise::citation(),
            crate::measurement::weighting::WeightingFilter::citation(),
            crate::measurement::sweep::citation(),
            crate::measurement::sweep::farina_citation(),
            crate::measurement::sweep::gated_response_citation(),
            crate::measurement::ccir468::citation(),
            crate::shared::reference_levels::citation(),
        ];
        for c in &citations {
            assert!(
                citation_standard_resolves(&c.standard, stddocs_root),
                "citation names an edition nobody holds under stddocs/: {c:?}"
            );
        }

        for (label, path) in [
            (".agents/qa.md", repo_root.join(".agents/qa.md")),
            ("ARCHITECTURE.md", repo_root.join("ARCHITECTURE.md")),
        ] {
            let doc = fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
            for rel in extract_stddocs_pdf_paths(&doc) {
                let full = repo_root.join(&rel);
                assert!(
                    full.is_file(),
                    "{label} cites `{rel}` which does not exist on disk"
                );
            }
        }
    }

    // ─── CalibrationSnapshot: SPL + mic_response provenance (#94) ────

    #[test]
    fn cal_snapshot_round_trips_spl_and_mic_response() {
        let snap = CalibrationSnapshot {
            output_channel: 0,
            input_channel: 1,
            vrms_at_0dbfs_out: Some(1.234),
            vrms_at_0dbfs_in: Some(0.567),
            ref_freq_hz: 1000.0,
            ref_level_dbfs: -10.0,
            mic_sensitivity_dbfs_at_94db_spl: Some(-31.7),
            mic_response: Some(MicResponseRef {
                n_points: 157,
                source_path: Some("/tmp/umik.frd".into()),
                imported_at: "2026-04-15T12:00:00Z".into(),
            }),
        };
        let json = serde_json::to_string(&snap).unwrap();
        let back: CalibrationSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(snap, back);
    }

    #[test]
    fn cal_snapshot_omits_mic_fields_when_absent() {
        // Voltage-only channel: the new fields must not appear in the
        // serialised JSON so reports stay compact and old readers stay
        // happy.
        let snap = CalibrationSnapshot {
            output_channel: 0,
            input_channel: 0,
            vrms_at_0dbfs_out: None,
            vrms_at_0dbfs_in: Some(1.0),
            ref_freq_hz: 1000.0,
            ref_level_dbfs: -10.0,
            mic_sensitivity_dbfs_at_94db_spl: None,
            mic_response: None,
        };
        let json = serde_json::to_string(&snap).unwrap();
        assert!(!json.contains("mic_sensitivity_dbfs_at_94db_spl"), "{json}");
        assert!(!json.contains("mic_response"), "{json}");
    }

    // ─── ProcessingChain (#105) ─────────────────────────────────────────

    #[test]
    fn processing_chain_default_is_all_off() {
        let p = ProcessingChain::default();
        assert_eq!(p.weighting, "off");
        assert_eq!(p.smoothing_bpo, None);
        assert_eq!(p.time_integration, "off");
        assert!(!p.mic_correction_applied);
    }

    #[test]
    fn processing_chain_round_trips() {
        let p = ProcessingChain {
            weighting: "a".into(),
            smoothing_bpo: Some(6),
            time_integration: "fast".into(),
            mic_correction_applied: true,
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: ProcessingChain = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn legacy_schema_v2_report_decodes_with_default_processing_chain() {
        // A v2 report (post-#94, pre-#105) lacks `processing_chain`,
        // and predates the v4 `data`-is-an-array shape. Must still
        // decode under the current struct, with `processing_chain`
        // defaulting to "all-off" and `data` wrapped into a
        // single-element payload vec.
        let legacy = r#"{
            "schema_version": 2,
            "ac_version": "0.1.0",
            "timestamp_utc": "2026-04-22T00:00:00Z",
            "method": {"kind":"stepped_sine","n_points":1},
            "stimulus": {"sample_rate_hz":48000,"f_start_hz":1000,"f_stop_hz":1000,"level_dbfs":-20,"n_points":1},
            "integration": {"duration_s":1.0,"window":"hann"},
            "data": {"kind":"frequency_response","points":[]}
        }"#;
        let r: MeasurementReport =
            serde_json::from_str(legacy).expect("legacy v2 report must still decode");
        assert_eq!(r.schema_version, 2);
        assert_eq!(r.processing_chain, ProcessingChain::default());
        assert_eq!(r.data.len(), 1);
        assert!(r.data[0].standard.is_empty());
        assert!(r.data[0].gate.is_none());
        assert!(matches!(
            r.data[0].data,
            MeasurementData::FrequencyResponse { .. }
        ));
    }

    #[test]
    fn legacy_schema_v1_report_decodes_with_new_snapshot_fields_defaulted() {
        // A `schema_version: 1` report from before #94 lacks the
        // mic_sensitivity / mic_response fields entirely, and predates
        // the v4 `data`-is-an-array shape. It must still decode under
        // the new struct, with the new fields defaulting to None/empty.
        let legacy = r#"{
            "schema_version": 1,
            "ac_version": "0.1.0",
            "timestamp_utc": "2026-04-21T00:00:00Z",
            "method": {"kind":"stepped_sine","n_points":1},
            "stimulus": {"sample_rate_hz":48000,"f_start_hz":1000,"f_stop_hz":1000,"level_dbfs":-20,"n_points":1},
            "integration": {"duration_s":1.0,"window":"hann"},
            "calibration": {
                "output_channel": 0,
                "input_channel":  0,
                "vrms_at_0dbfs_out": 1.0,
                "vrms_at_0dbfs_in":  0.5,
                "ref_freq_hz":   1000.0,
                "ref_level_dbfs": -10.0
            },
            "data": {"kind":"frequency_response","points":[]}
        }"#;
        let r: MeasurementReport =
            serde_json::from_str(legacy).expect("legacy v1 report must still decode");
        let cal = r.calibration.expect("calibration block present");
        assert!(cal.mic_sensitivity_dbfs_at_94db_spl.is_none());
        assert!(cal.mic_response.is_none());
        assert_eq!(cal.vrms_at_0dbfs_in, Some(0.5));
        // Note: schema_version on the loaded struct is 1, not the
        // current SCHEMA_VERSION — the value reflects what was on disk.
        assert_eq!(r.schema_version, 1);
        assert_eq!(r.data.len(), 1);
        assert!(r.position.is_none());
    }

    #[test]
    fn legacy_schema_v3_report_decodes_data_object_as_single_payload() {
        // A v3 report (post-#105, pre-#280) has `processing_chain` but
        // still the old bare-object `data` shape and a `standard`
        // field sitting on `method` (the bug #280 exists to fix). It
        // must still decode: `processing_chain` reads as recorded, the
        // stray `method.standard` is silently dropped (unknown field),
        // and `data` is wrapped into one payload.
        let legacy = r#"{
            "schema_version": 3,
            "ac_version": "0.2.0",
            "timestamp_utc": "2026-05-01T00:00:00Z",
            "method": {"kind":"stepped_sine","n_points":0,"standard":{"standard":"IEC 61260-1:2014","clause":"§5.2.1","verified":true}},
            "stimulus": {"sample_rate_hz":48000,"f_start_hz":100,"f_stop_hz":1000,"level_dbfs":-20,"n_points":0},
            "integration": {"duration_s":1.0,"window":"none"},
            "processing_chain": {"weighting":"a","time_integration":"fast","mic_correction_applied":true},
            "data": {"kind":"spectrum_bands","bpo":3,"class":"Class 1","centres_hz":[100.0],"levels_dbfs":[-30.0]}
        }"#;
        let r: MeasurementReport =
            serde_json::from_str(legacy).expect("legacy v3 report must still decode");
        assert_eq!(r.schema_version, 3);
        assert_eq!(r.processing_chain.weighting, "a");
        assert_eq!(r.data.len(), 1);
        assert!(r.data[0].standard.is_empty());
        assert!(matches!(
            r.data[0].data,
            MeasurementData::SpectrumBands { .. }
        ));
    }

    // ─── v4: multi-payload, gate, position (#280) ───────────────────────

    #[test]
    fn multi_payload_report_round_trips_with_distinct_citations_and_gate() {
        let mut r = sample_impulse_response_report();
        r.data.push(MeasurementPayload {
            data: MeasurementData::FrequencyResponse { points: vec![] },
            standard: vec![
                crate::shared::reference_levels::citation(),
                crate::measurement::thd::citation(),
            ],
            gate: Some(GateParams {
                gate_start_s: 0.0029,
                gate_length_s: 0.020,
                window_kind: "tukey0.25".into(),
                f_low_hz: 1.0 / 0.020,
            }),
        });
        let json = r.to_json().unwrap();
        let r2: MeasurementReport = serde_json::from_str(&json).unwrap();
        assert_eq!(r, r2);
        assert_eq!(r2.data.len(), 2);
        assert_eq!(r2.data[1].standard.len(), 2);
        assert_eq!(r2.data[1].gate.as_ref().unwrap().f_low_hz, 50.0);
    }

    #[test]
    fn multi_payload_csv_does_not_collapse_into_one_table() {
        let mut r = sample_impulse_response_report();
        r.data.push(MeasurementPayload {
            data: MeasurementData::SpectrumBands {
                bpo: 3,
                class: "Class 1".into(),
                centres_hz: vec![100.0],
                levels_dbfs: vec![-30.0],
            },
            standard: vec![],
            gate: Some(GateParams {
                gate_start_s: 0.0,
                gate_length_s: 0.02,
                window_kind: "hann".into(),
                f_low_hz: 50.0,
            }),
        });
        let csv = r.to_csv().unwrap();
        assert!(csv.contains("# payload 1: impulse_response"));
        assert!(csv.contains("# payload 2: spectrum_bands  gate=0.0ms+20.0ms hann f_low=50.0Hz"));
        assert!(csv.contains("sample_idx,time_s,order,amplitude"));
        assert!(csv.contains("centre_hz,level_dbfs,bpo,class"));
    }

    #[test]
    fn n_averages_round_trips_and_omits_absent_field() {
        let mut r = sample_report();
        r.integration.n_averages = Some(8);
        let json = r.to_json().unwrap();
        assert!(json.contains("\"n_averages\": 8"), "{json}");
        let r2: MeasurementReport = serde_json::from_str(&json).unwrap();
        assert_eq!(r, r2);

        // A report with no averages count omits the field entirely
        // rather than serialising a misleading default.
        let bare = sample_report();
        let bare_json = bare.to_json().unwrap();
        assert!(!bare_json.contains("n_averages"), "{bare_json}");
    }

    #[test]
    fn position_snapshot_round_trips_and_omits_absent_fields() {
        let mut r = sample_report();
        r.position = Some(PositionSnapshot {
            temperature_c: Some(21.3),
            relative_humidity_pct: Some(45.0),
            source_height_m: Some(1.2),
            receiver_height_m: Some(1.1),
            distance_m: Some(1.0),
        });
        let json = r.to_json().unwrap();
        let r2: MeasurementReport = serde_json::from_str(&json).unwrap();
        assert_eq!(r, r2);

        // A report with no position data omits the field entirely.
        let bare = sample_report();
        let bare_json = bare.to_json().unwrap();
        assert!(!bare_json.contains("\"position\""), "{bare_json}");
    }

    // ─── GatedFrequencyResponse / noise_tail_start_s (#284) ──────────────

    #[test]
    fn gated_frequency_response_round_trips_and_labels_correctly() {
        let mut r = sample_impulse_response_report();
        r.data.push(MeasurementPayload {
            data: MeasurementData::GatedFrequencyResponse {
                points: vec![
                    GatedFrequencyResponsePoint {
                        freq_hz: 100.0,
                        magnitude_db: -0.5,
                        phase_deg: 12.3,
                    },
                    GatedFrequencyResponsePoint {
                        freq_hz: 1_000.0,
                        magnitude_db: -1.2,
                        phase_deg: -145.0,
                    },
                ],
            },
            standard: vec![
                crate::measurement::sweep::citation(),
                crate::measurement::sweep::gated_response_citation(),
            ],
            gate: Some(GateParams {
                gate_start_s: 0.0,
                gate_length_s: 0.020,
                window_kind: "tukey0.25".into(),
                f_low_hz: 50.0,
            }),
        });
        assert_eq!(r.data[1].data.kind_label(), "gated_frequency_response");
        let json = r.to_json().unwrap();
        let r2: MeasurementReport = serde_json::from_str(&json).unwrap();
        assert_eq!(r, r2);
        assert_eq!(r2.data[1].standard.len(), 2);
    }

    #[test]
    fn gated_frequency_response_csv_shape() {
        let mut r = sample_impulse_response_report();
        r.data = vec![MeasurementPayload {
            data: MeasurementData::GatedFrequencyResponse {
                points: vec![
                    GatedFrequencyResponsePoint {
                        freq_hz: 100.0,
                        magnitude_db: -0.5,
                        phase_deg: 12.3,
                    },
                    GatedFrequencyResponsePoint {
                        freq_hz: 1_000.0,
                        magnitude_db: -1.2,
                        phase_deg: -145.0,
                    },
                ],
            },
            standard: vec![],
            gate: None,
        }];
        let csv = r.to_csv().unwrap();
        assert!(csv.starts_with("# payload 1: gated_frequency_response"));
        assert!(csv.contains("freq_hz,magnitude_db,phase_deg"));
        // Payload comment + header + 2 data lines.
        assert_eq!(csv.lines().count(), 4);
    }

    #[test]
    fn impulse_response_noise_tail_start_s_round_trips_and_is_optional() {
        let mut r = sample_impulse_response_report();
        let MeasurementData::ImpulseResponse {
            noise_tail_start_s, ..
        } = &mut r.data[0].data
        else {
            panic!("expected ImpulseResponse");
        };
        *noise_tail_start_s = Some(3.0);
        let json = r.to_json().unwrap();
        assert!(json.contains("\"noise_tail_start_s\": 3.0"), "{json}");
        let r2: MeasurementReport = serde_json::from_str(&json).unwrap();
        assert_eq!(r, r2);

        // Absent field omits the key entirely (legacy reports decode
        // with `None`, not a misleading `0.0`).
        let bare = sample_impulse_response_report();
        let bare_json = bare.to_json().unwrap();
        assert!(!bare_json.contains("noise_tail_start_s"), "{bare_json}");
        let bare2: MeasurementReport = serde_json::from_str(&bare_json).unwrap();
        assert_eq!(bare2, bare);
    }
}
