//! Shared data types that cross module boundaries.
//!
//! All types derive `Serialize`/`Deserialize` so they can be sent over ZMQ
//! as JSON without extra boilerplate in the daemon layer.

use serde::{Deserialize, Serialize};

/// Full result from a single [`crate::analysis::analyze`] call.
///
/// Field names are kept identical to the Python `analyze()` return dict so
/// the Python client deserialises them without changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisResult {
    /// Detected fundamental frequency (Hz). Equal to the `fundamental`
    /// argument; the peak finder may shift it to the nearest bin.
    pub fundamental_hz: f64,

    /// Level of the fundamental in the windowed spectrum (dBFS).
    /// Uses the same window-corrected normalization as the Python server.
    pub fundamental_dbfs: f64,

    /// Time-domain RMS of the captured signal (5% trim from each end).
    /// Multiply by `vrms_at_0dbfs_in` (from calibration) to get physical Vrms.
    pub linear_rms: f64,

    /// Total Harmonic Distortion (%).
    pub thd_pct: f64,

    /// Total Harmonic Distortion + Noise (%).
    pub thdn_pct: f64,

    /// Harmonic amplitudes: `[(freq_hz, amplitude), ...]` for 2nd, 3rd, …
    /// Serialises as JSON `[[f, a], [f, a], ...]` — matches Python tuple list.
    pub harmonic_levels: Vec<(f64, f64)>,

    /// Noise floor estimated by subtracting all harmonics from the time
    /// domain and computing the residual RMS (dBFS).
    pub noise_floor_dbfs: f64,

    /// One-sided amplitude spectrum (magnitude, windowed + normalised).
    /// Length = N/2 + 1.  Downsampled to ≤ 1000 points by the daemon before
    /// sending over ZMQ.
    pub spectrum: Vec<f64>,

    /// Frequency axis for `spectrum` (Hz).  `freqs[k] = k * sr / N`.
    pub freqs: Vec<f64>,

    /// `true` if any sample (after 5% trim) reached ≥ 0.9999 FS.
    pub clipping: bool,

    /// `true` when the 2nd harmonic dominates THD at low frequencies,
    /// indicating a capacitively-coupled path rather than real distortion.
    pub ac_coupled: bool,
}

/// One column of a Morlet CWT waterfall — a set of magnitudes sampled at
/// the centre of the analysed buffer, one per log-spaced scale.
///
/// Published on DATA:5557 in place of spectrum frames when the daemon's
/// analysis mode is `"cwt"`. Consumers reach into `frequencies` directly as
/// a log-spaced frequency axis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CwtFrame {
    /// Magnitude per scale in dBFS, length equals `frequencies.len()`.
    /// Already `1/sqrt(scale)`-normalised so equal-amplitude sines land at
    /// the same dB regardless of their centre frequency.
    pub magnitudes: Vec<f32>,

    /// Hz per scale (log-spaced), one-for-one with `magnitudes`. UI uses
    /// this as the waterfall's frequency axis with `log_spaced = true`.
    pub frequencies: Vec<f32>,

    /// Monotonic timestamp in nanoseconds; convention matches the other
    /// ac-ui frame timestamps.
    pub timestamp: u64,
}
