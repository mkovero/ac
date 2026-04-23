//! `MeasurementReport` — the Tier 1 archival artifact emitted by
//! reproducible measurement commands (`ac plot`, future `ac sweep`,
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
pub const SCHEMA_VERSION: u32 = 1;

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
    pub data: MeasurementData,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// The measurement technique. `kind` is a discriminant so new methods
/// (Farina sweep, pink-noise, etc.) extend the enum without breaking
/// existing readers.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MeasurementMethod {
    /// Discrete-frequency stepped-sine sweep — one tone per bin, fundamental
    /// analyzed in isolation (`measurement::thd::analyze`). Used by `plot`.
    SteppedSine {
        n_points: usize,
        #[serde(skip_serializing_if = "Option::is_none")]
        standard: Option<StandardsCitation>,
    },
    /// Continuous log-swept sine (Farina ESS) — stimulus is a single
    /// exponential sweep from `f1_hz` to `f2_hz` over `duration_s`; the
    /// captured response is processed by deconvolution or a fractional-
    /// octave filterbank. Used by `sweep_ir`.
    SweptSine {
        f1_hz: f64,
        f2_hz: f64,
        duration_s: f64,
        #[serde(skip_serializing_if = "Option::is_none")]
        standard: Option<StandardsCitation>,
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

    /// Flat CSV of the report's data payload. The header and column set
    /// depend on the `MeasurementData` variant — callers that need a
    /// specific schema should branch on `method` / `data` themselves.
    pub fn to_csv(&self) -> Result<String> {
        let mut s = String::new();
        match &self.data {
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
        Ok(s)
    }

    pub fn write_to(&self, path: &Path) -> Result<()> {
        let json = self.to_json()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
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
            method: MeasurementMethod::SteppedSine {
                n_points: 3,
                standard: Some(crate::measurement::thd::citation()),
            },
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
            },
            calibration: None,
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
            notes: None,
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
        // Header + 3 data lines.
        assert_eq!(a.lines().count(), 4);
        assert!(a.starts_with("freq_hz,fundamental_dbfs,"));
    }

    #[test]
    fn swept_sine_method_round_trip() {
        let mut r = sample_report();
        r.method = MeasurementMethod::SweptSine {
            f1_hz: 20.0,
            f2_hz: 20_000.0,
            duration_s: 3.0,
            standard: Some(crate::measurement::sweep::citation()),
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
    fn schema_version_present() {
        let r = sample_report();
        let json = r.to_json().unwrap();
        assert!(json.contains("\"schema_version\": 1"));
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
            method: MeasurementMethod::SteppedSine {
                n_points: 0,
                standard: Some(crate::measurement::filterbank::Filterbank::citation()),
            },
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
            },
            calibration: None,
            data: MeasurementData::SpectrumBands {
                bpo: 3,
                class: "Class 1".into(),
                centres_hz: vec![100.0, 125.893, 158.489],
                levels_dbfs: vec![-30.0, -20.0, -40.0],
            },
            notes: None,
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
        assert!(csv.starts_with("centre_hz,level_dbfs,bpo,class"));
        assert_eq!(csv.lines().count(), 4);
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
                standard: Some(crate::measurement::sweep::citation()),
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
            },
            calibration: None,
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
            notes: None,
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
        assert!(csv.starts_with("sample_idx,time_s,order,amplitude"));
        // Header + 5 linear rows + 5 harmonic rows.
        assert_eq!(csv.lines().count(), 11);
    }

    fn sample_noise_report() -> MeasurementReport {
        MeasurementReport {
            schema_version: SCHEMA_VERSION,
            ac_version: "0.1.0".into(),
            timestamp_utc: "2026-04-22T12:00:00Z".into(),
            method: MeasurementMethod::SteppedSine {
                n_points: 0,
                standard: Some(crate::measurement::noise::citation()),
            },
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
            },
            calibration: None,
            data: MeasurementData::NoiseResult {
                sample_rate_hz: 48_000,
                duration_s: 0.9,
                unweighted_dbfs: -98.4,
                a_weighted_dbfs: -103.1,
                ccir_weighted_dbfs: None,
            },
            notes: None,
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
        assert!(csv.starts_with("sample_rate_hz,duration_s,unweighted_dbfs,"));
        assert_eq!(csv.lines().count(), 2);
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
    /// with `schema_version == 1`. See #72 for the audit workflow.
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

        // Round-trip each through a full MeasurementReport.
        for c in citations {
            let mut r = sample_report();
            r.method = MeasurementMethod::SteppedSine {
                n_points: 1,
                standard: Some(c.clone()),
            };
            let json = r.to_json().unwrap();
            assert!(json.contains("\"schema_version\": 1"));
            let r2: MeasurementReport = serde_json::from_str(&json).unwrap();
            assert_eq!(r, r2);
        }
    }
}
