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

use ac_core::measurement::report::{MeasurementData, MeasurementReport};

use crate::ir::ArrivalMarker;
use crate::readout::format_sweep_ir_header;
use crate::scene::{Provenance, Source, Trace};
use crate::ticks::{time_axis, time_to_x, Axis};
use crate::transfer::{format_delay_readout, SPEED_OF_SOUND_DEFAULT_M_S};

/// Mirrors [`crate::ir::IrScene`]'s own downsample target — display
/// column density, not a shared constant. See that module's doc on its
/// own copy for why the coincidence is commented rather than
/// deduplicated across producers.
const IR_MAX_SAMPLES: usize = 2000;

/// The two ways a load can fail, named the way #308's UX comment
/// specifies — deliberately not one merged "cannot open" state: they
/// send the operator to different places to check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
}

impl SweepIrFault {
    /// Header line — occupies the same panel-geometry slot the success
    /// frame's header does, so nothing jumps when a bad file replaces a
    /// good one.
    pub fn header(&self) -> &'static str {
        match self {
            SweepIrFault::NotASweepDerivedIr => "IR — file open failed",
            SweepIrFault::NoGate => "IR — sweep-derived     no gate on this report",
        }
    }

    /// Names what to check; never asserts a cause — same rule
    /// [`crate::fault::Fault::detail`] documents for `NO LOCK`. The
    /// loader cannot know whether a bad file is a live snapshot, a
    /// report from an ungated measurement, a stale format, or operator
    /// error.
    pub fn detail(&self) -> &'static str {
        match self {
            SweepIrFault::NotASweepDerivedIr => {
                "not a MeasurementReport — check this is report JSON from a swept-sine \
                 transfer run, not a live snapshot"
            }
            SweepIrFault::NoGate => {
                "report has no gate bounds — check it came from a gated IR measurement, \
                 not an ungated sweep"
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
        let arrival = ArrivalMarker {
            position: if has_span {
                time_to_x(arrival_ms, t_origin_ms, t_max_ms)
            } else {
                0.5
            },
            // A gate-confirmed arrival is a completed, deterministic
            // measurement, not a provisional live lock (#308 review,
            // implementation notes) — `Some(true)` reads through
            // `format_delay_readout`'s own `delay_ms >= 0.0` guard, so
            // a peak that lands before the gate's zero-delay reference
            // still prints ms-only rather than fabricating a negative
            // distance.
            text: format_delay_readout(arrival_ms, Some(true), SPEED_OF_SOUND_DEFAULT_M_S),
        };

        // The window is centred on the zero-delay reference (the
        // daemon's own `gate_start_s = -(half window)/sr`), so the
        // recorded gate length's half-span is the same figure the time
        // axis's own `-x..+x` endpoints show.
        let half_span_ms = stats.gate_window_s * 1000.0 / 2.0;
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
        GateParams, IntegrationParams, MeasurementMethod, MeasurementPayload, ProcessingChain,
        StimulusParams, SCHEMA_VERSION,
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

    // dt_ms=250 at sr=4000 (1000/4000*1=0.25 -> ms step), gate window
    // 5 samples wide centred on a peak two samples after the zero-delay
    // reference — round figures chosen the same way `ir.rs`'s
    // `locked_input` picks its fixture, so hand-checking the span and
    // the arrival position is easy.
    fn gated_report(gate: Option<GateParams>) -> MeasurementReport {
        let mut r = base_report();
        r.data.push(MeasurementPayload {
            data: MeasurementData::ImpulseResponse {
                sample_rate_hz: 4_000,
                f1_hz: 20.0,
                f2_hz: 20_000.0,
                duration_s: 1.0,
                linear_ir: vec![0.0, 0.0, 1.0, -0.5, 0.0],
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
            gate_start_s: -0.000625, // -2 samples * 0.25 ms at 4 kHz
            gate_length_s: 0.00125,  // 5 samples * 0.25 ms
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
        assert_eq!(scene.trace.segments[0].len(), 5);
        assert_eq!(scene.trace.provenance.source, Source::Snapshot);
        assert_eq!(scene.trace.provenance.sr, 4_000);
    }

    // The magnitude peak (index 2, value 1.0) sits at `window_len/2`
    // (5/2=2), so `delay_samples` is 0 and the arrival lands on the
    // gate's own zero-delay reference sample. The trough at index 3
    // (-0.5) autoscales below the peak.
    #[test]
    fn trace_is_autoscaled_and_arrival_lands_on_the_zero_delay_sample() {
        let r = gated_report(Some(locked_gate()));
        let scene = SweepIrScene::from_report(&r).unwrap();
        let ys: Vec<f64> = scene.trace.segments[0].iter().map(|p| p.1).collect();
        assert_eq!(ys, vec![0.5, 0.5, 1.0, 0.25, 0.5]);
        // `t_origin_ms` is the recorded gate start (-0.625 ms);
        // `t_max_ms` is the last of 5 samples at 0.25 ms/sample
        // (-0.625 + 4*0.25 = 0.375 ms) — the same `(n-1)*dt` endpoint
        // convention `crate::ir::IrScene` uses.
        let expected_x = time_to_x(0.0, -0.625, 0.375);
        assert!((scene.arrival.position - expected_x).abs() < 1e-9);
    }

    #[test]
    fn arrival_marker_reuses_format_delay_readout_verbatim() {
        let r = gated_report(Some(locked_gate()));
        let scene = SweepIrScene::from_report(&r).unwrap();
        let stats = r.ir_stats().unwrap();
        let want = format_delay_readout(
            stats.arrival_s * 1000.0,
            Some(true),
            SPEED_OF_SOUND_DEFAULT_M_S,
        );
        assert_eq!(scene.arrival.text, want);
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
