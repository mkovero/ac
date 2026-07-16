//! Tier 2 — full calibrated derivation for one meas/ref pair: H1, per-
//! channel calibrated spectra (D18), and a broadband `spl` scalar, from
//! raw ref/meas samples and calibration.
//!
//! This is the same sequence `ac-daemon`'s live `transfer_stream` handler
//! composes per pair per tick (handoff: transfer-frame-v2 M0) — pulled out
//! here so `ac-core::snapshot`'s offline reprocessing (handoff: snapshot-
//! backend M1, deliverable 5, "no reimplementation") calls the identical
//! low-level functions (`h1_estimate_with_delay`, `spectrum_to_columns_wire`,
//! `weighted_broadband_dbfs`) the live path does. The live daemon handler
//! itself is not yet refactored to call this composition (a separate,
//! lower-risk follow-up on already-shipped M0 code) — both call sites
//! reduce to the same primitives either way.

use crate::shared::calibration::Calibration;
use crate::visualize::aggregate::{spectrum_to_columns_wire, transfer_spectrum_n_columns};
use crate::visualize::spl_level::weighted_broadband_dbfs;
use crate::visualize::transfer::{h1_estimate_with_delay, TransferResult};
use crate::visualize::weighting_curves::WeightingCurve;

/// Fixed log-column grid lower bound (D18) — 20 Hz, matching the M0 wire
/// contract and `ac-daemon`'s `spec_f_min`.
pub const SPEC_F_MIN_HZ: f64 = 20.0;

/// Full calibrated derivation for one pair: H1 plus the calibrated
/// spectra and SPL that would appear on a `transfer_stream` wire frame
/// for the same raw input.
pub struct PairDerivation {
    pub h1: TransferResult,
    pub spec_freqs: Vec<f64>,
    /// Linear amplitude, calibrated (voltage + mic curve), band-power
    /// aggregated — same contract as the wire's `meas_spectrum`.
    pub meas_spectrum: Vec<f64>,
    /// Same, reference channel (no mic curve, matches the wire contract).
    pub ref_spectrum: Vec<f64>,
    /// Weighted broadband SPL for this window, computed from the
    /// *uncalibrated* (pre-voltage-cal) amplitude — see
    /// `shared::calibration`'s "layer topology" module doc: voltage cal
    /// and SPL cal are parallel readings off the same raw digital
    /// amplitude, not composed. `None` when `meas_cal` has
    /// no SPL calibration layer. Unlike the live wire's `spl` (which is
    /// F/S time-integrated across ticks), this is the single-window
    /// value — the natural quantity for a static reprocessed capture;
    /// callers wanting a time-evolving trace can call `derive_pair` on
    /// successive sub-windows and integrate themselves.
    pub spl: Option<f64>,
    /// The weighting curve actually used to compute `spl` on *this*
    /// call — the caller's argument, echoed back (handoff: parity-
    /// completion M1.5, deliverable 3's edge case). Reprocessing under a
    /// weighting different from the snapshot's capture-time
    /// `ChannelMeta::weighting` is expected and supported (D10/D11
    /// edit-time freedom) — this field is what tells a caller which one
    /// actually produced `spl`, so it never has to be inferred from (or
    /// confused with) the capture-time provenance, which stays untouched
    /// in `SnapshotMeta`.
    pub spl_weighting: WeightingCurve,
}

/// Subtract `curve`'s per-frequency correction from `amp` in the linear
/// domain (`amp *= 10^(-correction_db/20)`) — the mic over-reads by
/// `correction_db`, so this recovers the acoustic truth. Same scaling
/// `ac-daemon`'s live path applies inline; small enough, and specific
/// enough to the linear-amplitude convention, that it doesn't warrant a
/// third home beyond "wherever needs it" (the daemon's dB-domain sibling,
/// `apply_mic_curve_inplace_f64`, lives in `ac-daemon::handlers::mic`
/// since it predates this module and operates in a different domain).
fn apply_mic_curve_linear(
    curve: &crate::shared::calibration::MicResponse,
    freqs: &[f64],
    amp: &mut [f64],
) {
    for (a, &f) in amp.iter_mut().zip(freqs.iter()) {
        let corr_db = curve.correction_at(f as f32) as f64;
        *a *= 10f64.powf(-corr_db / 20.0);
    }
}

/// Derive H1 + calibrated spectra + SPL for one pair from raw ref/meas
/// samples. `delay_samples` is the pre-estimated ref↔meas propagation
/// delay (see `estimate_delay_samples`); `weighting` is the session's
/// (or, for reprocessing, the caller's chosen) SPL weighting curve.
pub fn derive_pair(
    ref_samples: &[f32],
    meas_samples: &[f32],
    sr: u32,
    delay_samples: i64,
    meas_cal: Option<&Calibration>,
    ref_cal: Option<&Calibration>,
    weighting: WeightingCurve,
) -> PairDerivation {
    let h1 = h1_estimate_with_delay(ref_samples, meas_samples, sr, delay_samples);

    let mut mc_meas_amp = h1.meas_amp.clone();
    if let Some(curve) = meas_cal.and_then(|c| c.mic_response.as_ref()) {
        apply_mic_curve_linear(curve, &h1.freqs, &mut mc_meas_amp);
    }

    let spl = meas_cal
        .and_then(Calibration::spl_offset_db)
        .map(|offset| weighted_broadband_dbfs(&mc_meas_amp, &h1.freqs, weighting) + offset);

    let mut meas_amp_wire = mc_meas_amp;
    if let Some(scale) = meas_cal.and_then(|c| c.vrms_at_0dbfs_in) {
        for v in meas_amp_wire.iter_mut() {
            *v *= scale;
        }
    }
    let mut ref_amp_wire = h1.ref_amp.clone();
    if let Some(scale) = ref_cal.and_then(|c| c.vrms_at_0dbfs_in) {
        for v in ref_amp_wire.iter_mut() {
            *v *= scale;
        }
    }

    let f_max = sr as f64 / 2.0;
    let n_columns = transfer_spectrum_n_columns(SPEC_F_MIN_HZ, f_max);
    let (meas_spectrum, spec_freqs) =
        spectrum_to_columns_wire(&meas_amp_wire, sr as f64, SPEC_F_MIN_HZ, f_max, n_columns);
    let (ref_spectrum, _) =
        spectrum_to_columns_wire(&ref_amp_wire, sr as f64, SPEC_F_MIN_HZ, f_max, n_columns);

    PairDerivation {
        h1,
        spec_freqs,
        meas_spectrum,
        ref_spectrum,
        spl,
        spl_weighting: weighting,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 48_000;
    const N: usize = 3 * SR as usize;

    fn sine(n: usize, freq_hz: f64, sr: u32, amp: f64) -> Vec<f32> {
        (0..n)
            .map(|i| {
                (amp * (2.0 * std::f64::consts::PI * freq_hz * i as f64 / sr as f64).sin()) as f32
            })
            .collect()
    }

    #[test]
    fn uncalibrated_pair_has_no_spl_but_full_spectra() {
        let sig = sine(N, 1_000.0, SR, 0.3);
        let d = derive_pair(&sig, &sig, SR, 0, None, None, WeightingCurve::Z);
        assert!(d.spl.is_none());
        assert!(!d.meas_spectrum.is_empty());
        assert!(!d.ref_spectrum.is_empty());
        assert_eq!(d.spec_freqs.len(), d.meas_spectrum.len());
        assert_eq!(d.spec_freqs.len(), d.ref_spectrum.len());
    }

    #[test]
    fn spl_calibrated_pair_reports_offset_applied() {
        let sig = sine(N, 1_000.0, SR, 0.3);
        let cal = Calibration {
            output_channel: 0,
            input_channel: 0,
            ref_freq: 1000.0,
            vrms_at_0dbfs_out: None,
            vrms_at_0dbfs_in: None,
            ref_dbfs: -10.0,
            mic_sensitivity_dbfs_at_94db_spl: Some(-20.0),
            mic_response: None,
        };
        let offset = cal.spl_offset_db().unwrap();
        let d = derive_pair(&sig, &sig, SR, 0, Some(&cal), None, WeightingCurve::Z);
        let spl = d
            .spl
            .expect("spl must be Some when meas_cal has an SPL layer");
        // Sanity: spl - offset should be a plausible dBFS broadband level
        // for a -10 dBFS-ish tone (not literally -10, since it's a
        // full-band power sum, but nowhere near +/-100 dB off).
        assert!(
            (spl - offset).abs() < 60.0,
            "spl-offset={} looks implausible for a 0.3-amplitude tone",
            spl - offset
        );
    }

    #[test]
    fn voltage_cal_scales_meas_spectrum_but_not_spl() {
        let sig = sine(N, 1_000.0, SR, 0.3);
        let cal_uncal = None;
        let d_uncal = derive_pair(&sig, &sig, SR, 0, cal_uncal, None, WeightingCurve::Z);

        let cal_v = Calibration {
            output_channel: 0,
            input_channel: 0,
            ref_freq: 1000.0,
            vrms_at_0dbfs_out: None,
            vrms_at_0dbfs_in: Some(2.0),
            ref_dbfs: -10.0,
            mic_sensitivity_dbfs_at_94db_spl: None,
            mic_response: None,
        };
        let d_v = derive_pair(&sig, &sig, SR, 0, Some(&cal_v), None, WeightingCurve::Z);

        let peak_uncal = d_uncal
            .meas_spectrum
            .iter()
            .cloned()
            .fold(0.0_f64, f64::max);
        let peak_v = d_v.meas_spectrum.iter().cloned().fold(0.0_f64, f64::max);
        assert!(
            (peak_v / peak_uncal - 2.0).abs() < 0.01,
            "voltage cal (scale=2.0) should double meas_spectrum's peak: uncal={peak_uncal} v={peak_v}"
        );
        // spl is None for both (no SPL layer on either cal) — voltage
        // cal must not have produced one.
        assert!(d_uncal.spl.is_none());
        assert!(d_v.spl.is_none());
    }
}
