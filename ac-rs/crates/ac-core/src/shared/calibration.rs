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

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::shared::conversions::{dbfs_to_vrms, fmt_vpp, fmt_vrms, vrms_to_dbu};

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

/// Conditions τ (interface round-trip latency) was measured under. τ is a
/// property of this whole tuple, not of the interface alone — a period-size
/// change alone can move it by milliseconds — so lookup
/// ([`Calibration::tau_for`]) is exact-match on every field, never
/// nearest-neighbour or interpolated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TauConditions {
    pub device: u32,
    pub backend: String,
    pub sample_rate: u32,
    /// `None` means "not applicable to this backend" (it cannot report a
    /// period/buffer size at all), not "unknown" — see
    /// `AudioEngine::period_size`. Two runs on such a backend at different
    /// real buffer sizes will spuriously exact-match; this is a documented
    /// limitation of that backend, not new to this field.
    pub period_size: Option<u32>,
    pub output_port: String,
    pub input_port: String,
}

/// One τ measurement: the conditions it was taken under, the value, when,
/// and how. Stored in [`Calibration::tau_history`] as an append-only list —
/// entries are never overwritten or removed, so a stale value never
/// silently replaces a good one; [`Calibration::tau_for`] picks among them
/// by exact condition match.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TauEntry {
    pub conditions: TauConditions,
    pub tau_s: f64,
    /// RFC3339 timestamp of the measurement.
    pub measured_at: String,
    /// Free-text description of the method, e.g. `"farina_short_ess"`.
    pub method: String,
    /// How many independently-lifecycled readings agreed before this entry
    /// was stored (#347). `0` on any entry written before this field
    /// existed — `#[serde(default)]` so those deserialize to `0` rather
    /// than being indistinguishable from a corroborated one. A caller that
    /// writes this field must never write `1`: since #347, a lone reading
    /// is no longer a storable outcome — corroborated entries store `>= 2`.
    #[serde(default)]
    pub agreement_count: u32,
}

/// Why an exact-match τ lookup missed. Names the delta to the nearest
/// stored entry rather than silently interpolating, falling back to
/// "closest", or proceeding uncorrected — see the acceptance criteria on
/// issue #281.
#[derive(Debug, Clone, PartialEq)]
pub struct TauRefusal {
    pub requested: TauConditions,
    /// Nearest entry by fewest differing condition fields, ties broken by
    /// most recent `measured_at`. `None` when no entry exists at all for
    /// this calibration key.
    pub nearest: Option<TauEntry>,
    /// Condition field names (see [`TauConditions`]) that differ between
    /// `requested` and `nearest`, in tuple order. Empty when `nearest` is
    /// `None`.
    pub differing_fields: Vec<&'static str>,
}

impl TauRefusal {
    /// Diagnostic message naming the delta — the point of refusing instead
    /// of guessing is that a reader can see *why* in one line, without
    /// opening `cal.json` by hand.
    pub fn message(&self) -> String {
        match &self.nearest {
            None => format!(
                "no \u{3c4} history recorded for device {} / {} backend yet \u{2014} run `ac \
                 calibrate` with loopback patched to measure one",
                self.requested.device, self.requested.backend
            ),
            Some(nearest) => {
                let deltas: Vec<String> = self
                    .differing_fields
                    .iter()
                    .map(|&f| {
                        format!(
                            "{f} (requested {}, stored {})",
                            tau_field_value(&self.requested, f),
                            tau_field_value(&nearest.conditions, f)
                        )
                    })
                    .collect();
                format!(
                    "no \u{3c4} entry for these exact conditions; nearest stored entry \
                     (measured {}) differs in {}",
                    nearest.measured_at,
                    deltas.join(", ")
                )
            }
        }
    }
}

fn tau_field_value(c: &TauConditions, field: &str) -> String {
    match field {
        "device" => c.device.to_string(),
        "backend" => c.backend.clone(),
        "sample_rate" => format!("{} Hz", c.sample_rate),
        "period_size" => c
            .period_size
            .map(|p| p.to_string())
            .unwrap_or_else(|| "n/a".to_string()),
        "output_port" => c.output_port.clone(),
        "input_port" => c.input_port.clone(),
        _ => "?".to_string(),
    }
}

/// Outcome of comparing two independently-lifecycled τ readings (#347). A
/// single reading is not a measurement of τ on this stack — a
/// graph-buffering shift of exactly one period is invisible within one
/// client lifetime and stable to 0.001 frames within it, so nothing short
/// of a second, separately-lifecycled reading can catch it (see the
/// module-level "third parallel layer" doc). [`compare_tau_readings`]
/// decides whether two such readings corroborate each other.
#[derive(Debug, Clone, PartialEq)]
pub enum TauComparison {
    /// The two readings match to the whole sample
    /// (`round((b - a) * sample_rate) == 0`).
    Agree,
    /// The two readings disagree and neither may be stored.
    /// [`TauDisagreement::periods`] tells a period-shift (software, #347's
    /// own root cause) apart from any other mismatch (a different fault).
    Disagree(TauDisagreement),
}

/// The delta between two disagreeing τ readings, in whole samples, plus
/// enough of the two raw readings to name in a diagnostic message — see
/// [`TauDisagreement::message`].
#[derive(Debug, Clone, PartialEq)]
pub struct TauDisagreement {
    pub reading1_s: f64,
    pub reading2_s: f64,
    /// `round((reading2_s - reading1_s) * sample_rate)`. Always nonzero —
    /// a zero delta is [`TauComparison::Agree`], not a `TauDisagreement`.
    pub delta_samples: i64,
    pub sample_rate: u32,
    pub period_size: Option<u32>,
    /// `Some(n)` (`n != 0`) when `delta_samples` is an exact multiple of
    /// `period_size` — a graph-buffering shift, not hardware drift.
    /// `None` when it isn't, or `period_size` is unknown for this backend:
    /// a different fault class, per #347's acceptance criteria.
    pub periods: Option<i64>,
}

impl TauDisagreement {
    /// Diagnostic message naming the delta in both samples (the causal,
    /// period-quantized unit) and milliseconds (what an operator holds in
    /// their head) — see #347's acceptance criteria: a message that only
    /// says "readings differ" would pass on ordinary jitter and miss the
    /// point.
    pub fn message(&self) -> String {
        let delta_ms = self.delta_samples as f64 / self.sample_rate as f64 * 1000.0;
        match self.periods {
            Some(n) => {
                let period = self
                    .period_size
                    .expect("periods is Some only when period_size is Some");
                format!(
                    "\u{3c4} readings disagree by exactly {} period{} of {period} samples \
                     ({:.3} samples \u{2192} {:.3} samples, \u{394} {} samples = {delta_ms:.4} \
                     ms at {} Hz) \u{2014} a graph-buffering shift, not hardware drift",
                    n.unsigned_abs(),
                    if n.unsigned_abs() == 1 { "" } else { "s" },
                    self.reading1_s * self.sample_rate as f64,
                    self.reading2_s * self.sample_rate as f64,
                    self.delta_samples,
                    self.sample_rate,
                )
            }
            None => format!(
                "\u{3c4} readings disagree, not a period multiple ({:.3} samples \u{2192} \
                 {:.3} samples, \u{394} {} samples = {delta_ms:.4} ms at {} Hz)",
                self.reading1_s * self.sample_rate as f64,
                self.reading2_s * self.sample_rate as f64,
                self.delta_samples,
                self.sample_rate,
            ),
        }
    }
}

/// Compare two independently-lifecycled τ readings (#347) and classify the
/// result. Works in whole samples, derived directly from the issue's own
/// rig data (`+1024.000` exact, fractional part unchanged across the
/// jump): `delta_samples = round((reading2_s - reading1_s) * sample_rate)`.
pub fn compare_tau_readings(
    reading1_s: f64,
    reading2_s: f64,
    sample_rate: u32,
    period_size: Option<u32>,
) -> TauComparison {
    let delta_samples = ((reading2_s - reading1_s) * sample_rate as f64).round() as i64;
    if delta_samples == 0 {
        return TauComparison::Agree;
    }
    let periods = period_size.and_then(|p| {
        let p = p as i64;
        (p != 0 && delta_samples % p == 0).then_some(delta_samples / p)
    });
    TauComparison::Disagree(TauDisagreement {
        reading1_s,
        reading2_s,
        delta_samples,
        sample_rate,
        period_size,
        periods,
    })
}

/// Condition fields that differ between `a` and `b`, in a fixed order.
fn tau_diff_fields(a: &TauConditions, b: &TauConditions) -> Vec<&'static str> {
    let mut out = Vec::new();
    if a.device != b.device {
        out.push("device");
    }
    if a.backend != b.backend {
        out.push("backend");
    }
    if a.sample_rate != b.sample_rate {
        out.push("sample_rate");
    }
    if a.period_size != b.period_size {
        out.push("period_size");
    }
    if a.output_port != b.output_port {
        out.push("output_port");
    }
    if a.input_port != b.input_port {
        out.push("input_port");
    }
    out
}

/// Parsed and validated mic frequency-response correction curve.
///
/// `freqs_hz[i]` is monotonically increasing (asserted on import). At any
/// reading frequency `f`, the mic over-reads the true level by
/// `correction_at(f)` dB, so consumers SUBTRACT this from the captured
/// magnitude to recover the truth: `corrected_dbfs = raw_dbfs - correction`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MicResponse {
    pub freqs_hz: Vec<f32>,
    pub gain_db: Vec<f32>,
    /// Original .frd / .txt path the curve was imported from. Informational
    /// only — never re-read at runtime; the curve data above is the source
    /// of truth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    /// RFC3339 timestamp of the import. Lets the user tell at a glance
    /// when a curve was attached without diffing cal.json.
    pub imported_at: String,
}

impl MicResponse {
    /// Hard upper bound on point count — typical mic .frd files have
    /// 100–500 points (1/24 to 1/48 octave); 4096 is generous and keeps
    /// cal.json under ~50 KB per channel.
    pub const MAX_POINTS: usize = 4096;
    /// Hard lower bound — fewer than 16 points produces an
    /// uncomfortably coarse log-linear interpolation across the audio band.
    pub const MIN_POINTS: usize = 16;

    /// Linear interpolation of `gain_db` in log-frequency space. Frequencies
    /// outside the curve's range clamp to the nearest endpoint (constant
    /// extrapolation — better than zero-extrapolation for room-acoustic work
    /// where the curve usually defines just the audio band).
    pub fn correction_at(&self, freq_hz: f32) -> f32 {
        if self.freqs_hz.is_empty() {
            return 0.0;
        }
        if !freq_hz.is_finite() || freq_hz <= 0.0 {
            return self.gain_db[0];
        }
        if freq_hz <= self.freqs_hz[0] {
            return self.gain_db[0];
        }
        let last = self.freqs_hz.len() - 1;
        if freq_hz >= self.freqs_hz[last] {
            return self.gain_db[last];
        }
        // Binary search for the bracketing pair.
        let i = self
            .freqs_hz
            .partition_point(|&f| f <= freq_hz)
            .saturating_sub(1);
        let f_lo = self.freqs_hz[i];
        let f_hi = self.freqs_hz[i + 1];
        let g_lo = self.gain_db[i];
        let g_hi = self.gain_db[i + 1];
        let log_lo = f_lo.ln();
        let log_hi = f_hi.ln();
        let log_f = freq_hz.ln();
        let t = ((log_f - log_lo) / (log_hi - log_lo)).clamp(0.0, 1.0);
        g_lo + (g_hi - g_lo) * t
    }
}

/// Parse the two-column ASCII format used by Behringer / Dayton / miniDSP
/// mic calibration files. One `<freq_hz> <gain_db>` pair per line, optional
/// whitespace, comments starting with `*` or `#` are ignored. An optional
/// third column (phase) is ignored. Validates monotonically increasing
/// frequencies, finite values, and the [`MicResponse::MIN_POINTS`] /
/// [`MicResponse::MAX_POINTS`] bounds.
pub fn parse_mic_curve(text: &str, source_path: Option<String>) -> Result<MicResponse> {
    let mut freqs = Vec::new();
    let mut gains = Vec::new();
    for (line_no, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('*') || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let mut cols = line.split_whitespace();
        let f_tok = cols.next();
        let g_tok = cols.next();
        let (f_str, g_str) = match (f_tok, g_tok) {
            (Some(f), Some(g)) => (f, g),
            _ => anyhow::bail!(
                "line {}: expected `freq_hz gain_db [phase]`, got {raw:?}",
                line_no + 1
            ),
        };
        let f: f32 = f_str.parse().map_err(|e| {
            anyhow::anyhow!("line {}: failed to parse freq {f_str:?}: {e}", line_no + 1)
        })?;
        let g: f32 = g_str.parse().map_err(|e| {
            anyhow::anyhow!("line {}: failed to parse gain {g_str:?}: {e}", line_no + 1)
        })?;
        if !f.is_finite() || f <= 0.0 {
            anyhow::bail!("line {}: freq must be > 0 Hz, got {f}", line_no + 1);
        }
        if !g.is_finite() {
            anyhow::bail!("line {}: gain must be finite, got {g}", line_no + 1);
        }
        if let Some(&prev) = freqs.last() {
            if f <= prev {
                anyhow::bail!(
                    "line {}: frequencies must increase strictly (got {f} after {prev})",
                    line_no + 1
                );
            }
        }
        freqs.push(f);
        gains.push(g);
    }
    if freqs.len() < MicResponse::MIN_POINTS {
        anyhow::bail!(
            "mic curve too sparse: got {} points, need ≥ {}",
            freqs.len(),
            MicResponse::MIN_POINTS
        );
    }
    if freqs.len() > MicResponse::MAX_POINTS {
        anyhow::bail!(
            "mic curve too dense: got {} points, max {}",
            freqs.len(),
            MicResponse::MAX_POINTS
        );
    }
    Ok(MicResponse {
        freqs_hz: freqs,
        gain_db: gains,
        source_path,
        imported_at: crate::shared::time::now_utc_iso8601(),
    })
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
            ref_freq: 1000.0,
            vrms_at_0dbfs_out: None,
            vrms_at_0dbfs_in: None,
            ref_dbfs: -10.0,
            mic_sensitivity_dbfs_at_94db_spl: None,
            mic_response: None,
            tau_history: Vec::new(),
        }
    }

    /// File key: `out{N}_in{M}`.
    pub fn key(&self) -> String {
        format!("out{}_in{}", self.output_channel, self.input_channel)
    }

    pub fn output_ok(&self) -> bool {
        self.vrms_at_0dbfs_out.is_some()
    }
    pub fn input_ok(&self) -> bool {
        self.vrms_at_0dbfs_in.is_some()
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

    /// Mic frequency-response correction at `freq_hz`, in dB. The mic
    /// over-reads by this much; subtract from a captured magnitude to
    /// recover truth. `None` when no curve is loaded.
    pub fn mic_correction_at(&self, freq_hz: f32) -> Option<f32> {
        self.mic_response.as_ref().map(|r| r.correction_at(freq_hz))
    }

    /// Exact-match τ lookup. Refuses rather than interpolating or falling
    /// back to "closest" — a stale τ is a silent-wrongness bug (issue
    /// #281), so a miss must say so and name the delta, not degrade.
    pub fn tau_for(&self, cond: &TauConditions) -> Result<&TauEntry, Box<TauRefusal>> {
        if let Some(hit) = self.tau_history.iter().find(|e| &e.conditions == cond) {
            return Ok(hit);
        }
        let mut nearest: Option<&TauEntry> = None;
        let mut best_diff = usize::MAX;
        for e in &self.tau_history {
            let n_diff = tau_diff_fields(cond, &e.conditions).len();
            let better = n_diff < best_diff
                || (n_diff == best_diff
                    && nearest
                        .map(|n| e.measured_at > n.measured_at)
                        .unwrap_or(true));
            if better {
                best_diff = n_diff;
                nearest = Some(e);
            }
        }
        let differing_fields = nearest
            .map(|n| tau_diff_fields(cond, &n.conditions))
            .unwrap_or_default();
        Err(Box::new(TauRefusal {
            requested: cond.clone(),
            nearest: nearest.cloned(),
            differing_fields,
        }))
    }

    // -----------------------------------------------------------------------
    // Persistence
    // -----------------------------------------------------------------------

    /// Persist this calibration entry into the shared cal.json file.
    /// Existing entries for other channel pairs are preserved.
    pub fn save(&self, path: Option<&Path>) -> Result<()> {
        let path = path
            .map(|p| p.to_path_buf())
            .unwrap_or_else(default_cal_path);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }

        let mut all: HashMap<String, CalibrationEntry> = if path.exists() {
            let raw = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            serde_json::from_str(&raw).unwrap_or_default()
        } else {
            HashMap::new()
        };

        all.insert(
            self.key(),
            CalibrationEntry {
                output_channel: self.output_channel,
                input_channel: self.input_channel,
                ref_freq: self.ref_freq,
                vrms_at_0dbfs_out: self.vrms_at_0dbfs_out,
                vrms_at_0dbfs_in: self.vrms_at_0dbfs_in,
                ref_dbfs: self.ref_dbfs,
                mic_sensitivity_dbfs_at_94db_spl: self.mic_sensitivity_dbfs_at_94db_spl,
                mic_response: self.mic_response.clone(),
                tau_history: self.tau_history.clone(),
            },
        );

        let out = serde_json::to_string_pretty(&all)?;
        std::fs::write(&path, out).with_context(|| format!("writing {}", path.display()))?;
        eprintln!(
            "  Calibration saved -> {}  (key: {})",
            path.display(),
            self.key()
        );
        Ok(())
    }

    /// Load calibration for a specific output/input channel pair.
    /// Returns `Ok(None)` if the file or key doesn't exist.
    pub fn load(
        output_channel: u32,
        input_channel: u32,
        path: Option<&Path>,
    ) -> Result<Option<Self>> {
        let path = path
            .map(|p| p.to_path_buf())
            .unwrap_or_else(default_cal_path);
        let all = read_all_entries(&path)?;
        let key = format!("out{}_in{}", output_channel, input_channel);
        Ok(all.get(&key).map(Calibration::from_entry))
    }

    /// Load the first calibration matching `output_channel`, any input.
    pub fn load_output_only(output_channel: u32, path: Option<&Path>) -> Result<Option<Self>> {
        let path = path
            .map(|p| p.to_path_buf())
            .unwrap_or_else(default_cal_path);
        let all = read_all_entries(&path)?;
        let prefix = format!("out{}_in", output_channel);
        Ok(all
            .values()
            .find(|e| format!("out{}_in{}", e.output_channel, e.input_channel).starts_with(&prefix))
            .map(Calibration::from_entry))
    }

    /// Load all stored calibration entries.
    pub fn load_all(path: Option<&Path>) -> Result<Vec<Self>> {
        let path = path
            .map(|p| p.to_path_buf())
            .unwrap_or_else(default_cal_path);
        let all = read_all_entries(&path)?;
        Ok(all.values().map(Calibration::from_entry).collect())
    }

    /// Print a human-readable calibration summary to stderr.
    pub fn summary(&self) {
        eprintln!(
            "\n  -- Calibration  [{}] ----------------------------------",
            self.key()
        );
        if let Some(v) = self.vrms_at_0dbfs_out {
            eprintln!(
                "  Output: 0 dBFS = {}  =  {:+.2} dBu  =  {}",
                fmt_vrms(v),
                vrms_to_dbu(v),
                fmt_vpp(v)
            );
        } else {
            eprintln!("  Output: not calibrated");
        }
        if let Some(v) = self.vrms_at_0dbfs_in {
            eprintln!(
                "  Input:  0 dBFS = {}  =  {:+.2} dBu  =  {}",
                fmt_vrms(v),
                vrms_to_dbu(v),
                fmt_vpp(v)
            );
        } else {
            eprintln!("  Input:  not calibrated");
        }
        eprintln!("  --------------------------------------------------------------\n");
    }

    fn from_entry(e: &CalibrationEntry) -> Self {
        Self {
            output_channel: e.output_channel,
            input_channel: e.input_channel,
            ref_freq: e.ref_freq,
            vrms_at_0dbfs_out: e.vrms_at_0dbfs_out,
            vrms_at_0dbfs_in: e.vrms_at_0dbfs_in,
            ref_dbfs: e.ref_dbfs,
            mic_sensitivity_dbfs_at_94db_spl: e.mic_sensitivity_dbfs_at_94db_spl,
            mic_response: e.mic_response.clone(),
            tau_history: e.tau_history.clone(),
        }
    }

    /// Load the existing calibration entry for a channel pair, or return a
    /// fresh one with defaults. Used by partial-update handlers (voltage
    /// cal + SPL cal write to disjoint fields and must not clobber each
    /// other's prior values).
    pub fn load_or_new(output_channel: u32, input_channel: u32, path: Option<&Path>) -> Self {
        Self::load(output_channel, input_channel, path)
            .ok()
            .flatten()
            .unwrap_or_else(|| Self::new(output_channel, input_channel))
    }
}

// -----------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------

/// Default calibration file path: `~/.config/ac/cal.json`.
pub fn default_cal_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home)
        .join(".config")
        .join("ac")
        .join("cal.json")
}

fn read_all_entries(path: &Path) -> Result<HashMap<String, CalibrationEntry>> {
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let raw =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    // Silently ignore malformed files — return empty map.
    Ok(serde_json::from_str(&raw).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_and_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cal.json");

        let mut cal = Calibration::new(0, 0);
        cal.vrms_at_0dbfs_out = Some(1.234);
        cal.vrms_at_0dbfs_in = Some(0.567);
        cal.save(Some(&path)).unwrap();

        let loaded = Calibration::load(0, 0, Some(&path)).unwrap().unwrap();
        assert!((loaded.vrms_at_0dbfs_out.unwrap() - 1.234).abs() < 1e-10);
        assert!((loaded.vrms_at_0dbfs_in.unwrap() - 0.567).abs() < 1e-10);
    }

    #[test]
    fn missing_key_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cal.json");
        // Write a different key
        let mut cal = Calibration::new(1, 0);
        cal.vrms_at_0dbfs_out = Some(1.0);
        cal.save(Some(&path)).unwrap();

        let result = Calibration::load(0, 0, Some(&path)).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn load_all_returns_all_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cal.json");

        for (out_ch, in_ch) in [(0u32, 0u32), (0, 1), (1, 0)] {
            let mut cal = Calibration::new(out_ch, in_ch);
            cal.vrms_at_0dbfs_out = Some(1.0);
            cal.save(Some(&path)).unwrap();
        }

        let all = Calibration::load_all(Some(&path)).unwrap();
        assert_eq!(all.len(), 3);
    }

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
    fn spl_field_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cal.json");

        let mut cal = Calibration::new(2, 3);
        cal.vrms_at_0dbfs_out = Some(1.0);
        cal.vrms_at_0dbfs_in = Some(0.5);
        cal.mic_sensitivity_dbfs_at_94db_spl = Some(-31.7);
        cal.save(Some(&path)).unwrap();

        let loaded = Calibration::load(2, 3, Some(&path)).unwrap().unwrap();
        assert_eq!(loaded.mic_sensitivity_dbfs_at_94db_spl, Some(-31.7));
    }

    // ─── mic_curve parser ───────────────────────────────────────────────

    fn dummy_curve_text(n: usize) -> String {
        // Geometric sweep from 20 Hz to 20 kHz, gain rising linearly with
        // log-frequency from 0 to 4 dB. Useful as a "known curve".
        let mut s = String::from("* a comment\n");
        let log_min = 20.0_f32.ln();
        let log_max = 20_000.0_f32.ln();
        for i in 0..n {
            let t = i as f32 / (n - 1) as f32;
            let f = (log_min + t * (log_max - log_min)).exp();
            let g = 4.0 * t;
            s.push_str(&format!("{f}\t{g}\n"));
        }
        s
    }

    #[test]
    fn parse_mic_curve_round_trip() {
        let text = dummy_curve_text(64);
        let r = parse_mic_curve(&text, Some("/tmp/test.frd".into())).unwrap();
        assert_eq!(r.freqs_hz.len(), 64);
        assert_eq!(r.gain_db.len(), 64);
        assert!((r.freqs_hz[0] - 20.0).abs() < 0.01);
        assert!((r.gain_db[0]).abs() < 0.01);
        assert!((r.freqs_hz.last().unwrap() - 20_000.0).abs() < 1.0);
        assert!((r.gain_db.last().unwrap() - 4.0).abs() < 0.01);
        assert_eq!(r.source_path.as_deref(), Some("/tmp/test.frd"));
    }

    #[test]
    fn parse_mic_curve_skips_comments() {
        let text = "# header\n* freq gain\n100 0.5\n200 0.8\n300 1.1\n400 1.4\n500 1.7\n\
                    600 2.0\n700 2.3\n800 2.6\n900 2.9\n1000 3.2\n1100 3.5\n1200 3.8\n\
                    1300 4.1\n1400 4.4\n1500 4.7\n1600 5.0\n1700 5.3\n";
        let r = parse_mic_curve(text, None).unwrap();
        assert_eq!(r.freqs_hz.len(), 17);
    }

    #[test]
    fn parse_mic_curve_third_column_ignored() {
        let mut text = String::new();
        for i in 0..20 {
            let f = 100.0_f32 * 1.2_f32.powi(i);
            text.push_str(&format!("{f}\t0.{i}\t-12.5\n"));
        }
        let r = parse_mic_curve(&text, None).unwrap();
        assert_eq!(r.freqs_hz.len(), 20);
    }

    #[test]
    fn parse_mic_curve_rejects_too_few_points() {
        let text = "100 0\n200 1\n300 2\n";
        let err = parse_mic_curve(text, None).unwrap_err();
        assert!(err.to_string().contains("too sparse"), "got {err}");
    }

    #[test]
    fn parse_mic_curve_rejects_non_monotonic() {
        let mut text = dummy_curve_text(20);
        text.push_str("50 0\n"); // out-of-order
        let err = parse_mic_curve(&text, None).unwrap_err();
        assert!(err.to_string().contains("strictly"), "got {err}");
    }

    #[test]
    fn parse_mic_curve_rejects_zero_freq() {
        let text = format!("0 0\n{}", dummy_curve_text(20));
        let err = parse_mic_curve(&text, None).unwrap_err();
        assert!(err.to_string().contains("> 0 Hz"), "got {err}");
    }

    #[test]
    fn correction_at_endpoints_clamps() {
        let r = parse_mic_curve(&dummy_curve_text(50), None).unwrap();
        // Below the first freq: clamps to first gain (0).
        assert!((r.correction_at(1.0) - 0.0).abs() < 0.01);
        // Above the last: clamps to last gain (4.0).
        assert!((r.correction_at(50_000.0) - 4.0).abs() < 0.01);
    }

    #[test]
    fn correction_at_interpolates_in_log_freq() {
        // Curve: linear gain ramp 0..4 dB over log(20..20k). At
        // geometric mid-point (sqrt(20*20000) ≈ 632), correction = 2 dB.
        let r = parse_mic_curve(&dummy_curve_text(50), None).unwrap();
        let mid = (20.0_f32 * 20_000.0).sqrt();
        let g = r.correction_at(mid);
        assert!(
            (g - 2.0).abs() < 0.1,
            "got {g} dB at f={mid}, expected ≈ 2.0"
        );
    }

    #[test]
    fn correction_at_negative_freq_clamps_to_first() {
        let r = parse_mic_curve(&dummy_curve_text(50), None).unwrap();
        assert!((r.correction_at(-100.0) - r.gain_db[0]).abs() < 1e-6);
    }

    #[test]
    fn mic_response_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cal.json");
        let curve = parse_mic_curve(&dummy_curve_text(40), Some("/foo/bar.frd".into())).unwrap();

        let mut cal = Calibration::new(0, 1);
        cal.mic_response = Some(curve.clone());
        cal.save(Some(&path)).unwrap();

        let loaded = Calibration::load(0, 1, Some(&path)).unwrap().unwrap();
        let r = loaded.mic_response.expect("curve missing after reload");
        assert_eq!(r.freqs_hz.len(), curve.freqs_hz.len());
        assert!((r.freqs_hz[0] - curve.freqs_hz[0]).abs() < 1e-3);
        assert_eq!(r.source_path, curve.source_path);
        assert_eq!(r.imported_at, curve.imported_at);
    }

    #[test]
    fn mic_curve_save_preserves_voltage_and_spl() {
        // Same composition guarantee #63 introduced for voltage↔SPL must
        // hold for mic-curve as well: writing one field via load_or_new
        // doesn't lose the others.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cal.json");

        let mut a = Calibration::new(0, 0);
        a.vrms_at_0dbfs_in = Some(0.5);
        a.mic_sensitivity_dbfs_at_94db_spl = Some(-30.0);
        a.save(Some(&path)).unwrap();

        let mut b = Calibration::load_or_new(0, 0, Some(&path));
        b.mic_response = Some(parse_mic_curve(&dummy_curve_text(20), None).unwrap());
        b.save(Some(&path)).unwrap();

        let loaded = Calibration::load(0, 0, Some(&path)).unwrap().unwrap();
        assert_eq!(loaded.vrms_at_0dbfs_in, Some(0.5));
        assert_eq!(loaded.mic_sensitivity_dbfs_at_94db_spl, Some(-30.0));
        assert!(loaded.mic_response.is_some());
    }

    #[test]
    fn voltage_save_preserves_existing_spl() {
        // Workflow: user runs SPL cal first (sets only the SPL field), then
        // later runs voltage cal. The voltage handler uses load_or_new and
        // mutates only the voltage fields, so the SPL value must survive.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cal.json");

        let mut spl_cal = Calibration::new(0, 1);
        spl_cal.mic_sensitivity_dbfs_at_94db_spl = Some(-29.4);
        spl_cal.save(Some(&path)).unwrap();

        // Voltage-cal handler simulation — load existing, set voltage,
        // save; SPL field stays.
        let mut cal = Calibration::load_or_new(0, 1, Some(&path));
        cal.vrms_at_0dbfs_out = Some(1.234);
        cal.vrms_at_0dbfs_in = Some(0.567);
        cal.save(Some(&path)).unwrap();

        let loaded = Calibration::load(0, 1, Some(&path)).unwrap().unwrap();
        assert_eq!(loaded.mic_sensitivity_dbfs_at_94db_spl, Some(-29.4));
        assert_eq!(loaded.vrms_at_0dbfs_out, Some(1.234));
        assert_eq!(loaded.vrms_at_0dbfs_in, Some(0.567));
    }

    // ─── τ (interface latency) history — issue #281 ────────────────────

    fn dummy_conditions() -> TauConditions {
        TauConditions {
            device: 0,
            backend: "jack".to_string(),
            sample_rate: 48_000,
            period_size: Some(1024),
            output_port: "system:playback_1".to_string(),
            input_port: "system:capture_2".to_string(),
        }
    }

    fn dummy_tau_entry(cond: TauConditions, tau_s: f64) -> TauEntry {
        TauEntry {
            conditions: cond,
            tau_s,
            measured_at: crate::shared::time::now_utc_iso8601(),
            method: "farina_short_ess".to_string(),
            agreement_count: 2,
        }
    }

    #[test]
    fn tau_for_exact_match_hits() {
        let mut cal = Calibration::new(0, 0);
        let cond = dummy_conditions();
        cal.tau_history
            .push(dummy_tau_entry(cond.clone(), 0.0011931));
        let hit = cal.tau_for(&cond).expect("exact match should hit");
        assert!((hit.tau_s - 0.0011931).abs() < 1e-12);
    }

    #[test]
    fn tau_for_refuses_on_period_size_change_and_names_the_delta() {
        // #281 acceptance criterion: "a synthetic entry recorded at one
        // period size is refused at another, with the delta in the
        // message" — τ moves by milliseconds on a period-size change, so
        // this must never silently degrade to the stored value.
        let mut cal = Calibration::new(0, 0);
        let stored = dummy_conditions();
        cal.tau_history
            .push(dummy_tau_entry(stored.clone(), 0.0011931));

        let mut requested = stored.clone();
        requested.period_size = Some(256);

        let refusal = cal
            .tau_for(&requested)
            .expect_err("period-size mismatch must refuse, not degrade");
        assert_eq!(refusal.differing_fields, vec!["period_size"]);
        assert_eq!(refusal.nearest.as_ref().unwrap().tau_s, 0.0011931);
        let msg = refusal.message();
        assert!(
            msg.contains("period_size"),
            "message must name the differing field: {msg}"
        );
        assert!(
            msg.contains("256"),
            "message must name the requested value: {msg}"
        );
        assert!(
            msg.contains("1024"),
            "message must name the stored value: {msg}"
        );
    }

    #[test]
    fn tau_for_refuses_with_no_nearest_when_history_is_empty() {
        let cal = Calibration::new(0, 0);
        let refusal = cal.tau_for(&dummy_conditions()).unwrap_err();
        assert!(refusal.nearest.is_none());
        assert!(refusal.differing_fields.is_empty());
        assert!(refusal.message().contains("no \u{3c4} history"));
    }

    #[test]
    fn tau_history_round_trips_alongside_voltage_and_spl() {
        // Same composition guarantee as `mic_curve_save_preserves_voltage_
        // and_spl`, extended to the third layer: appending a τ entry must
        // not disturb the other two.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cal.json");

        let mut a = Calibration::new(0, 0);
        a.vrms_at_0dbfs_out = Some(1.234);
        a.vrms_at_0dbfs_in = Some(0.567);
        a.mic_sensitivity_dbfs_at_94db_spl = Some(-30.0);
        a.save(Some(&path)).unwrap();

        let mut b = Calibration::load_or_new(0, 0, Some(&path));
        b.tau_history
            .push(dummy_tau_entry(dummy_conditions(), 0.0011931));
        b.save(Some(&path)).unwrap();

        let loaded = Calibration::load(0, 0, Some(&path)).unwrap().unwrap();
        assert_eq!(loaded.vrms_at_0dbfs_out, Some(1.234));
        assert_eq!(loaded.vrms_at_0dbfs_in, Some(0.567));
        assert_eq!(loaded.mic_sensitivity_dbfs_at_94db_spl, Some(-30.0));
        assert_eq!(loaded.tau_history.len(), 1);
        assert!((loaded.tau_history[0].tau_s - 0.0011931).abs() < 1e-12);
    }

    // ─── compare_tau_readings — issue #347 ──────────────────────────────

    #[test]
    fn compare_tau_readings_exact_match_agrees() {
        let cmp = compare_tau_readings(0.001, 0.001, 48_000, Some(1024));
        assert_eq!(cmp, TauComparison::Agree);
    }

    #[test]
    fn compare_tau_readings_sub_sample_jitter_still_agrees() {
        // #347 acceptance: within-lifecycle stability is 0.001 frame; the
        // comparator must not flag that as a disagreement.
        let cmp = compare_tau_readings(0.001, 0.001 + 1e-9, 48_000, Some(1024));
        assert_eq!(cmp, TauComparison::Agree);
    }

    #[test]
    fn compare_tau_readings_refuses_on_period_shift_and_names_the_period() {
        // #347's own rig data: 4262.064 frames -> 5286.064 frames at
        // 96 kHz, exactly +1024.000 samples = one period, fractional part
        // unchanged. A test that only checks "readings differ" would pass
        // on noise and miss the point — assert the period is *named*.
        let sr = 96_000;
        let period = 1024u32;
        let reading1_s = 4262.064 / sr as f64;
        let reading2_s = 5286.064 / sr as f64;
        let cmp = compare_tau_readings(reading1_s, reading2_s, sr, Some(period));
        let TauComparison::Disagree(d) = cmp else {
            panic!("expected a period-shift disagreement, got {cmp:?}");
        };
        assert_eq!(d.delta_samples, 1024);
        assert_eq!(d.periods, Some(1));
        let msg = d.message();
        assert!(
            msg.contains("1 period"),
            "message must name the period count: {msg}"
        );
        assert!(
            msg.contains("1024"),
            "message must name the period size: {msg}"
        );
        assert!(
            msg.contains("10.6667 ms"),
            "message must name the delta in ms: {msg}"
        );
    }

    #[test]
    fn compare_tau_readings_multi_period_shift_names_the_count() {
        let sr = 48_000;
        let period = 512u32;
        let reading1_s = 1000.0 / sr as f64;
        let reading2_s = (1000.0 + 1536.0) / sr as f64; // 3 periods
        let cmp = compare_tau_readings(reading1_s, reading2_s, sr, Some(period));
        let TauComparison::Disagree(d) = cmp else {
            panic!("expected a period-shift disagreement, got {cmp:?}");
        };
        assert_eq!(d.periods, Some(3));
        assert!(d.message().contains("3 periods"), "got {}", d.message());
    }

    #[test]
    fn compare_tau_readings_non_period_delta_is_a_different_fault() {
        // #347 acceptance: "a disagreement that is not a multiple of the
        // period is a different fault and should say so" — not laundered
        // through the same message as a period-shift.
        let sr = 96_000;
        let reading1_s = 4262.064 / sr as f64;
        let reading2_s = 4290.500 / sr as f64; // delta 28.436 -> rounds to 28
        let cmp = compare_tau_readings(reading1_s, reading2_s, sr, Some(1024));
        let TauComparison::Disagree(d) = cmp else {
            panic!("expected a disagreement, got {cmp:?}");
        };
        assert_eq!(d.periods, None);
        let msg = d.message();
        assert!(
            msg.contains("not a period multiple"),
            "message must say this is a different fault class: {msg}"
        );
    }

    #[test]
    fn compare_tau_readings_unknown_period_size_is_never_a_period_shift() {
        // A backend that can't report a period size (AudioEngine::
        // period_size -> None) can never corroborate the period-shift
        // classification, even if the delta happens to look tidy.
        let cmp = compare_tau_readings(0.0, 1024.0 / 48_000.0, 48_000, None);
        let TauComparison::Disagree(d) = cmp else {
            panic!("expected a disagreement, got {cmp:?}");
        };
        assert_eq!(d.periods, None);
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
