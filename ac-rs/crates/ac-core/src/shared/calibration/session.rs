//! Per-session verification of stored calibration layers — issue #466.
//!
//! A stored τ and a stored voltage scale are keyed on configuration, and
//! both can be invalidated by events the key does not record: an interface
//! power cycle reset the FF400's analog level settings (+6 dB, then +3 dB)
//! while ALSA readback still showed the baseline, and τ re-picks by whole
//! SYT-interval steps on a device enumeration (#461). Where a reference
//! loopback is patched, both quantities are cheap to measure, so this module
//! judges a fresh measurement against the stored layer and says, per layer,
//! whether it is **verified**, **refused** or **unverified**.
//!
//! Everything here is pure: numbers in, verdicts out. The daemon owns the
//! probe (audio I/O), the records and the gating; this module owns the
//! rules those follow, so each rule has one place and one test.
//!
//! # Rules, in one place
//!
//! * **Layers are independent.** A τ that agrees says nothing about level,
//!   and a level that agrees says nothing about τ.
//! * **Time rule.** A `verified` record applies only within the daemon
//!   process that made it (verified records are never persisted), within the
//!   device-enumeration epoch it was made in, and only to the stored value it
//!   judged (its identity). A `refused` record is persisted and survives an
//!   epoch change and a restart; only a later passing check on the same
//!   loopback, or a newly stored value, clears it.
//! * **Pair rule.** A refusal on the loopback propagates — voltage to every
//!   voltage entry, τ to every stored τ that shares device, backend, sample
//!   rate and period size with the check. Verification never propagates.
//! * **Precedence.** `not_stored` > `refusals_unreadable` > every other
//!   `unverified` cause. Only a `verified`/`refused` verdict of the same
//!   identity from this process decides over an unreadable refusal record.
//!
//! # The level tolerance, and why S_min is coupled to it
//!
//! [`LOOP_GAIN_TOLERANCE_DB`] is `max(k·s, 0.02 dB)` with
//! `k = t(1 − α/2, n − 1)·√2 = 4.897·√2 = 6.93` for `n = 20`, `α = 1e-4`
//! (provenance: derived; √2 because Δ is the difference of two single
//! probes). `s` is the standard deviation of 20 checks on the rig
//! (provenance: measured — **pending**, rig step 1). Until that record
//! exists the constant is a provisional 0.10 dB (provenance: assumed). The
//! 0.02 dB floor is twice the 0.01 dB print resolution (provenance:
//! assumed). The rule refuses on `|Δ| > T`.
//!
//! [`PROBE_SNR_MIN_DB`] is not a free constant: it is the in-lobe SNR at
//! which noise inside the tone's main lobe biases the loop gain by at most
//! `T/4`, `S_min = −20·log10(10^(T/80) − 1)` (provenance: derived). Change
//! one and the other must follow; `snr_min_tracks_the_tolerance` records the
//! coupling.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::epoch::{DeviceEpoch, EnumerationCheck, BOUNDARY_HOST_REBOOTED};
use super::store::SESSION_REFUSALS_FILE;
use super::tau::{compare_tau_readings, TauComparison, TauConditions};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Level tolerance, dB. **Provisional** (provenance: assumed) until rig
/// step 1 measures `s` — see the module doc for the rule that replaces it.
pub const LOOP_GAIN_TOLERANCE_DB: f64 = 0.10;

/// Lower bound on the tolerance: 2× the 0.01 dB print resolution
/// (provenance: assumed).
pub const LOOP_GAIN_TOLERANCE_FLOOR_DB: f64 = 0.02;

/// `t(1 − α/2, 19)·√2` for `α = 1e-4` (provenance: derived).
pub const LOOP_GAIN_TOLERANCE_K: f64 = 6.93;

/// Minimum in-lobe SNR for a present tone at [`LOOP_GAIN_TOLERANCE_DB`]:
/// `snr_min_for_tolerance(LOOP_GAIN_TOLERANCE_DB)` (provenance: derived).
pub const PROBE_SNR_MIN_DB: f64 = 50.804_982_967_692_37;

/// Probe tone frequency, Hz — the same tone `calibrate` step 2 captures.
pub const PROBE_FREQ_HZ: f64 = 1000.0;

/// Settle time between setting the tone and capturing, seconds. Shared
/// with `calibrate` step 2 so the baseline and the probe read the same way.
pub const PROBE_SETTLE_S: f64 = 0.15;

/// Probe capture length, seconds. Shared with `calibrate` step 2.
pub const PROBE_CAPTURE_S: f64 = 0.3;

/// A τ agrees when it rounds to the same whole sample — the comparator
/// [`compare_tau_readings`] uses, so `|Δ| < 0.5` sample before rounding
/// (triage AC3: provenance measured, 49 + 10 sample-identical readings).
pub const LATENCY_TOLERANCE_SAMPLES: f64 = 0.5;

/// Hann window equivalent noise bandwidth, bins.
const HANN_ENBW_BINS: f64 = 1.5;

/// `20·log10(√2)`: a peak-referenced level minus this is RMS-referenced.
const PEAK_TO_RMS_DB: f64 = 3.010_299_956_639_812;

/// Floor used when a block's RMS is zero — the same 1e-12 floor as the
/// daemon's `rms_to_dbfs`.
const RMS_FLOOR: f64 = 1e-12;

/// `max(k·s, floor)` — the tolerance rule, for rig step 1 to evaluate.
pub fn tolerance_from_repeatability(s_db: f64) -> f64 {
    (LOOP_GAIN_TOLERANCE_K * s_db).max(LOOP_GAIN_TOLERANCE_FLOOR_DB)
}

/// The in-lobe SNR at which white residual noise biases a tone's level by
/// at most `tolerance_db / 4`.
pub fn snr_min_for_tolerance(tolerance_db: f64) -> f64 {
    -20.0 * (10f64.powf(tolerance_db / 80.0) - 1.0).log10()
}

/// Noise inside the tone's main lobe, relative to the tone, dB.
///
/// `fundamental_dbfs` is peak-referenced and `noise_floor_dbfs` is the
/// broadband residual RMS, so the fundamental is converted to RMS and the
/// Hann processing gain `(n/2) / ENBW` is applied: the residual's power is
/// spread over `n/2` bins, of which 1.5 fall in the lobe. Assumes a white
/// residual (provenance: assumed) — a mains harmonic at the probe frequency
/// breaks that, and rig step 1's `s` measures its effect.
pub fn in_lobe_snr_db(fundamental_dbfs: f64, noise_floor_dbfs: f64, n: usize) -> f64 {
    fundamental_dbfs - PEAK_TO_RMS_DB - noise_floor_dbfs
        + 10.0 * ((n as f64 / 2.0) / HANN_ENBW_BINS).log10()
}

/// `20·log10(√2·rms)` with the 1e-12 RMS floor: a peak-referenced level of
/// the whole block. A tone of peak amplitude A adds A²/2 to the block's
/// mean square, so this never reads below the tone it contains.
pub fn total_peak_dbfs(block: &[f32]) -> f64 {
    let sum_sq: f64 = block.iter().map(|&x| (x as f64).powi(2)).sum();
    let rms = (sum_sq / block.len().max(1) as f64).sqrt();
    PEAK_TO_RMS_DB + 20.0 * rms.max(RMS_FLOOR).log10()
}

// ---------------------------------------------------------------------------
// Stored baseline
// ---------------------------------------------------------------------------

/// The loop gain `calibrate` measured on a pair, and the conditions it was
/// measured under. `loop_gain_db = fundamental_dbfs − drive_dbfs`, both
/// peak-referenced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoopGainBaseline {
    pub loop_gain_db: f64,
    pub freq_hz: f64,
    pub drive_dbfs: f64,
    /// RFC3339. The baseline's identity: a verdict judged a different
    /// `measured_at` says nothing about this one.
    pub measured_at: String,
    pub epoch: DeviceEpoch,
}

// ---------------------------------------------------------------------------
// Verdicts
// ---------------------------------------------------------------------------

/// Unit of a verdict's `measured`/`stored`/`delta`/`tolerance`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerdictUnit {
    #[serde(rename = "samples")]
    Samples,
    #[serde(rename = "dB")]
    Db,
}

/// Where a verdict's measurement came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckSource {
    /// A probe an emitting command ran before its own emission.
    Probe,
    /// `plot_ir`'s same-capture reference leg (#460).
    SameCapture,
    /// The `session_check` command.
    Explicit,
}

/// A refused Δ that is a bound, not a reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeltaBound {
    /// The true Δ is at most `delta`: no tone was found, and the whole
    /// return was quieter than the stored loop would make the tone alone.
    AtMost,
}

/// The evidence behind a verified or refused verdict.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    pub measured: f64,
    pub stored: f64,
    /// Signed, `measured − stored`.
    pub delta: f64,
    pub tolerance: f64,
    pub unit: VerdictUnit,
    /// When the judged stored value was measured.
    pub stored_at: String,
    pub checked_at: String,
    pub source: CheckSource,
}

/// Why a layer is neither verified nor refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnverifiedCause {
    NoLoopback,
    NotMeasured,
    NoBaseline,
    BaselineDriveDiffers,
    NotStored,
    NotCovered,
    NotChecked,
    RefusalsUnreadable,
    /// A cause this build does not know (a newer daemon). Reads as
    /// unverified — never as verified — and its reason prints verbatim.
    #[serde(other)]
    Unknown,
}

impl UnverifiedCause {
    /// Higher wins. `not_stored` > `refusals_unreadable` > the rest.
    pub fn rank(self) -> u8 {
        match self {
            UnverifiedCause::NotStored => 2,
            UnverifiedCause::RefusalsUnreadable => 1,
            _ => 0,
        }
    }
}

/// One layer's verdict. `state` tags it on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LayerVerdict {
    Verified(Evidence),
    Refused {
        #[serde(flatten)]
        evidence: Evidence,
        /// The loopback key whose refusal propagated here; absent on a
        /// direct refusal.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        via: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delta_bound: Option<DeltaBound>,
    },
    Unverified {
        cause: UnverifiedCause,
        /// `<observation>` or `<observation>; check: <places>`.
        reason: String,
    },
}

impl LayerVerdict {
    pub fn unverified(cause: UnverifiedCause, reason: impl Into<String>) -> Self {
        LayerVerdict::Unverified {
            cause,
            reason: reason.into(),
        }
    }

    pub fn is_verified(&self) -> bool {
        matches!(self, LayerVerdict::Verified(_))
    }

    pub fn is_refused(&self) -> bool {
        matches!(self, LayerVerdict::Refused { .. })
    }

    /// Verified or refused: a verdict that says something about the value.
    pub fn is_decisive(&self) -> bool {
        self.is_verified() || self.is_refused()
    }

    pub fn evidence(&self) -> Option<&Evidence> {
        match self {
            LayerVerdict::Verified(e) | LayerVerdict::Refused { evidence: e, .. } => Some(e),
            LayerVerdict::Unverified { .. } => None,
        }
    }

    pub fn cause(&self) -> Option<UnverifiedCause> {
        match self {
            LayerVerdict::Unverified { cause, .. } => Some(*cause),
            _ => None,
        }
    }

    /// The same refusal, marked as propagated from `loopback_key`.
    fn propagated_from(&self, loopback_key: &str) -> Option<LayerVerdict> {
        match self {
            LayerVerdict::Refused {
                evidence,
                delta_bound,
                ..
            } => Some(LayerVerdict::Refused {
                evidence: evidence.clone(),
                via: Some(loopback_key.to_string()),
                delta_bound: *delta_bound,
            }),
            _ => None,
        }
    }
}

/// What a probe of the loopback read, from **one** captured block.
#[derive(Debug, Clone, PartialEq)]
pub struct ProbeReading {
    /// `Err` carries the capture error. A failed capture is never silence.
    pub capture: Result<(), String>,
    pub xruns_delta: u32,
    /// [`total_peak_dbfs`] of the block.
    pub total_peak_dbfs: f64,
    /// Present when the THD analysis found a tone.
    pub tone: Option<ToneReading>,
}

/// The analysed tone of a probe block.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ToneReading {
    pub fundamental_dbfs: f64,
    pub noise_floor_dbfs: f64,
    /// Block length, samples.
    pub n: usize,
}

impl ProbeReading {
    /// A probe whose capture failed.
    pub fn capture_failed(reason: impl Into<String>, xruns_delta: u32) -> Self {
        ProbeReading {
            capture: Err(reason.into()),
            xruns_delta,
            total_peak_dbfs: f64::NEG_INFINITY,
            tone: None,
        }
    }

    /// In-lobe SNR, when a tone was analysed.
    pub fn snr_db(&self) -> Option<f64> {
        self.tone
            .map(|t| in_lobe_snr_db(t.fundamental_dbfs, t.noise_floor_dbfs, t.n))
    }

    /// `fundamental − drive`, when a tone was analysed.
    pub fn loop_gain_db(&self, drive_dbfs: f64) -> Option<f64> {
        self.tone.map(|t| t.fundamental_dbfs - drive_dbfs)
    }

    /// The numbers a rig record reads off the frame, at full precision.
    pub fn summary(&self, drive_dbfs: f64) -> ProbeSummary {
        ProbeSummary {
            loop_gain_db: self.loop_gain_db(drive_dbfs),
            total_peak_dbfs: self
                .total_peak_dbfs
                .is_finite()
                .then_some(self.total_peak_dbfs),
            snr_db: self.snr_db(),
            snr_min_db: PROBE_SNR_MIN_DB,
            xruns: self.xruns_delta,
            capture_error: self.capture.clone().err(),
        }
    }
}

/// A probe's raw readings as carried on the record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbeSummary {
    pub loop_gain_db: Option<f64>,
    pub total_peak_dbfs: Option<f64>,
    pub snr_db: Option<f64>,
    pub snr_min_db: f64,
    pub xruns: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_error: Option<String>,
}

/// Who judged, and when.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckCtx {
    /// The loopback's calibration key.
    pub key: String,
    pub checked_at: String,
    pub source: CheckSource,
}

fn no_baseline(key: &str) -> LayerVerdict {
    LayerVerdict::unverified(
        UnverifiedCause::NoBaseline,
        format!(
            "[{key}] has no loop-gain baseline; check: re-run `ac calibrate`, both legs measured"
        ),
    )
}

fn drive_differs(baseline_drive: f64, drive: f64) -> LayerVerdict {
    LayerVerdict::unverified(
        UnverifiedCause::BaselineDriveDiffers,
        format!(
            "baseline driven at {baseline_drive:.1} dBFS, check at {drive:.1}; \
             check: re-run `ac calibrate` at the default level"
        ),
    )
}

/// Whether a stored baseline can be judged at `drive_dbfs` at all. When
/// this is `Some`, no probe is needed and the verdict is final.
pub fn voltage_precheck(
    baseline: Option<&LoopGainBaseline>,
    drive_dbfs: f64,
    key: &str,
) -> Option<LayerVerdict> {
    match baseline {
        None => Some(no_baseline(key)),
        // Loop gain is not assumed independent of drive: converter
        // linearity is unscored at hundredths of a dB.
        Some(b) if (b.drive_dbfs - drive_dbfs).abs() > 1e-9 => {
            Some(drive_differs(b.drive_dbfs, drive_dbfs))
        }
        Some(_) => None,
    }
}

/// Judge a probe against the stored loop-gain baseline. The rules, in order:
///
/// 1. capture error → `not_measured` (never refused);
/// 2. xrun during the probe → `not_measured`;
/// 3. in-lobe SNR ≥ `S_min` (tone present) → Δ = `(fundamental − drive) −
///    baseline`, refused on `|Δ| > T`;
/// 4. tone absent **and** `total − drive < baseline − T` → refused, with
///    `delta_bound: at_most` (the whole return, noise included, is quieter
///    than the stored loop would make the tone alone);
/// 5. tone absent otherwise → `not_measured`.
pub fn judge_voltage(
    baseline: Option<&LoopGainBaseline>,
    probe: &ProbeReading,
    drive_dbfs: f64,
    ctx: &CheckCtx,
) -> LayerVerdict {
    if let Some(v) = voltage_precheck(baseline, drive_dbfs, &ctx.key) {
        return v;
    }
    let b = baseline.expect("precheck returns a verdict when the baseline is absent");
    if let Err(e) = &probe.capture {
        return LayerVerdict::unverified(
            UnverifiedCause::NotMeasured,
            format!("probe capture failed: {e}"),
        );
    }
    if probe.xruns_delta > 0 {
        return LayerVerdict::unverified(UnverifiedCause::NotMeasured, "xrun during probe");
    }
    let tolerance = LOOP_GAIN_TOLERANCE_DB;
    let evidence = |measured: f64| Evidence {
        measured,
        stored: b.loop_gain_db,
        delta: measured - b.loop_gain_db,
        tolerance,
        unit: VerdictUnit::Db,
        stored_at: b.measured_at.clone(),
        checked_at: ctx.checked_at.clone(),
        source: ctx.source,
    };
    let snr = probe.snr_db();
    if let (Some(snr), Some(tone)) = (snr, probe.tone) {
        if snr >= PROBE_SNR_MIN_DB {
            let ev = evidence(tone.fundamental_dbfs - drive_dbfs);
            return if ev.delta.abs() > tolerance {
                LayerVerdict::Refused {
                    evidence: ev,
                    via: None,
                    delta_bound: None,
                }
            } else {
                LayerVerdict::Verified(ev)
            };
        }
    }
    let total_gain = probe.total_peak_dbfs - drive_dbfs;
    if total_gain < b.loop_gain_db - tolerance {
        return LayerVerdict::Refused {
            evidence: evidence(total_gain),
            via: None,
            delta_bound: Some(DeltaBound::AtMost),
        };
    }
    let observation = match snr {
        Some(snr) => format!("tone SNR {snr:.1} dB, need {PROBE_SNR_MIN_DB:.1} dB"),
        None => format!(
            "no tone found at {PROBE_FREQ_HZ:.0} Hz, need tone SNR {PROBE_SNR_MIN_DB:.1} dB"
        ),
    };
    LayerVerdict::unverified(
        UnverifiedCause::NotMeasured,
        format!("{observation}; check: loopback cable, reference input gain"),
    )
}

/// The stored τ a latency check judges.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredTau {
    pub tau_s: f64,
    pub measured_at: String,
}

/// Judge a measured τ against the stored one, both for the same conditions.
/// `stored` is `Err(reason)` when no τ is stored for them; `measured` is
/// `Err(reason)` when the measurement refused itself.
pub fn judge_latency(
    stored: Result<&StoredTau, &str>,
    measured: Result<f64, &str>,
    sample_rate: u32,
    period_size: Option<u32>,
    ctx: &CheckCtx,
) -> LayerVerdict {
    let stored = match stored {
        Ok(s) => s,
        Err(reason) => return LayerVerdict::unverified(UnverifiedCause::NotStored, reason),
    };
    let measured_s = match measured {
        Ok(m) => m,
        Err(reason) => return LayerVerdict::unverified(UnverifiedCause::NotMeasured, reason),
    };
    let sr = sample_rate as f64;
    let (delta, agree) =
        match compare_tau_readings(stored.tau_s, measured_s, sample_rate, period_size) {
            TauComparison::Agree => (0.0, true),
            TauComparison::Disagree(d) => (d.delta_samples as f64, false),
        };
    let evidence = Evidence {
        measured: measured_s * sr,
        stored: stored.tau_s * sr,
        delta,
        tolerance: LATENCY_TOLERANCE_SAMPLES,
        unit: VerdictUnit::Samples,
        stored_at: stored.measured_at.clone(),
        checked_at: ctx.checked_at.clone(),
        source: ctx.source,
    };
    if agree {
        LayerVerdict::Verified(evidence)
    } else {
        LayerVerdict::Refused {
            evidence,
            via: None,
            delta_bound: None,
        }
    }
}

/// The `not_stored` reason for a τ lookup that missed.
pub fn no_stored_latency_reason(sample_rate: u32, period_size: Option<u32>) -> String {
    match period_size {
        Some(p) => format!("no stored latency at {sample_rate} Hz, period {p}"),
        None => format!("no stored latency at {sample_rate} Hz"),
    }
}

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

/// A calibration layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    Voltage,
    Latency,
}

/// The loopback a check measured.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoopbackRef {
    pub key: String,
    pub output_port: String,
    pub input_port: String,
}

/// What a check played into the loopback.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbeStimulus {
    pub level_dbfs: f64,
    pub freq_hz: f64,
}

/// A stored τ a latency refusal propagated to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LatencyReach {
    pub key: String,
    pub sample_rate: u32,
    pub period_size: Option<u32>,
}

/// The entries a refusal propagated to. Empty unless that layer is refused.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Reach {
    #[serde(default)]
    pub voltage: Vec<String>,
    #[serde(default)]
    pub latency: Vec<LatencyReach>,
}

/// The voltage value a check judged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VoltageIdentity {
    /// The baseline's `measured_at`.
    pub measured_at: String,
}

/// The τ entry a check judged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LatencyIdentity {
    pub measured_at: String,
    pub tau_s: f64,
    pub conditions: TauConditions,
}

/// The stored values a record's verdicts judged. A verdict applies only
/// while the stored value still has this identity.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct JudgedIdentity {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voltage: Option<VoltageIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency: Option<LatencyIdentity>,
}

/// Why a refused record was not written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersistErrorKind {
    /// `session_refusals.json` exists but cannot be read or parsed; the
    /// daemon never writes over it.
    Unreadable,
    /// The file was readable, and the write failed.
    WriteFailed,
    /// A kind this build does not know.
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersistError {
    pub kind: PersistErrorKind,
    /// The root io or serde error, with no path.
    pub detail: String,
}

/// One run of the session check.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionCheckRecord {
    /// `"<pid>-<seq>"`, unique within a daemon process.
    pub id: String,
    /// RFC3339 with milliseconds — records are ordered by it.
    pub ran_at: String,
    /// The device-enumeration epoch the check ran in.
    pub epoch: DeviceEpoch,
    pub source: CheckSource,
    /// The command that ran the check.
    pub cmd: String,
    pub loopback: LoopbackRef,
    /// `None` when the check emitted no probe tone.
    pub stimulus: Option<ProbeStimulus>,
    /// Emission length, seconds; `0` with no stimulus.
    pub duration_s: f64,
    #[serde(default)]
    pub reach: Reach,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency: Option<LayerVerdict>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voltage: Option<LayerVerdict>,
    #[serde(default)]
    pub judged: JudgedIdentity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe: Option<ProbeSummary>,
    /// Seconds between the two τ lifecycles' captures, when τ was measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_separation_s: Option<f64>,
    /// Present only on a record with a refused layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persisted: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persist_error: Option<PersistError>,
}

impl SessionCheckRecord {
    pub fn layer(&self, layer: Layer) -> Option<&LayerVerdict> {
        match layer {
            Layer::Voltage => self.voltage.as_ref(),
            Layer::Latency => self.latency.as_ref(),
        }
    }

    pub fn has_refusal(&self) -> bool {
        [Layer::Voltage, Layer::Latency]
            .iter()
            .any(|l| self.layer(*l).is_some_and(LayerVerdict::is_refused))
    }

    /// Mark whether the refusal reached disk. No-op on a record without one.
    pub fn set_persisted(&mut self, result: Result<(), PersistError>) {
        if !self.has_refusal() {
            self.persisted = None;
            self.persist_error = None;
            return;
        }
        match result {
            Ok(()) => {
                self.persisted = Some(true);
                self.persist_error = None;
            }
            Err(e) => {
                self.persisted = Some(false);
                self.persist_error = Some(e);
            }
        }
    }

    pub fn summary(&self) -> RecordSummary {
        RecordSummary {
            id: self.id.clone(),
            ran_at: self.ran_at.clone(),
            loopback: self.loopback.key.clone(),
            persisted: self.persisted,
            persist_error: self.persist_error.clone(),
        }
    }
}

/// Everything a stored layer needs to be told whether a refusal reaches it.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredEntry<'a> {
    pub key: &'a str,
    pub has_voltage: bool,
    pub tau_conditions: Vec<&'a TauConditions>,
}

/// Fill `record.reach` from the stored entries: a refusal propagates, a
/// verification never does. The loopback's own key is not listed.
pub fn propagate(record: &mut SessionCheckRecord, entries: &[StoredEntry<'_>]) {
    let own = record.loopback.key.clone();
    let mut reach = Reach::default();
    if record
        .voltage
        .as_ref()
        .is_some_and(LayerVerdict::is_refused)
    {
        let mut keys: Vec<String> = entries
            .iter()
            .filter(|e| e.has_voltage && e.key != own)
            .map(|e| e.key.to_string())
            .collect();
        keys.sort();
        keys.dedup();
        reach.voltage = keys;
    }
    if let (Some(true), Some(checked)) = (
        record.latency.as_ref().map(LayerVerdict::is_refused),
        record.judged.latency.as_ref().map(|l| &l.conditions),
    ) {
        let mut list: Vec<LatencyReach> = Vec::new();
        for e in entries.iter().filter(|e| e.key != own) {
            for c in &e.tau_conditions {
                let shares = c.device == checked.device
                    && c.backend == checked.backend
                    && c.sample_rate == checked.sample_rate
                    && c.period_size == checked.period_size;
                let item = LatencyReach {
                    key: e.key.to_string(),
                    sample_rate: c.sample_rate,
                    period_size: c.period_size,
                };
                if shares && !list.contains(&item) {
                    list.push(item);
                }
            }
        }
        list.sort_by(|a, b| a.key.cmp(&b.key));
        reach.latency = list;
    }
    record.reach = reach;
}

// ---------------------------------------------------------------------------
// Persisted refusals
// ---------------------------------------------------------------------------

/// Contents of `session_refusals.json`: the latest refused record per layer
/// per loopback key.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionRefusals {
    #[serde(default)]
    pub voltage: BTreeMap<String, SessionCheckRecord>,
    #[serde(default)]
    pub latency: BTreeMap<String, SessionCheckRecord>,
}

impl SessionRefusals {
    fn map(&self, layer: Layer) -> &BTreeMap<String, SessionCheckRecord> {
        match layer {
            Layer::Voltage => &self.voltage,
            Layer::Latency => &self.latency,
        }
    }

    fn map_mut(&mut self, layer: Layer) -> &mut BTreeMap<String, SessionCheckRecord> {
        match layer {
            Layer::Voltage => &mut self.voltage,
            Layer::Latency => &mut self.latency,
        }
    }
}

/// Merge this process's records into the file's contents: a refused layer
/// replaces an older file entry for its loopback, a verified layer clears an
/// older one. A process record wins a tie — it is the newer knowledge. The
/// result is what the file should hold; compare it with the input to decide
/// whether to write.
pub fn merge_refusals(file: &SessionRefusals, process: &[SessionCheckRecord]) -> SessionRefusals {
    let mut out = file.clone();
    let mut ordered: Vec<&SessionCheckRecord> = process.iter().collect();
    ordered.sort_by(|a, b| a.ran_at.cmp(&b.ran_at));
    for record in ordered {
        for layer in [Layer::Voltage, Layer::Latency] {
            let Some(verdict) = record.layer(layer) else {
                continue;
            };
            let key = record.loopback.key.clone();
            let older = out
                .map(layer)
                .get(&key)
                .is_none_or(|existing| existing.ran_at <= record.ran_at);
            if !older {
                continue;
            }
            if verdict.is_refused() {
                let mut stored = record.clone();
                stored.persisted = Some(true);
                stored.persist_error = None;
                out.map_mut(layer).insert(key, stored);
            } else if verdict.is_verified() {
                out.map_mut(layer).remove(&key);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Effective verdict: time rule, pair rule, precedence
// ---------------------------------------------------------------------------

/// The records a verdict is decided from.
#[derive(Debug, Clone, Copy)]
pub struct RecordSet<'a> {
    /// Records made by this daemon process, verified and refused alike.
    pub process: &'a [SessionCheckRecord],
    /// `session_refusals.json`, or the observation that it is unreadable.
    pub file: Result<&'a SessionRefusals, &'a str>,
}

/// Where checks can reach now.
#[derive(Debug, Clone, Copy)]
pub struct CheckScope<'a> {
    /// The configured reference loopback's key, if any.
    pub loopback_key: Option<&'a str>,
    /// The drive level a check on the loopback uses.
    pub drive_dbfs: f64,
    /// The device-enumeration epoch now.
    pub current_epoch: &'a DeviceEpoch,
}

/// The stored value a layer verdict is about.
#[derive(Debug, Clone, Copy)]
pub enum Target<'a> {
    Voltage {
        key: &'a str,
        has_voltage: bool,
        baseline: Option<&'a LoopGainBaseline>,
    },
    Latency {
        key: &'a str,
        /// The τ entry resolved now, or the `not_stored` reason.
        stored: Result<(&'a StoredTau, &'a TauConditions), &'a str>,
    },
}

impl Target<'_> {
    fn layer(&self) -> Layer {
        match self {
            Target::Voltage { .. } => Layer::Voltage,
            Target::Latency { .. } => Layer::Latency,
        }
    }

    fn key(&self) -> &str {
        match self {
            Target::Voltage { key, .. } | Target::Latency { key, .. } => key,
        }
    }

    /// The `not_stored` verdict, when nothing is stored.
    fn not_stored(&self) -> Option<LayerVerdict> {
        match self {
            Target::Voltage {
                has_voltage: false, ..
            } => Some(LayerVerdict::unverified(
                UnverifiedCause::NotStored,
                "no voltage calibration stored",
            )),
            Target::Voltage { .. } => None,
            Target::Latency {
                stored: Err(reason),
                ..
            } => Some(LayerVerdict::unverified(
                UnverifiedCause::NotStored,
                *reason,
            )),
            Target::Latency { .. } => None,
        }
    }

    /// Whether `record` judged the value stored now.
    fn same_identity(&self, record: &SessionCheckRecord) -> bool {
        match (self, &record.judged) {
            (Target::Voltage { baseline, .. }, j) => match (baseline, &j.voltage) {
                (Some(b), Some(v)) => b.measured_at == v.measured_at,
                _ => false,
            },
            (
                Target::Latency {
                    stored: Ok((s, c)), ..
                },
                j,
            ) => j.latency.as_ref().is_some_and(|l| {
                l.measured_at == s.measured_at && l.tau_s == s.tau_s && &&l.conditions == c
            }),
            (Target::Latency { .. }, _) => false,
        }
    }

    /// Whether `record`'s propagated refusal reaches this value.
    fn in_reach(&self, record: &SessionCheckRecord) -> bool {
        match self {
            Target::Voltage { key, baseline, .. } => {
                // A value stored after the refusal is not the one refused.
                let restored = baseline.is_some_and(|b| b.measured_at > record.ran_at);
                !restored && record.reach.voltage.iter().any(|k| k == key)
            }
            Target::Latency {
                key,
                stored: Ok((s, c)),
            } => {
                s.measured_at <= record.ran_at
                    && record.reach.latency.iter().any(|r| {
                        r.key == *key
                            && r.sample_rate == c.sample_rate
                            && r.period_size == c.period_size
                    })
            }
            Target::Latency { .. } => false,
        }
    }
}

/// A verdict together with the record that decided it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Effective {
    pub verdict: LayerVerdict,
    pub record: Option<RecordSummary>,
}

/// The parts of a deciding record a reader needs beside the verdict.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordSummary {
    pub id: String,
    pub ran_at: String,
    pub loopback: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persisted: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persist_error: Option<PersistError>,
}

#[derive(Clone, Copy)]
struct Candidate<'a> {
    record: &'a SessionCheckRecord,
    from_process: bool,
}

/// Order: newer `ran_at` wins, and on a tie a process record wins.
fn newer(a: &Candidate<'_>, b: &Candidate<'_>) -> bool {
    (a.record.ran_at.as_str(), a.from_process) > (b.record.ran_at.as_str(), b.from_process)
}

fn latest<'a>(items: impl Iterator<Item = Candidate<'a>>) -> Option<Candidate<'a>> {
    items.fold(None, |best, c| match best {
        Some(b) if !newer(&c, &b) => Some(b),
        _ => Some(c),
    })
}

/// The noun a crossed epoch names, for `not_checked`.
fn boundary_noun(check: &EnumerationCheck) -> &'static str {
    match check {
        EnumerationCheck::Crossed { boundary, .. }
            if boundary.starts_with(BOUNDARY_HOST_REBOOTED) =>
        {
            "reboot"
        }
        EnumerationCheck::Crossed { .. } => "re-enumeration",
        _ => "device enumeration change",
    }
}

/// `not_checked` reasons (the time rule).
pub const NOT_CHECKED_NO_CHECK: &str = "no check has run";
pub const NOT_CHECKED_STORED_AFTER: &str = "value stored after last check";

/// The reason an unreadable refusal record gives every layer it hides.
pub fn refusals_unreadable_reason(observation: &str) -> String {
    format!(
        "{SESSION_REFUSALS_FILE} unreadable: {observation}; \
         check: its permissions and contents, beside cal.json"
    )
}

/// The head of an `unverified` verdict, as every surface prints it after
/// `UNVERIFIED — `: `not_measured` says the check did not measure (the
/// reason is the observation beneath it), `refusals_unreadable` names the
/// file only, and every other cause prints its observation (the reason up to
/// `; check: `). Chosen from `cause`, never by matching the reason.
pub fn unverified_head(cause: UnverifiedCause, reason: &str) -> String {
    match cause {
        UnverifiedCause::NotMeasured => "check did not measure".to_string(),
        UnverifiedCause::RefusalsUnreadable => format!("{SESSION_REFUSALS_FILE} unreadable"),
        _ => split_check(reason).0.to_string(),
    }
}

/// Split a reason on its `; check: ` separator.
pub fn split_check(reason: &str) -> (&str, Option<&str>) {
    match reason.split_once("; check: ") {
        Some((obs, check)) => (obs, Some(check)),
        None => (reason, None),
    }
}

/// An RFC3339 timestamp to whole seconds, `2026-09-16T14:02:11Z`. Text
/// that does not parse is returned unchanged.
pub fn whole_seconds(ts: &str) -> String {
    match chrono::DateTime::parse_from_rfc3339(ts) {
        Ok(t) => t
            .with_timezone(&chrono::Utc)
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        Err(_) => ts.to_string(),
    }
}

/// The verdict that applies to `target` now, from every record there is.
/// See the module doc for the rules; the order below is their precedence.
pub fn effective(target: Target<'_>, set: RecordSet<'_>, scope: CheckScope<'_>) -> Effective {
    let plain = |verdict: LayerVerdict| Effective {
        verdict,
        record: None,
    };
    if let Some(v) = target.not_stored() {
        return plain(v);
    }
    let layer = target.layer();
    let key = target.key();

    let empty = SessionRefusals::default();
    let file = set.file.unwrap_or(&empty);
    let decisive = set
        .process
        .iter()
        .filter(|r| r.layer(layer).is_some_and(LayerVerdict::is_decisive))
        .map(|record| Candidate {
            record,
            from_process: true,
        })
        .chain(file.map(layer).values().map(|record| Candidate {
            record,
            from_process: false,
        }))
        .filter(|c| c.record.layer(layer).is_some_and(LayerVerdict::is_decisive));
    let candidates: Vec<Candidate<'_>> = decisive.collect();

    let decided = |c: Candidate<'_>, verdict: LayerVerdict| {
        (
            c.from_process,
            Effective {
                verdict,
                record: Some(c.record.summary()),
            },
        )
    };

    let direct = latest(
        candidates
            .iter()
            .copied()
            .filter(|c| c.record.loopback.key == key),
    );
    let mut stale: Option<String> = None;
    let mut verified: Option<(bool, Effective)> = None;
    let mut refused: Option<(bool, Effective)> = None;
    if let Some(c) = direct {
        let verdict = c.record.layer(layer).cloned().expect("filtered decisive");
        if !target.same_identity(c.record) {
            stale = Some(NOT_CHECKED_STORED_AFTER.to_string());
        } else if verdict.is_refused() {
            refused = Some(decided(c, verdict));
        } else {
            let check = EnumerationCheck::of(Some(&c.record.epoch), scope.current_epoch);
            if c.from_process && check.is_same() {
                verified = Some(decided(c, verdict));
            } else {
                stale = Some(format!("last check predates the {}", boundary_noun(&check)));
            }
        }
    }

    if refused.is_none() {
        // A refusal on another loopback reaches this value when it is that
        // loopback's latest decisive record.
        let mut loopbacks: Vec<&str> = candidates
            .iter()
            .map(|c| c.record.loopback.key.as_str())
            .filter(|k| *k != key)
            .collect();
        loopbacks.sort_unstable();
        loopbacks.dedup();
        let via = loopbacks
            .into_iter()
            .filter_map(|lk| {
                latest(
                    candidates
                        .iter()
                        .copied()
                        .filter(|c| c.record.loopback.key == lk),
                )
            })
            .filter(|c| {
                c.record.layer(layer).is_some_and(LayerVerdict::is_refused)
                    && target.in_reach(c.record)
            });
        if let Some(c) = latest(via) {
            let verdict = c
                .record
                .layer(layer)
                .and_then(|v| v.propagated_from(&c.record.loopback.key))
                .expect("filtered refused");
            refused = Some(decided(c, verdict));
        }
    }

    // Over an unreadable record only this process's verdicts decide.
    let unreadable = set.file.err();
    for (from_process, eff) in [refused, verified].into_iter().flatten() {
        if from_process || unreadable.is_none() {
            return eff;
        }
    }
    if let Some(observation) = unreadable {
        return plain(LayerVerdict::unverified(
            UnverifiedCause::RefusalsUnreadable,
            refusals_unreadable_reason(observation),
        ));
    }
    if let Some(reason) = stale {
        return plain(LayerVerdict::unverified(
            UnverifiedCause::NotChecked,
            reason,
        ));
    }
    // A check from this process that ran on this value but did not measure.
    let attempted = set
        .process
        .iter()
        .filter(|r| r.loopback.key == key)
        .filter_map(|r| r.layer(layer).map(|v| (r, v)))
        .filter(|(_, v)| !v.is_decisive())
        .max_by(|a, b| a.0.ran_at.cmp(&b.0.ran_at));
    if let Some((r, v)) = attempted {
        return Effective {
            verdict: v.clone(),
            record: Some(r.summary()),
        };
    }
    match scope.loopback_key {
        None => plain(LayerVerdict::unverified(
            UnverifiedCause::NoLoopback,
            "no reference loopback configured",
        )),
        Some(lk) if lk != key => plain(LayerVerdict::unverified(
            UnverifiedCause::NotCovered,
            format!("check covers [{lk}], not [{key}]"),
        )),
        Some(_) => {
            if let Target::Voltage { baseline, .. } = target {
                if let Some(v) = voltage_precheck(baseline, scope.drive_dbfs, key) {
                    return plain(v);
                }
            }
            plain(LayerVerdict::unverified(
                UnverifiedCause::NotChecked,
                NOT_CHECKED_NO_CHECK,
            ))
        }
    }
}

/// A layer verdict for a value no fresh check measured: the effective
/// verdict, with `refusals_unreadable` taking precedence over every cause
/// but `not_stored` (a consumer that meets the file unreadable must say so).
pub fn with_precedence(verdict: LayerVerdict, unreadable: Option<&str>) -> LayerVerdict {
    match (&verdict, unreadable) {
        (LayerVerdict::Unverified { cause, .. }, Some(observation))
            if cause.rank() < UnverifiedCause::RefusalsUnreadable.rank() =>
        {
            LayerVerdict::unverified(
                UnverifiedCause::RefusalsUnreadable,
                refusals_unreadable_reason(observation),
            )
        }
        _ => verdict,
    }
}

#[cfg(test)]
mod tests {
    use super::super::epoch::fixtures::observed;
    use super::super::tau::fixtures::dummy_conditions;
    use super::*;

    const DRIVE: f64 = -40.0;
    const BASE_GAIN: f64 = -0.60;

    fn epoch_a() -> DeviceEpoch {
        observed("boot-a", &[("/dev/fw1", "2026-09-15T08:00:05Z")])
    }

    fn epoch_rebooted() -> DeviceEpoch {
        observed("boot-b", &[("/dev/fw1", "2026-09-16T08:00:05Z")])
    }

    fn baseline() -> LoopGainBaseline {
        LoopGainBaseline {
            loop_gain_db: BASE_GAIN,
            freq_hz: PROBE_FREQ_HZ,
            drive_dbfs: DRIVE,
            measured_at: "2026-09-15T23:43:04Z".to_string(),
            epoch: epoch_a(),
        }
    }

    fn ctx(key: &str) -> CheckCtx {
        CheckCtx {
            key: key.to_string(),
            checked_at: "2026-09-16T14:02:11.000Z".to_string(),
            source: CheckSource::Probe,
        }
    }

    /// A clean probe whose tone reads `gain` dB above the drive.
    fn tone_probe(gain: f64, snr_db: f64) -> ProbeReading {
        let n = 28_800;
        let fundamental = DRIVE + gain;
        // Invert `in_lobe_snr_db` for the requested SNR.
        let noise = fundamental - PEAK_TO_RMS_DB - snr_db + 10.0 * ((n as f64 / 2.0) / 1.5).log10();
        ProbeReading {
            capture: Ok(()),
            xruns_delta: 0,
            total_peak_dbfs: fundamental,
            tone: Some(ToneReading {
                fundamental_dbfs: fundamental,
                noise_floor_dbfs: noise,
                n,
            }),
        }
    }

    // ─── sign rules ─────────────────────────────────────────────────────

    #[test]
    fn sign_a_silent_return_is_refused_as_a_bound() {
        let probe = ProbeReading {
            capture: Ok(()),
            xruns_delta: 0,
            total_peak_dbfs: -240.0,
            tone: None,
        };
        let v = judge_voltage(Some(&baseline()), &probe, DRIVE, &ctx("out1_in1"));
        match v {
            LayerVerdict::Refused {
                evidence,
                delta_bound,
                via,
            } => {
                assert_eq!(delta_bound, Some(DeltaBound::AtMost));
                assert_eq!(via, None);
                assert!((evidence.delta - (-240.0 - DRIVE - BASE_GAIN)).abs() < 1e-9);
            }
            other => panic!("expected refused, got {other:?}"),
        }
    }

    /// The rejected implementation (revision 1) compared the broadband
    /// noise floor against the baseline, which refuses a correct but noisy
    /// loop. This test computes that rule on the same numbers and shows it
    /// would have refused, so the test can tell the two apart.
    #[test]
    fn sign_b_a_noisy_tone_at_the_baseline_is_unverified_not_refused() {
        let probe = tone_probe(BASE_GAIN, PROBE_SNR_MIN_DB - 10.0);
        let v = judge_voltage(Some(&baseline()), &probe, DRIVE, &ctx("out1_in1"));
        assert_eq!(v.cause(), Some(UnverifiedCause::NotMeasured), "{v:?}");
        match &v {
            LayerVerdict::Unverified { reason, .. } => {
                assert!(reason.contains("tone SNR"), "{reason}");
                assert!(reason.contains("need 50.8 dB"), "{reason}");
            }
            _ => unreachable!(),
        }

        let floor = probe.tone.unwrap().noise_floor_dbfs;
        let rejected_refuses = floor - DRIVE < BASE_GAIN - LOOP_GAIN_TOLERANCE_DB;
        assert!(
            rejected_refuses,
            "revision 1's rule must refuse this fixture, or the test proves nothing"
        );
    }

    #[test]
    fn sign_c_a_capture_error_is_never_refused() {
        let probe = ProbeReading::capture_failed("device gone", 0);
        let v = judge_voltage(Some(&baseline()), &probe, DRIVE, &ctx("out1_in1"));
        assert_eq!(v.cause(), Some(UnverifiedCause::NotMeasured));
    }

    #[test]
    fn sign_d_a_three_db_shift_with_the_tone_present_is_refused() {
        let probe = tone_probe(BASE_GAIN + 3.0, 90.0);
        let v = judge_voltage(Some(&baseline()), &probe, DRIVE, &ctx("out1_in1"));
        assert!(v.is_refused(), "{v:?}");
        let e = v.evidence().unwrap();
        assert!((e.delta - 3.0).abs() < 1e-9);
        assert_eq!(e.unit, VerdictUnit::Db);
    }

    #[test]
    fn sign_e_a_delta_within_tolerance_is_verified() {
        for d in [
            0.0,
            LOOP_GAIN_TOLERANCE_DB - 1e-9,
            -LOOP_GAIN_TOLERANCE_DB + 1e-9,
        ] {
            let probe = tone_probe(BASE_GAIN + d, 90.0);
            let v = judge_voltage(Some(&baseline()), &probe, DRIVE, &ctx("out1_in1"));
            assert!(v.is_verified(), "Δ {d}: {v:?}");
        }
        let probe = tone_probe(BASE_GAIN + LOOP_GAIN_TOLERANCE_DB + 1e-6, 90.0);
        assert!(judge_voltage(Some(&baseline()), &probe, DRIVE, &ctx("k")).is_refused());
    }

    #[test]
    fn an_xrun_during_the_probe_is_not_measured() {
        let mut probe = tone_probe(BASE_GAIN + 3.0, 90.0);
        probe.xruns_delta = 1;
        let v = judge_voltage(Some(&baseline()), &probe, DRIVE, &ctx("out1_in1"));
        assert_eq!(v.cause(), Some(UnverifiedCause::NotMeasured));
    }

    #[test]
    fn a_missing_or_differently_driven_baseline_is_not_judged() {
        let probe = tone_probe(BASE_GAIN + 3.0, 90.0);
        let v = judge_voltage(None, &probe, DRIVE, &ctx("out1_in1"));
        assert_eq!(v.cause(), Some(UnverifiedCause::NoBaseline));
        let mut b = baseline();
        b.drive_dbfs = -30.0;
        let v = judge_voltage(Some(&b), &probe, DRIVE, &ctx("out1_in1"));
        assert_eq!(v.cause(), Some(UnverifiedCause::BaselineDriveDiffers));
    }

    #[test]
    fn latency_one_sample_off_is_refused_and_exact_is_verified() {
        let stored = StoredTau {
            tau_s: 1711.0 / 96_000.0,
            measured_at: "2026-09-15T23:43:04Z".to_string(),
        };
        let v = judge_latency(
            Ok(&stored),
            Ok(1711.0 / 96_000.0),
            96_000,
            Some(256),
            &ctx("k"),
        );
        assert!(v.is_verified());
        let v = judge_latency(
            Ok(&stored),
            Ok(1712.0 / 96_000.0),
            96_000,
            Some(256),
            &ctx("k"),
        );
        assert!(v.is_refused());
        assert_eq!(v.evidence().unwrap().delta, 1.0);
        let v = judge_latency(Err("none"), Ok(0.0), 96_000, Some(256), &ctx("k"));
        assert_eq!(v.cause(), Some(UnverifiedCause::NotStored));
        let v = judge_latency(
            Ok(&stored),
            Err("2 lifetimes disagree"),
            96_000,
            None,
            &ctx("k"),
        );
        assert_eq!(v.cause(), Some(UnverifiedCause::NotMeasured));
    }

    // ─── records, propagation, time rule ────────────────────────────────

    fn record(key: &str, ran_at: &str, voltage: LayerVerdict) -> SessionCheckRecord {
        SessionCheckRecord {
            id: format!("1-{ran_at}"),
            ran_at: ran_at.to_string(),
            epoch: epoch_a(),
            source: CheckSource::Explicit,
            cmd: "session_check".to_string(),
            loopback: LoopbackRef {
                key: key.to_string(),
                output_port: "system:playback_2".to_string(),
                input_port: "system:capture_2".to_string(),
            },
            stimulus: Some(ProbeStimulus {
                level_dbfs: DRIVE,
                freq_hz: PROBE_FREQ_HZ,
            }),
            duration_s: PROBE_SETTLE_S + PROBE_CAPTURE_S,
            reach: Reach::default(),
            latency: None,
            voltage: Some(voltage),
            judged: JudgedIdentity {
                voltage: Some(VoltageIdentity {
                    measured_at: baseline().measured_at,
                }),
                latency: None,
            },
            probe: None,
            latency_separation_s: None,
            persisted: None,
            persist_error: None,
        }
    }

    fn refused_v() -> LayerVerdict {
        judge_voltage(
            Some(&baseline()),
            &tone_probe(BASE_GAIN + 3.0, 90.0),
            DRIVE,
            &ctx("out3_in3"),
        )
    }

    fn verified_v() -> LayerVerdict {
        judge_voltage(
            Some(&baseline()),
            &tone_probe(BASE_GAIN, 90.0),
            DRIVE,
            &ctx("out3_in3"),
        )
    }

    fn entries() -> Vec<StoredEntry<'static>> {
        vec![
            StoredEntry {
                key: "out1_in1",
                has_voltage: true,
                tau_conditions: vec![],
            },
            StoredEntry {
                key: "out3_in3",
                has_voltage: true,
                tau_conditions: vec![],
            },
            StoredEntry {
                key: "out2_in2",
                has_voltage: false,
                tau_conditions: vec![],
            },
        ]
    }

    fn voltage_target<'a>(key: &'a str, b: &'a LoopGainBaseline) -> Target<'a> {
        Target::Voltage {
            key,
            has_voltage: true,
            baseline: Some(b),
        }
    }

    fn scope<'a>(loopback: Option<&'a str>, epoch: &'a DeviceEpoch) -> CheckScope<'a> {
        CheckScope {
            loopback_key: loopback,
            drive_dbfs: DRIVE,
            current_epoch: epoch,
        }
    }

    #[test]
    fn a_refusal_propagates_and_names_its_source() {
        let mut r = record("out3_in3", "2026-09-16T14:02:11.000Z", refused_v());
        propagate(&mut r, &entries());
        assert_eq!(r.reach.voltage, vec!["out1_in1".to_string()]);

        let b = baseline();
        let now = epoch_a();
        let recs = [r];
        let eff = effective(
            voltage_target("out1_in1", &b),
            RecordSet {
                process: &recs,
                file: Ok(&SessionRefusals::default()),
            },
            scope(Some("out3_in3"), &now),
        );
        match eff.verdict {
            LayerVerdict::Refused { via, .. } => assert_eq!(via.as_deref(), Some("out3_in3")),
            other => panic!("expected refused via, got {other:?}"),
        }
    }

    #[test]
    fn a_verification_never_propagates() {
        let mut r = record("out3_in3", "2026-09-16T14:02:11.000Z", verified_v());
        propagate(&mut r, &entries());
        assert_eq!(r.reach, Reach::default());

        let b = baseline();
        let now = epoch_a();
        let recs = [r];
        let eff = effective(
            voltage_target("out1_in1", &b),
            RecordSet {
                process: &recs,
                file: Ok(&SessionRefusals::default()),
            },
            scope(Some("out3_in3"), &now),
        );
        assert_eq!(eff.verdict.cause(), Some(UnverifiedCause::NotCovered));
    }

    #[test]
    fn latency_refusal_reaches_entries_sharing_device_rate_and_period() {
        let cond = dummy_conditions();
        let mut other_rate = dummy_conditions();
        other_rate.sample_rate = 96_000;
        let mut acoustic = dummy_conditions();
        acoustic.input_port = "fake:capture_1".to_string();
        let mut r = record("out0_in0", "2026-09-16T14:02:11.000Z", verified_v());
        r.voltage = None;
        r.latency = Some(judge_latency(
            Ok(&StoredTau {
                tau_s: 0.01,
                measured_at: "2026-09-15T00:00:00Z".to_string(),
            }),
            Ok(0.02),
            48_000,
            Some(1024),
            &ctx("out0_in0"),
        ));
        r.judged.latency = Some(LatencyIdentity {
            measured_at: "2026-09-15T00:00:00Z".to_string(),
            tau_s: 0.01,
            conditions: cond.clone(),
        });
        let entries = vec![
            StoredEntry {
                key: "out0_in0",
                has_voltage: false,
                tau_conditions: vec![&cond],
            },
            StoredEntry {
                key: "out0_in1",
                has_voltage: false,
                tau_conditions: vec![&acoustic, &other_rate],
            },
        ];
        propagate(&mut r, &entries);
        assert_eq!(
            r.reach.latency,
            vec![LatencyReach {
                key: "out0_in1".to_string(),
                sample_rate: 48_000,
                period_size: Some(1024),
            }]
        );
        assert!(r.reach.voltage.is_empty());
    }

    #[test]
    fn time_rule_verified_expires_on_a_crossed_epoch() {
        let b = baseline();
        let recs = [record("out1_in1", "2026-09-16T14:02:11.000Z", verified_v())];
        let set = RecordSet {
            process: &recs,
            file: Ok(&SessionRefusals::default()),
        };
        let same = epoch_a();
        let eff = effective(
            voltage_target("out1_in1", &b),
            set,
            scope(Some("out1_in1"), &same),
        );
        assert!(eff.verdict.is_verified());

        let later = epoch_rebooted();
        let eff = effective(
            voltage_target("out1_in1", &b),
            set,
            scope(Some("out1_in1"), &later),
        );
        match eff.verdict {
            LayerVerdict::Unverified {
                cause: UnverifiedCause::NotChecked,
                reason,
            } => assert_eq!(reason, "last check predates the reboot"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn time_rule_refused_survives_a_crossed_epoch_and_a_restart() {
        let b = baseline();
        let mut file = SessionRefusals::default();
        file.voltage.insert(
            "out1_in1".to_string(),
            record("out1_in1", "2026-09-16T14:02:11.000Z", refused_v()),
        );
        let later = epoch_rebooted();
        let eff = effective(
            voltage_target("out1_in1", &b),
            RecordSet {
                process: &[],
                file: Ok(&file),
            },
            scope(Some("out1_in1"), &later),
        );
        assert!(eff.verdict.is_refused(), "{:?}", eff.verdict);
    }

    #[test]
    fn time_rule_refused_clears_when_a_new_value_is_stored() {
        let mut b = baseline();
        b.measured_at = "2026-09-17T09:00:00Z".to_string();
        let recs = [record("out1_in1", "2026-09-16T14:02:11.000Z", refused_v())];
        let now = epoch_a();
        let eff = effective(
            voltage_target("out1_in1", &b),
            RecordSet {
                process: &recs,
                file: Ok(&SessionRefusals::default()),
            },
            scope(Some("out1_in1"), &now),
        );
        match eff.verdict {
            LayerVerdict::Unverified {
                cause: UnverifiedCause::NotChecked,
                reason,
            } => assert_eq!(reason, NOT_CHECKED_STORED_AFTER),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_later_pass_on_the_loopback_clears_its_refusal_and_the_propagated_one() {
        let b = baseline();
        let mut file = SessionRefusals::default();
        let mut refused = record("out3_in3", "2026-09-16T14:02:11.000Z", refused_v());
        propagate(&mut refused, &entries());
        file.voltage.insert("out3_in3".to_string(), refused);
        let recs = [record("out3_in3", "2026-09-16T15:00:00.000Z", verified_v())];
        let now = epoch_a();
        let set = RecordSet {
            process: &recs,
            file: Ok(&file),
        };
        let own = effective(
            voltage_target("out3_in3", &b),
            set,
            scope(Some("out3_in3"), &now),
        );
        assert!(own.verdict.is_verified(), "{:?}", own.verdict);
        let other = effective(
            voltage_target("out1_in1", &b),
            set,
            scope(Some("out3_in3"), &now),
        );
        assert_eq!(other.verdict.cause(), Some(UnverifiedCause::NotCovered));
    }

    #[test]
    fn nothing_recorded_reads_by_scope() {
        let b = baseline();
        let now = epoch_a();
        let set = RecordSet {
            process: &[],
            file: Ok(&SessionRefusals::default()),
        };
        let cause = |lk: Option<&str>, key: &str| {
            effective(voltage_target(key, &b), set, scope(lk, &now))
                .verdict
                .cause()
        };
        assert_eq!(cause(None, "out1_in1"), Some(UnverifiedCause::NoLoopback));
        assert_eq!(
            cause(Some("out3_in3"), "out1_in1"),
            Some(UnverifiedCause::NotCovered)
        );
        assert_eq!(
            cause(Some("out1_in1"), "out1_in1"),
            Some(UnverifiedCause::NotChecked)
        );
        let none = effective(
            Target::Voltage {
                key: "out1_in1",
                has_voltage: true,
                baseline: None,
            },
            set,
            scope(Some("out1_in1"), &now),
        );
        assert_eq!(none.verdict.cause(), Some(UnverifiedCause::NoBaseline));
    }

    // ─── unreadable refusal record ──────────────────────────────────────

    #[test]
    fn precedence_not_stored_then_unreadable_then_the_rest() {
        use UnverifiedCause::*;
        for cause in [
            NoLoopback,
            NotMeasured,
            NoBaseline,
            BaselineDriveDiffers,
            NotCovered,
            NotChecked,
            Unknown,
        ] {
            let v = with_precedence(LayerVerdict::unverified(cause, "x"), Some("bad"));
            assert_eq!(v.cause(), Some(RefusalsUnreadable), "{cause:?}");
        }
        let v = with_precedence(LayerVerdict::unverified(NotStored, "x"), Some("bad"));
        assert_eq!(v.cause(), Some(NotStored));
        assert!(with_precedence(verified_v(), Some("bad")).is_verified());
        assert!(with_precedence(refused_v(), Some("bad")).is_refused());

        let b = baseline();
        let now = epoch_a();
        let nothing = effective(
            Target::Voltage {
                key: "out1_in1",
                has_voltage: false,
                baseline: Some(&b),
            },
            RecordSet {
                process: &[],
                file: Err("expected value at line 1 column 1"),
            },
            scope(Some("out1_in1"), &now),
        );
        assert_eq!(nothing.verdict.cause(), Some(NotStored));
    }

    #[test]
    fn unreadable_file_a_process_verdict_decides_the_rest_read_unreadable() {
        let b = baseline();
        let now = epoch_a();
        let recs = [record("out1_in1", "2026-09-16T14:02:11.000Z", verified_v())];
        let set = RecordSet {
            process: &recs,
            file: Err("expected value at line 1 column 1"),
        };
        let own = effective(
            voltage_target("out1_in1", &b),
            set,
            scope(Some("out1_in1"), &now),
        );
        assert!(own.verdict.is_verified());
        let other = effective(
            voltage_target("out3_in3", &b),
            set,
            scope(Some("out1_in1"), &now),
        );
        match other.verdict {
            LayerVerdict::Unverified {
                cause: UnverifiedCause::RefusalsUnreadable,
                reason,
            } => {
                assert!(reason.starts_with("session_refusals.json unreadable: expected value"));
                assert!(reason.ends_with("check: its permissions and contents, beside cal.json"));
            }
            v => panic!("{v:?}"),
        }
    }

    /// UX (f): a check from this process that did not measure decides
    /// nothing about refusals the unreadable file may hold.
    #[test]
    fn unreadable_file_a_not_measured_record_leaves_the_layer_unreadable() {
        let b = baseline();
        let now = epoch_a();
        let not_measured = judge_voltage(
            Some(&b),
            &tone_probe(BASE_GAIN, PROBE_SNR_MIN_DB - 10.0),
            DRIVE,
            &ctx("out1_in1"),
        );
        assert_eq!(not_measured.cause(), Some(UnverifiedCause::NotMeasured));
        let recs = [record("out1_in1", "2026-09-16T14:02:11.000Z", not_measured)];
        let set = RecordSet {
            process: &recs,
            file: Err("Permission denied (os error 13)"),
        };
        let eff = effective(
            voltage_target("out1_in1", &b),
            set,
            scope(Some("out1_in1"), &now),
        );
        assert_eq!(
            eff.verdict.cause(),
            Some(UnverifiedCause::RefusalsUnreadable)
        );
        assert_eq!(
            recs[0].voltage.as_ref().unwrap().cause(),
            Some(UnverifiedCause::NotMeasured),
            "the attempted verdict itself is kept"
        );
        // Readable: the attempted verdict is what the layer shows.
        let eff = effective(
            voltage_target("out1_in1", &b),
            RecordSet {
                process: &recs,
                file: Ok(&SessionRefusals::default()),
            },
            scope(Some("out1_in1"), &now),
        );
        assert_eq!(eff.verdict.cause(), Some(UnverifiedCause::NotMeasured));
    }

    #[test]
    fn unreadable_file_versus_missing_file_reads_differently() {
        let b = baseline();
        let now = epoch_a();
        let unreadable = effective(
            voltage_target("out1_in1", &b),
            RecordSet {
                process: &[],
                file: Err("bad"),
            },
            scope(Some("out1_in1"), &now),
        );
        let missing = effective(
            voltage_target("out1_in1", &b),
            RecordSet {
                process: &[],
                file: Ok(&SessionRefusals::default()),
            },
            scope(Some("out1_in1"), &now),
        );
        assert_eq!(
            unreadable.verdict.cause(),
            Some(UnverifiedCause::RefusalsUnreadable)
        );
        assert_eq!(missing.verdict.cause(), Some(UnverifiedCause::NotChecked));
    }

    #[test]
    fn merge_process_record_wins_and_other_identities_survive() {
        let mut file = SessionRefusals::default();
        file.voltage.insert(
            "out1_in1".to_string(),
            record("out1_in1", "2026-09-16T14:02:11.000Z", refused_v()),
        );
        file.voltage.insert(
            "out3_in3".to_string(),
            record("out3_in3", "2026-09-16T10:00:00.000Z", refused_v()),
        );
        let mut held = record("out1_in1", "2026-09-16T14:02:11.000Z", refused_v());
        held.id = "process".to_string();
        held.persisted = Some(false);
        let pass = record("out5_in5", "2026-09-16T15:00:00.000Z", verified_v());
        let merged = merge_refusals(&file, &[held, pass]);
        assert_eq!(merged.voltage["out1_in1"].id, "process");
        assert_eq!(merged.voltage["out1_in1"].persisted, Some(true));
        assert!(merged.voltage.contains_key("out3_in3"));
        assert!(!merged.voltage.contains_key("out5_in5"));

        let clear = record("out3_in3", "2026-09-16T15:00:00.000Z", verified_v());
        let merged = merge_refusals(&merged, &[clear]);
        assert!(!merged.voltage.contains_key("out3_in3"));

        // An older process pass does not clear a newer file refusal.
        let stale_pass = record("out1_in1", "2026-09-16T09:00:00.000Z", verified_v());
        let again = merge_refusals(&merged, &[stale_pass]);
        assert!(again.voltage.contains_key("out1_in1"));
    }

    #[test]
    fn unverified_heads_are_chosen_by_cause() {
        use UnverifiedCause::*;
        assert_eq!(
            unverified_head(NotMeasured, "xrun during probe"),
            "check did not measure"
        );
        assert_eq!(
            unverified_head(RefusalsUnreadable, &refusals_unreadable_reason("bad")),
            "session_refusals.json unreadable"
        );
        assert_eq!(
            unverified_head(NoBaseline, "[out1_in1] has no loop-gain baseline; check: x"),
            "[out1_in1] has no loop-gain baseline"
        );
        assert_eq!(
            whole_seconds("2026-09-16T14:02:11.345Z"),
            "2026-09-16T14:02:11Z"
        );
        assert_eq!(whole_seconds("garbage"), "garbage");
    }

    #[test]
    fn an_unknown_cause_reads_as_unverified() {
        let v: LayerVerdict = serde_json::from_str(
            r#"{"state":"unverified","cause":"from_the_future","reason":"new daemon"}"#,
        )
        .unwrap();
        assert_eq!(v.cause(), Some(UnverifiedCause::Unknown));
        assert!(!v.is_verified());
        let v = with_precedence(v, None);
        assert!(!v.is_verified());
    }

    #[test]
    fn verdict_wire_shapes() {
        let v = serde_json::to_value(refused_v()).unwrap();
        assert_eq!(v["state"], "refused");
        assert_eq!(v["unit"], "dB");
        assert_eq!(v["source"], "probe");
        assert!(v.get("via").is_none());
        let back: LayerVerdict = serde_json::from_value(v).unwrap();
        assert_eq!(back, refused_v());
        let v = serde_json::to_value(LayerVerdict::unverified(
            UnverifiedCause::RefusalsUnreadable,
            "r",
        ))
        .unwrap();
        assert_eq!(
            v,
            serde_json::json!({"state":"unverified","cause":"refusals_unreadable","reason":"r"})
        );
        let mut r = record("out1_in1", "2026-09-16T14:02:11.000Z", refused_v());
        r.set_persisted(Err(PersistError {
            kind: PersistErrorKind::WriteFailed,
            detail: "No space left on device (os error 28)".to_string(),
        }));
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["persisted"], false);
        assert_eq!(v["persist_error"]["kind"], "write_failed");
        let back: SessionCheckRecord = serde_json::from_value(v).unwrap();
        assert_eq!(back, r);
        let mut ok = record("out1_in1", "2026-09-16T14:02:11.000Z", verified_v());
        ok.set_persisted(Ok(()));
        assert!(serde_json::to_value(&ok)
            .unwrap()
            .get("persisted")
            .is_none());
    }

    // ─── constants ──────────────────────────────────────────────────────

    #[test]
    fn tolerance_constant_is_within_its_bounds() {
        const {
            assert!(LOOP_GAIN_TOLERANCE_DB < 3.0);
            assert!(LOOP_GAIN_TOLERANCE_DB >= LOOP_GAIN_TOLERANCE_FLOOR_DB);
            assert!(LOOP_GAIN_TOLERANCE_FLOOR_DB >= 0.02);
        }
        assert_eq!(tolerance_from_repeatability(0.0), 0.02);
        assert!((tolerance_from_repeatability(0.1) - 0.693).abs() < 1e-12);
    }

    #[test]
    fn tolerance_k_is_the_derived_quantile() {
        // t(1 − 5e-5, 19) = 4.897 (tables); √2 for a difference of two probes.
        assert!((4.897 * 2f64.sqrt() - LOOP_GAIN_TOLERANCE_K).abs() < 0.005);
    }

    #[test]
    fn snr_min_tracks_the_tolerance() {
        assert!((snr_min_for_tolerance(LOOP_GAIN_TOLERANCE_DB) - PROBE_SNR_MIN_DB).abs() < 1e-9);
        assert!((snr_min_for_tolerance(0.02) - 64.794).abs() < 1e-3);
        // At S_min, in-lobe noise moves the level by at most T/4.
        let bias = 20.0 * (1.0 + 10f64.powf(-PROBE_SNR_MIN_DB / 20.0)).log10();
        assert!((bias - LOOP_GAIN_TOLERANCE_DB / 4.0).abs() < 1e-9);
    }

    #[test]
    fn in_lobe_snr_of_a_white_noise_fixture_matches_its_analytic_value() {
        let sr = 48_000u32;
        let n = 14_400usize;
        let amp = 0.01f64;
        let mut state: u64 = 0x2545_f491_4f6c_dd1d;
        let mut uniform = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 11) as f64 / (1u64 << 53) as f64 - 0.5
        };
        let noise_amp = 1e-4;
        let block: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f64 / sr as f64;
                (amp * (2.0 * std::f64::consts::PI * PROBE_FREQ_HZ * t).sin()
                    + noise_amp * uniform()) as f32
            })
            .collect();
        let r = crate::measurement::thd::analyze(&block, sr, PROBE_FREQ_HZ, 10).unwrap();
        let sigma2 = noise_amp * noise_amp / 12.0;
        // Tone power over the noise power inside a 1.5-bin lobe of n/2 bins.
        let analytic = 10.0 * ((amp * amp / 2.0) / (sigma2 * 1.5 / (n as f64 / 2.0))).log10();
        let got = in_lobe_snr_db(r.fundamental_dbfs, r.noise_floor_dbfs, n);
        assert!(
            (got - analytic).abs() < 0.3,
            "got {got}, analytic {analytic}"
        );
    }

    #[test]
    fn total_peak_level_never_reads_below_the_tone() {
        let n = 4800;
        let block: Vec<f32> = (0..n)
            .map(|i| (0.01 * (i as f64 * 0.13).sin()) as f32)
            .collect();
        let total = total_peak_dbfs(&block);
        assert!((total - (-40.0)).abs() < 0.1, "{total}");
        assert!((total_peak_dbfs(&[0.0; 10]) - (-240.0 + PEAK_TO_RMS_DB)).abs() < 1e-9);
    }
}
