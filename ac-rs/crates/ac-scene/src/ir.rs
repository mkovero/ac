//! IR scene type (#286): the live-arrival h(t) panel, fed by both
//! producers — the daemon's `visualize/ir` wire sidecar, and (for
//! symmetry) a `PairDerivation.h1` read back from an opened `.acsnap`.
//! Both are the **same kind of measurement**: Welch-averaged H₁,
//! IFFT'd, mic curve deliberately not applied, not a basis for a gated
//! measurement. Neither is the sweep-derived IR (`ac plot ir`'s
//! `MeasurementPayload`/`GateParams`) — that lives in a disjoint file
//! format `ac-view` does not open yet (architect review, option A) and
//! has its own follow-up issue (#308).
//!
//! Mirrors [`crate::transfer::TransferInput`] / [`crate::transfer::TransferScene`]'s
//! canonical-intermediate split: [`IrInput`] is what a wire frame and a
//! snapshot derivation both funnel through, [`IrScene`] is what a view
//! draws verbatim.

use ac_core::visualize::pair_derivation::PairDerivation;
use ac_core::visualize::transfer::impulse_response_from_h;

use crate::scene::{Provenance, Source, Trace};
use crate::ticks::{time_axis, time_to_x, Axis};
use crate::transfer::{format_delay_readout, SPEED_OF_SOUND_DEFAULT_M_S};
use crate::wire::IrWireFrame;

/// The header line every IR panel draws verbatim, top of the pane.
///
/// A single constant, not built per frame: both producers in scope for
/// this display (live wire sidecar, `.acsnap`-derived `PairDerivation`)
/// are the identical kind of measurement (architect review, option A —
/// the sweep-derived kind is a different producer and is out of scope,
/// #308). Naming what the trace is NOT ("not a gated measurement") is
/// the acceptance-criterion requirement that the two kinds must never
/// be visually mistaken for each other; there being only one in-scope
/// kind here doesn't relax that — a future Frame C panel gets its own
/// header, this one never grows a branch to become it.
pub const IR_HEADER: &str =
    "IR — live arrival     H\u{2081} Welch, 1 Hz res  \u{b7}  mic curve not applied  \u{b7}  not a gated measurement";

/// Mirrors the daemon's own downsample target
/// (`ac-daemon/src/handlers/transfer.rs`'s `IR_MAX_SAMPLES`) so a
/// snapshot-derived panel and a live one carry the same column density.
/// Not shared as a crate constant across `ac-core`/`ac-daemon`/`ac-scene`
/// — the daemon's copy is wire economy for its own IFFT output, this
/// one is display density for a re-derived one, and the two doing the
/// same downsampling for different reasons is coincidence worth a
/// comment, not a reason to introduce a shared dependency for one usize.
const IR_MAX_SAMPLES: usize = 2000;

/// The canonical intermediate both a live `visualize/ir` wire frame and
/// a `.acsnap`-derived [`PairDerivation`] funnel through.
pub struct IrInput {
    /// h(t), `fftshift`-centred.
    pub samples: Vec<f32>,
    /// ms per sample.
    pub dt_ms: f64,
    /// The first sample's time, ms (negative).
    pub t_origin_ms: f64,
    pub delay_ms: f64,
    /// [`crate::wire::WireFrame::delay_locked`]'s three-way meaning.
    pub delay_locked: Option<bool>,
    pub speed_of_sound_m_s: Option<f64>,
    pub channel_role: String,
    pub source: Source,
    pub sr: u32,
}

impl IrInput {
    /// Adapt a live `visualize/ir` wire frame.
    pub fn from_wire_frame(frame: &IrWireFrame) -> IrInput {
        IrInput {
            samples: frame.samples.clone(),
            dt_ms: frame.dt_ms,
            t_origin_ms: frame.t_origin_ms,
            delay_ms: frame.delay_ms,
            delay_locked: frame.delay_locked,
            // The `visualize/ir` sidecar carries no speed-of-sound field
            // of its own (`ZMQ.md:2094`) — the readout falls back to the
            // default, same as a `WireFrame` predating #243.
            speed_of_sound_m_s: None,
            channel_role: format!("meas_{}", frame.meas_channel),
            source: Source::Live,
            sr: frame.sr,
        }
    }

    /// Adapt an offline snapshot derivation — the same IFFT the daemon
    /// runs on its own full-resolution H1 (`impulse_response_from_h`),
    /// over `d.h1.{re,im}` instead of the live per-tick result, then
    /// downsampled to the same target density as the wire sidecar.
    pub fn from_pair_derivation(d: &PairDerivation, channel_role: &str, sr: u32) -> IrInput {
        let ir_full = impulse_response_from_h(&d.h1.re, &d.h1.im);
        let stride = (ir_full.len() / IR_MAX_SAMPLES).max(1);
        let samples: Vec<f32> = ir_full.iter().step_by(stride).copied().collect();
        let dt_ms = 1000.0 / sr as f64 * stride as f64;
        let t_origin_ms = -((samples.len() / 2) as f64) * dt_ms;
        IrInput {
            samples,
            dt_ms,
            t_origin_ms,
            delay_ms: d.h1.delay_ms,
            // A `PairDerivation` records no lock verdict (same reasoning
            // as `TransferInput::from_pair_derivation`) — `None` reads
            // through `format_delay_readout` as milliseconds without
            // metres.
            delay_locked: None,
            speed_of_sound_m_s: None,
            channel_role: channel_role.to_string(),
            source: Source::Snapshot,
            sr,
        }
    }
}

/// One vertical marker on the time axis: where the frame's own delay
/// sits, in normalized x plus the verbatim readout string underneath.
#[derive(Debug, Clone, PartialEq)]
pub struct ArrivalMarker {
    /// Normalized x, [`crate::ticks::time_to_x`]'s mapping — not
    /// clamped to `[0,1]`. A lock outside the displayed `±t_origin_ms`
    /// window runs off-canvas, which is honest (the same argument
    /// `TransferScene::from_input`'s magnitude pane makes for an
    /// over-range value): pinning it to the pane edge would fabricate a
    /// position at exactly the moment the true one doesn't fit.
    pub position: f64,
    /// `"4.82 ms  (1.66 m)"` or, unlocked, `"0.00 ms"` —
    /// [`format_delay_readout`], reused verbatim rather than
    /// reimplemented (the design's explicit instruction: this display
    /// inherits the "no metres before lock" rule, it does not re-decide
    /// it).
    pub text: String,
}

/// Everything the IR panel draws, with no numeric work left for the
/// renderer.
#[derive(Debug, Clone, PartialEq)]
pub struct IrScene {
    /// h(t) as one polyline — never gapped like the transfer trace
    /// (there is no coherence mask over a time-domain sample), so
    /// always a single segment when non-empty.
    pub trace: Trace,
    /// Time axis over the frame's own `[t_origin_ms, t_origin_ms +
    /// (n-1)*dt_ms]` span — never inferred, never zoomed; the frame's
    /// fields are the whole of the range (acceptance criterion: both
    /// endpoints are frame-supplied, not computed in the view).
    pub time_axis: Axis,
    pub arrival: ArrivalMarker,
    /// [`IR_HEADER`], carried on the scene so `ac-view` draws it without
    /// holding a copy of its own (the same reason `smoothing_readout`
    /// travels on `TransferScene` rather than living as a literal in
    /// `ac-view`).
    pub header: &'static str,
}

impl IrScene {
    /// Build the scene from `input`. No caller-supplied range: unlike
    /// the frequency/dB axes (whose ranges are a *display* choice — a
    /// zoom level), the IR panel's time span IS the frame's own
    /// `t_origin_ms`/`dt_ms`/sample count, so there is nothing for a
    /// caller to supply.
    pub fn from_input(input: &IrInput) -> IrScene {
        let n = input.samples.len();
        let t_min_ms = input.t_origin_ms;
        let t_max_ms = t_min_ms + (n.saturating_sub(1)) as f64 * input.dt_ms;

        let provenance = Provenance {
            channel_role: input.channel_role.clone(),
            source: input.source,
            sr: input.sr,
        };

        // A degenerate span (no samples, non-finite dt, or a stride so
        // large the whole frame collapses to one time) draws no trace
        // and no axis rather than fabricating a range — the same
        // defensive posture `ticks::freq_axis`/`db_axis` take.
        let has_span = n > 1 && t_max_ms > t_min_ms;

        let trace = if has_span {
            // Autoscaled to this frame's own peak — the trace has no
            // calibrated amplitude (mic curve not applied, no voltage
            // cal), so there is no fixed unit to hold a fixed viewport
            // scale to; a peak of exactly 0.0 (digital silence) falls
            // back to 1.0 so every sample still maps to the mid-line
            // rather than dividing by zero.
            let peak = input
                .samples
                .iter()
                .fold(0.0_f64, |m, &s| m.max((s as f64).abs()));
            let peak = if peak > 0.0 { peak } else { 1.0 };
            let points: Vec<(f64, f64)> = input
                .samples
                .iter()
                .enumerate()
                .map(|(i, &s)| {
                    let t_ms = t_min_ms + i as f64 * input.dt_ms;
                    let x = time_to_x(t_ms, t_min_ms, t_max_ms);
                    let y = 0.5 + 0.5 * (s as f64 / peak);
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
            time_axis(t_min_ms, t_max_ms)
        } else {
            Axis { ticks: Vec::new() }
        };

        let arrival = ArrivalMarker {
            position: if has_span {
                time_to_x(input.delay_ms, t_min_ms, t_max_ms)
            } else {
                0.5
            },
            text: format_delay_readout(
                input.delay_ms,
                input.delay_locked,
                input
                    .speed_of_sound_m_s
                    .unwrap_or(SPEED_OF_SOUND_DEFAULT_M_S),
            ),
        };

        IrScene {
            trace,
            time_axis,
            arrival,
            header: IR_HEADER,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // dt_ms=250, t_origin_ms=-500 -> five samples span exactly
    // -500/-250/0/250/500 ms, the same round figures the UX mockup uses,
    // and land on tick-friendly positions without hitting any
    // float-rounding tie in the assertions below.
    fn locked_input(samples: Vec<f32>) -> IrInput {
        IrInput {
            samples,
            dt_ms: 250.0,
            t_origin_ms: -500.0,
            delay_ms: 0.0,
            delay_locked: Some(true),
            speed_of_sound_m_s: None,
            channel_role: "meas_0".to_string(),
            source: Source::Live,
            sr: 48_000,
        }
    }

    // The trace is autoscaled to the frame's own peak: a sample equal to
    // the peak lands at y=1.0, half the peak at y=0.75, silence at y=0.5.
    #[test]
    fn trace_is_autoscaled_to_the_frames_own_peak() {
        let input = locked_input(vec![0.0, 0.0, 1.0, -0.5, 0.0]);
        let scene = IrScene::from_input(&input);
        let ys: Vec<f64> = scene.trace.segments[0].iter().map(|p| p.1).collect();
        assert_eq!(ys, vec![0.5, 0.5, 1.0, 0.25, 0.5]);
    }

    #[test]
    fn silent_frame_maps_every_sample_to_the_midline_not_a_divide_by_zero() {
        let input = locked_input(vec![0.0, 0.0, 0.0, 0.0, 0.0]);
        let scene = IrScene::from_input(&input);
        for &(_, y) in &scene.trace.segments[0] {
            assert_eq!(y, 0.5);
        }
    }

    // x maps the frame's own t_origin_ms/dt_ms span, endpoints exactly
    // on the pane edges — five samples at dt_ms=250 span -500..+500 ms.
    #[test]
    fn trace_x_spans_the_frames_own_time_range_exactly() {
        let input = locked_input(vec![0.0, 0.0, 0.0, 0.0, 0.0]);
        let scene = IrScene::from_input(&input);
        let xs: Vec<f64> = scene.trace.segments[0].iter().map(|p| p.0).collect();
        assert_eq!(xs, vec![0.0, 0.25, 0.5, 0.75, 1.0]);
    }

    #[test]
    fn time_axis_matches_the_computed_span() {
        let input = locked_input(vec![0.0, 0.0, 0.0, 0.0, 0.0]);
        let scene = IrScene::from_input(&input);
        let labels: Vec<&str> = scene
            .time_axis
            .ticks
            .iter()
            .map(|t| t.label.as_str())
            .collect();
        assert_eq!(labels, vec!["-500 ms", "0", "+500 ms"]);
    }

    // Arrival marker text is byte-identical to `format_delay_readout`'s
    // own output — proves this module calls it rather than
    // reimplementing the "no metres before lock" rule.
    #[test]
    fn arrival_marker_reuses_format_delay_readout_verbatim() {
        let mut input = locked_input(vec![0.0, 1.0, -1.0, 0.0]);
        input.delay_ms = 4.82;
        input.delay_locked = Some(true);
        let scene = IrScene::from_input(&input);
        assert_eq!(
            scene.arrival.text,
            format_delay_readout(4.82, Some(true), SPEED_OF_SOUND_DEFAULT_M_S)
        );
        assert!(scene.arrival.text.contains("4.82 ms"));
        assert!(scene.arrival.text.contains('m'));
    }

    #[test]
    fn arrival_marker_drops_metres_when_unlocked() {
        let mut input = locked_input(vec![0.0, 1.0, -1.0, 0.0]);
        input.delay_ms = 0.0;
        input.delay_locked = Some(false);
        let scene = IrScene::from_input(&input);
        assert_eq!(scene.arrival.text, "0.00 ms");
    }

    // An empty frame (no samples yet — session just opened) draws
    // nothing rather than fabricating a range.
    #[test]
    fn empty_samples_produce_no_trace_and_no_axis() {
        let input = locked_input(Vec::new());
        let scene = IrScene::from_input(&input);
        assert!(scene.trace.segments.is_empty());
        assert!(scene.time_axis.ticks.is_empty());
    }

    #[test]
    fn header_names_what_the_trace_is_not() {
        assert!(IR_HEADER.contains("not a gated measurement"));
        assert!(IR_HEADER.contains("mic curve not applied"));
    }

    // Snapshot adapter: the same IFFT the daemon runs, over a
    // `PairDerivation`'s H1, produces a non-empty, correctly-provenanced
    // input — mirrors `TransferInput`'s
    // `snapshot_stored_delay_round_trips_into_the_derot_mode` coverage.
    #[test]
    fn from_pair_derivation_builds_a_snapshot_sourced_input() {
        use ac_core::visualize::transfer::h1_estimate_with_delay;
        let sr = 48_000u32;
        let n = sr as usize;
        let r: Vec<f32> = (0..n).map(|i| ((i as f64 * 0.01).sin()) as f32).collect();
        let m = r.clone();
        let h1 = h1_estimate_with_delay(&r, &m, sr, 144); // 3.0 ms at 48 kHz
        let d = PairDerivation {
            h1,
            spec_freqs: vec![],
            meas_spectrum: vec![],
            ref_spectrum: vec![],
            spl: None,
            spl_weighting: ac_core::visualize::weighting_curves::WeightingCurve::Z,
        };
        let input = IrInput::from_pair_derivation(&d, "meas_0", sr);
        assert_eq!(input.source, Source::Snapshot);
        assert_eq!(input.channel_role, "meas_0");
        assert!((input.delay_ms - 3.0).abs() < 1e-9);
        assert!(input.delay_locked.is_none());
        assert!(!input.samples.is_empty());
        assert!(input.samples.len() <= IR_MAX_SAMPLES);

        let scene = IrScene::from_input(&input);
        assert!(!scene.trace.segments.is_empty());
        assert_eq!(scene.trace.segments[0].len(), input.samples.len());
    }
}
