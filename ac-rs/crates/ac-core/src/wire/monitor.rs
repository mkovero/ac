//! The `monitor_spectrum` DATA frames in the first cut: `visualize/spectrum`
//! (`ZMQ.md`, `### spectrum frame`) and its `measurement/loudness` sidecar
//! (`ZMQ.md`, `### measurement/loudness frame`).
//!
//! The monitor's other frames (`visualize/cwt`, `cqt`, `reassigned`,
//! `fractional_octave`, `fractional_octave_leq`, `scope`) are still built as
//! JSON; typing them is a #112 follow-up.

use serde::{Deserialize, Serialize};

use crate::shared::calibration::LayerVerdict;

/// The `visualize/spectrum` frame, emitted per channel per tick in FFT mode.
///
/// Two shapes. When THD analysis finds a fundamental the frame carries the
/// tone readouts (`freq_hz` … `clipping`); when it does not, those keys are
/// **absent**, not `null`. Each is `skip_serializing_if = "Option::is_none"`
/// so the two shapes survive a round trip. `in_dbu` is the one tone readout
/// that is `null` inside the THD branch (no voltage calibration), so it is a
/// double option: absent, present-and-`null`, or a value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpectrumFrame {
    /// Always `"visualize/spectrum"` on this frame.
    #[serde(rename = "type", default)]
    pub frame_type: String,
    /// Always `"monitor_spectrum"`.
    #[serde(default)]
    pub cmd: String,
    /// Stamped at the daemon's publish seam; see
    /// [`crate::wire::check_wire_version`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wire_version: Option<u32>,
    /// Input channel index this frame describes.
    pub channel: u32,
    /// Total channels being monitored (frame count per cycle).
    #[serde(default)]
    pub n_channels: u32,
    /// Sample rate, Hz.
    #[serde(default)]
    pub sr: u32,
    /// Column centre frequencies, log-spaced.
    #[serde(deserialize_with = "super::nullable_f64_vec")]
    pub freqs: Vec<f64>,
    /// Linear amplitude, one-sided — NOT dB. A non-finite column travels as
    /// `null` and reads back as NaN.
    #[serde(deserialize_with = "super::nullable_f64_vec")]
    pub spectrum: Vec<f64>,
    /// dBFS → dBu offset; `null` when uncalibrated or refused.
    #[serde(default)]
    pub dbu_offset_db: Option<f64>,
    /// #466: recorded verdict on the channel's stored scale, taken once at
    /// monitor start; `null` when none is stored.
    #[serde(default)]
    pub voltage_check: Option<LayerVerdict>,
    /// Additive dBFS → dB SPL offset; `null` without an SPL calibration.
    #[serde(default)]
    pub spl_offset_db: Option<f64>,
    /// `"on"` | `"off"` | `"none"` — mic frequency-response state.
    #[serde(default)]
    pub mic_correction: String,
    /// Engine xrun count.
    #[serde(default)]
    pub xruns: u32,
    /// The engine that produced the frame.
    #[serde(default)]
    pub backend: String,

    // ---- THD branch only: absent when no fundamental was resolved ----
    /// Auto-detected dominant frequency, Hz.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freq_hz: Option<f64>,
    /// Parabolic-interpolated `[freq_hz, dbfs]` pairs, strongest first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peaks: Option<Vec<[f64; 2]>>,
    /// Fundamental level, dBFS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fundamental_dbfs: Option<f64>,
    /// Harmonic residual / total output, percent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thd_pct: Option<f64>,
    /// Notched residual / total output, percent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thdn_pct: Option<f64>,
    /// Analog-domain level, dBu. `Some(None)` (wire `null`) inside the THD
    /// branch when the channel has no voltage calibration; `None` (absent)
    /// outside it.
    #[serde(
        default,
        deserialize_with = "super::present",
        skip_serializing_if = "Option::is_none"
    )]
    pub in_dbu: Option<Option<f64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clipping: Option<bool>,
}

/// The `measurement/loudness` sidecar (BS.1770-5 / EBU R128), emitted per
/// channel per tick alongside the monitor's spectrum-shaped frame.
///
/// No in-tree consumer reads it (#112 assumption A1); its guard is the
/// lossless round-trip and the `ZMQ.md` key parity check.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoudnessFrame {
    /// Always `"measurement/loudness"`.
    #[serde(rename = "type", default)]
    pub frame_type: String,
    /// Always `"monitor_spectrum"`.
    #[serde(default)]
    pub cmd: String,
    /// Stamped at the daemon's publish seam.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wire_version: Option<u32>,
    #[serde(default)]
    pub channel: u32,
    #[serde(default)]
    pub n_channels: u32,
    #[serde(default)]
    pub sr: u32,
    /// 400 ms window; `null` pre-gate.
    #[serde(default)]
    pub momentary_lkfs: Option<f64>,
    /// 3 s window.
    #[serde(default)]
    pub short_term_lkfs: Option<f64>,
    /// Gated, since worker start (or last reset).
    #[serde(default)]
    pub integrated_lkfs: Option<f64>,
    /// EBU Tech 3342 loudness range.
    #[serde(default)]
    pub lra_lu: f64,
    /// 4× polyphase oversampled peak.
    #[serde(default)]
    pub true_peak_dbtp: Option<f64>,
    /// Wall-clock since the gate first opened.
    #[serde(default)]
    pub gated_duration_s: f64,
    /// When set, the LKFS values render as K-weighted dB SPL.
    #[serde(default)]
    pub spl_offset_db: Option<f64>,
    /// Whether the LKFS values were computed on mic-corrected samples.
    #[serde(default)]
    pub mic_correction: String,
    /// UNIX-epoch nanoseconds.
    #[serde(default)]
    pub timestamp: u64,
    #[serde(default)]
    pub xruns: u32,
    #[serde(default)]
    pub backend: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn round_trip<T: Serialize + for<'de> Deserialize<'de>>(v: &Value) -> Value {
        serde_json::to_value(serde_json::from_value::<T>(v.clone()).expect("deserialize"))
            .expect("serialize")
    }

    #[test]
    fn spectrum_no_thd_branch_keeps_tone_readouts_absent() {
        let v = json!({
            "type": "visualize/spectrum", "cmd": "monitor_spectrum", "channel": 0,
            "n_channels": 1, "sr": 48000, "freqs": [100.0], "spectrum": [0.5],
            "dbu_offset_db": null, "voltage_check": null, "spl_offset_db": null,
            "mic_correction": "none", "xruns": 0, "backend": "fake"
        });
        assert_eq!(round_trip::<SpectrumFrame>(&v), v);
    }

    #[test]
    fn spectrum_thd_branch_keeps_a_null_in_dbu_present() {
        let v = json!({
            "type": "visualize/spectrum", "cmd": "monitor_spectrum", "channel": 0,
            "n_channels": 1, "sr": 48000, "freqs": [100.0], "spectrum": [0.5],
            "dbu_offset_db": null, "voltage_check": null, "spl_offset_db": null,
            "mic_correction": "none", "xruns": 0, "backend": "fake",
            "freq_hz": 1000.0, "peaks": [[1000.0, -20.0]], "fundamental_dbfs": -20.0,
            "thd_pct": 0.01, "thdn_pct": 0.02, "in_dbu": null, "clipping": false
        });
        assert_eq!(round_trip::<SpectrumFrame>(&v), v);
    }

    #[test]
    fn a_null_spectrum_column_reads_as_nan_and_writes_back_as_null() {
        let v = json!({"channel": 0, "freqs": [1.0, 2.0], "spectrum": [null, 0.5]});
        let f: SpectrumFrame = serde_json::from_value(v).unwrap();
        assert!(f.spectrum[0].is_nan());
        assert_eq!(
            serde_json::to_value(&f).unwrap()["spectrum"],
            json!([null, 0.5])
        );
    }
}
