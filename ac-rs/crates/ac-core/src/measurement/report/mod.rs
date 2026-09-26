//! `MeasurementReport` — the Tier 1 archival artifact emitted by
//! reproducible measurement commands (`ac plot`, `ac plot ir`, future
//! `ac noise`). Serialises to self-describing JSON for archiving and
//! to CSV for spreadsheet tools.
//!
//! Schema is explicitly versioned via [`SCHEMA_VERSION`]; readers
//! that see an unknown version must refuse to decode. See
//! `ARCHITECTURE.md` for the tiered model.
//!
//! The supported read boundary is [`MeasurementReport::from_json`] and
//! [`MeasurementReport::from_value`] (#429): both accept
//! `schema_version` in [`MIN_SCHEMA_VERSION`]`..=`[`SCHEMA_VERSION`] and
//! refuse anything else with a [`ReportReadError`] before the body is
//! decoded. Every shipped reader goes through them. The `Deserialize`
//! impl is the unchecked structural decoder, kept for internal and test
//! decoding; a plain `serde_json::from_*::<MeasurementReport>` bypasses
//! the version gate.
//!
//! This module owns the report envelope — the version, the top-level
//! struct, and its JSON form. The rest is split by what it describes:
//!
//! - [`payload`] — the measured results and the per-payload gate.
//! - [`provenance`] — stimulus, interface, calibration and environment
//!   blocks; everything that makes a payload interpretable but holds no
//!   measured value itself.
//! - [`ir_stats`] — read-out quantities derived from an impulse-response
//!   payload, and the trust verdict on its peak.
//! - [`csv`] — the flat spreadsheet rendering.
//!
//! Every public type is re-exported here, so `measurement::report::X`
//! stays the path for all of them regardless of which file X lives in.
//!
//! Each of those modules carries its own tests; the sample reports they
//! share live in the test-only [`fixtures`].

use std::fmt;
use std::fs;
use std::ops::RangeInclusive;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::shared::calibration::EnumerationCheck;

#[cfg(test)]
mod arrival_suite;
mod csv;
#[cfg(test)]
mod fixtures;
mod ir_stats;
#[cfg(test)]
mod late_edge_replay;
mod payload;
#[cfg(test)]
mod pre_impulse_region_replay;
mod provenance;

#[cfg(test)]
pub(crate) use ir_stats::band_limited_arrival;
pub use ir_stats::{
    arrival_snr_low_reason, high_pass_check_tolerance_samples, pre_impulse_snr_scope, ArrivalCheck,
    DistanceCheck, DistanceWindow, HighPassAdvisory, HighPassCheck, HighPassSource, IrStats,
    IrVerdict, LatencyBasis, LiveOffset, OnsetStanding, PreImpulseAnchor, PreImpulseSnrScope,
    ScoredSweepParam, WithheldBasis, ARRIVAL_BROADBAND_COMPARABLE_DB, ARRIVAL_CROSS_CHECK_BASIS,
    ARRIVAL_CROSS_CHECK_TOLERANCE_S, ARRIVAL_EARLIER_COMPARABLE_DB, ARRIVAL_LOBE_MARGIN_MIN_DB,
    ARRIVAL_SNR_BASIS, ARRIVAL_SNR_MIN_DB, ARRIVAL_SNR_UNMEASURED_REASON,
    DISTANCE_SPEED_OF_SOUND_REL_TOL, DISTANCE_TAPE_TOLERANCE_M, PRE_IMPULSE_SNR_CHECKS_CHAIN,
    PRE_IMPULSE_SNR_CHECKS_SWEEP, PRE_IMPULSE_SNR_MIN_DB,
};
pub use payload::{
    FrequencyResponsePoint, GateParams, GatedFrequencyResponsePoint, MeasurementData,
    MeasurementPayload,
};
pub use provenance::{
    CalibrationSnapshot, IntegrationParams, InterfaceLatency, MeasuredLatency,
    MeasuredReferenceLatency, MeasurementMethod, MicResponseRef, PositionSnapshot, ProcessingChain,
    ReferenceLatency, StandardsCitation, StimulusParams,
};

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
///   v1-v4 decode unchanged: the field defaults to `None`, which readers
///   treat as no τ-corrected flight time being derivable from the
///   uncorrected arrival (#391 — the ms → m conversion this used to feed
///   is gone; τ itself, and this field, are not).
/// - v6: optional capture `backend`; v1-v5 reports decode with it absent.
/// - v7: optional `reference_latency: ReferenceLatency` records τ of the
///   *reference* loopback pair, read from a reference leg captured in the
///   same run (#460). It feeds only the onset search's causal bound and is
///   never subtracted from the arrival (`interface_latency` is the capture
///   pair's own τ). A stored τ cannot stand in for it: it re-picks by a
///   multiple of the FireWire SYT interval on every device enumeration
///   (#461). Also from v7, `position` may be present with only
///   `distance_m`. v1-v6 reports decode with the field absent, which
///   readers treat as no reference, so no causal bound.
/// - v8: `MeasuredReferenceLatency` gains optional
///   `pre_impulse_snr_floor_db` — the noiseless deconvolution floor the
///   reference reading was judged against (#471). The reference leg's SNR
///   threshold is derived from its own stimulus rather than fixed, because
///   the statistic is a property of the sweep shape and not of the capture's
///   noise: it moves ~18 dB between `plot ir`'s default band and a narrow
///   one, while 100 dB of added noise moves it by nothing. Absent on v1-v7
///   reports and on any reading judged by the fixed threshold instead (no
///   floor could be established); a reader that finds it absent knows only
///   that the reading passed *some* threshold, not which.
/// - v9: optional `reference_stored_latency: InterfaceLatency` records the τ
///   `calibrate` has on file for the *reference* pair, resolved by the same
///   exact-match lookup `interface_latency` uses (#359). Compared in
///   [`IrStats::arrival_check`] against this run's same-capture
///   `reference_latency` to catch the one-period graph-buffering shift #347
///   documents for `calibrate` — `plot_ir` had no equivalent corroboration
///   at all. Absent on v1-v8 reports and whenever no reference is
///   configured, which reads as [`ArrivalCheck::Unchecked`].
/// - v10: `MeasuredLatency` gains optional `enumeration: EnumerationCheck`,
///   in both `interface_latency` and `reference_stored_latency` — how the
///   stored τ's device-enumeration epoch related to this capture's, frozen
///   at capture (#461). A stored τ re-picks by whole SYT-interval steps
///   across a reboot or interface power cycle with every `TauConditions`
///   field matching, so a crossed boundary flags the value; it never
///   refuses it. Absent on v1-v9 reports, which readers treat as
///   `not_recorded`, never as `same`.
/// - v11: `CalibrationSnapshot` gains optional `voltage_check: LayerVerdict`
///   — the session check's verdict on the voltage layer, frozen at capture
///   (#466). A refused verdict comes with both `vrms_at_0dbfs_*` absent, so
///   the report's values are in dBFS. Absent on v1-v10 reports, which
///   readers show as "not recorded", never as verified. `MeasuredLatency`
///   gains optional `session_check` / `session_check_loopback`, set on
///   `interface_latency` by every v11 `plot_ir` run that resolves a τ; a
///   refused verdict withholds `IrStats::flight_time_s`.
/// - v12: optional top-level `inter_pair_offset: InterPairOffset` on
///   `plot_ir` reports (#544) — the capture pair's τ minus the reference
///   pair's τ, both read in one `calibrate` capture. From v12 the flight
///   time is `arrival − (reference_latency + offset)`: the stored absolute
///   τ in `interface_latency` is kept as provenance but no longer
///   subtracted, `arrival_check` is a drift readout that withholds nothing,
///   and a refused `interface_latency.session_check` no longer withholds
///   the flight time. Absent on v1-v11 reports, whose re-derived flight
///   time is withheld as predating v12.
pub const SCHEMA_VERSION: u32 = 12;

/// Oldest `schema_version` [`MeasurementReport::from_json`] /
/// [`MeasurementReport::from_value`] still read (#429). Everything from
/// here to [`SCHEMA_VERSION`] decodes; the legacy `data` shapes of v1-v3
/// are converted by the `Deserialize` impl.
pub const MIN_SCHEMA_VERSION: u32 = 1;

/// Why [`MeasurementReport::from_json`] / [`MeasurementReport::from_value`]
/// refused a report (#429).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportReadError {
    /// Not JSON, not an object, `schema_version` missing or not a
    /// non-negative integer, or the body did not decode.
    Malformed(String),
    /// An integer `schema_version` outside
    /// [`MIN_SCHEMA_VERSION`]`..=`[`SCHEMA_VERSION`], including 0. The
    /// body was not read. `found` is `u64` so an oversized value is
    /// reported as found, never truncated.
    UnsupportedSchema {
        found: u64,
        supported: RangeInclusive<u32>,
    },
}

impl fmt::Display for ReportReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReportReadError::Malformed(msg) => write!(f, "malformed MeasurementReport: {msg}"),
            ReportReadError::UnsupportedSchema { found, supported } => write!(
                f,
                "unsupported measurement report schema: found v{found}, supported v{}–v{}",
                supported.start(),
                supported.end()
            ),
        }
    }
}

impl std::error::Error for ReportReadError {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct MeasurementReport {
    pub schema_version: u32,
    pub ac_version: String,
    pub timestamp_utc: String,
    /// Live audio backend that produced this capture. This is distinct from
    /// the backend nested in `interface_latency`, which describes the τ
    /// measurement rather than this report's capture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
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
    ///
    /// From v12 (#544) this is provenance only: [`IrStats::flight_time_s`]
    /// no longer subtracts it, because a τ stored days earlier does not
    /// share this capture's transport state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface_latency: Option<InterfaceLatency>,
    /// Round-trip latency of the reference loopback pair, measured from a
    /// reference leg captured in the same run as this report's IR (#460) —
    /// or why no valid reading exists. τ of a *different* pair than
    /// `interface_latency`: never subtract it from the arrival on its own.
    /// From v12 (#544) [`IrStats::flight_time_s`] subtracts it together
    /// with `inter_pair_offset`, and the causal bound reads it as before.
    /// `plot_ir` always records it —
    /// `unavailable` with a reason when no reference is configured — so
    /// `None` means a report written before v7, or a producer that captures
    /// no reference leg.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_latency: Option<ReferenceLatency>,
    /// τ `calibrate` has on file for the *reference* pair, looked up by the
    /// same exact-match rule `interface_latency` uses (#359, schema v9).
    /// Reuses [`InterfaceLatency`] rather than a new type: `Measured`
    /// carries the reference pair's stored τ, `Unavailable` names why none
    /// applies. [`IrStats::arrival_check`] compares this against
    /// `reference_latency`'s same-capture reading to catch a graph-
    /// buffering shift between the lifetime that stored this and the
    /// lifetime that captured this report — the same fault #347 guards for
    /// `calibrate`, on the path #347 doesn't cover. `None` on reports
    /// written before v9, and whenever `plot_ir` has no reference
    /// configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_stored_latency: Option<InterfaceLatency>,
    /// The inter-pair offset between the capture pair and the reference
    /// pair, resolved from calibration by exact topology match (#544,
    /// schema v12). With `reference_latency` it is the latency
    /// [`IrStats::flight_time_s`] subtracts. `None` on reports written
    /// before v12 and on producers that are not `plot_ir`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inter_pair_offset: Option<InterPairOffset>,
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

/// The inter-pair offset a `plot_ir` capture resolved (#544, schema v12):
/// how much longer the capture pair's path is than the reference pair's,
/// read from a `calibrate` capture that had both legs. Stored instead of
/// either absolute τ, because only a difference taken inside one capture is
/// common-mode to the converter, transport and graph state of that capture.
///
/// What it does not cover: a pair whose offset was never measured (refused
/// as [`InterPairOffset::Unavailable`], never assumed zero), and a topology
/// other than the one it was measured on — the lookup key is every
/// `TauConditions` field plus both reference ports.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum InterPairOffset {
    Measured(MeasuredInterPairOffset),
    /// The capture pair is the reference pair: the offset is zero by
    /// definition, not by measurement.
    Identity,
    /// No reference loopback is configured, so there is no pair to take an
    /// offset against.
    NotConfigured,
    /// No offset on file for this topology. `reason` is
    /// `<pair> against ref <reference>; <observation>[; <observation>…];
    /// check: <places>` — the pair named, then one observation per
    /// differing field, as [`crate::shared::calibration::PairOffsetRefusal`]
    /// renders them.
    Unavailable {
        reason: String,
    },
}

/// An offset that matched this capture's topology exactly. Flattened like
/// [`MeasuredLatency`], so the report reads without `cal.json`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct MeasuredInterPairOffset {
    /// `tau_s − reference_tau_s`, seconds. Positive when the capture pair's
    /// path is the longer one.
    pub offset_s: f64,
    /// The capture pair's τ in the calibrate capture the offset came from.
    pub tau_s: f64,
    /// The reference pair's τ in that same capture.
    pub reference_tau_s: f64,
    /// RFC3339 timestamp of that calibrate capture.
    pub measured_at: String,
    pub output_port: String,
    pub input_port: String,
    pub reference_output_port: String,
    pub reference_input_port: String,
    pub sample_rate_hz: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period_size: Option<u32>,
    /// How the offset's device-enumeration epoch relates to this capture's,
    /// frozen at capture. A flag, never a gate (#461's rule).
    pub enumeration: EnumerationCheck,
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

impl MeasurementReport {
    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(self).context("encode MeasurementReport as JSON")
    }

    /// Checked decode from JSON text (#429). Parses, then applies the one
    /// version gate in [`MeasurementReport::from_value`].
    pub fn from_json(text: &str) -> std::result::Result<Self, ReportReadError> {
        let value: serde_json::Value = serde_json::from_str(text)
            .map_err(|e| ReportReadError::Malformed(format!("not JSON: {e}")))?;
        Self::from_value(value)
    }

    /// Checked decode from a JSON value (#429). Refuses a `schema_version`
    /// outside [`MIN_SCHEMA_VERSION`]`..=`[`SCHEMA_VERSION`] before the
    /// body is read; otherwise delegates to the structural `Deserialize`.
    pub fn from_value(value: serde_json::Value) -> std::result::Result<Self, ReportReadError> {
        let Some(obj) = value.as_object() else {
            return Err(ReportReadError::Malformed(
                "report is not a JSON object".to_string(),
            ));
        };
        let Some(raw) = obj.get("schema_version") else {
            return Err(ReportReadError::Malformed(
                "report has no schema_version".to_string(),
            ));
        };
        let Some(found) = raw.as_u64() else {
            return Err(ReportReadError::Malformed(
                "report schema_version is not an integer".to_string(),
            ));
        };
        let supported = MIN_SCHEMA_VERSION..=SCHEMA_VERSION;
        if found < u64::from(*supported.start()) || found > u64::from(*supported.end()) {
            return Err(ReportReadError::UnsupportedSchema { found, supported });
        }
        serde_json::from_value(value).map_err(|e| ReportReadError::Malformed(e.to_string()))
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
    use super::fixtures::*;
    use super::*;

    #[test]
    fn report_serializes_round_trip() {
        let r = sample_report();
        let json = r.to_json().unwrap();
        let r2: MeasurementReport = serde_json::from_str(&json).unwrap();
        assert_eq!(r, r2);
    }

    #[test]
    fn schema_version_present() {
        let r = sample_report();
        let json = r.to_json().unwrap();
        assert!(json.contains("\"schema_version\": 12"));
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

    /// A `StandardsCitation` from any Tier 1 measurement module survives a
    /// full report round-trip at the current `SCHEMA_VERSION`. That the
    /// citations are themselves populated and resolve to a held document is
    /// `measurement::citation_audit`'s job, not this module's.
    #[test]
    fn citations_round_trip_through_a_report() {
        for c in crate::measurement::citation_audit::every_citation() {
            let mut r = sample_report();
            r.data[0].standard = vec![c.clone()];
            let json = r.to_json().unwrap();
            assert!(json.contains("\"schema_version\": 12"));
            let r2: MeasurementReport = serde_json::from_str(&json).unwrap();
            assert_eq!(r, r2);
        }
    }

    // ─── CalibrationSnapshot: SPL + mic_response provenance (#94) ────

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
        let r = MeasurementReport::from_json(legacy).expect("legacy v2 report must still decode");
        assert_eq!(r.schema_version, 2);
        assert_eq!(r.backend, None);
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
        let r = MeasurementReport::from_json(legacy).expect("legacy v1 report must still decode");
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

    /// #466: v11 freezes the voltage verdict with the snapshot, and a v10
    /// snapshot without it decodes as "not recorded" (`None`).
    #[test]
    fn v11_voltage_check_round_trips_and_v10_decodes_without_it() {
        let mut r = sample_report();
        r.calibration = Some(refused_calibration());
        let json = r.to_json().unwrap();
        assert!(json.contains("\"voltage_check\""), "{json}");
        assert!(json.contains("\"state\": \"refused\""), "{json}");
        let back: MeasurementReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r);

        let v10 = r#"{"output_channel": 0, "input_channel": 0,
            "vrms_at_0dbfs_in": 0.5, "ref_freq_hz": 1000.0, "ref_level_dbfs": -10.0}"#;
        let snap: CalibrationSnapshot = serde_json::from_str(v10).unwrap();
        assert_eq!(snap.voltage_check, None);
    }

    /// #544: v12's `inter_pair_offset` round-trips in every state, and a v11
    /// report decodes without it.
    #[test]
    fn v12_inter_pair_offset_round_trips_and_v11_decodes_without_it() {
        let states = [
            InterPairOffset::Measured(MeasuredInterPairOffset {
                offset_s: 46.0 / 96_000.0,
                tau_s: 1757.0 / 96_000.0,
                reference_tau_s: 1711.0 / 96_000.0,
                measured_at: "2026-09-21T16:05:40Z".into(),
                output_port: "system:playback_1".into(),
                input_port: "system:capture_1".into(),
                reference_output_port: "system:playback_2".into(),
                reference_input_port: "system:capture_2".into(),
                sample_rate_hz: 96_000,
                period_size: Some(256),
                enumeration: EnumerationCheck::Same,
            }),
            InterPairOffset::Identity,
            InterPairOffset::NotConfigured,
            InterPairOffset::Unavailable {
                reason: "[out0_in0] against ref [out1_in1]; no \u{3c4} on file for this pair"
                    .into(),
            },
        ];
        for state in states {
            let mut r = sample_impulse_response_report();
            r.inter_pair_offset = Some(state);
            let json = r.to_json().unwrap();
            assert!(json.contains("\"inter_pair_offset\""), "{json}");
            assert_eq!(MeasurementReport::from_json(&json).unwrap(), r);
        }
        let mut v11 = serde_json::to_value(sample_impulse_response_report()).unwrap();
        v11["schema_version"] = serde_json::json!(11);
        let r = MeasurementReport::from_value(v11).unwrap();
        assert_eq!(r.inter_pair_offset, None);
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
        let r = MeasurementReport::from_json(legacy).expect("legacy v3 report must still decode");
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

    // ─── #429: checked constructors refuse unsupported versions ─────────

    /// The current fixture's JSON with `schema_version` rewritten to `v`.
    fn sample_value_at(v: serde_json::Value) -> serde_json::Value {
        let mut value = serde_json::to_value(sample_report()).unwrap();
        value["schema_version"] = v;
        value
    }

    #[test]
    fn checked_constructors_round_trip_the_current_report() {
        let r = sample_report();
        let json = r.to_json().unwrap();
        assert_eq!(MeasurementReport::from_json(&json).unwrap(), r);
        let value = serde_json::to_value(&r).unwrap();
        assert_eq!(MeasurementReport::from_value(value).unwrap(), r);
    }

    #[test]
    fn every_supported_version_decodes_through_the_gate() {
        for v in MIN_SCHEMA_VERSION..=SCHEMA_VERSION {
            let value = sample_value_at(serde_json::json!(v));
            let r = MeasurementReport::from_value(value.clone())
                .unwrap_or_else(|e| panic!("v{v} must decode: {e}"));
            assert_eq!(r.schema_version, v);
            let text = serde_json::to_string(&value).unwrap();
            assert_eq!(MeasurementReport::from_json(&text).unwrap(), r);
        }
    }

    #[test]
    fn minimal_v5_report_decodes_through_from_json() {
        // v4 array-shaped `data`, no optional v5+ blocks present.
        let v5 = r#"{
            "schema_version": 5,
            "ac_version": "0.2.0",
            "timestamp_utc": "2026-06-01T00:00:00Z",
            "method": {"kind":"stepped_sine","n_points":1},
            "stimulus": {"sample_rate_hz":48000,"f_start_hz":1000,"f_stop_hz":1000,"level_dbfs":-20,"n_points":1},
            "integration": {"duration_s":1.0,"window":"hann"},
            "data": [{"data": {"kind":"frequency_response","points":[]}, "standard": []}]
        }"#;
        let r = MeasurementReport::from_json(v5).expect("v5 report must decode");
        assert_eq!(r.schema_version, 5);
        assert_eq!(r.data.len(), 1);
        assert!(r.interface_latency.is_none());
    }

    #[test]
    fn unsupported_versions_are_refused_with_found_and_supported() {
        // `SCHEMA_VERSION + 1` is the case that goes red if the constant is
        // bumped without the gate meaning to accept it; 999 alone would not.
        for found in [0u64, 999, u64::from(SCHEMA_VERSION) + 1, u64::MAX] {
            let value = sample_value_at(serde_json::json!(found));
            let expected = ReportReadError::UnsupportedSchema {
                found,
                supported: MIN_SCHEMA_VERSION..=SCHEMA_VERSION,
            };
            assert_eq!(
                MeasurementReport::from_value(value.clone()),
                Err(expected.clone())
            );
            let text = serde_json::to_string(&value).unwrap();
            assert_eq!(MeasurementReport::from_json(&text), Err(expected));
        }
    }

    #[test]
    fn unsupported_version_is_refused_before_the_body_is_read() {
        // A future report whose body the current struct cannot decode still
        // reads as a version refusal, not as malformed.
        let future = r#"{"schema_version": 999, "data": "a shape nobody knows"}"#;
        assert!(matches!(
            MeasurementReport::from_json(future),
            Err(ReportReadError::UnsupportedSchema { found: 999, .. })
        ));
    }

    #[test]
    fn missing_or_non_integer_schema_version_is_malformed() {
        let mut missing = serde_json::to_value(sample_report()).unwrap();
        missing.as_object_mut().unwrap().remove("schema_version");
        let cases = [
            missing,
            sample_value_at(serde_json::json!("11")),
            sample_value_at(serde_json::json!(-1)),
            sample_value_at(serde_json::json!(5.5)),
            serde_json::json!([1, 2, 3]),
        ];
        for value in cases {
            assert!(
                matches!(
                    MeasurementReport::from_value(value.clone()),
                    Err(ReportReadError::Malformed(_))
                ),
                "{value}"
            );
        }
        assert!(matches!(
            MeasurementReport::from_json("not json at all"),
            Err(ReportReadError::Malformed(_))
        ));
    }

    #[test]
    fn supported_version_with_a_bad_body_is_malformed() {
        let bad = r#"{"schema_version": 11, "data": []}"#;
        assert!(matches!(
            MeasurementReport::from_json(bad),
            Err(ReportReadError::Malformed(_))
        ));
    }

    #[test]
    fn unsupported_schema_display_names_found_and_range() {
        let e = ReportReadError::UnsupportedSchema {
            found: 999,
            supported: MIN_SCHEMA_VERSION..=SCHEMA_VERSION,
        };
        assert_eq!(
            e.to_string(),
            format!(
                "unsupported measurement report schema: found v999, supported v1–v{SCHEMA_VERSION}"
            )
        );
    }
}
