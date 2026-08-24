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

use ac_core::measurement::report::{
    ArrivalDistance, IrVerdict, MeasurementData, MeasurementReport, PRE_IMPULSE_SNR_MIN_DB,
};

use crate::ir::ArrivalMarker;
use crate::readout::format_sweep_ir_header;
use crate::scene::{Provenance, Source, Trace};
use crate::ticks::{time_axis, time_to_x, Axis};
use crate::transfer::{format_delay_readout_plain, SPEED_OF_SOUND_DEFAULT_M_S};

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

impl SweepIrFault {
    /// Header line — occupies the same panel-geometry slot the success
    /// frame's header does, so nothing jumps when a bad file replaces a
    /// good one.
    pub fn header(&self) -> String {
        match self {
            SweepIrFault::NotASweepDerivedIr => "IR — file open failed".to_string(),
            SweepIrFault::NoGate => "IR — sweep-derived     no gate on this report".to_string(),
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
    /// report from an ungated measurement, a stale format, low drive
    /// level, mic gain, distance, or room noise.
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
            SweepIrFault::LowPreImpulseSnr {
                pre_impulse_snr_db,
                reason,
            } => {
                if pre_impulse_snr_db.is_finite() {
                    format!(
                        "pre-impulse SNR {pre_impulse_snr_db:.1} dB below required \
                         {PRE_IMPULSE_SNR_MIN_DB:.1} dB — check drive level, mic gain, \
                         distance, room noise"
                    )
                } else {
                    // Same `reason` text as `header()` above, not a
                    // hardcoded "(silence)" — see that arm's comment.
                    format!("{reason} — check drive level, mic gain, distance, room noise")
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
        // uncorrected interface latency (`IrStats::arrival_s`'s own doc);
        // it must not be converted to a distance without a calibrated τ
        // (#283, `ArrivalDistance`'s doc: "no third case that returns an
        // uncorrected arrival as if it were a distance"). Route through
        // `MeasurementReport::ir_arrival_distance()` — the one place that
        // subtraction is allowed to happen — rather than hand-rolling the
        // `delay_locked` argument: only a `Known` result (a measured τ on
        // this report) both unlocks metres *and* supplies the
        // τ-corrected delay to print them from; `Unavailable` (the common
        // case — no τ measured with this run) prints ms only, same as the
        // live panel's own `delay_locked: Some(false)` path.
        let (readout_delay_ms, delay_locked, speed_of_sound_m_s) =
            match report.ir_arrival_distance() {
                ArrivalDistance::Known {
                    tau_s,
                    speed_of_sound_m_s,
                    ..
                } => (
                    (stats.arrival_s - tau_s) * 1000.0,
                    Some(true),
                    speed_of_sound_m_s,
                ),
                ArrivalDistance::Unavailable { .. } => {
                    (arrival_ms, Some(false), SPEED_OF_SOUND_DEFAULT_M_S)
                }
            };
        let arrival = ArrivalMarker {
            position: if has_span {
                time_to_x(arrival_ms, t_origin_ms, t_max_ms)
            } else {
                0.5
            },
            // `format_delay_readout_plain`'s own `delay_ms >= 0.0` guard
            // means a peak that lands before the gate's zero-delay
            // reference (or, in the `Known` branch, before τ) still prints
            // ms-only rather than fabricating a negative distance.
            text: format_delay_readout_plain(readout_delay_ms, delay_locked, speed_of_sound_m_s),
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
        MeasurementPayload, ProcessingChain, StimulusParams, SCHEMA_VERSION,
    };

    fn base_report() -> MeasurementReport {
        MeasurementReport {
            schema_version: SCHEMA_VERSION,
            ac_version: "0.1.0".into(),
            timestamp_utc: "2026-08-16T00:00:00Z".into(),
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

    // Same 20-sample gate as `locked_gate`, but the peak sits one sample
    // after the window's centre (index 11, not index 10) so
    // `delay_samples = 1` and `arrival_s > 0` — every other fixture in
    // this module peaks exactly on the zero-delay sample, which made
    // `Some(true)` and the correct τ-gated `delay_locked` value
    // indistinguishable (both print `"0.00 ms (0.00 m)"`).
    fn gated_report_with_delayed_peak() -> MeasurementReport {
        let mut r = base_report();
        r.data.push(MeasurementPayload {
            data: MeasurementData::ImpulseResponse {
                sample_rate_hz: 4_000,
                f1_hz: 20.0,
                f2_hz: 20_000.0,
                duration_s: 1.0,
                linear_ir: ir20_with_peak(11),
                harmonics: vec![],
                noise_tail_start_s: None,
            },
            standard: vec![],
            gate: Some(locked_gate()),
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
    fn arrival_marker_reuses_format_delay_readout_verbatim() {
        // `base_report()` sets `interface_latency: None`, so this is the
        // no-τ path: `delay_locked` must gate off `Some(false)`, not the
        // unconditional `Some(true)` this test previously pinned.
        let r = gated_report(Some(locked_gate()));
        let scene = SweepIrScene::from_report(&r).unwrap();
        let stats = r.ir_stats().unwrap();
        let want = format_delay_readout_plain(
            stats.arrival_s * 1000.0,
            Some(false),
            SPEED_OF_SOUND_DEFAULT_M_S,
        );
        assert_eq!(scene.arrival.text, want);
    }

    #[test]
    fn arrival_distance_is_not_shown_without_a_measured_interface_latency() {
        // Nonzero delay (see `gated_report_with_delayed_peak`'s doc) so a
        // metres figure derived from the uncorrected round trip would be
        // visibly nonzero too, not masked by a delay of exactly 0.
        let r = gated_report_with_delayed_peak(); // interface_latency: None
        let scene = SweepIrScene::from_report(&r).unwrap();
        assert!(
            !scene.arrival.text.contains('('),
            "no measured \u{3c4} on this report — arrival text must not report metres: {}",
            scene.arrival.text
        );
        assert!(scene.arrival.text.ends_with("ms"));
    }

    #[test]
    fn arrival_distance_is_tau_corrected_when_interface_latency_is_measured() {
        let mut r = gated_report_with_delayed_peak();
        // delay_samples=1 at sr=4000 -> arrival_s = 0.25 ms. A τ larger
        // than the raw round trip pins the corrected figure to a
        // different, checkable sign/magnitude than the uncorrected one.
        r.interface_latency = Some(measured_tau(0.0001)); // 0.1 ms
        let scene = SweepIrScene::from_report(&r).unwrap();
        let ArrivalDistance::Known {
            tau_s,
            speed_of_sound_m_s,
            ..
        } = r.ir_arrival_distance()
        else {
            panic!("interface_latency is Measured, ir_arrival_distance() must be Known");
        };
        let stats = r.ir_stats().unwrap();
        let corrected_ms = (stats.arrival_s - tau_s) * 1000.0;
        assert!(
            (corrected_ms - 0.15).abs() < 1e-9,
            "expected 0.25ms - 0.1ms = 0.15ms, got {corrected_ms}"
        );
        let want = format_delay_readout_plain(corrected_ms, Some(true), speed_of_sound_m_s);
        assert_eq!(scene.arrival.text, want);
        // Not the uncorrected round-trip figure (#283's forbidden case).
        assert_ne!(
            scene.arrival.text,
            format!("{:.2} ms", stats.arrival_s * 1000.0)
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
        assert!(fault.detail().contains("check drive level"));
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
        assert!(no_signal.detail().contains("check drive level"));
        assert!(guard_band.detail().contains("check drive level"));
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
    }
}
