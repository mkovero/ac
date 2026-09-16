//! Derived read-out quantities for an impulse-response payload, and the
//! verdict on whether its peak is a trustworthy deconvolution result
//! (#376). Computed once here so `ac-cli`'s text read-out and
//! `ac-scene`'s sweep-IR panel cannot disagree about a capture.

use super::{GateParams, InterfaceLatency, MeasurementData, MeasurementReport, ReferenceLatency};
use crate::measurement::sweep::{
    ir_peak, BoundInputs, CausalBound, EdgeGuard, MissingBoundInput, OnsetEstimate, OnsetPick,
    WindowLimit,
};
use crate::shared::calibration::{
    compare_tau_readings, EnumerationCheck, TauComparison, TauDisagreement,
};

/// Minimum pre-impulse SNR, in dB, below which a deconvolution is
/// reported as failed rather than as a result (#376). Below this floor
/// the linear-IR peak is not reliably the system response — it can land
/// wherever the pre-impulse noise floor happens to be largest, producing
/// a plausible-looking arrival/distance from noise.
///
/// Value: 18.0 dB — the worst observed *bad* capture in the rig table in
/// the #376 issue body (−42 dBFS drive, pre-impulse SNR up to 16.5 dB with
/// a peak index far from the true arrival) plus a 1.5 dB margin. The raw
/// log and results doc the issue body itself cites for that table
/// (`audit/rig-353-2026-08-23/ladder-3m.log`,
/// `work/rig/rig-2026-08-23-onset-353-results.md`) never landed in this
/// repo — the table reproduced in the issue is the only source checked
/// here; do not add a citation to either path without confirming the file
/// exists first. The same table's worst observed *good* capture (−36 dBFS
/// drive) reaches down to 14.5 dB, so
/// no single threshold separates this dataset cleanly — 18.0 dB is set
/// at or above the worst bad case rather than at the overlap's midpoint,
/// so a false refusal (cheap: re-run) is preferred over a false accept
/// (expensive: a silently wrong logged distance). This also means some
/// borderline-good low-drive captures near the boundary will be
/// refused — low drive is the operator-encouraged *safe* choice under
/// the rig's emission consent rules, so that blind spot is real and
/// documented here rather than picked by eye.
pub const PRE_IMPULSE_SNR_MIN_DB: f64 = 18.0;

impl MeasurementReport {
    /// Derived read-out quantities for the report's first
    /// `ImpulseResponse` payload: arrival timing, peak magnitude,
    /// pre-impulse SNR, and the time gate's low-frequency limit. `None`
    /// when no payload carries an impulse response, or its linear IR is
    /// empty (see issue #283).
    ///
    /// Arrival (`delay_samples`/`arrival_s`) is always the IR's magnitude
    /// peak ([`IrStats::arrival_source`] is [`ArrivalSource::Peak`]). The
    /// onset estimate ([`crate::measurement::sweep::estimate_onset`]) is
    /// carried beside it as a diagnostic, with its standing in
    /// [`IrStats::onset_standing`]; it does not affect any number (#346
    /// architect revision 4, operator decision 2026-09-16).
    /// When this report carries both a measured same-capture reference
    /// latency and a recorded `position.distance_m`, the onset estimate is
    /// bound to reject any candidate earlier than pure flight time allows.
    pub fn ir_stats(&self) -> Option<IrStats> {
        let (payload, sample_rate_hz, linear_ir) =
            self.data.iter().find_map(|p| match &p.data {
                MeasurementData::ImpulseResponse {
                    sample_rate_hz,
                    linear_ir,
                    ..
                } => Some((p, sample_rate_hz, linear_ir)),
                _ => None,
            })?;
        if linear_ir.is_empty() || *sample_rate_hz == 0 {
            return None;
        }
        let window_len = linear_ir.len();
        let (peak_index, peak_magnitude) = ir_peak(linear_ir);

        // `extract_irs` (`measurement::sweep`) centres the gate at the
        // sweep endpoint — the position an identity (zero-delay) system
        // would peak at.
        let centre = window_len / 2;

        let pre_region = pre_impulse_region(linear_ir, peak_index);
        // Same formula `ac-daemon`'s τ gate calls (#368) — one definition
        // of "pre-impulse SNR", not two that can drift.
        let pre_impulse_snr_db =
            crate::measurement::sweep::pre_impulse_snr_db(linear_ir, peak_index);

        // The onset picker's validity gate runs off a *median* floor over
        // the same region, not off `pre_impulse_snr_db`'s RMS one — see
        // [`onset_floor`] for why the two coexist rather than one
        // replacing the other.
        let onset_floor = onset_floor(pre_region);

        // The earliest sample the capture's own geometry admits as an onset
        // (#460): pure flight time from the same-capture reference latency
        // and the operator-entered distance, as a sample index. Never from
        // the stored `interface_latency`: it re-picks by a multiple of the
        // FireWire SYT interval on every device enumeration (#461), which
        // exceeds the bound's margin, and it is resolved from calibration
        // rather than measured with this IR.
        let causal_bound = causal_bound(self, centre, *sample_rate_hz);

        // #359: corroborate this capture's same-capture reference τ against
        // what `calibrate` has on file for that pair, before any flight
        // time is derived from it. A single `plot_ir` capture is one
        // client lifetime; the check gates the only arrival subtraction
        // this report performs.
        let arrival_check = arrival_check(self, *sample_rate_hz);

        let onset = crate::measurement::sweep::estimate_onset(
            linear_ir,
            peak_index,
            *sample_rate_hz,
            onset_floor,
            &causal_bound,
        );
        let verdict = ir_verdict(peak_magnitude, pre_region, pre_impulse_snr_db);
        let onset_standing = onset_standing(&causal_bound, &onset, &verdict);
        let onset_index = onset.index;
        let onset_rule = onset.rule;

        // The arrival is the magnitude peak, unconditionally (#346 architect
        // revision 4). The bounded onset's pre-registered rig check refused
        // to conclude (pupu, 2026-09-16, 0/12 captures passed the edge
        // guard), and the operator accepted the peak with its tag.
        let arrival_source = ArrivalSource::Peak;
        let delay_samples = peak_index as i64 - centre as i64;
        let arrival_s = delay_samples as f64 / *sample_rate_hz as f64;
        let (gate_window_s, gate_f_low_hz, gate_window_kind) =
            resolve_gate(payload.gate.as_ref(), window_len, *sample_rate_hz);

        // #359: the τ subtraction this report can offer — gated by
        // `arrival_check`. `interface_latency` must be a measured τ for
        // *this* capture pair, and the reference-pair check must not have
        // found a disagreement: on `PeriodShift`/`Mismatch` the stored τ is
        // shown to be from a different lifetime state than this capture, so
        // subtracting it would reproduce the exact silently-wrong number
        // this issue exists to stop. Under `Unchecked` the flight time is
        // still produced — the check simply could not run — never withheld
        // for a reason it does not have.
        let flight_time_s = match (&self.interface_latency, &arrival_check) {
            (Some(InterfaceLatency::Measured(m)), ArrivalCheck::Agree)
            | (Some(InterfaceLatency::Measured(m)), ArrivalCheck::Unchecked { .. }) => {
                Some(arrival_s - m.tau_s)
            }
            _ => None,
        };
        // #461: the capture pair's stored-τ enumeration check, carried beside
        // the flight time it qualifies. Read as frozen; never recomputed.
        let interface_latency_enumeration = match &self.interface_latency {
            Some(InterfaceLatency::Measured(m)) => m.enumeration.clone(),
            _ => None,
        };

        Some(IrStats {
            sample_rate_hz: *sample_rate_hz,
            window_len,
            peak_index,
            peak_magnitude,
            onset_index,
            onset_rule,
            causal_bound,
            arrival_source,
            onset_standing,
            delay_samples,
            arrival_s,
            arrival_check,
            flight_time_s,
            interface_latency_enumeration,
            pre_impulse_snr_db,
            gate_window_s,
            gate_f_low_hz,
            gate_window_kind,
            verdict,
        })
    }
}

/// The onset diagnostic's standing (#346 architect revision 4): a verdict
/// on the onset pick, not a gate on the arrival, which is always the peak.
/// The conditions are checked in this order; the first that fails is the
/// named standing, and [`OnsetStanding::Unscored`] means all held:
///
/// 1. the causal bound is enforced;
/// 2. the bound set the search window's start (it is not earlier than the
///    search span);
/// 3. the picker did not decline;
/// 4. the pick is not on the window start;
/// 5. the edge-following guard passed;
/// 6. the deconvolution verdict is not `Failed`.
///
/// Reads only values `ir_stats` already computed, never `onset.rule`.
pub(super) fn onset_standing(
    causal_bound: &CausalBound,
    onset: &OnsetEstimate,
    verdict: &IrVerdict,
) -> OnsetStanding {
    if !matches!(causal_bound, CausalBound::Enforced { .. }) {
        return OnsetStanding::NoCausalBound;
    }
    let OnsetPick::Picked {
        limit,
        pinned,
        edge_guard,
        ..
    } = &onset.pick
    else {
        // A bound at or after the peak is a decline, not a non-binding
        // bound: the bound was the limit, and it left nothing to search.
        return OnsetStanding::PickerDeclined;
    };
    if *limit != WindowLimit::CausalBound {
        return OnsetStanding::BoundNotBinding;
    }
    if *pinned {
        return OnsetStanding::PickOnWindowStart;
    }
    // `estimate_onset` runs the guard on every clear pick in a window the
    // bound started, so a guard that did not run does not arise here; it
    // is refused as unchecked rather than read as a pass.
    match edge_guard {
        Some(EdgeGuard::Passed) => {}
        Some(EdgeGuard::Failed { repick }) => {
            return OnsetStanding::EdgeFollowing { repick: *repick };
        }
        None => return OnsetStanding::EdgeFollowing { repick: None },
    }
    if matches!(verdict, IrVerdict::Failed { .. }) {
        return OnsetStanding::DeconvolutionFailed;
    }
    OnsetStanding::Unscored
}

/// Corroborate `report`'s same-capture reference τ ([`ReferenceLatency`])
/// against the τ `calibrate` has on file for that same reference pair
/// ([`MeasurementReport::reference_stored_latency`], #359).
///
/// A single `plot_ir` capture is one client lifetime, and a graph-buffering
/// shift of exactly one period is invisible within one lifetime (#347). The
/// reference leg is read in the same lifetime as the IR itself (#460), so
/// comparing it against an independently-lifecycled stored reading is the
/// same "measure a difference within a single client" remedy #347 uses for
/// `calibrate` — reused via [`compare_tau_readings`] rather than
/// reimplemented, so the wording is recognisably the same fault (AC2/AC3).
///
/// Any input other than two measured readings is [`ArrivalCheck::Unchecked`],
/// naming what is missing. This never reads the *capture's own*
/// `interface_latency`: that is a different pair's τ (#461), and unrelated
/// to whether the reference pair's lifetime matches this one.
pub(super) fn arrival_check(report: &MeasurementReport, sample_rate_hz: u32) -> ArrivalCheck {
    let same_capture = match &report.reference_latency {
        Some(ReferenceLatency::Measured(r)) => Ok(r.tau_s),
        Some(ReferenceLatency::Unavailable { reason }) => Err(reason.clone()),
        None => Err("no same-capture reference in this report".to_string()),
    };
    let stored = match &report.reference_stored_latency {
        Some(InterfaceLatency::Measured(m)) => Ok((m.tau_s, m.period_size)),
        Some(InterfaceLatency::Unavailable { reason }) => Err(reason.clone()),
        None => Err(
            "no stored latency for the reference pair (report predates schema v9, or no \
             reference configured)"
                .to_string(),
        ),
    };
    match (same_capture, stored) {
        (Ok(same_capture_tau_s), Ok((stored_tau_s, period_size))) => {
            match compare_tau_readings(
                stored_tau_s,
                same_capture_tau_s,
                sample_rate_hz,
                period_size,
            ) {
                TauComparison::Agree => ArrivalCheck::Agree,
                TauComparison::Disagree(d) if d.periods.is_some() => ArrivalCheck::PeriodShift(d),
                TauComparison::Disagree(d) => ArrivalCheck::Mismatch(d),
            }
        }
        (Err(reason), _) | (_, Err(reason)) => ArrivalCheck::Unchecked { reason },
    }
}

/// Build the onset search's causal bound for `report` (#460).
///
/// Enforced only when the report carries a measured same-capture
/// [`ReferenceLatency`] *and* a finite, positive `position.distance_m`.
/// Anything else is [`CausalBound::Unavailable`] naming what is missing. A
/// report without the field (written before schema v7, or by a producer
/// with no reference) counts as the reference missing. The stored
/// `interface_latency` is deliberately not read: see #461.
pub(super) fn causal_bound(
    report: &MeasurementReport,
    centre: usize,
    sample_rate_hz: u32,
) -> CausalBound {
    let distance_m = report
        .position
        .as_ref()
        .and_then(|p| p.distance_m)
        .filter(|d| d.is_finite() && *d > 0.0);
    let reference = match &report.reference_latency {
        Some(ReferenceLatency::Measured(r)) => Ok(r.tau_s),
        Some(ReferenceLatency::Unavailable { reason }) => Err(reason.clone()),
        None => Err("no same-capture reference in this report".to_string()),
    };
    match (reference, distance_m) {
        (Ok(reference_tau_s), Some(distance_m)) => {
            let temperature_c = report.position.as_ref().and_then(|p| p.temperature_c);
            let c = crate::shared::conversions::speed_of_sound_from_config(temperature_c);
            let offset = (reference_tau_s + distance_m / c) * sample_rate_hz as f64;
            CausalBound::Enforced {
                index: (centre as f64 + offset).round().max(0.0) as usize,
                inputs: BoundInputs {
                    reference_tau_s,
                    distance_m,
                    speed_of_sound_m_s: c,
                    temperature_c,
                },
            }
        }
        (Ok(_), None) => CausalBound::Unavailable(MissingBoundInput::Distance),
        (Err(reason), Some(_)) => {
            CausalBound::Unavailable(MissingBoundInput::ReferenceLatency { reason })
        }
        (Err(reference_reason), None) => {
            CausalBound::Unavailable(MissingBoundInput::Both { reference_reason })
        }
    }
}

/// Pre-impulse noise floor region: everything strictly before the peak,
/// minus a small guard band so the peak's own skirt doesn't bias the
/// floor estimate upward. Empty when the guard band consumes the whole
/// pre-peak window — which [`ir_verdict`] treats as a failure, not as a
/// clean floor.
///
/// The guard arithmetic itself lives in `measurement::sweep` (#368), so
/// `ac-daemon`'s τ gate and this read-out cannot drift apart on what
/// "pre-impulse" means; this only turns the length into the slice
/// [`ir_verdict`] needs for its empty check.
pub(super) fn pre_impulse_region(linear_ir: &[f64], peak_index: usize) -> &[f64] {
    &linear_ir[..crate::measurement::sweep::pre_impulse_region_len(linear_ir.len(), peak_index)]
}

/// Contamination-robust pre-impulse floor: the median absolute sample of
/// `pre_region`, scaled by the standard MAD-to-σ constant so it targets
/// the same quantity [`crate::measurement::sweep::pre_impulse_snr_db`]'s
/// RMS floor does on clean
/// noise. `0.0` for an empty region.
///
/// #353 (option A′), retained under #378 with a narrower job. The RMS
/// floor has a breakdown point of zero — a single sample of the peak's own
/// sustained energy bleeding into `pre_region` moves it, by an amount that
/// scales with that sample's amplitude. A rank statistic has a 50%
/// breakdown point: up to half of `pre_region` can be lobe by count and
/// the median still reads the noise floor.
///
/// #378 demoted it from `estimate_onset`'s threshold input to its validity
/// gate — the picker takes no threshold at all, and this floor now decides
/// only whether the search window holds anything above the floor, never
/// where inside it the onset is. Its 50% breakdown point is what makes
/// that gate trustworthy on a contaminated pre-impulse region.
/// [`crate::measurement::sweep::pre_impulse_snr_db`] and its RMS floor
/// stay exactly as they were —
/// this is a second floor for a second question, not a replacement (#346
/// architect review, #378 AC5).
pub(super) fn onset_floor(pre_region: &[f64]) -> f64 {
    /// Φ⁻¹(0.75) — the standard MAD-to-σ constant, not a tuned value.
    const MAD_TO_SIGMA: f64 = 0.6744897501960817;
    if pre_region.is_empty() {
        return 0.0;
    }
    let mut abs_vals: Vec<f64> = pre_region.iter().map(|v| v.abs()).collect();
    abs_vals.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = abs_vals.len();
    let median = if n % 2 == 1 {
        abs_vals[n / 2]
    } else {
        (abs_vals[n / 2 - 1] + abs_vals[n / 2]) / 2.0
    };
    median / MAD_TO_SIGMA
}

/// Gate duration, low-frequency limit and window shape for an IR payload.
///
/// Prefers the gate the producer actually applied. #280 stores `f_low_hz`
/// on the payload precisely so a reader does not recompute it; falling
/// back to `window_len / sample_rate_hz` only covers legacy (v1-v3)
/// reports, where no gate was recorded and the rectangular `extract_irs`
/// window is the only gate that could have produced this payload.
pub(super) fn resolve_gate(
    gate: Option<&GateParams>,
    window_len: usize,
    sample_rate_hz: u32,
) -> (f64, f64, String) {
    match gate {
        Some(g) => (g.gate_length_s, g.f_low_hz, g.window_kind.clone()),
        None => {
            let len_s = window_len as f64 / sample_rate_hz as f64;
            (len_s, 1.0 / len_s, "rectangular (not recorded)".to_string())
        }
    }
}

/// Whether a capture's peak is trustworthy enough to present as a result
/// (#376). Split out of [`MeasurementReport::ir_stats`] so this rule —
/// the one `ac-cli` and `ac-scene` both read through
/// [`IrStats::verdict`] — can be exercised directly, without assembling
/// a whole report around it.
///
/// `snr_db` goes to +inf two different ways, and only one of them is a
/// failure. A zero floor against a nonzero peak (`rms == 0.0`,
/// `pre_region` nonempty) is the *best* possible capture — infinite SNR,
/// not an unmeasurable one — and clears any finite threshold below, so it
/// falls through to the ordinary threshold comparison rather than being
/// special-cased out. What fails closed is the case with nothing to
/// measure at all: an empty `pre_region` (the guard band consumed the
/// whole pre-peak window) or a zero peak (nothing captured, so there is
/// no signal to compare a floor against either) — absence of proof of a
/// good floor is not the same as proof of one.
pub(super) fn ir_verdict(peak_magnitude: f64, pre_region: &[f64], snr_db: f64) -> IrVerdict {
    if peak_magnitude == 0.0 {
        IrVerdict::Failed {
            reason: "no signal captured (linear IR is all zero)".to_string(),
        }
    } else if pre_region.is_empty() {
        IrVerdict::Failed {
            reason: "no measurable pre-impulse floor (peak too close to \
                     the start of the gated window)"
                .to_string(),
        }
    } else if snr_db < PRE_IMPULSE_SNR_MIN_DB {
        IrVerdict::Failed {
            reason: "pre-impulse SNR below threshold".to_string(),
        }
    } else {
        IrVerdict::Ok
    }
}

/// See [`MeasurementReport::ir_stats`].
#[derive(Debug, Clone, PartialEq)]
pub struct IrStats {
    pub sample_rate_hz: u32,
    /// Length of the gated linear IR, in samples.
    pub window_len: usize,
    /// Index of the peak-magnitude sample within the gated IR, and what
    /// `delay_samples` / `arrival_s` are derived from. On a
    /// multi-way loudspeaker this sits a group-delay offset past the
    /// wavefront, so a peak-derived absolute arrival carries that offset
    /// (#346).
    pub peak_index: usize,
    /// `|linear_ir[peak_index]|`.
    pub peak_magnitude: f64,
    /// Index of the estimated onset within the gated IR — see
    /// [`crate::measurement::sweep::estimate_onset`]. A diagnostic, never
    /// the arrival: #378's AC6 rig run (pupu, 2026-09-15) found the
    /// unbounded onset's 1.000 m → 2.000 m increment missing
    /// `transfer_stream`'s by 143.75 samples, against the peak's 8.62, and
    /// the bounded onset's pre-registered rig check (pupu, 2026-09-16)
    /// refused to conclude. [`Self::onset_standing`] states its standing.
    pub onset_index: usize,
    /// The rule that produced `onset_index`, from
    /// [`crate::measurement::sweep::OnsetEstimate::rule`] — states
    /// whether a causal bound (known geometry) was enforced, so a
    /// persisted onset can be told apart from a bare peak read a year
    /// later (#346 acceptance criterion 4).
    pub onset_rule: String,
    /// The causal bound the onset search ran under, or which input it
    /// lacked (#460). Carries the bound's inputs so a read-out prints them
    /// rather than re-deriving them; `min_admissible_index()` is the index.
    pub causal_bound: CausalBound,
    /// Which rule produced `delay_samples` / `arrival_s` (#346 acceptance
    /// criterion 4). Always [`ArrivalSource::Peak`].
    pub arrival_source: ArrivalSource,
    /// The onset diagnostic's standing: the first of `onset_standing`'s
    /// conditions that failed, or [`OnsetStanding::Unscored`] when all
    /// held. It does not affect any number on this struct.
    pub onset_standing: OnsetStanding,
    /// Signed offset of the arrival — `peak_index` — from the gate centre
    /// (`window_len / 2`), in samples. Positive means the response arrived after the
    /// zero-delay reference position.
    pub delay_samples: i64,
    /// `delay_samples / sample_rate_hz` — arrival time relative to the
    /// gate's zero-delay reference. This is **not** acoustic path delay:
    /// it still contains any uncorrected interface latency, which is why
    /// it must not be converted to a distance without a calibrated τ.
    pub arrival_s: f64,
    /// Corroboration of this capture's same-capture reference τ against the
    /// stored τ `calibrate` has on file for that pair (#359). Gates
    /// [`Self::flight_time_s`]: a disagreement means the stored value is
    /// from a lifetime whose graph state does not match this capture's, and
    /// subtracting it would silently reproduce the fault this check exists
    /// to catch.
    pub arrival_check: ArrivalCheck,
    /// `arrival_s − interface_latency.tau_s` — the one τ subtraction this
    /// report can offer: a peak arrival minus a peak-picked τ.
    /// `Some` only when `interface_latency` is a measured
    /// τ for *this* capture pair **and** `arrival_check` is not a
    /// disagreement; `None` on `PeriodShift`/`Mismatch` even though
    /// `interface_latency` is measured, and `None` whenever no τ was
    /// resolved for this capture pair at all. Still `Some` under
    /// `ArrivalCheck::Unchecked` — the check did not run, which is not a
    /// reason to withhold a value it never disputed.
    pub flight_time_s: Option<f64>,
    /// How the capture pair's stored τ — the one [`Self::flight_time_s`]
    /// subtracts — related to this capture's device-enumeration epoch
    /// (#461), copied from `interface_latency`. `None` when that is not a
    /// measured τ, or when the report predates schema v10. A flag, not a
    /// gate: the flight time is produced either way, and
    /// [`Self::interface_latency_unverified`] says whether it must be
    /// qualified.
    pub interface_latency_enumeration: Option<EnumerationCheck>,
    /// `20·log10(peak_magnitude / rms(pre-impulse region))`. `+inf` when
    /// no pre-impulse energy was measurable at all (silent floor).
    pub pre_impulse_snr_db: f64,
    /// Gate window duration, in seconds — the recorded
    /// [`GateParams::gate_length_s`] when the payload carries one.
    pub gate_window_s: f64,
    /// The lowest frequency for which one full period fits inside the
    /// gate window. Read from [`GateParams::f_low_hz`] when recorded;
    /// content below it is not reliably resolved by a gate this short.
    pub gate_f_low_hz: f64,
    /// Window shape the gate applied, from [`GateParams::window_kind`].
    /// `"rectangular (not recorded)"` for legacy reports that stored no
    /// gate — an inference from `extract_irs`, flagged as such so a
    /// reader does not mistake it for a recorded value.
    pub gate_window_kind: String,
    /// Whether this capture's peak is trustworthy enough to present as a
    /// result, per [`PRE_IMPULSE_SNR_MIN_DB`] (#376). Computed once here
    /// so `ac-cli`'s text read-out and `ac-scene`'s sweep-IR panel read
    /// the same verdict rather than each re-deriving their own rule from
    /// [`Self::pre_impulse_snr_db`].
    pub verdict: IrVerdict,
}

impl IrStats {
    /// Whether [`Self::flight_time_s`] rests on a stored τ that was not
    /// shown to belong to this capture's device enumeration (#461): any
    /// check other than `Same`, including a missing one. `false` when no
    /// flight time was produced — there is nothing to qualify.
    pub fn interface_latency_unverified(&self) -> bool {
        self.flight_time_s.is_some()
            && !matches!(
                self.interface_latency_enumeration,
                Some(EnumerationCheck::Same)
            )
    }
}

/// Verdict on whether an [`IrStats`] peak is a trustworthy deconvolution
/// result or noise-floor pickup masquerading as one (#376). `Failed`
/// never carries a computed arrival, distance, or peak-as-result — only
/// the reason, naming what to check without asserting a cause (drive
/// level, mic gain, distance, room noise are all plausible; the
/// instrument cannot tell which).
#[derive(Debug, Clone, PartialEq)]
pub enum IrVerdict {
    Ok,
    Failed { reason: String },
}

/// Which rule produced an [`IrStats`] arrival (#346 acceptance criterion 4).
/// An enum so the tag is typed. A second variant needs a design decision
/// backed by a scored rig check (#346 architect revision 3, operator
/// decision 2026-09-16).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrivalSource {
    /// The magnitude peak (argmax |h|).
    Peak,
}

/// The standing of an [`IrStats`] onset diagnostic — one variant per
/// condition, in the order they are checked, plus [`Self::Unscored`] when
/// every condition held. None of them affects the arrival.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnsetStanding {
    /// No causal bound was enforced; [`IrStats::causal_bound`] names the
    /// missing input.
    NoCausalBound,
    /// A bound was enforced but the search span, not the bound, set the
    /// window start.
    BoundNotBinding,
    /// The onset picker declined; [`IrStats::onset_rule`] names the case.
    PickerDeclined,
    /// The pick sits on the window start, so the true onset may lie
    /// earlier.
    PickOnWindowStart,
    /// The edge-following guard failed. `repick: Some(r)`: the re-pick
    /// with the window start moved
    /// [`crate::measurement::sweep::EDGE_GUARD_EXTENSION_M`] earlier went
    /// to sample `r`, so the pick follows the window edge. `repick: None`:
    /// no re-pick ran, so the pick could not be checked. Mirrors
    /// [`crate::measurement::sweep::EdgeGuard::Failed`].
    EdgeFollowing { repick: Option<usize> },
    /// The deconvolution verdict is `Failed`.
    DeconvolutionFailed,
    /// Every condition held: bounded, binding, clear of the window start,
    /// guard passed, verdict not `Failed`. No rig score authorises this
    /// pick as an arrival: the pre-registered run (pupu, 2026-09-16)
    /// refused to conclude, and the operator declined a new estimator.
    Unscored,
}

/// Outcome of corroborating this capture's same-capture reference τ against
/// the stored τ `calibrate` has on file for that pair (#359). Never a bare
/// "corroborated" — #363's vocabulary rule applies here too: state the
/// evidence, not the verdict. `PeriodShift` and `Mismatch` both carry the
/// [`TauDisagreement`] that produced them, split on whether the delta is an
/// exact multiple of the period — the same distinction #347 draws for
/// `calibrate`'s own τ readings, via the same comparator
/// ([`compare_tau_readings`]).
#[derive(Debug, Clone, PartialEq)]
pub enum ArrivalCheck {
    /// The two readings match to the whole sample.
    Agree,
    /// Disagree by an exact multiple of the period — a graph-buffering
    /// shift, not hardware drift (#347's own finding, reused verbatim).
    PeriodShift(TauDisagreement),
    /// Disagree by an amount that is not a period multiple — a different
    /// fault (e.g. the #461 SYT re-pick), not this issue's failure mode.
    Mismatch(TauDisagreement),
    /// Not checked: `reason` names what is missing — no same-capture
    /// reference, a reference reading that failed its own gates, or no
    /// stored τ for the reference pair.
    Unchecked { reason: String },
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::*;
    use super::super::*;
    use super::*;

    #[test]
    fn ir_stats_reports_delay_samples_relative_to_gate_centre() {
        // Peak 32 samples after the window centre — the fake backend's
        // fixed loopback delay (see `ac-daemon/src/audio/fake.rs`).
        let window_len = 1024;
        let centre = window_len / 2;
        let r = ir_report_with_peak(window_len, centre + 32, 1.0, 0.0, 48_000);
        let stats = r.ir_stats().expect("impulse response data present");
        assert_eq!(stats.delay_samples, 32);
        assert!((stats.arrival_s - 32.0 / 48_000.0).abs() < 1e-12);
        assert_eq!(stats.peak_index, centre + 32);
        assert_eq!(stats.peak_magnitude, 1.0);
    }

    #[test]
    fn ir_stats_delay_is_negative_when_peak_precedes_centre() {
        let window_len = 1024;
        let centre = window_len / 2;
        let r = ir_report_with_peak(window_len, centre - 10, 1.0, 0.0, 48_000);
        let stats = r.ir_stats().unwrap();
        assert_eq!(stats.delay_samples, -10);
        assert!(stats.arrival_s < 0.0);
    }

    #[test]
    fn ir_stats_pre_impulse_snr_reflects_noise_floor() {
        let window_len = 1024;
        let centre = window_len / 2;
        // Peak of 1.0 against a 0.01 floor -> 20*log10(100) = 40 dB.
        let r = ir_report_with_peak(window_len, centre, 1.0, 0.01, 48_000);
        let stats = r.ir_stats().unwrap();
        assert!(
            (stats.pre_impulse_snr_db - 40.0).abs() < 0.5,
            "pre_impulse_snr_db = {}",
            stats.pre_impulse_snr_db
        );
    }

    #[test]
    fn ir_stats_snr_is_infinite_over_true_silence() {
        let window_len = 1024;
        let centre = window_len / 2;
        let r = ir_report_with_peak(window_len, centre, 1.0, 0.0, 48_000);
        let stats = r.ir_stats().unwrap();
        assert!(stats.pre_impulse_snr_db.is_infinite());
    }

    #[test]
    fn ir_stats_falls_back_to_window_duration_when_no_gate_recorded() {
        let r = ir_report_with_peak(4_800, 2_400, 1.0, 0.0, 48_000);
        let stats = r.ir_stats().unwrap();
        // 4800 samples @ 48 kHz = 100 ms window -> f_low = 10 Hz.
        assert!((stats.gate_window_s - 0.1).abs() < 1e-12);
        assert!((stats.gate_f_low_hz - 10.0).abs() < 1e-9);
        // A legacy report's gate is inferred, and must say so — a reader
        // must not take "rectangular" here for a recorded fact.
        assert!(
            stats.gate_window_kind.contains("not recorded"),
            "inferred gate must be flagged: {}",
            stats.gate_window_kind
        );
    }

    /// The recorded gate wins over the `window_len / sample_rate` guess.
    /// This is the case that separates the two: a gate whose recorded
    /// `f_low_hz` and length disagree with what the IR length implies
    /// (a half-length gate on a zero-padded payload) — if `ir_stats`
    /// recomputed instead of reading, it would report 10 Hz, not 20.
    #[test]
    fn ir_stats_prefers_the_recorded_gate_over_the_ir_length() {
        let mut r = ir_report_with_peak(4_800, 2_400, 1.0, 0.0, 48_000);
        r.data[0].gate = Some(GateParams {
            gate_start_s: 0.0,
            gate_length_s: 0.05,
            window_kind: "half-hann".into(),
            f_low_hz: 20.0,
        });
        let stats = r.ir_stats().unwrap();
        assert!((stats.gate_window_s - 0.05).abs() < 1e-12);
        assert!((stats.gate_f_low_hz - 20.0).abs() < 1e-9);
        assert_eq!(stats.gate_window_kind, "half-hann");
    }

    /// [`ir_verdict`] direct, without a report around it: the threshold is
    /// a `<` on `PRE_IMPULSE_SNR_MIN_DB`, so a capture sitting exactly on
    /// the floor passes and one a hair under it fails.
    #[test]
    fn ir_verdict_threshold_is_inclusive_at_the_floor() {
        let floor = [0.0, 0.0, 0.0, 0.0];
        assert_eq!(
            ir_verdict(1.0, &floor, PRE_IMPULSE_SNR_MIN_DB),
            IrVerdict::Ok
        );
        assert!(matches!(
            ir_verdict(1.0, &floor, PRE_IMPULSE_SNR_MIN_DB - 0.001),
            IrVerdict::Failed { .. }
        ));
    }

    /// The two ways `snr_db` reaches `+inf` are not the same verdict: a
    /// silent floor under a real peak is the best possible capture, while
    /// an empty pre-impulse region means nothing was measured at all.
    #[test]
    fn ir_verdict_separates_a_silent_floor_from_an_unmeasured_one() {
        assert_eq!(ir_verdict(1.0, &[0.0, 0.0], f64::INFINITY), IrVerdict::Ok);
        assert!(matches!(
            ir_verdict(1.0, &[], f64::INFINITY),
            IrVerdict::Failed { .. }
        ));
    }

    /// A zero peak fails ahead of every other branch — an all-zero IR has
    /// no signal to compare a floor against, however clean the floor looks.
    #[test]
    fn ir_verdict_fails_a_zero_peak_before_reading_the_snr() {
        assert!(matches!(
            ir_verdict(0.0, &[0.0, 0.0], f64::INFINITY),
            IrVerdict::Failed { .. }
        ));
    }

    #[test]
    fn ir_stats_verdict_ok_when_snr_clears_the_threshold() {
        let window_len = 1024;
        let centre = window_len / 2;
        // Peak 1.0 against a 0.1 floor -> 20*log10(10) = 20 dB, above the
        // 18.0 dB threshold.
        let r = ir_report_with_peak(window_len, centre, 1.0, 0.1, 48_000);
        let stats = r.ir_stats().unwrap();
        assert_eq!(stats.verdict, IrVerdict::Ok);
    }

    #[test]
    fn ir_stats_verdict_failed_when_snr_is_below_the_threshold() {
        let window_len = 1024;
        let centre = window_len / 2;
        // Peak 1.0 against a 0.2 floor -> 20*log10(5) \u{2248} 14.0 dB,
        // below the 18.0 dB threshold — the #376 failure shape: a plausible
        // number, but a noise-floor-scale peak.
        let r = ir_report_with_peak(window_len, centre, 1.0, 0.2, 48_000);
        let stats = r.ir_stats().unwrap();
        assert_eq!(
            stats.verdict,
            IrVerdict::Failed {
                reason: "pre-impulse SNR below threshold".to_string()
            }
        );
    }

    #[test]
    fn ir_stats_verdict_ok_on_a_perfectly_clean_capture() {
        // A zero floor against a nonzero peak is +inf SNR, but it is the
        // *best* possible capture, not an unmeasurable one — the floor was
        // measured, and it measured to exactly zero. This must not be
        // confused with a genuine failure (#387 QA correctness #1).
        let window_len = 1024;
        let centre = window_len / 2;
        let r = ir_report_with_peak(window_len, centre, 1.0, 0.0, 48_000);
        let stats = r.ir_stats().unwrap();
        assert!(stats.pre_impulse_snr_db.is_infinite());
        assert_eq!(stats.verdict, IrVerdict::Ok);
    }

    #[test]
    fn ir_stats_verdict_failed_when_nothing_was_captured() {
        // Peak magnitude itself is zero -> the whole linear IR is zero,
        // i.e. there is no signal to compare a floor against at all. This
        // is the genuine "no measurable floor" failure, distinct from the
        // clean-capture case above.
        let window_len = 1024;
        let r = ir_report_with_peak(window_len, window_len / 2, 0.0, 0.0, 48_000);
        let stats = r.ir_stats().unwrap();
        assert_eq!(
            stats.verdict,
            IrVerdict::Failed {
                reason: "no signal captured (linear IR is all zero)".to_string()
            }
        );
    }

    #[test]
    fn ir_stats_verdict_failed_when_guard_band_consumes_the_whole_pre_region() {
        // Peak sits inside the guard band from the start of the window, so
        // `pre_region` is empty — there is no data at all to measure a
        // floor from, regardless of what the peak itself looks like.
        let window_len = 1024;
        let r = ir_report_with_peak(window_len, 3, 1.0, 0.1, 48_000);
        let stats = r.ir_stats().unwrap();
        assert_eq!(
            stats.verdict,
            IrVerdict::Failed {
                reason: "no measurable pre-impulse floor (peak too close to \
                         the start of the gated window)"
                    .to_string()
            }
        );
    }

    #[test]
    fn ir_stats_none_for_non_impulse_response_report() {
        let r = sample_report(); // FrequencyResponse variant
        assert!(r.ir_stats().is_none());
    }

    /// A Farina capture emits several payloads; the impulse response is
    /// not necessarily first. `ir_stats` must find it rather than read
    /// `data[0]` and give up.
    #[test]
    fn ir_stats_finds_the_ir_payload_behind_another_payload() {
        let mut r = ir_report_with_peak(1_024, 1_024 / 2 + 32, 1.0, 0.0, 48_000);
        let ir_payload = r.data.remove(0);
        r.data = vec![
            MeasurementPayload {
                data: MeasurementData::FrequencyResponse { points: vec![] },
                standard: Vec::new(),
                gate: None,
            },
            ir_payload,
        ];
        assert_eq!(r.ir_stats().unwrap().delay_samples, 32);
    }

    #[test]
    fn ir_stats_none_for_empty_linear_ir() {
        let mut r = sample_impulse_response_report();
        r.data = vec![MeasurementPayload {
            data: MeasurementData::ImpulseResponse {
                sample_rate_hz: 48_000,
                f1_hz: 20.0,
                f2_hz: 20_000.0,
                duration_s: 1.0,
                linear_ir: vec![],
                noise_tail_start_s: None,
                harmonics: vec![],
            },
            standard: Vec::new(),
            gate: None,
        }];
        assert!(r.ir_stats().is_none());
    }

    // ─── interface latency (τ), archived alongside the arrival ─────────

    /// #378 contingency (AC6 rig run, 2026-09-15), kept by #346 for the
    /// unbounded case: with no causal bound `delay_samples` / `arrival_s`
    /// are peak-derived, and the onset is still computed and carried as a
    /// diagnostic. Tested against the rejected wiring — a
    /// synthetic multi-way-like IR where sustained onset energy sits well
    /// before the peak, so an arrival still taken from the onset would
    /// differ from the peak-derived value asserted here.
    #[test]
    fn ir_stats_arrival_is_peak_derived_onset_is_diagnostic() {
        let window_len = 1024;
        let centre = window_len / 2;
        let noise = 0.001;
        let peak_true = centre + 100;
        let onset_true = peak_true - 20; // within the guard band before the peak
        let mut ir: Vec<f64> = vec![noise; window_len];
        for v in ir.iter_mut().take(peak_true + 1).skip(onset_true) {
            *v = 0.3;
        }
        ir[peak_true] = 1.0;

        let peak_index = ir
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
            .unwrap()
            .0;
        assert_eq!(
            peak_index, peak_true,
            "test setup: peak must be at peak_true"
        );

        let r = ir_report_with_custom_ir(ir, 48_000);
        let stats = r.ir_stats().unwrap();
        assert_eq!(stats.peak_index, peak_true);
        assert_eq!(
            stats.onset_index, onset_true,
            "the onset is still estimated and carried as a diagnostic"
        );
        assert_ne!(
            stats.delay_samples,
            onset_true as i64 - centre as i64,
            "arrival must not be the onset-derived delay — #378 contingency"
        );
        assert_eq!(stats.delay_samples, peak_true as i64 - centre as i64);
        assert!((stats.arrival_s - (peak_true as f64 - centre as f64) / 48_000.0).abs() < 1e-12);
        assert!(stats.onset_rule.contains("no causal bound"));
        assert_eq!(stats.arrival_source, ArrivalSource::Peak);
        assert_eq!(stats.onset_standing, OnsetStanding::NoCausalBound);
    }

    /// Noise in ±1e-4, from a fixed hash so the fixture is reproducible.
    fn hashed_noise(window_len: usize) -> Vec<f64> {
        (0..window_len)
            .map(|i| {
                let mut s = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
                s ^= s >> 29;
                ((s >> 11) as f64 / (1u64 << 53) as f64 - 0.5) * 2e-4
            })
            .collect()
    }

    /// #346 acceptance criterion 2, tested against the rejected
    /// implementation: a multi-way-like IR — sustained energy from the
    /// wavefront to a peak 20 samples later, as on pupu at 2 m — with a
    /// measured same-capture reference and a distance whose causal bound
    /// sits 20 samples before the wavefront, so every onset condition holds
    /// and the pick is the wavefront. The rejected implementation — the
    /// promotion withdrawn by #346 architect revision 4 — is that bounded
    /// onset as the arrival. It is computed inline and must not be what
    /// `ir_stats` reports; the peak is.
    #[test]
    fn ir_stats_keeps_the_peak_arrival_over_an_unscored_onset() {
        let window_len = 1024;
        let sr = 96_000u32;
        let centre = window_len / 2;
        let peak_true = centre + 200;
        let wavefront = peak_true - 20;
        let bound_index = wavefront - 20;
        let mut ir = hashed_noise(window_len);
        for v in ir.iter_mut().take(peak_true).skip(wavefront) {
            *v += 0.3;
        }
        ir[peak_true] = 1.0;

        let (argmax, _) = crate::measurement::sweep::ir_peak(&ir);
        assert_eq!(argmax, peak_true, "test setup");

        let mut r = ir_report_with_custom_ir(ir.clone(), sr);
        let c = crate::shared::conversions::speed_of_sound_from_config(Some(20.0));
        r.position = Some(PositionSnapshot {
            temperature_c: Some(20.0),
            distance_m: Some((bound_index - centre) as f64 / sr as f64 * c),
            ..Default::default()
        });
        r.reference_latency = Some(measured_reference(0.0));

        let stats = r.ir_stats().unwrap();
        assert_eq!(stats.verdict, IrVerdict::Ok, "test setup");
        assert_eq!(
            stats.causal_bound.min_admissible_index(),
            Some(bound_index),
            "test setup"
        );
        assert_eq!(stats.onset_standing, OnsetStanding::Unscored, "test setup");

        // The rejected implementation: the bounded onset as the arrival.
        let rejected = crate::measurement::sweep::estimate_onset(
            &ir,
            argmax,
            sr,
            onset_floor(pre_impulse_region(&ir, argmax)),
            &stats.causal_bound,
        );
        assert_eq!(rejected.index, wavefront, "test setup");
        assert_eq!(stats.onset_index, wavefront, "test setup");
        assert_ne!(wavefront, peak_true, "test setup");

        assert_eq!(stats.arrival_source, ArrivalSource::Peak);
        assert_eq!(stats.delay_samples, peak_true as i64 - centre as i64);
        assert_ne!(
            stats.delay_samples,
            rejected.index as i64 - centre as i64,
            "the arrival must not be the bounded onset — #346 architect revision 4"
        );
        assert!((stats.arrival_s - stats.delay_samples as f64 / sr as f64).abs() < 1e-15);
    }

    /// #346: flight time is the peak arrival minus the stored peak-picked
    /// τ, as on main, even when every onset condition holds. The
    /// onset-derived value is computed inline and must differ.
    #[test]
    fn ir_stats_flight_time_subtracts_tau_from_the_peak_over_an_unscored_onset() {
        let window_len = 1024;
        let sr = 96_000u32;
        let centre = window_len / 2;
        let peak_true = centre + 200;
        let wavefront = peak_true - 20;
        let bound_index = wavefront - 20;
        let mut ir = hashed_noise(window_len);
        for v in ir.iter_mut().take(peak_true).skip(wavefront) {
            *v += 0.3;
        }
        ir[peak_true] = 1.0;
        let mut r = ir_report_with_custom_ir(ir, sr);
        let c = crate::shared::conversions::speed_of_sound_from_config(Some(20.0));
        r.position = Some(PositionSnapshot {
            temperature_c: Some(20.0),
            distance_m: Some((bound_index - centre) as f64 / sr as f64 * c),
            ..Default::default()
        });
        r.reference_latency = Some(measured_reference(0.0));
        let tau_s = 30.0 / sr as f64;
        r.interface_latency = Some(measured_tau(tau_s));

        let stats = r.ir_stats().unwrap();
        assert_eq!(stats.onset_standing, OnsetStanding::Unscored, "test setup");
        assert_eq!(stats.onset_index, wavefront, "test setup");
        assert_eq!(stats.arrival_source, ArrivalSource::Peak);
        let ft = stats.flight_time_s.expect("flight time");
        let peak_ft = (peak_true as f64 - centre as f64) / sr as f64 - tau_s;
        let onset_ft = (wavefront as f64 - centre as f64) / sr as f64 - tau_s;
        assert!((ft - peak_ft).abs() < 1e-15, "{ft} vs {peak_ft}");
        assert!(
            (ft - onset_ft).abs() > 1e-6,
            "flight time must not come from the onset"
        );
    }

    /// Each onset condition, failed alone, names itself — in the order
    /// `onset_standing` checks them — and all holding is `Unscored`.
    #[test]
    fn onset_standing_names_the_first_failed_condition() {
        use crate::measurement::sweep::{
            BoundInputs, EdgeGuard, MissingBoundInput, OnsetEstimate, OnsetPick, WindowLimit,
        };
        let enforced = CausalBound::Enforced {
            index: 100,
            inputs: BoundInputs {
                reference_tau_s: 0.0,
                distance_m: 1.0,
                speed_of_sound_m_s: 343.0,
                temperature_c: None,
            },
        };
        let unbounded = CausalBound::Unavailable(MissingBoundInput::Distance);
        let picked = |limit, pinned, edge_guard| OnsetEstimate {
            index: 110,
            rule: String::new(),
            pick: OnsetPick::Picked {
                window_start: 100,
                limit,
                pinned,
                edge_guard,
            },
        };
        let good = picked(WindowLimit::CausalBound, false, Some(EdgeGuard::Passed));
        let declined = OnsetEstimate {
            index: 120,
            rule: String::new(),
            pick: OnsetPick::Declined,
        };
        let failed = IrVerdict::Failed {
            reason: "pre-impulse SNR below threshold".into(),
        };
        assert_eq!(
            onset_standing(&enforced, &good, &IrVerdict::Ok),
            OnsetStanding::Unscored
        );
        let cases = [
            (
                &unbounded,
                good.clone(),
                IrVerdict::Ok,
                OnsetStanding::NoCausalBound,
            ),
            (
                &enforced,
                picked(WindowLimit::SearchSpan, false, Some(EdgeGuard::Passed)),
                IrVerdict::Ok,
                OnsetStanding::BoundNotBinding,
            ),
            (
                &enforced,
                declined,
                IrVerdict::Ok,
                OnsetStanding::PickerDeclined,
            ),
            (
                &enforced,
                picked(WindowLimit::CausalBound, true, None),
                IrVerdict::Ok,
                OnsetStanding::PickOnWindowStart,
            ),
            (
                &enforced,
                picked(
                    WindowLimit::CausalBound,
                    false,
                    Some(EdgeGuard::Failed { repick: Some(117) }),
                ),
                IrVerdict::Ok,
                OnsetStanding::EdgeFollowing { repick: Some(117) },
            ),
            (
                &enforced,
                picked(
                    WindowLimit::CausalBound,
                    false,
                    Some(EdgeGuard::Failed { repick: None }),
                ),
                IrVerdict::Ok,
                OnsetStanding::EdgeFollowing { repick: None },
            ),
            (
                &enforced,
                picked(WindowLimit::CausalBound, false, None),
                IrVerdict::Ok,
                OnsetStanding::EdgeFollowing { repick: None },
            ),
            (
                &enforced,
                good.clone(),
                failed,
                OnsetStanding::DeconvolutionFailed,
            ),
        ];
        for (bound, onset, verdict, standing) in cases {
            assert_eq!(
                onset_standing(bound, &onset, &verdict),
                standing,
                "{standing:?}"
            );
        }
    }

    /// End to end through `ir_stats`: a bound inside the wavefront's rise
    /// leaves a homogeneous window, the edge guard fires, and the onset
    /// standing names it — the bound, not the IR, set that pick. The
    /// arrival is the peak regardless.
    #[test]
    fn ir_stats_keeps_the_peak_when_the_pick_follows_the_window_edge() {
        let window_len = 1024;
        let sr = 96_000u32;
        let centre = window_len / 2;
        let peak_true = centre + 300;
        let onset_true = peak_true - 110;
        let bound_index = onset_true + 50;
        let mut ir = hashed_noise(window_len);
        let rise = (peak_true - onset_true) as f64;
        for (i, v) in ir.iter_mut().enumerate() {
            if (onset_true..=peak_true).contains(&i) {
                let x = (i - onset_true) as f64 / rise;
                *v += x * x;
            }
        }
        let mut r = ir_report_with_custom_ir(ir, sr);
        let c = crate::shared::conversions::speed_of_sound_from_config(Some(20.0));
        r.position = Some(PositionSnapshot {
            temperature_c: Some(20.0),
            distance_m: Some((bound_index - centre) as f64 / sr as f64 * c),
            ..Default::default()
        });
        r.reference_latency = Some(measured_reference(0.0));

        let stats = r.ir_stats().unwrap();
        assert_eq!(
            stats.causal_bound.min_admissible_index(),
            Some(bound_index),
            "test setup"
        );
        assert!(
            stats.onset_index > bound_index,
            "test setup: pick must be clear of the window start, got {}",
            stats.onset_index
        );
        assert!(
            matches!(
                stats.onset_standing,
                OnsetStanding::EdgeFollowing { repick: Some(r) }
                    if r.abs_diff(stats.onset_index) > 1
            ),
            "{:?}: {}",
            stats.onset_standing,
            stats.onset_rule
        );
        assert_eq!(stats.arrival_source, ArrivalSource::Peak);
        assert_eq!(stats.delay_samples, stats.peak_index as i64 - centre as i64);
    }

    /// #346 / #460: when a report carries both a same-capture reference latency and
    /// a recorded `position.distance_m`, `ir_stats` must convert them
    /// into a causal bound and enforce it — proven with a capture whose
    /// unbounded answer is non-causal, so the bound actively changes the
    /// result rather than agreeing with it by coincidence.
    ///
    /// #378 changed how: the bound is the search window's lower limit,
    /// so the non-causal candidate is outside the picker's reach rather
    /// than being clamped after the fact. What is asserted is the same
    /// requirement — a bounded capture never reads earlier than pure
    /// flight time allows — expressed against a window instead of a
    /// walk.
    #[test]
    fn ir_stats_wires_a_causal_bound_from_position_and_reference_latency() {
        let window_len = 1024;
        let sr = 48_000u32;
        let centre = window_len / 2;
        let peak_true = centre + 100;
        let bound_index = peak_true - 12;
        let wavefront = peak_true - 7; // inside the admissible window
        let pre_ring = peak_true - 52; // below it — non-causal
        let mut ir: Vec<f64> = (0..window_len)
            .map(|i| {
                let mut s = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
                s ^= s >> 29;
                ((s >> 11) as f64 / (1u64 << 53) as f64 - 0.5) * 2e-4
            })
            .collect();
        for (i, v) in ir.iter_mut().enumerate() {
            if (pre_ring..wavefront).contains(&i) {
                *v += 0.02 * ((i - pre_ring) as f64 * 0.7).sin();
            } else if (wavefront..=peak_true).contains(&i) {
                *v += 0.3 * (i - wavefront + 1) as f64 / 8.0;
            }
        }
        ir[peak_true] = 1.0;

        let mut r = ir_report_with_custom_ir(ir, sr);
        let unbounded = r.ir_stats().unwrap();
        assert!(
            unbounded.onset_index < bound_index,
            "test setup: the unbounded pick must be non-causal, got {}",
            unbounded.onset_index
        );
        assert!(unbounded.onset_rule.contains("no causal bound"));

        let bound_offset_samples = (bound_index - centre) as f64;
        let c = crate::shared::conversions::speed_of_sound_from_config(Some(20.0));
        r.position = Some(PositionSnapshot {
            temperature_c: Some(20.0),
            distance_m: Some(bound_offset_samples / sr as f64 * c),
            ..Default::default()
        });
        r.reference_latency = Some(measured_reference(0.0));

        let bounded = r.ir_stats().unwrap();
        assert_eq!(
            bounded.causal_bound.min_admissible_index(),
            Some(bound_index)
        );
        assert!(
            bounded.onset_index >= bound_index,
            "causal bound must exclude the non-causal candidate, got {}",
            bounded.onset_index
        );
        assert_ne!(bounded.onset_index, unbounded.onset_index);
        assert!(bounded.onset_rule.contains("causal bound enforced"));
        assert!(bounded
            .onset_rule
            .contains(&format!("window start at sample {bound_index}")));
    }

    /// #460 AC6, tested against the rejected implementation: a report that
    /// carries a *stored* measured τ and a distance, but no same-capture
    /// reference, must not get a bound. The bound the stored-τ wiring would
    /// have built is computed here, and must not be the window start.
    #[test]
    fn ir_stats_never_builds_the_bound_from_stored_interface_latency() {
        let window_len = 1024;
        let sr = 48_000u32;
        let centre = window_len / 2;
        let peak_true = centre + 100;
        let mut ir: Vec<f64> = (0..window_len)
            .map(|i| {
                let mut s = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
                s ^= s >> 29;
                ((s >> 11) as f64 / (1u64 << 53) as f64 - 0.5) * 2e-4
            })
            .collect();
        for (i, v) in ir
            .iter_mut()
            .enumerate()
            .take(peak_true + 1)
            .skip(peak_true - 7)
        {
            *v += 0.3 * (i + 8 - peak_true) as f64 / 8.0;
        }
        ir[peak_true] = 1.0;

        let mut r = ir_report_with_custom_ir(ir, sr);
        let distance_m = 0.05;
        r.position = Some(PositionSnapshot {
            temperature_c: Some(20.0),
            distance_m: Some(distance_m),
            ..Default::default()
        });
        let stored_tau_s = 0.001;
        r.interface_latency = Some(measured_tau(stored_tau_s));
        r.reference_latency = None;

        // The rejected wiring: the pre-#460 bound built from the stored τ.
        let c = crate::shared::conversions::speed_of_sound_from_config(Some(20.0));
        let stored_bound =
            (centre as f64 + (stored_tau_s + distance_m / c) * sr as f64).round() as usize;
        assert!(
            stored_bound < peak_true,
            "test setup: the stored-τ bound must be admissible, got {stored_bound}"
        );

        let stats = r.ir_stats().unwrap();
        assert_eq!(
            stats.causal_bound.min_admissible_index(),
            None,
            "a stored τ must never enforce the bound"
        );
        assert!(
            matches!(
                stats.causal_bound,
                CausalBound::Unavailable(MissingBoundInput::ReferenceLatency { .. })
            ),
            "{:?}",
            stats.causal_bound
        );
        assert!(
            stats
                .onset_rule
                .contains("no causal bound (reference latency unavailable)"),
            "{}",
            stats.onset_rule
        );
        assert!(
            !stats
                .onset_rule
                .contains(&format!("window start at sample {stored_bound}")),
            "the stored-τ bound set the window: {}",
            stats.onset_rule
        );
    }

    /// #346 (QA on #352, correctness issue 2) / #353: `floor_rms`'s guard
    /// band (`(window_len / 32).max(8)`, pre-existing) is sized off
    /// `window_len` alone — nothing ties it to how wide the peak's own
    /// sustained-energy run actually is. When that run is wider than the
    /// guard, it bleeds into `pre_region` and inflates `floor_rms`. Before
    /// #353, `estimate_onset` thresholded directly off that inflated
    /// value and the backward search stopped at the peak instead of the
    /// true onset — silently reproducing the very peak-as-arrival bug
    /// #346 exists to fix, for a signal shape the guard band was not
    /// sized for.
    ///
    /// #353 (architect revision 2, option A′) fixes this: `estimate_onset`
    /// now thresholds against a median-based floor over the same
    /// `pre_region`, which a lobe this size (contaminating 52/152 ≈ 34% of
    /// `pre_region`, comfortably under the estimator's 50% breakdown
    /// point) does not move. This test — a small window (guard = 8,
    /// `window_len` = 256) with a sustained run wide enough to defeat the
    /// old RMS floor — now asserts the corrected behaviour.
    #[test]
    fn ir_stats_onset_threshold_is_coupled_to_the_guard_band_a_wide_lobe_can_break() {
        let window_len = 256;
        let sr = 48_000u32;
        let noise = 0.001;
        let peak_true = 160;
        let onset_true = 100; // 60 samples wide — wider than guard = 8
        let mut ir = vec![noise; window_len];
        for v in ir.iter_mut().take(peak_true + 1).skip(onset_true) {
            *v = 0.3;
        }
        ir[peak_true] = 1.0;

        let r = ir_report_with_custom_ir(ir, sr);
        let stats = r.ir_stats().unwrap();
        // The median floor (#353) is not moved by this lobe: the backward
        // walk now reaches the true onset instead of stopping at the peak.
        assert_ne!(
            stats.onset_index, peak_true,
            "if this now equals peak_true, the median-floor fix (#353) has \
             regressed to the old RMS-floor behaviour — update this test's \
             assertions and its doc comment"
        );
        assert_eq!(
            stats.onset_index, onset_true,
            "median floor (#353) has a 50% breakdown point; this lobe \
             contaminates only ~34% of pre_region, well under it, so the \
             backward walk must reach the true onset"
        );
    }

    /// The rule #378 rejects, computed inline so these tests measure it
    /// rather than asserting "the new answer is closer to truth": a
    /// threshold 12 dB above the supplied pre-impulse floor, walked
    /// backward from the peak. Nothing in `ac-core` implements this any
    /// more — it exists only here, as the comparison.
    fn rejected_level_crossing_rule(ir: &[f64], peak_index: usize, floor: f64) -> usize {
        let threshold = floor * 10f64.powf(12.0 / 20.0);
        let mut onset = peak_index;
        while onset > 0 && ir[onset - 1].abs() > threshold {
            onset -= 1;
        }
        onset
    }

    /// #353's median floor, recomputed here over an arbitrary region so
    /// the rejected rule can be driven by either floor.
    fn median_abs_over_phi_inv(region: &[f64]) -> f64 {
        if region.is_empty() {
            return 0.0;
        }
        let mut abs_vals: Vec<f64> = region.iter().map(|v| v.abs()).collect();
        abs_vals.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let n = abs_vals.len();
        let median = if n % 2 == 1 {
            abs_vals[n / 2]
        } else {
            (abs_vals[n / 2 - 1] + abs_vals[n / 2]) / 2.0
        };
        median / 0.674_489_750_196_081_7
    }

    /// #353 acceptance criterion 3, carried into #378: names the input
    /// that makes the *rejected* level-crossing rule go red under both
    /// the RMS floor and the median floor (option A′), per the
    /// architect's revision-2 probe table (2026-08-23) — bracketing the
    /// crossings, not locating them to the sample, exactly as that
    /// comment states. All cases share `window_len = 256`,
    /// `peak_true = 160`, `guard = 8` (so `pre_end = 152`); `width` is
    /// `peak_true - onset_true`, i.e. how far the sustained run reaches
    /// back from the peak.
    ///
    /// Both rejected rules are computed inline against each input rather
    /// than trusted from an earlier revision (`test-against-the-rejected-
    /// implementation`). #353's own evidence is kept intact — the RMS
    /// floor breaks first, the median floor survives to its 50%
    /// breakdown point — and #378's claim is added on top: the shipped
    /// picker takes no threshold from either floor, so contamination
    /// that moves both of them does not move its answer at all.
    #[test]
    fn ir_stats_onset_floor_breakdown_point_matches_the_measured_table() {
        let window_len = 256;
        let sr = 48_000u32;
        let noise = 0.001;
        let peak_true = 160;
        let guard = (window_len / 32).max(8); // 8

        // (lobe width, RMS-floor level rule reaches onset_true?,
        //  median-floor level rule reaches onset_true?)
        let cases = [
            (16, true, true),   // RMS: 5% contaminated, green
            (18, false, true),  // RMS: 6.6% contaminated, red; median: green
            (80, false, true),  // RMS: red; median: 47% contaminated, green
            (90, false, false), // median: 54% contaminated, past 50% — red
        ];

        for (width, rms_reaches_onset, median_reaches_onset) in cases {
            let onset_true = peak_true - width;
            let mut ir = vec![noise; window_len];
            for v in ir.iter_mut().take(peak_true + 1).skip(onset_true) {
                *v = 0.3;
            }
            ir[peak_true] = 1.0;

            let pre_end = peak_true.saturating_sub(guard);
            let pre_region = &ir[..pre_end];
            let rms_floor = {
                let mean_sq =
                    pre_region.iter().map(|v| v * v).sum::<f64>() / pre_region.len() as f64;
                mean_sq.sqrt()
            };
            let median_floor = median_abs_over_phi_inv(pre_region);

            assert_eq!(
                rejected_level_crossing_rule(&ir, peak_true, rms_floor) == onset_true,
                rms_reaches_onset,
                "width {width}: RMS-floor level rule onset = {}, expected \
                 reaches_onset={rms_reaches_onset}",
                rejected_level_crossing_rule(&ir, peak_true, rms_floor)
            );
            assert_eq!(
                rejected_level_crossing_rule(&ir, peak_true, median_floor) == onset_true,
                median_reaches_onset,
                "width {width}: median-floor level rule onset = {}, expected \
                 reaches_onset={median_reaches_onset}",
                rejected_level_crossing_rule(&ir, peak_true, median_floor)
            );

            let r = ir_report_with_custom_ir(ir, sr);
            let stats = r.ir_stats().unwrap();
            assert_eq!(
                stats.onset_index, onset_true,
                "width {width}: #378's picker takes no threshold from either \
                 floor, so guard-band contamination that moves both rejected \
                 rules must not move it"
            );
        }
    }

    /// #353 acceptance criterion 5, carried into #378: the guard stays
    /// fixed and content-independent (option A′ added a distribution
    /// property, not a second constant coupled to the first) — asserted
    /// as an estimator-vs-estimator agreement rather than a hidden
    /// tolerance between the two floors, per the architect's own
    /// instruction ("do not convert that into a dB tolerance on the two
    /// floors; that number has not been measured and would ship as
    /// `assumed`"). On a clean pre-impulse region the RMS floor and the
    /// median floor must send the rejected level rule to the *same*
    /// onset index — checked over many seeded noise realisations so this
    /// is a statement, not a coincidence (architect's own probe: 200
    /// seeds, `{0: 200}`).
    ///
    /// #378 adds the stronger claim on the same 200 captures: the
    /// shipped picker's answer is identical whichever floor is handed to
    /// it, because the floor is a validity gate now and not a threshold.
    #[test]
    fn ir_stats_onset_floor_agrees_with_rms_floor_on_clean_pre_impulse_noise() {
        use rand::rngs::StdRng;
        use rand::{Rng, SeedableRng};

        let window_len: usize = 1024;
        let sr = 48_000u32;
        let peak_true: usize = 700;
        let onset_true = peak_true - 20; // within the guard band before the peak
        let guard = (window_len / 32).max(8);
        assert!(
            onset_true >= peak_true.saturating_sub(guard),
            "test setup: run must stay inside the guard band so both floors \
             see clean noise only"
        );

        for seed in 0u64..200 {
            let mut rng = StdRng::seed_from_u64(0xB353_0000 + seed);
            let mut ir: Vec<f64> = (0..window_len)
                .map(|_| (rng.gen::<f64>() * 2.0 - 1.0) * 0.001)
                .collect();
            for v in ir.iter_mut().take(peak_true + 1).skip(onset_true) {
                *v = 0.3;
            }
            ir[peak_true] = 1.0;

            let pre_end = peak_true.saturating_sub(guard);
            let pre_region = &ir[..pre_end];
            let rms_floor = {
                let mean_sq =
                    pre_region.iter().map(|v| v * v).sum::<f64>() / pre_region.len() as f64;
                mean_sq.sqrt()
            };
            let median_floor = median_abs_over_phi_inv(pre_region);

            assert_eq!(
                rejected_level_crossing_rule(&ir, peak_true, rms_floor),
                rejected_level_crossing_rule(&ir, peak_true, median_floor),
                "seed {seed}: median floor (#353) must agree with the RMS \
                 floor on an uncontaminated pre-impulse region"
            );

            let picked_with_rms = crate::measurement::sweep::estimate_onset(
                &ir,
                peak_true,
                sr,
                rms_floor,
                &CausalBound::Unavailable(MissingBoundInput::Both {
                    reference_reason: String::new(),
                }),
            );
            let r = ir_report_with_custom_ir(ir, sr);
            let stats = r.ir_stats().unwrap();
            assert_eq!(
                stats.onset_index, picked_with_rms.index,
                "seed {seed}: #378's pick must not depend on which floor \
                 reaches the validity gate"
            );
        }
    }

    // ─── #359: arrival_check / flight_time_s ─────────────────────────────

    /// Moved down from `ac-scene::sweep_ir` (#359): the τ subtraction
    /// itself now lives here, gated by `arrival_check`, so `ac-cli` and
    /// `ac-scene` both read one already-checked number rather than each
    /// re-deriving it. No same-capture reference at all reads `Unchecked`
    /// (nothing to check against, not a dispute) — the flight time is
    /// still produced from the capture's own `interface_latency`.
    #[test]
    fn ir_stats_flight_time_is_tau_corrected_when_interface_latency_is_measured() {
        let sr = 4_000u32;
        let window_len = 1024;
        let centre = window_len / 2;
        // delay_samples = 1 -> arrival_s = 0.25 ms at 4 kHz.
        let mut r = ir_report_with_peak(window_len, centre + 1, 1.0, 0.0, sr);
        r.interface_latency = Some(measured_tau(0.0001)); // 0.1 ms
        let stats = r.ir_stats().unwrap();
        assert!(
            matches!(stats.arrival_check, ArrivalCheck::Unchecked { .. }),
            "no reference at all must read Unchecked, not a disagreement: {:?}",
            stats.arrival_check
        );
        let flight_ms = stats
            .flight_time_s
            .expect("Unchecked must still produce a flight time")
            * 1000.0;
        assert!(
            (flight_ms - 0.15).abs() < 1e-9,
            "expected 0.25ms - 0.1ms = 0.15ms, got {flight_ms}"
        );
    }

    /// #359 AC4: a same-capture reference reading exactly one period ahead
    /// of the stored reference τ is reported as a graph-buffering shift,
    /// naming the period — #347's own wording, reused via
    /// [`compare_tau_readings`] rather than reimplemented (AC2).
    #[test]
    fn arrival_check_detects_an_exact_period_shift_and_names_the_period() {
        let sr = 48_000u32;
        let period = 1024u32;
        let stored_tau_s = 0.0119;
        let same_capture_tau_s = stored_tau_s + period as f64 / sr as f64;
        let mut r = ir_report_with_peak(1024, 600, 1.0, 0.0, sr);
        r.reference_latency = Some(measured_reference(same_capture_tau_s));
        r.reference_stored_latency = Some(stored_reference_tau(stored_tau_s, Some(period)));
        let stats = r.ir_stats().unwrap();
        match &stats.arrival_check {
            ArrivalCheck::PeriodShift(d) => {
                assert_eq!(d.periods, Some(1));
                assert!(d.message().contains("1024 samples"), "{}", d.message());
            }
            other => panic!("expected PeriodShift, got {other:?}"),
        }
    }

    /// #359 AC3, tested against the rejected implementation: a delta one
    /// sample off an exact period must never read as a period shift — a
    /// test that only checked "a difference" would pass on this input too.
    #[test]
    fn arrival_check_a_near_period_delta_is_mismatch_not_period_shift() {
        let sr = 48_000u32;
        let period = 1024u32;
        let stored_tau_s = 0.0119;
        let same_capture_tau_s = stored_tau_s + (period as f64 + 1.0) / sr as f64;
        let mut r = ir_report_with_peak(1024, 600, 1.0, 0.0, sr);
        r.reference_latency = Some(measured_reference(same_capture_tau_s));
        r.reference_stored_latency = Some(stored_reference_tau(stored_tau_s, Some(period)));
        let stats = r.ir_stats().unwrap();
        match &stats.arrival_check {
            ArrivalCheck::Mismatch(d) => {
                assert_eq!(d.periods, None);
                assert_eq!(d.delta_samples, period as i64 + 1);
            }
            other => panic!("expected Mismatch, got {other:?}"),
        }
    }

    /// A sub-sample difference is ordinary rounding noise, not a fault —
    /// [`compare_tau_readings`]'s whole-sample agreement rule, inherited
    /// from #347 unchanged.
    #[test]
    fn arrival_check_a_fractional_sample_difference_is_noise_not_a_fault() {
        let sr = 48_000u32;
        let stored_tau_s = 0.0119;
        let same_capture_tau_s = stored_tau_s + 0.3 / sr as f64;
        let mut r = ir_report_with_peak(1024, 600, 1.0, 0.0, sr);
        r.reference_latency = Some(measured_reference(same_capture_tau_s));
        r.reference_stored_latency = Some(stored_reference_tau(stored_tau_s, Some(1024)));
        let stats = r.ir_stats().unwrap();
        assert_eq!(stats.arrival_check, ArrivalCheck::Agree);
    }

    /// A backend that cannot report a period size must never let a delta
    /// that happens to equal 1024 samples read as a period shift — there is
    /// no period on file to have been a multiple of.
    #[test]
    fn arrival_check_an_unknown_period_size_never_reads_as_a_period_shift() {
        let sr = 48_000u32;
        let stored_tau_s = 0.0119;
        let same_capture_tau_s = stored_tau_s + 1024.0 / sr as f64;
        let mut r = ir_report_with_peak(1024, 600, 1.0, 0.0, sr);
        r.reference_latency = Some(measured_reference(same_capture_tau_s));
        r.reference_stored_latency = Some(stored_reference_tau(stored_tau_s, None));
        let stats = r.ir_stats().unwrap();
        match &stats.arrival_check {
            ArrivalCheck::Mismatch(d) => assert_eq!(d.periods, None),
            other => panic!("expected Mismatch (unknown period size), got {other:?}"),
        }
    }

    /// The test that fails if the gate is removed (#359): a detected period
    /// shift must withhold the flight time even though `interface_latency`
    /// is measured for this capture pair — the stored reference τ is shown
    /// to be from a different lifetime state, and subtracting it would
    /// reproduce this issue's own failure shape.
    #[test]
    fn arrival_check_period_shift_withholds_the_flight_time() {
        let sr = 48_000u32;
        let period = 1024u32;
        let stored_tau_s = 0.0119;
        let same_capture_tau_s = stored_tau_s + period as f64 / sr as f64;
        let mut r = ir_report_with_peak(1024, 600, 1.0, 0.0, sr);
        r.interface_latency = Some(measured_tau(0.001));
        r.reference_latency = Some(measured_reference(same_capture_tau_s));
        r.reference_stored_latency = Some(stored_reference_tau(stored_tau_s, Some(period)));
        let stats = r.ir_stats().unwrap();
        assert!(matches!(stats.arrival_check, ArrivalCheck::PeriodShift(_)));
        assert_eq!(
            stats.flight_time_s, None,
            "a period shift must withhold the flight time even though \
             interface_latency is measured"
        );
    }

    /// A v8 report (written before this field existed) reads `Unchecked` —
    /// not a fault, just nothing to compare — and the flight time still
    /// follows `interface_latency` alone, unaffected by a check that never
    /// ran.
    #[test]
    fn arrival_check_a_pre_v9_report_is_unchecked_but_flight_time_still_follows_interface_latency()
    {
        let sr = 48_000u32;
        let mut r = ir_report_with_peak(1024, 600, 1.0, 0.0, sr);
        r.interface_latency = Some(measured_tau(0.001));
        r.reference_latency = Some(measured_reference(0.002));
        r.reference_stored_latency = None; // v8 shape: field absent
        let stats = r.ir_stats().unwrap();
        assert!(matches!(
            stats.arrival_check,
            ArrivalCheck::Unchecked { .. }
        ));
        assert_eq!(
            stats.flight_time_s,
            Some(stats.arrival_s - 0.001),
            "Unchecked must not withhold a flight time it never disputed"
        );
    }

    // ─── #461: interface_latency_enumeration ─────────────────────────────

    /// A crossed epoch flags the flight time and never withholds it — the
    /// architect's "flag, not refuse" ruling. Every non-`Same` state flags,
    /// including a v9 report that carries no check at all.
    #[test]
    fn a_non_same_enumeration_flags_the_flight_time_without_withholding_it() {
        use crate::shared::calibration::EnumerationCheck;
        let sr = 48_000u32;
        let cases = [
            (Some(EnumerationCheck::Same), false),
            (
                Some(EnumerationCheck::Crossed {
                    boundary: "host rebooted".into(),
                    since: None,
                }),
                true,
            ),
            (
                Some(EnumerationCheck::NotObservable {
                    reason: "cpal backend has no enumeration probe".into(),
                }),
                true,
            ),
            (Some(EnumerationCheck::NotRecorded), true),
            (None, true),
        ];
        for (check, flagged) in cases {
            let mut r = ir_report_with_peak(1024, 600, 1.0, 0.0, sr);
            r.interface_latency = Some(measured_tau_with_check(0.001, check.clone()));
            let stats = r.ir_stats().unwrap();
            assert_eq!(stats.interface_latency_enumeration, check);
            assert_eq!(
                stats.flight_time_s,
                Some(stats.arrival_s - 0.001),
                "{check:?} must not withhold the flight time"
            );
            assert_eq!(stats.interface_latency_unverified(), flagged, "{check:?}");
        }
    }

    /// Nothing to qualify when no flight time exists.
    #[test]
    fn no_flight_time_is_never_flagged() {
        let r = ir_report_with_peak(1024, 600, 1.0, 0.0, 48_000);
        let stats = r.ir_stats().unwrap();
        assert_eq!(stats.flight_time_s, None);
        assert!(!stats.interface_latency_unverified());
    }
}
