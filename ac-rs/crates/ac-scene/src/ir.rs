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
use crate::ticks::{db_axis, db_to_y, time_axis, time_to_x, Axis};
use ac_core::wire::IrFrame;

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

/// [`IR_HEADER`] for the arrival IR (#706): the same measurement kind, from
/// 250 ms blocks — 4 Hz resolution, ±125 ms.
pub const IR_HEADER_ARRIVAL: &str =
    "IR — live arrival     H\u{2081} 250 ms blocks, 4 Hz res  \u{b7}  mic curve not applied  \u{b7}  not a gated measurement";

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
#[derive(Debug, Clone)]
pub struct IrInput {
    /// h(t), `fftshift`-centred.
    pub samples: Vec<f32>,
    /// ms per sample.
    pub dt_ms: f64,
    /// The first sample's time, ms (negative).
    pub t_origin_ms: f64,
    pub delay_ms: f64,
    /// The delay the IR is centred on — `t = 0` on its axis. The arrival
    /// marker sits at `delay_ms − centre_ms`: on the peak for a live IR
    /// (both are the held delay), off it by a stored run's nudge (#706,
    /// Codex review: the marker used to sit at `delay_ms` on this relative
    /// axis, off the trace for any delay over the IR's half-span).
    pub centre_ms: f64,
    /// The arrival IR (#706) rather than the 1 s one: names the header.
    pub arrival: bool,
    /// [`ac_core::wire::TransferFrame::delay_locked`]'s three-way meaning.
    pub delay_locked: Option<bool>,
    pub channel_role: String,
    pub source: Source,
    pub sr: u32,
    /// `20·log10|h|` and the ETC, dB re their peak, one per `samples`
    /// entry (`ac_core::visualize::ir_views`, bucket maxima). Empty from a
    /// daemon that predates them.
    pub log_db: Vec<f32>,
    pub etc_db: Vec<f32>,
}

/// Which view of the IR the panel draws (Smaart's live IR modes), cycled
/// by `Shift+H`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IrView {
    /// h(t), autoscaled to its own peak.
    #[default]
    Linear,
    /// `20·log10|h|`, dB re the peak.
    Log,
    /// The energy-time curve: the analytic signal's envelope, dB re its
    /// peak — each arrival at the level it reaches, without its cycles.
    Etc,
}

impl IrView {
    pub fn next(self) -> IrView {
        match self {
            IrView::Linear => IrView::Log,
            IrView::Log => IrView::Etc,
            IrView::Etc => IrView::Linear,
        }
    }

    /// The caption the panel draws for this view.
    pub fn label(self) -> &'static str {
        match self {
            IrView::Linear => "linear",
            IrView::Log => "log  (dB re peak)",
            IrView::Etc => "ETC  (dB re peak)",
        }
    }
}

/// The dB span of the log and ETC views: 80 dB below the peak, the depth
/// an energy-time curve is read over in a room; the curve itself goes on
/// down to −150 dB off the bottom of the pane.
pub const IR_DB_RANGE: (f64, f64) = (-80.0, 0.0);

impl IrInput {
    /// Adapt a live `visualize/ir` wire frame.
    pub fn from_wire_frame(frame: &IrFrame) -> IrInput {
        IrInput {
            samples: frame.samples.clone(),
            dt_ms: frame.dt_ms,
            t_origin_ms: frame.t_origin_ms,
            delay_ms: frame.delay_ms,
            // Both live IRs are centred on the delay the frame carries: the
            // 1 s one compensated for it, the arrival one aligned at it; an
            // unlocked pair's is 0 and unaligned.
            centre_ms: frame.delay_ms,
            arrival: frame.span.as_deref() == Some(ac_core::wire::IR_SPAN_ARRIVAL),
            delay_locked: frame.delay_locked,
            channel_role: format!("meas_{}", frame.meas_channel),
            source: Source::Live,
            sr: frame.sr,
            log_db: frame.log_db.clone(),
            etc_db: frame.etc_db.clone(),
        }
    }

    /// Adapt an offline snapshot derivation — the same IFFT the daemon
    /// runs on its own full-resolution H1 (`impulse_response_from_h`),
    /// over `d.h1.{re,im}` instead of the live per-tick result, then
    /// downsampled to the same target density as the wire sidecar.
    pub fn from_pair_derivation(d: &PairDerivation, channel_role: &str, sr: u32) -> IrInput {
        let ir_full = impulse_response_from_h(&d.h1.re, &d.h1.im);
        let stride = (ir_full.len() / IR_MAX_SAMPLES).max(1);
        use ac_core::visualize::ir_views;
        let samples = ir_views::bucket_peak(&ir_full, stride);
        let log_db = ir_views::bucket_max(&ir_views::log_db(&ir_full), stride);
        let etc_db = ir_views::bucket_max(&ir_views::etc_db(&ir_full), stride);
        let dt_ms = 1000.0 / sr as f64 * stride as f64;
        let t_origin_ms = -((samples.len() / 2) as f64) * dt_ms;
        IrInput {
            samples,
            dt_ms,
            t_origin_ms,
            delay_ms: d.h1.delay_ms,
            centre_ms: d.h1.delay_ms,
            arrival: false,
            // A `PairDerivation` records no lock verdict (same reasoning
            // as `TransferInput::from_pair_derivation`).
            delay_locked: None,
            channel_role: channel_role.to_string(),
            source: Source::Snapshot,
            sr,
            log_db,
            etc_db,
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
    /// `"4.82 ms"` — the frame's own delay, milliseconds only (#391).
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
    /// Which view the trace is (`Shift+H`).
    pub view: IrView,
    /// [`IrView::label`], or why the view has no trace (a daemon that
    /// sends no log/ETC data).
    pub view_readout: &'static str,
    /// dB gridlines for the log and ETC views ([`IR_DB_RANGE`]); empty for
    /// the linear view, which has no unit.
    pub level_axis: Axis,
    /// Whose IR this is (`"live"`, `"slot 2"`), set by the caller with
    /// [`IrScene::labelled`]; the panel shows one trace's IR at a time
    /// (#702), so it must say which.
    pub label: Option<String>,
}

impl IrScene {
    /// Build the scene from `input`. No caller-supplied range: unlike
    /// the frequency/dB axes (whose ranges are a *display* choice — a
    /// zoom level), the IR panel's time span IS the frame's own
    /// `t_origin_ms`/`dt_ms`/sample count, so there is nothing for a
    /// caller to supply.
    pub fn from_input(input: &IrInput) -> IrScene {
        Self::from_input_view(input, IrView::Linear)
    }

    /// Build the scene drawing `view`. The time axis and the arrival
    /// marker are the same in every view: the log and ETC views change the
    /// vertical reading only, never where the arrival is.
    /// The same scene, naming whose IR it is.
    pub fn labelled(self, label: impl Into<String>) -> IrScene {
        IrScene {
            label: Some(label.into()),
            ..self
        }
    }

    pub fn from_input_view(input: &IrInput, view: IrView) -> IrScene {
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

        let db_curve = match view {
            IrView::Linear => None,
            IrView::Log => Some(&input.log_db),
            IrView::Etc => Some(&input.etc_db),
        };
        // A dB view with no data for it (an older daemon): no trace, and
        // the caption says why, rather than a curve from something else.
        let missing = db_curve.is_some_and(|c| c.len() != n);
        let trace = if let (true, Some(curve), false) = (has_span, db_curve, missing) {
            let points: Vec<(f64, f64)> = curve
                .iter()
                .enumerate()
                .map(|(i, &db)| {
                    let t_ms = t_min_ms + i as f64 * input.dt_ms;
                    (
                        time_to_x(t_ms, t_min_ms, t_max_ms),
                        db_to_y(f64::from(db), IR_DB_RANGE.0, IR_DB_RANGE.1),
                    )
                })
                .collect();
            Trace::single(points, provenance)
        } else if db_curve.is_some() {
            Trace {
                segments: Vec::new(),
                provenance,
            }
        } else if has_span {
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
                time_to_x(input.delay_ms - input.centre_ms, t_min_ms, t_max_ms)
            } else {
                0.5
            },
            text: format!("{:.2} ms", input.delay_ms),
        };

        IrScene {
            trace,
            time_axis,
            arrival,
            header: if input.arrival {
                IR_HEADER_ARRIVAL
            } else {
                IR_HEADER
            },
            label: None,
            view,
            view_readout: if missing {
                "log/ETC not sent by this daemon"
            } else {
                view.label()
            },
            level_axis: if db_curve.is_some() && !missing {
                db_axis(IR_DB_RANGE.0, IR_DB_RANGE.1)
            } else {
                Axis { ticks: Vec::new() }
            },
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
            centre_ms: 0.0,
            arrival: false,
            delay_locked: Some(true),
            channel_role: "meas_0".to_string(),
            source: Source::Live,
            sr: 48_000,
            log_db: Vec::new(),
            etc_db: Vec::new(),
        }
    }

    /// #706 Codex: a live IR is centred on the delay its frame carries, so
    /// the arrival marker sits at t = 0 — on the peak — whatever that delay
    /// is. The rejected placement (the delay itself on this relative axis)
    /// is computed here: at 200 ms it is off a ±125 ms arrival IR entirely.
    /// The arrival IR also names itself in the header.
    #[test]
    fn a_live_marker_sits_on_the_centre_the_ir_is_aligned_at() {
        let wire = ac_core::wire::IrFrame {
            frame_type: "visualize/ir".into(),
            cmd: "transfer_stream".into(),
            wire_version: None,
            samples: vec![0.0, 0.1, 1.0, 0.1, 0.0],
            log_db: Vec::new(),
            etc_db: Vec::new(),
            sr: 48_000,
            stride: 1,
            dt_ms: 62.5,
            t_origin_ms: -125.0,
            ref_channel: 1,
            meas_channel: 0,
            delay_samples: 9_600,
            delay_ms: 200.0,
            delay_locked: Some(true),
            analysis_seq: 6,
            backend: String::new(),
            span: Some(ac_core::wire::IR_SPAN_ARRIVAL.into()),
        };
        let scene = IrScene::from_input(&IrInput::from_wire_frame(&wire));
        assert!((scene.arrival.position - 0.5).abs() < 1e-12);
        assert_eq!(scene.arrival.text, "200.00 ms");
        let rejected = time_to_x(200.0, -125.0, 125.0);
        assert!(
            rejected > 1.0,
            "the old placement was on the panel: {rejected}"
        );
        assert_eq!(scene.header, IR_HEADER_ARRIVAL);
        let mut long = wire;
        long.span = None;
        assert_eq!(
            IrScene::from_input(&IrInput::from_wire_frame(&long)).header,
            IR_HEADER
        );
    }

    /// The log and ETC views draw their dB curves on the fixed 80 dB span,
    /// with gridlines, at the same x and with the same arrival marker as
    /// the linear view; with no data for them they draw nothing and say
    /// why.
    #[test]
    fn log_and_etc_views_draw_db_on_the_same_time_axis() {
        let mut input = locked_input(vec![0.0, 0.5, 1.0, -0.5, 0.0]);
        input.log_db = vec![-150.0, -6.0, 0.0, -6.0, -150.0];
        input.etc_db = vec![-40.0, -3.0, 0.0, -3.0, -40.0];
        input.delay_ms = 250.0;
        let linear = IrScene::from_input(&input);
        let etc = IrScene::from_input_view(&input, IrView::Etc);
        let log = IrScene::from_input_view(&input, IrView::Log);
        let y = |s: &IrScene, i: usize| s.trace.segments[0][i].1;
        assert_eq!(y(&etc, 2), 1.0);
        assert!(
            (y(&etc, 0) - 0.5).abs() < 1e-12,
            "-40 dB sits halfway down 80 dB"
        );
        assert!(
            y(&log, 0) < 0.0,
            "-150 dB runs off the bottom, not pinned to it"
        );
        let xs = |s: &IrScene| s.trace.segments[0].iter().map(|p| p.0).collect::<Vec<_>>();
        assert_eq!(xs(&etc), xs(&linear));
        assert_eq!(etc.arrival, linear.arrival);
        assert_eq!(etc.view_readout, "ETC  (dB re peak)");
        assert!(!etc.level_axis.ticks.is_empty());
        assert!(linear.level_axis.ticks.is_empty());

        input.etc_db.clear();
        let old = IrScene::from_input_view(&input, IrView::Etc);
        assert!(old.trace.segments.is_empty());
        assert_eq!(old.view_readout, "log/ETC not sent by this daemon");
    }

    #[test]
    fn shift_h_cycles_linear_log_etc() {
        assert_eq!(IrView::default().next(), IrView::Log);
        assert_eq!(IrView::Log.next(), IrView::Etc);
        assert_eq!(IrView::Etc.next(), IrView::Linear);
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

    // Arrival marker text is always ms-only (#391) — no metres, and no
    // dependency on `delay_locked`.
    #[test]
    fn arrival_marker_prints_ms_only() {
        let mut input = locked_input(vec![0.0, 1.0, -1.0, 0.0]);
        input.delay_ms = 4.82;
        input.delay_locked = Some(true);
        let scene = IrScene::from_input(&input);
        assert_eq!(scene.arrival.text, "4.82 ms");
    }

    #[test]
    fn arrival_marker_prints_ms_only_when_unlocked_too() {
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
            mtw: None,
            welch_nperseg: ac_core::visualize::transfer::h1_nperseg(sr),
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

    // Cross-tier parity (QA follow-up on #286/PR #309): both producers
    // funnel the *same* underlying H1 through the *same* IFFT
    // (`impulse_response_from_h`) and the *same* stride-downsample
    // formula (this module's doc on `IR_MAX_SAMPLES` claims byte-for-byte
    // identity with `ac-daemon/src/handlers/transfer.rs`'s `ir_msg` block —
    // this test is what pins that claim down instead of leaving it as
    // prose). A synthetic `IrFrame` is built here by hand, running the
    // daemon's own downsample arithmetic verbatim, so it stands in for a
    // live wire frame carrying the identical H1 a `.acsnap`'s
    // `PairDerivation` also stores — the two `IrScene`s built from it must
    // then agree on everything the panel draws, not merely be close.
    #[test]
    fn live_wire_and_pair_derivation_agree_on_the_same_underlying_h1() {
        use ac_core::visualize::transfer::h1_estimate_with_delay;
        let sr = 48_000u32;
        let n = sr as usize;
        let r: Vec<f32> = (0..n).map(|i| ((i as f64 * 0.01).sin()) as f32).collect();
        let m = r.clone();
        let h1 = h1_estimate_with_delay(&r, &m, sr, 144); // 3.0 ms at 48 kHz

        let ir_full = impulse_response_from_h(&h1.re, &h1.im);
        // Mirrors `IR_MAX_SAMPLES` above and
        // `ac-daemon/src/handlers/transfer.rs`'s `IR_MAX_SAMPLES` — a
        // fresh local copy rather than reaching for either, since the
        // point of this test is that a wire frame built independently by
        // that formula still lands on the same numbers `from_pair_derivation`
        // computes internally.
        const DAEMON_IR_MAX_SAMPLES: usize = 2000;
        let stride = (ir_full.len() / DAEMON_IR_MAX_SAMPLES).max(1);
        let samples = ac_core::visualize::ir_views::bucket_peak(&ir_full, stride);
        let dt_ms = 1000.0 / sr as f64 * stride as f64;
        let t_origin_ms = -((samples.len() / 2) as f64) * dt_ms;

        let wire = IrFrame {
            frame_type: "visualize/ir".to_string(),
            cmd: "transfer_stream".to_string(),
            wire_version: None,
            analysis_seq: 0,
            backend: String::new(),
            samples,
            log_db: Vec::new(),
            etc_db: Vec::new(),
            sr,
            stride,
            dt_ms,
            t_origin_ms,
            ref_channel: 1,
            meas_channel: 0,
            delay_samples: h1.delay_samples,
            delay_ms: h1.delay_ms,
            // A `PairDerivation` records no lock verdict either — held
            // equal on both sides so this test isolates the numeric
            // (trace/axis/arrival-position) claim from the unrelated
            // "no metres before lock" formatting rule already covered by
            // `arrival_marker_drops_metres_when_unlocked` above.
            delay_locked: None,
            span: None,
        };

        let d = PairDerivation {
            h1: h1.clone(),
            spec_freqs: vec![],
            meas_spectrum: vec![],
            ref_spectrum: vec![],
            spl: None,
            spl_weighting: ac_core::visualize::weighting_curves::WeightingCurve::Z,
            mtw: None,
            welch_nperseg: ac_core::visualize::transfer::h1_nperseg(sr),
        };

        let live_scene = IrScene::from_input(&IrInput::from_wire_frame(&wire));
        let snap_scene = IrScene::from_input(&IrInput::from_pair_derivation(&d, "meas_0", sr));

        // Segments only, not the whole `Trace` — `provenance.source`
        // (`Live` vs `Snapshot`) is *supposed* to differ, that is the
        // correctly-tagged half of D15; the claim under test is that the
        // geometry agrees.
        assert_eq!(
            live_scene.trace.segments, snap_scene.trace.segments,
            "the wire-frame and snapshot paths painted different h(t) traces \
             for the identical underlying H1"
        );
        assert_eq!(
            live_scene.time_axis, snap_scene.time_axis,
            "the two paths disagree on the time axis for the identical H1"
        );
        assert_eq!(
            live_scene.arrival.position, snap_scene.arrival.position,
            "the two paths placed the arrival marker at different positions \
             for the identical H1"
        );
    }
}
