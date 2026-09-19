//! Frame C (#308): the sweep-derived, gated IR display — `ac plot ir`'s
//! `MeasurementReport` (Tier 1 report pipeline, produced by `plot_ir`
//! and rendered to HTML/PDF today), read back into a screen-drawable
//! scene.
//!
//! **Format decision, recorded so the next reader does not re-open the
//! question** (#308 architect review): `ac-view` gains a *second
//! loader* for `MeasurementReport` JSON (this module + `ac-view`'s
//! `report_flow`) rather than the daemon growing an `.acsnap`-compatible
//! sidecar. The report file already exists on disk (`ac plot ir`
//! writes it), the numeric work (arrival, gate window, `f_low_hz`) is
//! already done by [`ac_core::measurement::report::MeasurementReport::ir_stats`],
//! and forcing a Farina-deconvolved `linear_ir` through `.acsnap`'s
//! documented "raw pre-processing capture" container would fork that
//! format's contract. See the issue's architect comment for the full
//! tradeoff.
//!
//! This is a disjoint producer from [`crate::ir::IrScene`] — that
//! module's own doc names why the two must never share a type, a
//! header, or a construction path; this module is the "second loader"
//! that review chose, not a branch on `IrScene`.
//!
//! Rendering target and fault text are #286/#308's UX comment, carried
//! forward verbatim; this module is where they become plain data.

use std::ops::RangeInclusive;

use ac_core::measurement::report::{
    ArrivalCheck, ArrivalCrossCheck, ArrivalSource, DistanceCheck, IrVerdict, MeasurementData,
    MeasurementReport, PRE_IMPULSE_SNR_BASIS, PRE_IMPULSE_SNR_MIN_DB,
};

use crate::ir::ArrivalMarker;
use crate::readout::format_sweep_ir_header;
use crate::scene::{Provenance, Source, Trace};
use crate::ticks::{time_axis, time_to_x, Axis};

/// The marker's value and the word naming what it shows (#359): a
/// τ-corrected flight time reads `"flight"`, an uncorrected round trip
/// reads `"round trip"` — a withheld correction can no longer pass for an
/// applied one just because both used to print the same bare `"X.XX ms"`.
/// A detected disagreement is named on the round-trip figure, since that is
/// the only figure available once the correction is withheld.
///
/// `stats.flight_time_s` is `Some` only when [`IrStats::arrival_check`] is
/// not a disagreement; on `PeriodShift`/`Mismatch` it is `None` even with a
/// measured `interface_latency`, and the round trip prints instead, with
/// the disagreement named in the suffix.
///
/// A flight time whose stored τ was not shown to belong to this capture's
/// device enumeration (#461) — crossed, not observable, not recorded, or a
/// pre-v10 report with no check at all — carries `latency unverified`.
///
/// Prefixed with the rule that produced the arrival (#346 UX revision 4,
/// #537 UX): `peak above 2 kHz:` for the band-limited peak, with the corner
/// formatted from [`ArrivalSource::BandLimitedPeak`], or `broadband peak:`
/// when the band did not allow it (#537 UX revision 2) — the marker then
/// sits on the tallest peak again, and the prefix says so. The marker sits
/// on the arrival, which is no longer necessarily the tallest peak on
/// screen, so the prefix says which peak it marks; the cross-check suffix
/// says why the tallest one is not it. Both are acceptance criterion 4's tag
/// on a screenshot.
///
/// Suffix order (#537 UX revision 3): the cross-check suffix
/// ([`ArrivalCrossCheck`]) straight after the value's word, then the
/// #359/#461 suffixes in their order, then the distance suffix
/// ([`DistanceCheck`]) last — it qualifies the whole figure against an
/// external input.
fn arrival_marker_text(stats: &ac_core::measurement::report::IrStats) -> String {
    match stats.arrival_source {
        ArrivalSource::BandLimitedPeak { corner_hz } => format!(
            "peak above {}: {}",
            format_corner(corner_hz),
            arrival_marker_value(stats)
        ),
        ArrivalSource::Peak => format!("broadband peak: {}", arrival_marker_value(stats)),
    }
}

/// A typed distance as the marker echoes it: `2 m`, `0.5 m`.
fn format_distance(distance_m: f64) -> String {
    format!("{distance_m} m")
}

/// The distance check's part of the marker (#537 UX revision 3). On a
/// produced flight time: `, distance not given` when no distance was typed —
/// the narrowed claim, on every such capture — and `, distance not checked`
/// when the typed one is not a positive length. When the distance is the
/// first reason the flight time is withheld: `, later than 2 m allows` /
/// `, earlier than 2 m allows`, the words of the CLI's `withheld` row. Empty
/// otherwise: a flight time withheld upstream has no figure to qualify.
fn distance_suffix(stats: &ac_core::measurement::report::IrStats) -> String {
    let produced = stats.flight_time_s.is_some();
    let first_reason = !stats.arrival_cross_check.withholds_flight_time();
    match &stats.distance_check {
        DistanceCheck::NotGiven if produced => ", distance not given".to_string(),
        DistanceCheck::NotPositive { .. } if produced => ", distance not checked".to_string(),
        DistanceCheck::TooEarly { window, .. } if first_reason => format!(
            ", earlier than {} allows",
            format_distance(window.distance_m)
        ),
        DistanceCheck::TooLate { window, .. } if first_reason => {
            format!(", later than {} allows", format_distance(window.distance_m))
        }
        _ => String::new(),
    }
}

/// A high-pass corner as the marker prints it: `1 kHz`, `500 Hz`.
fn format_corner(corner_hz: f64) -> String {
    if corner_hz >= 1000.0 {
        format!("{} kHz", corner_hz / 1000.0)
    } else {
        format!("{corner_hz} Hz")
    }
}

/// The arrival cross-check's part of the marker (#537 UX). Empty on
/// `Agrees`.
fn cross_check_suffix(stats: &ac_core::measurement::report::IrStats) -> String {
    let gap_ms = |gap: i64| gap as f64 / stats.sample_rate_hz as f64 * 1000.0;
    match stats.arrival_cross_check {
        ArrivalCrossCheck::Agrees { .. } => String::new(),
        ArrivalCrossCheck::BroadbandLater { gap } | ArrivalCrossCheck::BroadbandEarlier { gap } => {
            format!(", broadband \u{394} {:+.2} ms", gap_ms(gap))
        }
        ArrivalCrossCheck::BandLimitUnavailable { .. } => ", not band-limited".to_string(),
        ArrivalCrossCheck::ArrivalAmbiguous { margin_db, .. } => {
            format!(", second lobe {:+.1} dB", -margin_db)
        }
        ArrivalCrossCheck::BandLimitedSnrLow { snr_db } => {
            format!(", arrival SNR {snr_db:.1} dB")
        }
        ArrivalCrossCheck::EarlierComparable { level_db, .. } => {
            format!(", earlier peak {level_db:+.1} dB")
        }
    }
}

/// [`arrival_marker_text`] without the rule prefix.
fn arrival_marker_value(stats: &ac_core::measurement::report::IrStats) -> String {
    let arrival_ms = stats.arrival_s * 1000.0;
    // #461: `latency unverified` qualifies the number, so it comes before
    // `ref unchecked`, which qualifies a check.
    let latency_unverified = stats.interface_latency_unverified();
    let (value_ms, word, tail) = match (stats.flight_time_s, &stats.arrival_check) {
        (Some(ft), ArrivalCheck::Unchecked { .. }) if latency_unverified => (
            ft * 1000.0,
            "flight",
            ", latency unverified, ref unchecked".to_string(),
        ),
        (Some(ft), ArrivalCheck::Unchecked { .. }) => {
            (ft * 1000.0, "flight", ", ref unchecked".to_string())
        }
        (Some(ft), _) if latency_unverified => {
            (ft * 1000.0, "flight", ", latency unverified".to_string())
        }
        (Some(ft), _) => (ft * 1000.0, "flight", String::new()),
        (None, ArrivalCheck::PeriodShift(d)) => {
            let n = d
                .periods
                .expect("PeriodShift always carries a period count");
            (
                arrival_ms,
                "round trip",
                format!(", {}-period shift", n.unsigned_abs()),
            )
        }
        (None, ArrivalCheck::Mismatch(d)) => (
            arrival_ms,
            "round trip",
            format!(", ref \u{394} {:+} samples", d.delta_samples),
        ),
        (None, _) => (arrival_ms, "round trip", String::new()),
    };
    format!(
        "{value_ms:.2} ms {word}{}{tail}{}",
        cross_check_suffix(stats),
        distance_suffix(stats)
    )
}

/// Mirrors [`crate::ir::IrScene`]'s own downsample target — display
/// column density, not a shared constant. See that module's doc on its
/// own copy for why the coincidence is commented rather than
/// deduplicated across producers.
const IR_MAX_SAMPLES: usize = 2000;

/// The two ways a load can fail, named the way #308's UX comment
/// specifies — deliberately not one merged "cannot open" state: they
/// send the operator to different places to check.
#[derive(Debug, Clone, PartialEq)]
pub enum SweepIrFault {
    /// The file did not decode as a `MeasurementReport` at all, or it
    /// did but carries no `ImpulseResponse` payload with a non-empty
    /// linear IR (e.g. a stepped-sine `ac plot` report). Same family per
    /// the architect's resolution of that ambiguity (#308 review, risk
    /// 2): both read as "not a sweep-derived IR" to an operator, and
    /// neither can be told apart from the other without asserting a
    /// cause the loader doesn't have.
    NotASweepDerivedIr,
    /// A valid `ImpulseResponse` payload, but its `gate` field is
    /// `None` — a legacy (pre-#280) report, or one from an ungated
    /// sweep. Decided directly off `payload.gate.is_some()`, never
    /// through [`MeasurementReport::ir_stats`]'s legacy-report fallback
    /// — that fallback exists for the HTML/PDF renderer's different
    /// contract (graceful inference), which conflicts with this
    /// display's "gate absent -> fail" requirement (#308 review, risk
    /// 1).
    NoGate,
    /// The file is a `MeasurementReport` whose integer `schema_version`
    /// this build does not read (#429) — refused before its body was
    /// decoded, so it says nothing about the payload kind. Carries the
    /// version found and the range this build reads, both straight from
    /// `ReportReadError::UnsupportedSchema`.
    UnsupportedSchema {
        found: u64,
        supported: RangeInclusive<u32>,
    },
    /// [`MeasurementReport::ir_stats`]'s `verdict` is `Failed` (#376):
    /// pre-impulse SNR too low, or non-finite, to trust the peak as a
    /// deconvolution result rather than noise-floor pickup. Carries the
    /// measured `pre_impulse_snr_db` so `header`/`detail` can show the
    /// same number a reader would see in `ac plot ir`'s text read-out,
    /// and the `IrVerdict::Failed` `reason` string verbatim — not a
    /// second, independently worded guess at it — so the non-finite case
    /// (which covers two different causes: no signal at all, or the
    /// guard band consuming the whole pre-region) reads identically here
    /// and in `ac-cli`'s `print_ir_report`. Both consumers read the one
    /// verdict `ir_stats` computes, so they cannot disagree about what
    /// counts as failed, or about why (#387 QA finding).
    LowPreImpulseSnr {
        pre_impulse_snr_db: f64,
        reason: String,
    },
}

/// Where to look when the pre-impulse gate refuses a capture — the sweep
/// first, then the capture chain; the same places `ac plot ir` names.
const LOW_SNR_CHECKS: &str =
    "sweep length, band, window, drive level, input gain, distance, room noise";

impl SweepIrFault {
    /// Header line — occupies the same panel-geometry slot the success
    /// frame's header does, so nothing jumps when a bad file replaces a
    /// good one.
    pub fn header(&self) -> String {
        match self {
            SweepIrFault::NotASweepDerivedIr => "IR — file open failed".to_string(),
            SweepIrFault::NoGate => "IR — sweep-derived     no gate on this report".to_string(),
            SweepIrFault::UnsupportedSchema { .. } => "Report not opened".to_string(),
            SweepIrFault::LowPreImpulseSnr {
                pre_impulse_snr_db,
                reason,
            } => {
                if pre_impulse_snr_db.is_finite() {
                    format!(
                        "IR — sweep-derived     pre-imp SNR {pre_impulse_snr_db:.1} dB, below \
                         {PRE_IMPULSE_SNR_MIN_DB:.1} dB threshold"
                    )
                } else {
                    // `reason` is `IrVerdict::Failed`'s own text — names
                    // the actual cause (no signal at all, vs. the guard
                    // band consuming the whole pre-region) instead of a
                    // single hardcoded "(silence)" that was wrong for one
                    // of the two (#387 QA finding).
                    format!("IR — sweep-derived     {reason}")
                }
            }
        }
    }

    /// Names what to check; never asserts a cause — same rule
    /// [`crate::fault::Fault::detail`] documents for `NO LOCK`. The
    /// loader cannot know whether a bad file is a live snapshot, a
    /// report from an ungated measurement, a stale format, a sweep
    /// configuration the threshold was not scored for, low drive level,
    /// input gain, distance, or room noise. The check list and the
    /// threshold's basis are the ones `ac plot ir` prints (#501).
    pub fn detail(&self) -> String {
        match self {
            SweepIrFault::NotASweepDerivedIr => {
                "not a MeasurementReport — check this is report JSON from a swept-sine \
                 transfer run, not a live snapshot"
                    .to_string()
            }
            SweepIrFault::NoGate => {
                "report has no gate bounds — check it came from a gated IR measurement, \
                 not an ungated sweep"
                    .to_string()
            }
            SweepIrFault::UnsupportedSchema { found, supported } => format!(
                "unsupported report schema v{found}\nthis version reads v{}–v{}",
                supported.start(),
                supported.end()
            ),
            SweepIrFault::LowPreImpulseSnr {
                pre_impulse_snr_db,
                reason,
            } => {
                if pre_impulse_snr_db.is_finite() {
                    format!(
                        "pre-impulse SNR {pre_impulse_snr_db:.1} dB below required \
                         {PRE_IMPULSE_SNR_MIN_DB:.1} dB ({PRE_IMPULSE_SNR_BASIS}) \
                         — check {LOW_SNR_CHECKS}"
                    )
                } else {
                    // Same `reason` text as `header()` above, not a
                    // hardcoded "(silence)" — see that arm's comment.
                    format!("{reason} — check {LOW_SNR_CHECKS}")
                }
            }
        }
    }
}

/// Everything the sweep-derived IR panel draws, with no numeric work
/// left for the renderer — mirrors [`crate::ir::IrScene`]'s contract.
#[derive(Debug, Clone, PartialEq)]
pub struct SweepIrScene {
    /// h(t) over the recorded gate window, as one polyline — never
    /// gapped, same reasoning as [`crate::ir::IrScene::trace`].
    pub trace: Trace,
    /// Time axis over the gate's own `[gate_start_s, gate_start_s +
    /// gate_length_s]` span (converted to ms) — never inferred, never
    /// zoomed, and never recomputed from the sample count: the
    /// recorded [`ac_core::measurement::report::GateParams`] is the
    /// whole of the range (#280's "store, don't re-derive" rule).
    pub time_axis: Axis,
    pub arrival: ArrivalMarker,
    /// `"IR — sweep-derived     gate ± 11.70 ms  ·  f_low 85 Hz  ·
    /// valid above f_low only"` — built per report (the gate and
    /// `f_low_hz` vary), unlike [`crate::ir::IR_HEADER`]'s fixed
    /// string.
    pub header: String,
}

impl SweepIrScene {
    /// Build the scene from a decoded `MeasurementReport`, or the
    /// [`SweepIrFault`] naming why it can't be shown.
    pub fn from_report(report: &MeasurementReport) -> Result<SweepIrScene, SweepIrFault> {
        let ir_payload = report.data.iter().find_map(|p| match &p.data {
            MeasurementData::ImpulseResponse {
                sample_rate_hz,
                linear_ir,
                ..
            } => Some((p, *sample_rate_hz, linear_ir)),
            _ => None,
        });
        let Some((payload, sample_rate_hz, linear_ir)) = ir_payload else {
            return Err(SweepIrFault::NotASweepDerivedIr);
        };
        if linear_ir.is_empty() || sample_rate_hz == 0 {
            return Err(SweepIrFault::NotASweepDerivedIr);
        }
        let gate = payload.gate.as_ref().ok_or(SweepIrFault::NoGate)?;

        // Gate confirmed present directly off the payload above, so
        // `ir_stats()` — which finds the same first `ImpulseResponse`
        // payload — takes its recorded-gate branch, never the legacy
        // rectangular-window fallback.
        let stats = report
            .ir_stats()
            .expect("gate confirmed present and linear_ir non-empty above");

        // A capture whose peak isn't trustworthy fails here, before any
        // trace or arrival geometry is built — same rule #376 applies to
        // the CLI text read-out (`ac-cli`'s `print_ir_report`).
        if let IrVerdict::Failed { reason } = &stats.verdict {
            return Err(SweepIrFault::LowPreImpulseSnr {
                pre_impulse_snr_db: stats.pre_impulse_snr_db,
                reason: reason.clone(),
            });
        }

        let stride = (linear_ir.len() / IR_MAX_SAMPLES).max(1);
        let samples: Vec<f64> = linear_ir.iter().step_by(stride).copied().collect();
        let dt_ms = 1000.0 / sample_rate_hz as f64 * stride as f64;
        // The recorded gate start, not a re-derived `-n/2` — see
        // `time_axis`'s own doc above.
        let t_origin_ms = gate.gate_start_s * 1000.0;
        let n = samples.len();
        let t_max_ms = t_origin_ms + (n.saturating_sub(1)) as f64 * dt_ms;

        let provenance = Provenance {
            channel_role: "meas".to_string(),
            source: Source::Snapshot,
            sr: sample_rate_hz,
        };

        // A degenerate span draws no trace and no axis rather than
        // fabricating a range — same defensive posture as
        // `crate::ir::IrScene::from_input`.
        let has_span = n > 1 && t_max_ms > t_origin_ms;

        let trace = if has_span {
            // Autoscaled to this frame's own peak — no calibrated
            // amplitude, per #308's "no amplitude axis units beyond
            // H(t)" rule. A peak of exactly 0.0 falls back to 1.0 so
            // every sample still maps to the mid-line.
            let peak = samples.iter().fold(0.0_f64, |m, &s| m.max(s.abs()));
            let peak = if peak > 0.0 { peak } else { 1.0 };
            let points: Vec<(f64, f64)> = samples
                .iter()
                .enumerate()
                .map(|(i, &s)| {
                    let t_ms = t_origin_ms + i as f64 * dt_ms;
                    let x = time_to_x(t_ms, t_origin_ms, t_max_ms);
                    let y = 0.5 + 0.5 * (s / peak);
                    (x, y)
                })
                .collect();
            Trace::single(points, provenance)
        } else {
            Trace {
                segments: Vec::new(),
                provenance,
            }
        };

        let time_axis = if has_span {
            time_axis(t_origin_ms, t_max_ms)
        } else {
            Axis { ticks: Vec::new() }
        };

        let arrival_ms = stats.arrival_s * 1000.0;
        // `stats.arrival_s` is a round-trip figure and still contains any
        // uncorrected interface latency (`IrStats::arrival_s`'s own doc).
        // `stats.flight_time_s` is the one already-checked τ subtraction
        // (#359): `Some` is the τ-corrected flight time, `None` prints the
        // raw round trip — named as such, so a withheld correction cannot
        // read as an applied one.
        let arrival = ArrivalMarker {
            position: if has_span {
                time_to_x(arrival_ms, t_origin_ms, t_max_ms)
            } else {
                0.5
            },
            text: arrival_marker_text(&stats),
        };

        // The recorded gate start, not `gate_window_s / 2`: the daemon's
        // own gate construction (`plot.rs`) truncates `gate_start_s` via
        // integer division on an odd-sample window while `gate_length_s`
        // stays untruncated, so the two can differ by up to half a
        // sample. `gate.gate_start_s` is the actual left bound; deriving
        // the header figure from it rather than re-halving the window
        // length is the same "store, don't re-derive" rule this module
        // already follows for the time axis above.
        let half_span_ms = -gate.gate_start_s * 1000.0;
        let header = format_sweep_ir_header(half_span_ms, stats.gate_f_low_hz);

        Ok(SweepIrScene {
            trace,
            time_axis,
            arrival,
            header,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ac_core::measurement::report::{
        GateParams, IntegrationParams, InterfaceLatency, MeasuredLatency, MeasurementMethod,
        MeasurementPayload, ProcessingChain, StimulusParams, MIN_SCHEMA_VERSION, SCHEMA_VERSION,
    };

    fn base_report() -> MeasurementReport {
        MeasurementReport {
            schema_version: SCHEMA_VERSION,
            ac_version: "0.1.0".into(),
            timestamp_utc: "2026-08-16T00:00:00Z".into(),
            backend: None,
            method: MeasurementMethod::SweptSine {
                f1_hz: 20.0,
                f2_hz: 20_000.0,
                duration_s: 1.0,
            },
            stimulus: StimulusParams {
                sample_rate_hz: 48_000,
                f_start_hz: 20.0,
                f_stop_hz: 20_000.0,
                level_dbfs: -6.0,
                n_points: 0,
            },
            integration: IntegrationParams {
                duration_s: 1.0,
                window: "farina-inverse".into(),
                n_averages: None,
            },
            calibration: None,
            position: None,
            interface_latency: None,
            reference_latency: None,
            reference_stored_latency: None,
            data: vec![],
            notes: None,
            processing_chain: ProcessingChain::default(),
        }
    }

    /// A 20-sample gated linear IR with a `0.001` noise floor everywhere
    /// except `peak_index` (`1.0`) and `peak_index + 1` (`-0.5`, the
    /// trough). 20 samples (not 5, as this fixture used before #376) is
    /// the minimum that clears `ir_stats`'s fixed 8-sample pre-impulse
    /// guard band with the peak still at the window centre — anything
    /// shorter always reads as `IrVerdict::Failed` (empty pre-impulse
    /// region), regardless of noise content, and would never reach the
    /// geometry this fixture exists to exercise. Peak-to-noise gives
    /// ~60 dB pre-impulse SNR, comfortably clear of the 18.0 dB
    /// threshold.
    fn ir20_with_peak(peak_index: usize) -> Vec<f64> {
        let mut ir = vec![0.001; 20];
        ir[peak_index] = 1.0;
        ir[peak_index + 1] = -0.5;
        ir
    }

    // dt_ms=250 at sr=4000 (1000/4000*1=0.25 -> ms step), gate window
    // 20 samples wide centred on the peak — round figures chosen the
    // same way `ir.rs`'s `locked_input` picks its fixture, so
    // hand-checking the span and the arrival position is easy.
    fn gated_report(gate: Option<GateParams>) -> MeasurementReport {
        let mut r = base_report();
        r.data.push(MeasurementPayload {
            data: MeasurementData::ImpulseResponse {
                sample_rate_hz: 4_000,
                f1_hz: 20.0,
                f2_hz: 20_000.0,
                duration_s: 1.0,
                linear_ir: ir20_with_peak(10),
                harmonics: vec![],
                noise_tail_start_s: None,
            },
            standard: vec![],
            gate,
        });
        r
    }

    fn locked_gate() -> GateParams {
        GateParams {
            gate_start_s: -0.0025, // -10 samples * 0.25 ms at 4 kHz (window centre)
            gate_length_s: 0.005,  // 20 samples * 0.25 ms
            window_kind: "rectangular".into(),
            f_low_hz: 800.0,
        }
    }

    // Round figures matching #308's UX mockup verbatim ("gate ± 11.70
    // ms  ·  f_low 85 Hz"), independent of the fixture's own sample
    // geometry — the header only reads `GateParams` directly, not the
    // sample count.
    fn header_gate() -> GateParams {
        GateParams {
            gate_start_s: -0.0117,
            gate_length_s: 0.0234,
            window_kind: "rectangular".into(),
            f_low_hz: 85.0,
        }
    }

    /// Sample rate of [`gated_report_with_delayed_peak`].
    const DELAYED_SR: u32 = 48_000;

    // The marker fixture: a single spike one sample after the window's
    // centre so `delay_samples = 1` and `arrival_s > 0` — the other
    // fixtures peak exactly on the zero-delay sample, which made
    // `Some(true)` and the correct τ-gated `delay_locked` value
    // indistinguishable (both print `"0.00 ms (0.00 m)"`). 48 kHz and 2048
    // samples, not the 4 kHz / 20-sample geometry fixture: #537's arrival is
    // picked above a 2 kHz corner, which needs the band to reach 4 kHz and
    // a pre-impulse region to measure the band-limited SNR in, so the
    // arrival here is band-limited and its cross-check `Agrees`.
    fn gated_report_with_delayed_peak() -> MeasurementReport {
        const LEN: usize = 2_048;
        let mut ir = vec![0.0; LEN];
        ir[LEN / 2 + 1] = 1.0;
        let span_s = LEN as f64 / DELAYED_SR as f64;
        let mut r = base_report();
        r.data.push(MeasurementPayload {
            data: MeasurementData::ImpulseResponse {
                sample_rate_hz: DELAYED_SR,
                f1_hz: 20.0,
                f2_hz: 20_000.0,
                duration_s: 1.0,
                linear_ir: ir,
                harmonics: vec![],
                noise_tail_start_s: None,
            },
            standard: vec![],
            gate: Some(GateParams {
                gate_start_s: -span_s / 2.0,
                gate_length_s: span_s,
                window_kind: "rectangular".into(),
                f_low_hz: 1.0 / span_s,
            }),
        });
        r
    }

    fn measured_tau(tau_s: f64) -> InterfaceLatency {
        InterfaceLatency::Measured(MeasuredLatency {
            tau_s,
            measured_at: "2026-08-16T00:00:00Z".into(),
            method: "farina_short_ess".into(),
            backend: "jack".into(),
            sample_rate_hz: 4_000,
            period_size: None,
            output_port: "out1".into(),
            input_port: "in1".into(),
            enumeration: Some(ac_core::shared::calibration::EnumerationCheck::Same),
            session_check: None,
            session_check_loopback: None,
        })
    }

    #[test]
    fn no_impulse_response_payload_is_not_a_sweep_derived_ir() {
        let r = base_report(); // data is empty
        assert_eq!(
            SweepIrScene::from_report(&r),
            Err(SweepIrFault::NotASweepDerivedIr)
        );
    }

    #[test]
    fn impulse_response_payload_with_no_gate_fails_with_no_gate() {
        let r = gated_report(None);
        assert_eq!(SweepIrScene::from_report(&r), Err(SweepIrFault::NoGate));
    }

    #[test]
    fn gate_absent_decision_does_not_go_through_the_legacy_fallback() {
        // A legacy (gate: None) report still has a working `ir_stats()`
        // via the rectangular-window fallback — proving the *fault*
        // decision reads `payload.gate` directly rather than reusing
        // that permissive path (#308 review, risk 1).
        let r = gated_report(None);
        assert!(
            r.ir_stats().is_some(),
            "ir_stats() should still succeed via its legacy fallback"
        );
        assert_eq!(SweepIrScene::from_report(&r), Err(SweepIrFault::NoGate));
    }

    #[test]
    fn a_gated_report_builds_a_scene_with_header_gate_and_f_low() {
        let r = gated_report(Some(header_gate()));
        let scene = SweepIrScene::from_report(&r).expect("gated report should build a scene");
        assert!(scene.header.contains("sweep-derived"));
        assert!(scene.header.contains("gate \u{b1} 11.70 ms"));
        assert!(scene.header.contains("f_low 85 Hz"));
        assert!(scene.header.contains("valid above f_low only"));
        assert_eq!(scene.trace.segments[0].len(), 20);
        assert_eq!(scene.trace.provenance.source, Source::Snapshot);
        assert_eq!(scene.trace.provenance.sr, 4_000);
    }

    // The magnitude peak (index 10, value 1.0) sits at `window_len/2`
    // (20/2=10), so `delay_samples` is 0 and the arrival lands on the
    // gate's own zero-delay reference sample. The trough at index 11
    // (-0.5) autoscales below the peak; the noise-floor samples (0.001)
    // autoscale to just above the trace's mid-line.
    #[test]
    fn trace_is_autoscaled_and_arrival_lands_on_the_zero_delay_sample() {
        let r = gated_report(Some(locked_gate()));
        let scene = SweepIrScene::from_report(&r).unwrap();
        let ys: Vec<f64> = scene.trace.segments[0].iter().map(|p| p.1).collect();
        assert_eq!(ys.len(), 20);
        assert_eq!(ys[10], 1.0, "peak autoscales to the top of the trace");
        assert_eq!(ys[11], 0.25, "trough at -0.5 autoscales below the peak");
        assert!(
            (ys[0] - 0.5005).abs() < 1e-9,
            "noise-floor sample sits just above the mid-line: {}",
            ys[0]
        );
        // `t_origin_ms` is the recorded gate start (-2.5 ms); `t_max_ms`
        // is the last of 20 samples at 0.25 ms/sample
        // (-2.5 + 19*0.25 = 2.25 ms) — the same `(n-1)*dt` endpoint
        // convention `crate::ir::IrScene` uses.
        let expected_x = time_to_x(0.0, -2.5, 2.25);
        assert!((scene.arrival.position - expected_x).abs() < 1e-9);
    }

    #[test]
    fn arrival_marker_prints_ms_only() {
        // `base_report()` sets `interface_latency: None` and no reference
        // at all, so this is the round-trip path (#359): named as such,
        // not the bare `"X.XX ms"` this used to print before the marker
        // could tell a withheld correction apart from an applied one. At
        // 4 kHz the band cannot reach an octave above #537's 2 kHz corner,
        // so the arrival is the broadband peak and the marker says so.
        let r = gated_report(Some(locked_gate()));
        let scene = SweepIrScene::from_report(&r).unwrap();
        let stats = r.ir_stats().unwrap();
        let want = format!(
            "broadband peak: {:.2} ms round trip, not band-limited",
            stats.arrival_s * 1000.0
        );
        assert_eq!(scene.arrival.text, want);
    }

    /// #359: the existing #283 test, now asserting the `round trip` wording
    /// — the marker names what it shows rather than reading identically to
    /// a τ-corrected flight time.
    #[test]
    fn arrival_distance_is_not_shown_without_a_measured_interface_latency() {
        // Nonzero delay (see `gated_report_with_delayed_peak`'s doc) so a
        // τ-corrected figure derived from the uncorrected round trip would
        // be visibly different too, not masked by a delay of exactly 0.
        let r = gated_report_with_delayed_peak(); // interface_latency: None
        let scene = SweepIrScene::from_report(&r).unwrap();
        let stats = r.ir_stats().unwrap();
        assert_eq!(
            scene.arrival.text,
            format!(
                "peak above 2 kHz: {:.2} ms round trip",
                stats.arrival_s * 1000.0
            ),
            "no measured \u{3c4} on this report — arrival text must be the raw round trip: {}",
            scene.arrival.text
        );
    }

    // `arrival_distance_is_tau_corrected_when_interface_latency_is_measured`
    // moved down into `ac_core::measurement::report::ir_stats` (#359): the
    // τ subtraction it tested now lives there, gated by `arrival_check`, so
    // this module has nothing left to prove about the number itself — only
    // that the marker names it correctly (below).

    /// #359: a detected period shift must withhold the τ-corrected figure
    /// and name the shift on the round trip instead — never silently fall
    /// back to printing the number as though it were still `"flight"`.
    #[test]
    fn arrival_marker_names_a_detected_period_shift_not_flight() {
        use ac_core::measurement::report::{
            InterfaceLatency, MeasuredLatency, MeasuredReferenceLatency, ReferenceLatency,
        };

        let mut r = gated_report_with_delayed_peak();
        r.interface_latency = Some(measured_tau(0.0001));
        let sr = DELAYED_SR;
        let period = 64u32;
        let stored_tau_s = 0.002;
        let same_capture_tau_s = stored_tau_s + period as f64 / sr as f64;
        r.reference_latency = Some(ReferenceLatency::Measured(MeasuredReferenceLatency {
            tau_s: same_capture_tau_s,
            pre_impulse_snr_db: Some(60.0),
            pre_impulse_snr_floor_db: Some(63.0),
            method: "farina_same_capture_reference_v1".into(),
            output_port: "ref_out".into(),
            input_port: "ref_in".into(),
        }));
        r.reference_stored_latency = Some(InterfaceLatency::Measured(MeasuredLatency {
            tau_s: stored_tau_s,
            measured_at: "2026-08-15T00:00:00Z".into(),
            method: "farina_short_ess".into(),
            backend: "fake".into(),
            sample_rate_hz: sr,
            period_size: Some(period),
            output_port: "ref_out".into(),
            input_port: "ref_in".into(),
            enumeration: Some(ac_core::shared::calibration::EnumerationCheck::Same),
            session_check: None,
            session_check_loopback: None,
        }));
        let scene = SweepIrScene::from_report(&r).unwrap();
        let stats = r.ir_stats().unwrap();
        assert!(matches!(
            stats.arrival_check,
            ac_core::measurement::report::ArrivalCheck::PeriodShift(_)
        ));
        assert_eq!(
            scene.arrival.text,
            format!(
                "peak above 2 kHz: {:.2} ms round trip, 1-period shift",
                stats.arrival_s * 1000.0
            ),
            "a period shift must name itself on the round trip, not read as flight: {}",
            scene.arrival.text
        );
        assert!(!scene.arrival.text.contains("flight"));
    }

    /// #359: the ordinary agree case must read plain "flight", not silently
    /// pick up a suffix meant for a different state.
    #[test]
    fn arrival_marker_names_flight_when_the_check_agrees() {
        use ac_core::measurement::report::{
            InterfaceLatency, MeasuredLatency, MeasuredReferenceLatency, ReferenceLatency,
        };

        let mut r = gated_report_with_delayed_peak();
        r.interface_latency = Some(measured_tau(0.0001)); // 0.1 ms
        let sr = DELAYED_SR;
        let tau_s = 0.002;
        r.reference_latency = Some(ReferenceLatency::Measured(MeasuredReferenceLatency {
            tau_s,
            pre_impulse_snr_db: Some(60.0),
            pre_impulse_snr_floor_db: Some(63.0),
            method: "farina_same_capture_reference_v1".into(),
            output_port: "ref_out".into(),
            input_port: "ref_in".into(),
        }));
        r.reference_stored_latency = Some(InterfaceLatency::Measured(MeasuredLatency {
            tau_s, // identical -> Agree
            measured_at: "2026-08-15T00:00:00Z".into(),
            method: "farina_short_ess".into(),
            backend: "fake".into(),
            sample_rate_hz: sr,
            period_size: Some(64),
            output_port: "ref_out".into(),
            input_port: "ref_in".into(),
            enumeration: Some(ac_core::shared::calibration::EnumerationCheck::Same),
            session_check: None,
            session_check_loopback: None,
        }));
        let scene = SweepIrScene::from_report(&r).unwrap();
        let stats = r.ir_stats().unwrap();
        assert_eq!(
            stats.arrival_check,
            ac_core::measurement::report::ArrivalCheck::Agree
        );
        let flight_ms = stats
            .flight_time_s
            .expect("Agree must produce a flight time")
            * 1000.0;
        assert_eq!(
            scene.arrival.text,
            format!("peak above 2 kHz: {flight_ms:.2} ms flight, distance not given")
        );
    }

    /// #359: a flight time computed alongside `Unchecked` must say so — the
    /// same operator-misreading risk Codex's round-2 finding fixed for the
    /// CLI, unaddressed here.
    #[test]
    fn arrival_marker_names_flight_ref_unchecked_when_the_check_never_ran() {
        let mut r = gated_report_with_delayed_peak();
        r.interface_latency = Some(measured_tau(0.0001));
        // No reference at all configured -> Unchecked, flight_time_s still Some.
        let scene = SweepIrScene::from_report(&r).unwrap();
        let stats = r.ir_stats().unwrap();
        assert!(matches!(
            stats.arrival_check,
            ac_core::measurement::report::ArrivalCheck::Unchecked { .. }
        ));
        let flight_ms = stats
            .flight_time_s
            .expect("Unchecked must still produce a flight time")
            * 1000.0;
        assert_eq!(
            scene.arrival.text,
            format!(
                "peak above 2 kHz: {flight_ms:.2} ms flight, ref unchecked, distance not given"
            )
        );
    }

    /// #461: a flight time over a stored τ that is not `same` enumeration
    /// carries `latency unverified`, before `ref unchecked`. A v9 report (no
    /// check at all) is flagged too: nobody checked it (UX revision 2).
    #[test]
    fn arrival_marker_flags_a_flight_time_over_an_unverified_latency() {
        use ac_core::measurement::report::{InterfaceLatency, MeasuredLatency};
        use ac_core::shared::calibration::EnumerationCheck;

        let with_check = |check: Option<EnumerationCheck>| {
            let mut r = gated_report_with_delayed_peak();
            let InterfaceLatency::Measured(m) = measured_tau(0.0001) else {
                unreachable!("measured_tau builds a measured latency")
            };
            r.interface_latency = Some(InterfaceLatency::Measured(MeasuredLatency {
                enumeration: check,
                ..m
            }));
            let stats = r.ir_stats().unwrap();
            let flight_ms = stats.flight_time_s.expect("flagged, not withheld") * 1000.0;
            (
                SweepIrScene::from_report(&r).unwrap().arrival.text,
                flight_ms,
            )
        };

        let (text, ms) = with_check(Some(EnumerationCheck::Same));
        assert_eq!(
            text,
            format!("peak above 2 kHz: {ms:.2} ms flight, ref unchecked, distance not given")
        );

        for check in [
            Some(EnumerationCheck::Crossed {
                boundary: "host rebooted".into(),
                since: Some("2026-09-16T13:41:52Z".into()),
            }),
            Some(EnumerationCheck::NotRecorded),
            Some(EnumerationCheck::NotObservable {
                reason: "cpal backend has no enumeration probe".into(),
            }),
            None,
        ] {
            let (text, ms) = with_check(check.clone());
            assert_eq!(
                text,
                format!(
                    "peak above 2 kHz: {ms:.2} ms flight, latency unverified, ref unchecked, \
                     distance not given"
                ),
                "{check:?}"
            );
            assert!(!text.contains("rebooted"), "the boundary stays in the CLI");
        }
    }

    /// #466 (R6-4): a stored τ the session check refused never reaches the
    /// marker. `ir_stats` withholds the flight time, so the marker falls to
    /// the round trip.
    #[test]
    fn arrival_marker_never_uses_a_refused_latency() {
        use ac_core::measurement::report::{InterfaceLatency, MeasuredLatency};
        use ac_core::shared::calibration::session::{CheckSource, Evidence, VerdictUnit};
        use ac_core::shared::calibration::LayerVerdict;

        let mut r = gated_report_with_delayed_peak();
        let InterfaceLatency::Measured(m) = measured_tau(0.0001) else {
            unreachable!("measured_tau builds a measured latency")
        };
        r.interface_latency = Some(InterfaceLatency::Measured(MeasuredLatency {
            session_check: Some(LayerVerdict::Refused {
                evidence: Evidence {
                    measured: 1743.0,
                    stored: 1711.0,
                    delta: 32.0,
                    tolerance: 0.0,
                    unit: VerdictUnit::Samples,
                    stored_at: "2026-08-16T00:00:00Z".into(),
                    checked_at: "2026-08-16T01:00:00Z".into(),
                    source: CheckSource::Explicit,
                },
                via: Some("out1_in1".into()),
                delta_bound: None,
            }),
            ..m
        }));
        let text = SweepIrScene::from_report(&r).unwrap().arrival.text;
        assert!(text.contains("round trip"), "{text}");
        assert!(!text.contains("flight"), "{text}");
    }

    /// #461: with the reference check agreeing, the flag is the only suffix.
    #[test]
    fn arrival_marker_flags_an_unverified_latency_when_the_check_agrees() {
        use ac_core::measurement::report::{
            InterfaceLatency, MeasuredLatency, MeasuredReferenceLatency, ReferenceLatency,
        };
        use ac_core::shared::calibration::EnumerationCheck;

        let mut r = gated_report_with_delayed_peak();
        r.interface_latency = Some(InterfaceLatency::Measured(MeasuredLatency {
            tau_s: 0.0001,
            measured_at: "2026-08-16T00:00:00Z".into(),
            method: "farina_short_ess".into(),
            backend: "jack".into(),
            sample_rate_hz: 4_000,
            period_size: None,
            output_port: "out1".into(),
            input_port: "in1".into(),
            enumeration: Some(EnumerationCheck::NotRecorded),
            session_check: None,
            session_check_loopback: None,
        }));
        let tau_s = 0.002;
        r.reference_latency = Some(ReferenceLatency::Measured(MeasuredReferenceLatency {
            tau_s,
            pre_impulse_snr_db: Some(60.0),
            pre_impulse_snr_floor_db: Some(63.0),
            method: "farina_same_capture_reference_v1".into(),
            output_port: "ref_out".into(),
            input_port: "ref_in".into(),
        }));
        r.reference_stored_latency = Some(InterfaceLatency::Measured(MeasuredLatency {
            tau_s,
            measured_at: "2026-08-15T00:00:00Z".into(),
            method: "farina_short_ess".into(),
            backend: "fake".into(),
            sample_rate_hz: 4_000,
            period_size: Some(64),
            output_port: "ref_out".into(),
            input_port: "ref_in".into(),
            enumeration: Some(EnumerationCheck::Same),
            session_check: None,
            session_check_loopback: None,
        }));
        let stats = r.ir_stats().unwrap();
        assert_eq!(
            stats.arrival_check,
            ac_core::measurement::report::ArrivalCheck::Agree
        );
        let flight_ms = stats.flight_time_s.unwrap() * 1000.0;
        assert_eq!(
            SweepIrScene::from_report(&r).unwrap().arrival.text,
            format!("peak above 2 kHz: {flight_ms:.2} ms flight, latency unverified, distance not given")
        );
    }

    /// #359: a non-period-multiple disagreement must name itself on the
    /// round trip as `ref Δ`, distinctly from a period shift.
    #[test]
    fn arrival_marker_names_a_mismatch_not_a_period_shift() {
        use ac_core::measurement::report::{
            InterfaceLatency, MeasuredLatency, MeasuredReferenceLatency, ReferenceLatency,
        };

        let mut r = gated_report_with_delayed_peak();
        let sr = DELAYED_SR;
        let period = 64u32;
        let stored_tau_s = 0.002;
        // one sample off an exact period -> Mismatch, not PeriodShift
        let same_capture_tau_s = stored_tau_s + (period as f64 + 1.0) / sr as f64;
        r.reference_latency = Some(ReferenceLatency::Measured(MeasuredReferenceLatency {
            tau_s: same_capture_tau_s,
            pre_impulse_snr_db: Some(60.0),
            pre_impulse_snr_floor_db: Some(63.0),
            method: "farina_same_capture_reference_v1".into(),
            output_port: "ref_out".into(),
            input_port: "ref_in".into(),
        }));
        r.reference_stored_latency = Some(InterfaceLatency::Measured(MeasuredLatency {
            tau_s: stored_tau_s,
            measured_at: "2026-08-15T00:00:00Z".into(),
            method: "farina_short_ess".into(),
            backend: "fake".into(),
            sample_rate_hz: sr,
            period_size: Some(period),
            output_port: "ref_out".into(),
            input_port: "ref_in".into(),
            enumeration: Some(ac_core::shared::calibration::EnumerationCheck::Same),
            session_check: None,
            session_check_loopback: None,
        }));
        let scene = SweepIrScene::from_report(&r).unwrap();
        let stats = r.ir_stats().unwrap();
        use ac_core::measurement::report::ArrivalCheck;
        let d = match &stats.arrival_check {
            ArrivalCheck::Mismatch(d) => d,
            other => panic!("expected Mismatch, got {other:?}"),
        };
        assert_eq!(
            scene.arrival.text,
            format!(
                "peak above 2 kHz: {:.2} ms round trip, ref \u{394} {:+} samples",
                stats.arrival_s * 1000.0,
                d.delta_samples,
            )
        );
        assert!(!scene.arrival.text.contains("flight"));
    }

    /// #346 UX revision 4, #537 UX: the marker names the rule that produced
    /// the arrival — `peak above 2 kHz:` — on every onset standing,
    /// including `Unscored`, where the rejected revision printed `onset`;
    /// `broadband peak:` when the arrival is the broadband peak.
    #[test]
    fn arrival_marker_names_the_rule_that_produced_the_arrival() {
        use ac_core::measurement::report::OnsetStanding;
        let r = gated_report_with_delayed_peak();
        let mut stats = r.ir_stats().unwrap();
        assert_eq!(
            stats.arrival_source,
            ArrivalSource::BandLimitedPeak { corner_hz: 2000.0 }
        );
        let value = arrival_marker_value(&stats);
        for standing in [OnsetStanding::NoCausalBound, OnsetStanding::Unscored] {
            stats.onset_standing = standing;
            assert_eq!(
                arrival_marker_text(&stats),
                format!("peak above 2 kHz: {value}"),
                "{standing:?}"
            );
        }
        stats.arrival_source = ArrivalSource::Peak;
        assert_eq!(
            arrival_marker_text(&stats),
            format!("broadband peak: {value}")
        );
    }

    /// A [`DistanceWindow`] at `distance_m`, default c.
    fn window(distance_m: f64) -> ac_core::measurement::report::DistanceWindow {
        ac_core::measurement::report::DistanceWindow::new(distance_m, None)
    }

    /// #537 UX revision 3's marker table, one row per standing, on the rig
    /// case's numbers (96 kHz). The cross-check suffix comes first, the
    /// #359/#461 suffixes next, the distance suffix last.
    #[test]
    fn arrival_marker_names_the_cross_check_standing() {
        let r = gated_report_with_delayed_peak();
        let base = r.ir_stats().unwrap();
        let consistent = DistanceCheck::Consistent {
            window: window(2.0),
            excess_s: 0.000377,
        };
        let with = |cc: ArrivalCrossCheck,
                    distance: DistanceCheck,
                    flight_ms: Option<f64>,
                    arrival_ms: f64| {
            let mut s = base.clone();
            s.sample_rate_hz = 96_000;
            s.arrival_cross_check = cc;
            s.distance_check = distance;
            s.flight_time_s = flight_ms.map(|ms| ms / 1000.0);
            s.arrival_s = arrival_ms / 1000.0;
            s.arrival_check = ArrivalCheck::Agree;
            s.interface_latency_enumeration =
                Some(ac_core::shared::calibration::EnumerationCheck::Same);
            s
        };
        let agrees = ArrivalCrossCheck::Agrees { gap: 128 };
        let cases = [
            (
                with(agrees.clone(), consistent.clone(), Some(6.208), 24.031),
                "peak above 2 kHz: 6.21 ms flight",
            ),
            (
                with(agrees.clone(), DistanceCheck::NotGiven, Some(0.0), 17.823),
                "peak above 2 kHz: 0.00 ms flight, distance not given",
            ),
            (
                with(
                    agrees.clone(),
                    DistanceCheck::TooLate {
                        window: window(2.0),
                        excess_s: 0.005377,
                    },
                    None,
                    29.031,
                ),
                "peak above 2 kHz: 29.03 ms round trip, later than 2 m allows",
            ),
            (
                with(
                    agrees.clone(),
                    DistanceCheck::TooEarly {
                        window: window(2.0),
                        excess_s: -0.000498,
                    },
                    None,
                    23.156,
                ),
                "peak above 2 kHz: 23.16 ms round trip, earlier than 2 m allows",
            ),
            (
                with(
                    ArrivalCrossCheck::BroadbandLater { gap: 1_440 },
                    consistent.clone(),
                    Some(6.208),
                    24.031,
                ),
                "peak above 2 kHz: 6.21 ms flight, broadband \u{394} +15.00 ms",
            ),
            (
                with(
                    ArrivalCrossCheck::ArrivalAmbiguous {
                        margin_db: 0.93,
                        offset: -17,
                    },
                    consistent.clone(),
                    None,
                    24.219,
                ),
                "peak above 2 kHz: 24.22 ms round trip, second lobe -0.9 dB",
            ),
            (
                with(
                    ArrivalCrossCheck::BandLimitedSnrLow { snr_db: 31.2 },
                    consistent.clone(),
                    None,
                    22.427,
                ),
                "peak above 2 kHz: 22.43 ms round trip, arrival SNR 31.2 dB",
            ),
            (
                with(
                    ArrivalCrossCheck::EarlierComparable {
                        index: 0,
                        level_db: -14.2,
                    },
                    // Also too late: the cross-check is the first reason,
                    // so the marker names it alone.
                    DistanceCheck::TooLate {
                        window: window(2.0),
                        excess_s: 0.005,
                    },
                    None,
                    24.031,
                ),
                "peak above 2 kHz: 24.03 ms round trip, earlier peak -14.2 dB",
            ),
            (
                with(
                    ArrivalCrossCheck::BroadbandEarlier { gap: -410 },
                    consistent.clone(),
                    None,
                    20.17,
                ),
                "peak above 2 kHz: 20.17 ms round trip, broadband \u{394} -4.27 ms",
            ),
        ];
        for (stats, want) in cases {
            assert_eq!(arrival_marker_text(&stats), want);
        }
        let mut unavailable = with(
            ArrivalCrossCheck::BandLimitUnavailable {
                band_top_hz: 2_000.0,
                required_hz: 4_000.0,
            },
            DistanceCheck::TooLate {
                window: window(2.0),
                excess_s: 0.0164,
            },
            None,
            40.083,
        );
        unavailable.arrival_source = ArrivalSource::Peak;
        assert_eq!(
            arrival_marker_text(&unavailable),
            "broadband peak: 40.08 ms round trip, not band-limited"
        );
        // A typed 0 m is never read as "not given".
        let zero = with(
            agrees.clone(),
            DistanceCheck::NotPositive { distance_m: 0.0 },
            Some(0.0),
            17.823,
        );
        assert_eq!(
            arrival_marker_text(&zero),
            "peak above 2 kHz: 0.00 ms flight, distance not checked"
        );
        // Order: cross-check, then #359/#461, then distance.
        let mut unchecked = with(
            ArrivalCrossCheck::BroadbandLater { gap: 1_440 },
            DistanceCheck::NotGiven,
            Some(6.208),
            24.031,
        );
        unchecked.arrival_check = ArrivalCheck::Unchecked {
            reason: String::new(),
        };
        assert_eq!(
            arrival_marker_text(&unchecked),
            "peak above 2 kHz: 6.21 ms flight, broadband \u{394} +15.00 ms, ref unchecked, \
             distance not given"
        );
    }

    #[test]
    fn header_gate_span_reads_the_recorded_gate_start_not_half_the_window() {
        // An asymmetric gate — `gate_length_s` is not `-2 * gate_start_s`
        // — the way the daemon's own integer-division truncation of
        // `gate_start_s` can produce (`plot.rs`'s `linear_gate_len / 2`).
        // The header must reflect the recorded left bound, not
        // `gate_window_s / 2`, which would silently disagree with it.
        let gate = GateParams {
            gate_start_s: -0.010,
            gate_length_s: 0.0234, // half would be 0.0117, not 0.010
            window_kind: "rectangular".into(),
            f_low_hz: 85.0,
        };
        let r = gated_report(Some(gate));
        let scene = SweepIrScene::from_report(&r).unwrap();
        assert!(
            scene.header.contains("gate \u{b1} 10.00 ms"),
            "expected the recorded gate_start_s (10.00 ms), got: {}",
            scene.header
        );
    }

    // A single-sample "IR" has no pre-impulse region `ir_stats` can
    // measure at all (its 8-sample guard band always empties it for a
    // window this small, regardless of content), so #376's low-SNR gate
    // now catches this case before the degenerate-span drawing code
    // (`has_span = n > 1`) is ever reached — a stronger and more honest
    // refusal than silently drawing an empty trace.
    #[test]
    fn degenerate_single_sample_span_fails_the_low_snr_gate() {
        let mut r = base_report();
        r.data.push(MeasurementPayload {
            data: MeasurementData::ImpulseResponse {
                sample_rate_hz: 4_000,
                f1_hz: 20.0,
                f2_hz: 20_000.0,
                duration_s: 1.0,
                linear_ir: vec![1.0],
                harmonics: vec![],
                noise_tail_start_s: None,
            },
            standard: vec![],
            gate: Some(locked_gate()),
        });
        assert_eq!(
            SweepIrScene::from_report(&r),
            Err(SweepIrFault::LowPreImpulseSnr {
                pre_impulse_snr_db: f64::INFINITY,
                reason: "no measurable pre-impulse floor (peak too close to \
                         the start of the gated window)"
                    .to_string(),
            })
        );
    }

    /// A gated report whose pre-impulse SNR is below the #376 threshold
    /// fails with `LowPreImpulseSnr`, before any trace or arrival is
    /// built. 20 samples, not 5: `ir_stats`'s guard band is
    /// `(window_len / 32).max(8) = 8` for any window under 256 samples, so
    /// a peak inside the first 8 samples empties `pre_region` and lands
    /// in the *non-finite*-SNR failure path instead of this one — a
    /// 5-sample fixture with the peak at index 2 did exactly that (#387
    /// QA test-coverage gap). Peak at index 10 leaves a 2-sample
    /// pre-region of 0.2 against a 1.0 peak -> ~14 dB, below the 18.0 dB
    /// threshold, and finite.
    fn gated_report_with_low_snr() -> MeasurementReport {
        let mut r = base_report();
        let mut linear_ir = vec![0.2; 20];
        linear_ir[10] = 1.0;
        r.data.push(MeasurementPayload {
            data: MeasurementData::ImpulseResponse {
                sample_rate_hz: 4_000,
                f1_hz: 20.0,
                f2_hz: 20_000.0,
                duration_s: 1.0,
                linear_ir,
                harmonics: vec![],
                noise_tail_start_s: None,
            },
            standard: vec![],
            gate: Some(locked_gate()),
        });
        r
    }

    #[test]
    fn low_pre_impulse_snr_fails_before_building_a_scene() {
        let r = gated_report_with_low_snr();
        let stats = r.ir_stats().unwrap();
        // The fixture must exercise the *finite*-below-threshold branch,
        // not the non-finite one (#387 QA test-coverage gap) — otherwise
        // the assertion below is tautological against whatever `ir_stats`
        // happens to compute, rather than checking this scene path
        // against a known value.
        assert!(
            stats.pre_impulse_snr_db.is_finite(),
            "fixture must exercise the finite branch: {stats:?}"
        );
        assert!(
            (stats.pre_impulse_snr_db - 13.98).abs() < 0.5,
            "pre_impulse_snr_db = {}",
            stats.pre_impulse_snr_db
        );
        let ac_core::measurement::report::IrVerdict::Failed { reason } = &stats.verdict else {
            panic!("fixture must exercise the #376 failure path: {stats:?}");
        };
        assert_eq!(
            SweepIrScene::from_report(&r),
            Err(SweepIrFault::LowPreImpulseSnr {
                pre_impulse_snr_db: stats.pre_impulse_snr_db,
                reason: reason.clone(),
            })
        );
    }

    #[test]
    fn low_pre_impulse_snr_header_and_detail_carry_the_measured_and_required_values() {
        let fault = SweepIrFault::LowPreImpulseSnr {
            pre_impulse_snr_db: 9.7,
            reason: "pre-impulse SNR below threshold".to_string(),
        };
        assert!(fault.header().contains("9.7 dB"));
        assert!(fault.header().contains("18.0 dB threshold"));
        assert!(fault.detail().contains("9.7 dB"));
        assert!(fault.detail().contains("required 18.0 dB"));
        // #501 UX: the exact GUI form of the CLI's basis row and check rows.
        assert_eq!(
            fault.detail(),
            "pre-impulse SNR 9.7 dB below required 18.0 dB (fixed threshold, scored for \
             the default sweep only) — check sweep length, band, window, drive level, \
             input gain, distance, room noise"
        );
        assert!(!fault.detail().contains("mic gain"));
    }

    /// The non-finite branch has two distinguishable causes — no signal
    /// captured at all, vs. the guard band consuming the whole
    /// pre-region — and `header`/`detail` must name each by its actual
    /// `IrVerdict::Failed` `reason`, not a single hardcoded "(silence)"
    /// that was wrong for the guard-band case (#387 QA finding: this
    /// test previously asserted the wrong label and could not have
    /// caught that).
    #[test]
    fn low_pre_impulse_snr_non_finite_names_the_actual_reason_not_a_fixed_label() {
        let no_signal = SweepIrFault::LowPreImpulseSnr {
            pre_impulse_snr_db: f64::INFINITY,
            reason: "no signal captured (linear IR is all zero)".to_string(),
        };
        let guard_band = SweepIrFault::LowPreImpulseSnr {
            pre_impulse_snr_db: f64::INFINITY,
            reason: "no measurable pre-impulse floor (peak too close to \
                     the start of the gated window)"
                .to_string(),
        };
        assert!(no_signal.header().contains("no signal captured"));
        assert!(guard_band
            .header()
            .contains("peak too close to the start of the gated window"));
        assert!(no_signal.detail().contains("no signal captured"));
        assert!(guard_band
            .detail()
            .contains("peak too close to the start of the gated window"));
        assert_eq!(
            no_signal.detail(),
            "no signal captured (linear IR is all zero) — check sweep length, band, \
             window, drive level, input gain, distance, room noise"
        );
        assert!(guard_band.detail().ends_with(
            " — check sweep length, band, window, drive level, input gain, distance, room noise"
        ));
        // The two causes must not collapse into one string, and neither
        // may claim silence — the peak was measurable in both cases.
        assert_ne!(no_signal.header(), guard_band.header());
        assert_ne!(no_signal.detail(), guard_band.detail());
        assert!(!no_signal.detail().contains("silence"));
        assert!(!guard_band.detail().contains("silence"));
    }

    #[test]
    fn fault_headers_and_details_name_what_to_check_not_a_cause() {
        assert_eq!(
            SweepIrFault::NotASweepDerivedIr.header(),
            "IR — file open failed"
        );
        assert!(SweepIrFault::NotASweepDerivedIr
            .detail()
            .contains("not a MeasurementReport"));
        assert!(SweepIrFault::NoGate.header().contains("no gate"));
        assert!(SweepIrFault::NoGate.detail().contains("gate bounds"));
        // Neither string asserts *why* the file is wrong, only what to
        // check — same rule as `Fault::detail`'s doc.
        for detail in [
            SweepIrFault::NotASweepDerivedIr.detail(),
            SweepIrFault::NoGate.detail(),
        ] {
            assert!(detail.contains("check"));
        }
        // #429: the schema refusal states evidence (found vs. readable
        // range), not a cause, and its digits come from the constants.
        let unsupported = unsupported_schema_fault();
        assert_eq!(unsupported.header(), "Report not opened");
        assert_eq!(
            unsupported.detail(),
            format!(
                "unsupported report schema v999\nthis version reads v{MIN_SCHEMA_VERSION}–v{SCHEMA_VERSION}"
            )
        );
    }

    fn unsupported_schema_fault() -> SweepIrFault {
        SweepIrFault::UnsupportedSchema {
            found: 999,
            supported: MIN_SCHEMA_VERSION..=SCHEMA_VERSION,
        }
    }

    #[test]
    fn the_two_failure_modes_are_distinct_strings() {
        assert_ne!(
            SweepIrFault::NotASweepDerivedIr.header(),
            SweepIrFault::NoGate.header()
        );
        assert_ne!(
            SweepIrFault::NotASweepDerivedIr.detail(),
            SweepIrFault::NoGate.detail()
        );
        // #429: a schema refusal is not evidence about payload kind, so it
        // must not read as "not a sweep-derived IR".
        let unsupported = unsupported_schema_fault();
        assert_ne!(
            unsupported.header(),
            SweepIrFault::NotASweepDerivedIr.header()
        );
        assert_ne!(
            unsupported.detail(),
            SweepIrFault::NotASweepDerivedIr.detail()
        );
    }
}
