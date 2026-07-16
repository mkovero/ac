//! Deserialization types for the `transfer_stream` v2 DATA frame
//! (`ZMQ.md` lines ~1572-1627). Deliberately narrow — only the fields
//! this crate's V1 spectrum view uses (architect review, decision 1);
//! H1/phase/coherence trace fields are M4+ scope and are not modelled
//! here. `serde` ignores JSON fields this struct doesn't name, so a real
//! wire frame deserializes fine even though this struct is a strict
//! subset of the schema.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct WireFrame {
    pub sr: u32,
    pub meas_channel: i64,
    pub ref_channel: i64,
    /// Log-spaced column centre frequencies, identical every frame in a
    /// session (D18).
    pub spec_freqs: Vec<f64>,
    /// LINEAR amplitude, band-power aggregated, calibrated. NOT dB —
    /// see [`crate::dbfs::linear_to_dbfs`], the crate's one conversion
    /// site.
    pub meas_spectrum: Vec<f64>,
    /// Same, reference channel (no mic curve).
    pub ref_spectrum: Vec<f64>,
    /// `null` when the meas channel has no SPL calibration layer.
    pub spl: Option<f64>,
    /// `"A"` | `"C"` | `"Z"` — echoes the session's `weighting` param.
    pub spl_weighting: String,
    /// `"fast"` | `"slow"` — echoes the session's `integration` param.
    pub spl_integration: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserializes_a_minimal_real_shaped_frame_ignoring_unknown_fields() {
        let json = r#"{
            "type": "transfer_stream",
            "cmd": "transfer_stream",
            "freqs": [1.0, 2.0],
            "magnitude_db": [0.0, 0.0],
            "phase_deg": [0.0, 0.0],
            "coherence": [1.0, 1.0],
            "re": [1.0, 1.0],
            "im": [0.0, 0.0],
            "delay_samples": 0,
            "delay_ms": 0.0,
            "meas_channel": 0,
            "ref_channel": 1,
            "sr": 48000,
            "mic_correction": "none",
            "spec_freqs": [100.0, 1000.0],
            "meas_spectrum": [0.1, 0.375],
            "ref_spectrum": [0.05, 0.2],
            "spl": -6.75,
            "spl_weighting": "Z",
            "spl_integration": "fast",
            "cal_tags": {
                "meas": {"voltage": "on", "spl": "on", "mic_curve": "none"},
                "ref": {"voltage": "on", "spl": "none", "mic_curve": "none"}
            }
        }"#;
        let frame: WireFrame = serde_json::from_str(json).expect("deserialize");
        assert_eq!(frame.sr, 48000);
        assert_eq!(frame.spec_freqs, vec![100.0, 1000.0]);
        assert_eq!(frame.spl, Some(-6.75));
        assert_eq!(frame.spl_weighting, "Z");
        assert_eq!(frame.spl_integration, "fast");
    }
}
