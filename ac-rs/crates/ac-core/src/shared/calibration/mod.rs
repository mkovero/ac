//! Calibration data — mirrors `ac/server/jack_calibration.py`.
//!
//! Reads and writes `~/.config/ac/cal.json`.  Key format: `out{N}_in{M}`.
//! The file is a flat JSON object; each value is a [`CalibrationEntry`].
//!
//! # Layer topology: voltage cal and SPL cal are parallel, not composed
//!
//! `vrms_at_0dbfs_in` (electrical) and `mic_sensitivity_dbfs_at_94db_spl`
//! (acoustic) are two **independent** readings off the same raw digital
//! amplitude — neither is applied through the other. Concretely: SPL
//! ([`Calibration::spl_offset_db`]) is computed from the *uncalibrated*
//! dBFS amplitude, not from the voltage-cal-scaled one.
//!
//! This is deliberate, not an oversight one call site happened to get
//! right. The 94 dB SPL pistonphone reference tone is itself captured as
//! a raw digital level, so `mic_sensitivity_dbfs_at_94db_spl` already
//! represents "what 94 dB SPL reads as raw dBFS". `dbspl = dbfs -
//! mic_sens + 94.0` therefore needs `dbfs` to be the same raw quantity
//! the pistonphone reading was taken against. Multiplying by
//! `vrms_at_0dbfs_in` first would rescale one side of that equation and
//! not the other, silently breaking the SPL reading the moment a
//! channel picks up an electrical calibration — voltage cal and SPL cal
//! would no longer agree on what "0 dBFS" means.
//!
//! Every call site that computes an absolute SPL number follows this
//! topology and is expected to keep doing so:
//! [`crate::visualize::pair_derivation::derive_pair`] (`spl` is derived
//! from `h1.meas_amp` before `meas_amp_wire`'s voltage scaling is
//! applied), `ac-daemon`'s live `transfer_stream` handler (same split,
//! `mc_meas_amp` vs. `meas_amp_wire`), and `monitor.rs` (never scales
//! `spec/cwt_mags` by `vrms_at_0dbfs_in`; voltage info ships only as the
//! separate `dbu_offset_db` field).
//!
//! **All three are machine-covered**, by two tests in `ac-daemon`'s
//! `it_cross_tier_parity.rs`, each asserting its own side stays put across
//! a *non-trivial* voltage-cal scale change rather than the trivial/unset
//! case: `parity_transfer_spl_is_independent_of_voltage_cal_scale` for
//! `derive_pair` and the `transfer_stream` handler, and
//! `parity_monitor_spl_is_independent_of_voltage_cal_scale` (#261) for
//! `monitor.rs`. Both were confirmed red against a deliberately composed
//! derivation before landing.
//!
//! **The invariant is that SPL does not move — never that it moves by
//! some amount when broken.** The symptom's size is
//! `20·log10(vrms_at_0dbfs_in)`, which is bounded below by nothing: a rig
//! whose full scale is 1 Vrms stores 1.0 and composes with an error of
//! **exactly 0 dB**. That is ordinary hardware, and on it the composed and
//! correct topologies are indistinguishable by the reading alone. No field
//! diagnosis can work from magnitude. The only reliable signal is the one
//! the tests assert: SPL changing when the *voltage* calibration changed
//! and nothing else did.
//!
//! **What composition costs when it is visible, measured rather than
//! predicted.** That constant is *Vrms extrapolated to full scale*,
//! `reading / amplitude(measured dBFS)` — not the operator's DMM reading.
//! In #261's falsification run a 5.0 V reading taken at a −13.01 dBFS cal
//! capture stored 22.36, so the monitor SPL moved **26.99 dB**, against the
//! 13.98 dB a reader would predict from the reading alone. Any bound sized
//! against 14 dB is safe; any *diagnosis* expecting 14 dB misreads the
//! symptom.
//!
//! **Re-calibrating does not mask it.** `calibrate_spl` derives
//! `mic_sensitivity_dbfs_at_94db_spl` from `capture_rms` on raw amplitude
//! and never reads `vrms_at_0dbfs_in`; `spl_offset_db` measured identical
//! (117.010) either side of the composed run. So the SPL layer stays
//! correct while the reading is wrong, and the first thing an operator
//! would try at the rig — re-run the SPL calibration — changes nothing.
//!
//! # A third parallel layer: interface latency (τ), issue #281
//!
//! [`Calibration::tau_history`] is a third layer under the same rule: no
//! function introduced for it takes a voltage field to produce a τ, or a
//! `TauEntry` to produce a Vrms/dBu/dB SPL value. τ is a property of
//! *(device, backend, sample rate, period size, port pair)* — not of the
//! electrical or acoustic calibration — so it is kept as an append-only
//! history looked up by exact condition match ([`Calibration::tau_for`]),
//! never applied through the voltage or SPL layers and never averaged or
//! interpolated across history entries. `tau_history_does_not_affect_
//! voltage_or_spl_derivations` below is the parity test for this layer;
//! there is no consumer of `tau_history` yet (applying τ to a live
//! measurement is out of scope for #281), so unlike the voltage/SPL pair
//! there is no second call site to test for the same independence yet.
//!
//! # Where each layer lives
//!
//! One file per layer, so "parallel, not composed" is a module boundary
//! and not only a paragraph: [`tau`] (τ), [`mic_response`] (mic curve),
//! and this module (voltage + SPL, the two fields read off the same raw
//! amplitude). [`store`] owns `cal.json` for all of them.

mod mic_response;
mod store;
mod tau;

pub use mic_response::{parse_mic_curve, MicResponse};
pub use store::{cal_key, default_cal_path};
pub use tau::{
    compare_tau_readings, TauComparison, TauConditions, TauDisagreement, TauEntry, TauRefusal,
};

use serde::{Deserialize, Serialize};

use crate::shared::conversions::dbfs_to_vrms;

/// Raw JSON representation stored in `cal.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationEntry {
    pub output_channel: u32,
    pub input_channel: u32,
    #[serde(default = "default_ref_freq")]
    pub ref_freq: f64,
    pub vrms_at_0dbfs_out: Option<f64>,
    pub vrms_at_0dbfs_in: Option<f64>,
    #[serde(default = "default_ref_dbfs")]
    pub ref_dbfs: f64,
    /// Captured input level (dBFS) when a 94 dB SPL pistonphone reference
    /// is applied to this channel. With this value, any other dBFS reading
    /// converts to dB SPL via `dbspl = dbfs - mic_sens_dbfs + 94.0`.
    /// `None` until the SPL calibration step has been run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mic_sensitivity_dbfs_at_94db_spl: Option<f64>,
    /// Mic frequency-response correction curve, imported from a
    /// manufacturer .frd / .txt file. Stored inline so cal.json stays
    /// self-contained — moving cal.json between machines / sessions
    /// doesn't strand the cal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mic_response: Option<MicResponse>,
    /// Interface-latency (τ) measurement history — see
    /// [`Calibration::tau_history`]. Append-only; never overwritten.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tau_history: Vec<TauEntry>,
}

fn default_ref_freq() -> f64 {
    1000.0
}
fn default_ref_dbfs() -> f64 {
    -10.0
}

/// High-level calibration object with computed helpers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Calibration {
    pub output_channel: u32,
    pub input_channel: u32,
    pub ref_freq: f64,
    pub vrms_at_0dbfs_out: Option<f64>,
    pub vrms_at_0dbfs_in: Option<f64>,
    pub ref_dbfs: f64,
    pub mic_sensitivity_dbfs_at_94db_spl: Option<f64>,
    pub mic_response: Option<MicResponse>,
    /// Interface-latency (τ) measurement history for this channel pair.
    /// Append-only — see [`TauEntry`] / [`Calibration::tau_for`].
    pub tau_history: Vec<TauEntry>,
}

/// Reference SPL of an acoustic pistonphone calibrator. ANSI S1.40 / IEC
/// 60942 Class 1 calibrators emit either 94 dB SPL or 114 dB SPL — we
/// hard-code 94 because that's the universally-supported value and lets
/// `dbfs_to_dbspl` stay parameterless.
pub const PISTONPHONE_REF_SPL: f64 = 94.0;

impl Calibration {
    pub fn new(output_channel: u32, input_channel: u32) -> Self {
        Self {
            output_channel,
            input_channel,
            ref_freq: default_ref_freq(),
            vrms_at_0dbfs_out: None,
            vrms_at_0dbfs_in: None,
            ref_dbfs: default_ref_dbfs(),
            mic_sensitivity_dbfs_at_94db_spl: None,
            mic_response: None,
            tau_history: Vec::new(),
        }
    }

    /// Convert a dBFS output level to physical Vrms using calibration.
    pub fn out_vrms(&self, dbfs: f64) -> Option<f64> {
        self.vrms_at_0dbfs_out.map(|v| dbfs_to_vrms(dbfs, v))
    }

    /// Convert a captured linear RMS (0–1 dBFS scale) to physical Vrms.
    pub fn in_vrms(&self, linear_rms: f64) -> Option<f64> {
        self.vrms_at_0dbfs_in.map(|v| linear_rms * v)
    }

    /// True when this channel has an SPL reference recorded.
    pub fn spl_calibrated(&self) -> bool {
        self.mic_sensitivity_dbfs_at_94db_spl.is_some()
    }

    /// Convert a dBFS reading to dB SPL using the pistonphone reference.
    /// Returns `None` when SPL calibration is unset.
    pub fn dbfs_to_dbspl(&self, dbfs: f64) -> Option<f64> {
        self.mic_sensitivity_dbfs_at_94db_spl
            .map(|m| dbfs - m + PISTONPHONE_REF_SPL)
    }

    /// Additive offset that converts dBFS → dB SPL (so `dbspl = dbfs +
    /// spl_offset_db()`). Returned for transport in wire frames; the UI
    /// applies it to whichever readout it's rendering. `None` when SPL
    /// calibration is unset. `dbfs` here means *uncalibrated* (no
    /// voltage-cal scale) — see the module-level "layer topology" doc.
    pub fn spl_offset_db(&self) -> Option<f64> {
        self.mic_sensitivity_dbfs_at_94db_spl
            .map(|m| PISTONPHONE_REF_SPL - m)
    }

    // -----------------------------------------------------------------------
    // On-disk representation
    // -----------------------------------------------------------------------
    //
    // `Calibration` and `CalibrationEntry` carry the same fields but not the
    // same serde attributes — the entry omits unset optional fields from
    // `cal.json`, while `Calibration` is also serialized whole into snapshot
    // frames. Both conversions below are written so a field added to either
    // struct fails to compile until it is mapped: the destructuring pattern
    // has no `..`, and neither does the struct literal.

    fn to_entry(&self) -> CalibrationEntry {
        let Calibration {
            output_channel,
            input_channel,
            ref_freq,
            vrms_at_0dbfs_out,
            vrms_at_0dbfs_in,
            ref_dbfs,
            mic_sensitivity_dbfs_at_94db_spl,
            mic_response,
            tau_history,
        } = self;
        CalibrationEntry {
            output_channel: *output_channel,
            input_channel: *input_channel,
            ref_freq: *ref_freq,
            vrms_at_0dbfs_out: *vrms_at_0dbfs_out,
            vrms_at_0dbfs_in: *vrms_at_0dbfs_in,
            ref_dbfs: *ref_dbfs,
            mic_sensitivity_dbfs_at_94db_spl: *mic_sensitivity_dbfs_at_94db_spl,
            mic_response: mic_response.clone(),
            tau_history: tau_history.clone(),
        }
    }

    fn from_entry(e: &CalibrationEntry) -> Self {
        let CalibrationEntry {
            output_channel,
            input_channel,
            ref_freq,
            vrms_at_0dbfs_out,
            vrms_at_0dbfs_in,
            ref_dbfs,
            mic_sensitivity_dbfs_at_94db_spl,
            mic_response,
            tau_history,
        } = e;
        Self {
            output_channel: *output_channel,
            input_channel: *input_channel,
            ref_freq: *ref_freq,
            vrms_at_0dbfs_out: *vrms_at_0dbfs_out,
            vrms_at_0dbfs_in: *vrms_at_0dbfs_in,
            ref_dbfs: *ref_dbfs,
            mic_sensitivity_dbfs_at_94db_spl: *mic_sensitivity_dbfs_at_94db_spl,
            mic_response: mic_response.clone(),
            tau_history: tau_history.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::tau::fixtures::{dummy_conditions, dummy_tau_entry};
    use super::*;

    #[test]
    fn out_vrms_computes_correctly() {
        let mut cal = Calibration::new(0, 0);
        cal.vrms_at_0dbfs_out = Some(1.0);
        // 0 dBFS → 1.0 Vrms
        assert!((cal.out_vrms(0.0).unwrap() - 1.0).abs() < 1e-12);
        // -20 dBFS → 0.1 Vrms
        assert!((cal.out_vrms(-20.0).unwrap() - 0.1).abs() < 1e-10);
    }

    #[test]
    fn dbfs_to_dbspl_round_trip() {
        // Pistonphone applied at 94 dB SPL captured -32 dBFS → mic
        // sensitivity is -32 dBFS @ 94 dB SPL. Re-applying the same dBFS
        // input must read 94 dB SPL.
        let mut cal = Calibration::new(0, 0);
        cal.mic_sensitivity_dbfs_at_94db_spl = Some(-32.0);
        let dbspl = cal.dbfs_to_dbspl(-32.0).unwrap();
        assert!(
            (dbspl - 94.0).abs() < 0.5,
            "round-trip got {dbspl}, expected 94"
        );

        // Linear: every 1 dB louder dBFS → 1 dB louder SPL.
        let dbspl_quieter = cal.dbfs_to_dbspl(-50.0).unwrap();
        assert!((dbspl_quieter - 76.0).abs() < 1e-9);
        let dbspl_louder = cal.dbfs_to_dbspl(-10.0).unwrap();
        assert!((dbspl_louder - 116.0).abs() < 1e-9);
    }

    #[test]
    fn spl_offset_db_matches_dbfs_to_dbspl() {
        let mut cal = Calibration::new(0, 0);
        cal.mic_sensitivity_dbfs_at_94db_spl = Some(-28.5);
        let off = cal.spl_offset_db().unwrap();
        for dbfs in &[-80.0, -45.5, -10.0, 0.0] {
            let direct = cal.dbfs_to_dbspl(*dbfs).unwrap();
            let via_off = dbfs + off;
            assert!((direct - via_off).abs() < 1e-12);
        }
    }

    #[test]
    fn spl_calibrated_predicate() {
        let mut cal = Calibration::new(0, 0);
        assert!(!cal.spl_calibrated());
        cal.mic_sensitivity_dbfs_at_94db_spl = Some(-30.0);
        assert!(cal.spl_calibrated());
    }

    #[test]
    fn tau_history_does_not_affect_voltage_or_spl_derivations() {
        // Parallel-not-composed layer topology (module docs, "A third
        // parallel layer" section): appending τ history must not move any
        // voltage- or SPL-derived value on the same entry.
        let mut cal = Calibration::new(0, 0);
        cal.vrms_at_0dbfs_out = Some(1.234);
        cal.vrms_at_0dbfs_in = Some(0.567);
        cal.mic_sensitivity_dbfs_at_94db_spl = Some(-30.0);

        let out_before = cal.out_vrms(-6.0);
        let in_before = cal.in_vrms(0.5);
        let spl_before = cal.dbfs_to_dbspl(-20.0);

        cal.tau_history
            .push(dummy_tau_entry(dummy_conditions(), 0.0011931));
        cal.tau_history
            .push(dummy_tau_entry(dummy_conditions(), 0.0025));

        assert_eq!(cal.out_vrms(-6.0), out_before);
        assert_eq!(cal.in_vrms(0.5), in_before);
        assert_eq!(cal.dbfs_to_dbspl(-20.0), spl_before);
    }
}
