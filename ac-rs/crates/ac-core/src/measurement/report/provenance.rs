//! Provenance blocks carried alongside a report's measured payloads:
//! how the stimulus was produced, what the interface and calibration
//! looked like, and what the environment was. None of these hold measured
//! values; they are what makes the values in [`super::MeasurementPayload`]
//! interpretable a year later.

use serde::{Deserialize, Serialize};

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

#[cfg(test)]
mod tests {
    use super::super::fixtures::*;
    use super::super::*;

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
    fn interface_latency_round_trips_through_json() {
        let mut r = ir_report_with_peak(1_024, 512, 1.0, 0.0, 48_000);
        r.interface_latency = Some(measured_tau(0.0011931));
        let back: MeasurementReport = serde_json::from_str(&r.to_json().unwrap()).unwrap();
        assert_eq!(back.interface_latency, r.interface_latency);
    }

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
}
