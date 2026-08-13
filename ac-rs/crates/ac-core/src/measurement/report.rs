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
pub const SCHEMA_VERSION: u32 = 4;

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
        }
        Ok(())
    }
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
        assert!(json.contains("\"schema_version\": 4"));
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
            data: vec![MeasurementPayload {
                data: MeasurementData::ImpulseResponse {
                    sample_rate_hz: 48_000,
                    f1_hz: 20.0,
                    f2_hz: 20_000.0,
                    duration_s: 1.0,
                    linear_ir: vec![0.0, 0.5, 1.0, 0.25, 0.0],
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
            assert!(json.contains("\"schema_version\": 4"));
            let r2: MeasurementReport = serde_json::from_str(&json).unwrap();
            assert_eq!(r, r2);
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
}
