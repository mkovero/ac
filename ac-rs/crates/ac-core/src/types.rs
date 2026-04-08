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
