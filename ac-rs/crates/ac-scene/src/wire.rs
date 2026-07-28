//! Deserialization types for the `transfer_stream` v2 DATA frame
//! (`ZMQ.md` lines ~1572-1627). `serde` ignores JSON fields this struct
//! doesn't name, so a real wire frame deserializes fine even though this
//! struct is a subset of the schema.
//!
//! The spectrum-view fields (M2, architect review decision 1) and the
//! transfer-view fields (M4a, §4.1/§4.2) both live here. The transfer
//! fields carry `#[serde(default)]` so a frame without H1 content still
//! parses into an (empty-trace) spectrum scene rather than failing the
//! whole deserialize — `ac-view` drops unparseable frames silently
//! (`app.rs`), so a hard requirement here would show up as a blank view,
//! not as an error.
//!
//! # `phase_deg` is NOT raw phase
//!
//! The daemon delay-compensates before forming H1:
//! `ac-core/src/visualize/transfer.rs` multiplies `Gxy` by
//! `exp(+j·2π·f·delay_samples/sr)` and takes `phase_deg = h1.arg()`
//! after that, with `delay_samples` estimated once per session and
//! frozen (D4). So this field carries
//!
//! ```text
//! φ_wire(f) = φ_raw(f) + 360·f·τ_sess
//! ```
//!
//! already de-rotated by the session's own delay. [`crate::transfer`]
//! owns the mapping from a display mode to the τ_derot that must be
//! applied on top of it; see that module's doc for the table. Treating
//! this field as raw phase double-compensates — the failure #180's
//! architect pass exists to have caught.

use serde::Deserialize;

/// Observed per-leg drive-path connection state (#205), echoed verbatim from
/// the frame's `conn_tags`. Values use the same `"on" | "off" | "none"`
/// vocabulary as `cal_tags`. Strings rather than an enum on purpose: this crate
/// echoes the daemon's vocabulary and must not silently normalise a value it
/// does not recognise into a healthy one.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ConnTags {
    #[serde(default)]
    pub out: Option<String>,
    #[serde(default)]
    pub ref_out: Option<String>,
}

/// One ladder rung's parameters, echoed in every frame so a saved frame stays
/// interpretable without knowing the daemon's layout rules.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MtwStage {
    #[serde(default)]
    pub decim: usize,
    #[serde(default)]
    pub rate: f64,
    #[serde(default)]
    pub df: f64,
    #[serde(default)]
    pub window_s: f64,
    /// Segment hop. `window_s / 2` for the 50% overlap the estimator uses —
    /// checked rather than assumed, see [`MtwColumns::variance_equivalent_n`].
    #[serde(default)]
    pub hop_s: f64,
    #[serde(default)]
    pub f_valid: f64,
    #[serde(default)]
    pub settling_s: f64,
}

/// The three-stage transfer columns — the display's actual source.
///
/// Column spacing is **not uniform** in log frequency: where the requested
/// density exceeds what a rung resolves, the grid widens instead of
/// interpolating. Anything consuming this must map each column by its own
/// `freqs[i]` rather than by index (which `freq_to_x` already does).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MtwColumns {
    pub freqs: Vec<f64>,
    #[serde(default)]
    pub f_lo: Vec<f64>,
    #[serde(default)]
    pub f_hi: Vec<f64>,
    pub magnitude_db: Vec<f64>,
    pub phase_deg: Vec<f64>,
    pub coherence: Vec<f64>,
    /// Bin width behind each column, in Hz.
    #[serde(default)]
    pub df: Vec<f64>,
    /// Analysis window behind each column, in seconds.
    #[serde(default)]
    pub window_s: Vec<f64>,
    /// Blocks actually averaged behind each column.
    ///
    /// This is a raw input, not a depth. The effective averaging depth is set
    /// by blocks **and** by [`Self::bins`], and no validated model combines
    /// them — see `design-mtw-ladder.md`. Do not derive a "corrected" figure
    /// from this field; the last two attempts were both further from the
    /// truth than the uncorrected value.
    #[serde(default)]
    pub n: Vec<f64>,
    #[serde(default)]
    pub stage: Vec<usize>,
    #[serde(default)]
    pub blend: Vec<f64>,
    /// Source bins behind each column. Never zero on a conforming frame —
    /// this is the honest-density guarantee made observable.
    #[serde(default)]
    pub bins: Vec<usize>,
    #[serde(default)]
    pub ppo: f64,
    #[serde(default)]
    pub n_blocks: usize,
    #[serde(default)]
    pub stages: Vec<MtwStage>,
}

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

    // ---- transfer view (§4.1) — the H grid, DISTINCT from `spec_freqs` ----
    /// H1 column centre frequencies. Not `spec_freqs`: that is the
    /// log-spaced spectrum grid, this is the linear FFT grid the H1
    /// estimate is computed on.
    #[serde(default)]
    pub freqs: Vec<f64>,
    /// `|H|` in dB, same length as [`Self::freqs`].
    #[serde(default)]
    pub magnitude_db: Vec<f64>,
    /// `arg(H)` in degrees, wrapped to `(-180, +180]` (the range of
    /// `Complex::arg`). **Already delay-compensated** — see the module
    /// doc.
    #[serde(default)]
    pub phase_deg: Vec<f64>,
    /// γ² in `[0,1]`, same length as [`Self::freqs`].
    #[serde(default)]
    pub coherence: Vec<f64>,
    /// The session's frozen delay estimate, in samples.
    #[serde(default)]
    pub delay_samples: i64,
    /// The same estimate in ms — τ_sess, the quantity the de-rotation
    /// mapping is written in terms of.
    #[serde(default)]
    pub delay_ms: f64,

    // ---- input level meters (§4.2) ----
    /// Raw capture peak, `20·log10(max|sample|)` over the frame's
    /// blocks, taken BEFORE any calibration or aggregation. `null` on
    /// the wire when the frame's peak is `-inf` (digital silence).
    ///
    /// `None` covers both `null` and a field absent entirely (a daemon
    /// predating §4.2). That collapse is deliberate and load-bearing:
    /// the display truth is identical for "silent" and "old daemon",
    /// so nothing downstream may distinguish them (M4a AC — no version
    /// sniffing).
    #[serde(default)]
    pub meas_peak_dbfs: Option<f64>,
    /// Same, reference channel.
    #[serde(default)]
    pub ref_peak_dbfs: Option<f64>,
    /// Observed drive-path connection state per leg (#205).
    ///
    /// `None` — the field absent — means the daemon **cannot observe** its
    /// graph (a non-JACK backend, `--fake-audio`, or a daemon predating #205).
    /// Unlike `meas_peak_dbfs` above, this collapse is *not* to a healthy
    /// default: unobservable and connected are different display truths, and
    /// rendering the first as the second is the exact lie #205 exists to fix.
    #[serde(default)]
    pub conn_tags: Option<ConnTags>,

    // ---- three-stage transfer columns — the display's source ----
    /// `None` until every ladder rung holds a full N blocks (2.56 s at the
    /// bottom rung), and on any daemon predating the ladder.
    ///
    /// Absent is **not** a reason to fall back to the Welch arrays above: the
    /// two are different measurements, and silently swapping between them
    /// mid-session would make the display's resolution and settling change
    /// without saying so. A frame without this contributes no transfer trace.
    #[serde(default)]
    pub mtw: Option<MtwColumns>,
}

impl MtwColumns {
    /// Every parallel array is the same length as `freqs`.
    ///
    /// The arrays are independent JSON fields, so nothing guarantees a short
    /// one is a truncation rather than a misalignment. Same argument as the
    /// Welch path's length check: a mismatched frame draws nothing rather than
    /// drawing a guess.
    pub fn lengths_agree(&self) -> bool {
        let n = self.freqs.len();
        self.magnitude_db.len() == n
            && self.phase_deg.len() == n
            && self.coherence.len() == n
            && (self.df.is_empty() || self.df.len() == n)
            && (self.window_s.is_empty() || self.window_s.len() == n)
            && (self.bins.is_empty() || self.bins.len() == n)
    }
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
