//! The measured payloads a report carries, and the per-payload gate that
//! makes a derived one reproducible. One capture can yield several
//! payloads (#280), so the citation and gate live here rather than on the
//! report as a whole.

use serde::{Deserialize, Serialize};

use super::StandardsCitation;

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

    /// Human-readable heading for the variant, used by the HTML and PDF
    /// renderers. Lives here rather than in either renderer so the two
    /// cannot drift apart in wording (they each carried a byte-identical
    /// private copy).
    pub fn display_title(&self) -> &'static str {
        match self {
            MeasurementData::FrequencyResponse { .. } => "Frequency Response",
            MeasurementData::SpectrumBands { .. } => "Spectrum Bands",
            MeasurementData::ImpulseResponse { .. } => "Impulse Response (Farina log sweep)",
            MeasurementData::NoiseResult { .. } => "Idle-channel Noise (AES17)",
            MeasurementData::GatedFrequencyResponse { .. } => "Frequency Response (gated)",
        }
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

#[cfg(test)]
mod tests {
    use super::super::fixtures::*;
    use super::super::*;

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

    #[test]
    fn spectrum_bands_round_trip() {
        let r = sample_spectrum_bands_report();
        let json = r.to_json().unwrap();
        let r2: MeasurementReport = serde_json::from_str(&json).unwrap();
        assert_eq!(r, r2);
    }

    #[test]
    fn impulse_response_round_trip() {
        let r = sample_impulse_response_report();
        let json = r.to_json().unwrap();
        let r2: MeasurementReport = serde_json::from_str(&json).unwrap();
        assert_eq!(r, r2);
    }

    #[test]
    fn noise_result_round_trip() {
        let r = sample_noise_report();
        let json = r.to_json().unwrap();
        let r2: MeasurementReport = serde_json::from_str(&json).unwrap();
        assert_eq!(r, r2);
    }

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
