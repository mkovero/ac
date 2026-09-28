//! M4a fixtures (#180): F1′ / F1″ / F2′ / F3 / F4 / F5.
//!
//! Every frame here is **daemon-shaped**: `phase_deg` is what the wire
//! actually carries, φ_wire = φ_raw + 360·f·τ_sess, because the daemon
//! delay-compensates before forming H1. The superseded F1/F2 built raw
//! phase by hand and would have passed against a de-rotation mapping
//! that double-compensates in session mode — the exact defect #180's
//! architect pass found. Expected values below are derived from the
//! corrected §6, independently of the implementation.

use ac_scene::transfer::{
    derotate_deg, meter_height, DerotMode, DisplayModes, MeterState, Smoothing, TransferInput,
    TransferScene,
};
use ac_scene::{FaultState, Source};

const SR: u32 = 48_000;
const FREQ_RANGE: (f64, f64) = (20.0, 20_000.0);
const DB_RANGE: (f64, f64) = (-80.0, 20.0);

/// A physical delay τ produces φ_raw(f) = −360·f·τ. Hand-derived, not
/// taken from the crate.
fn phi_raw(freq_hz: f64, tau_ms: f64) -> f64 {
    -360.0 * freq_hz * tau_ms / 1000.0
}

/// What the daemon publishes: raw phase plus the compensation it
/// applied, i.e. φ_raw(τ_true) + 360·f·τ_est.
fn phi_wire(freq_hz: f64, tau_true_ms: f64, tau_est_ms: f64) -> f64 {
    phi_raw(freq_hz, tau_true_ms) + 360.0 * freq_hz * tau_est_ms / 1000.0
}

fn input(freqs: Vec<f64>, phase_deg: Vec<f64>, delay_ms: f64) -> TransferInput {
    let n = freqs.len();
    TransferInput {
        freqs,
        magnitude_db: vec![-6.0206; n],
        phase_deg,
        coherence: vec![0.9; n],
        delay_ms,
        // These fixtures stipulate a held delay — that is what makes the
        // de-rotation cases meaningful.
        delay_locked: Some(true),
        delay_control: None,
        delay_tracking: false,
        meas_channel: 0,
        ref_channel: 1,
        meas_peak_dbfs: Some(-6.0206),
        ref_peak_dbfs: Some(-6.0206),
        channel_role: "meas_0".to_string(),
        source: Source::Live,
        sr: SR,
        // Welch-derived fixture: no per-column provenance to carry.
        column_df: Vec::new(),
        column_window_s: Vec::new(),
        column_n: Vec::new(),
        column_bins: Vec::new(),
        stages: Vec::new(),
        estimator: ac_scene::transfer::Estimator::Ladder,
        fault: None,
        calibration: None,
    }
}

fn scene(inp: &TransferInput, derot: DerotMode) -> TransferScene {
    let mut meters = (MeterState::default(), MeterState::default());
    TransferScene::from_input(
        inp,
        DisplayModes::new(derot, Smoothing::Off),
        FREQ_RANGE,
        DB_RANGE,
        &mut meters,
        &mut FaultState::default(),
        0.0,
    )
}

/// Recover the de-rotated phase in degrees from a normalized phase-pane
/// y coordinate, so assertions are written in degrees rather than in
/// pane space.
fn phase_deg_at(s: &TransferScene, seg: usize, i: usize) -> f64 {
    s.phase.segments[seg][i].1 * 360.0 - 180.0
}

// ---------------------------------------------------------------------
// F1′ — no mis-estimate: τ_true = τ_est = 2.5 ms.
//
// The daemon compensated exactly the delay that was there, so the wire
// carries phase ≡ 0. Session mode must show that untouched; raw mode
// must undo it and reproduce the hand-derived wrap pattern.
// ---------------------------------------------------------------------
#[test]
fn f1_prime_session_mode_shows_the_wire_untouched_and_raw_mode_undoes_it() {
    let freqs = vec![100.0, 250.0, 1_000.0];
    let wire: Vec<f64> = freqs.iter().map(|&f| phi_wire(f, 2.5, 2.5)).collect();
    // Precondition of the fixture: a perfectly-estimated delay leaves no
    // phase on the wire.
    for w in &wire {
        assert!(w.abs() < 1e-9, "fixture is not daemon-shaped: {w}");
    }
    let inp = input(freqs, wire, 2.5);

    // Session mode: 0.000° at every column. Verbatim-§6 (τ_derot =
    // +2.5 ms) would show +90.000° at 100 Hz — this is the assertion
    // that kills double-compensation.
    let s = scene(&inp, DerotMode::Session);
    for i in 0..3 {
        assert!(
            phase_deg_at(&s, 0, i).abs() < 1e-9,
            "session mode must show the wire as-is, got {}",
            phase_deg_at(&s, 0, i)
        );
    }

    // Raw mode: wrap(−360·f·0.0025).
    let s = scene(&inp, DerotMode::Raw);
    assert!((phase_deg_at(&s, 0, 0) - (-90.0)).abs() < 1e-9); // 100 Hz
    assert!((phase_deg_at(&s, 0, 1) - 135.0).abs() < 1e-9); // 250 Hz: −225 → +135
    assert!((phase_deg_at(&s, 0, 2) - 180.0).abs() < 1e-9); // 1 kHz: −900 → +180

    // #391: ms only, no metres conversion left to gate on a calibration.
    assert_eq!(s.delay_readout, "2.50 ms");
}

// ---------------------------------------------------------------------
// F1″ — mis-estimate: τ_true = 2.5 ms, τ_est = 2.0 ms.
//
// Session mode shows the residual, not zero. That residual is the thing
// the operator nulls, so "session mode must be flat" is a misreading
// this fixture exists to kill.
// ---------------------------------------------------------------------
#[test]
fn f1_double_prime_session_mode_shows_the_mis_estimate_residual() {
    let freqs = vec![100.0];
    let wire: Vec<f64> = freqs.iter().map(|&f| phi_wire(f, 2.5, 2.0)).collect();
    // φ_wire = −360·f·0.0005 ⇒ −18.000° at 100 Hz.
    assert!((wire[0] - (-18.0)).abs() < 1e-9);

    let inp = input(freqs, wire, 2.0);
    let s = scene(&inp, DerotMode::Session);
    assert!(
        (phase_deg_at(&s, 0, 0) - (-18.0)).abs() < 1e-9,
        "session mode must not flatten a real residual, got {}",
        phase_deg_at(&s, 0, 0)
    );

    // And raw mode recovers the true −360·f·0.0025 = −90° at 100 Hz.
    let s = scene(&inp, DerotMode::Raw);
    assert!((phase_deg_at(&s, 0, 0) - (-90.0)).abs() < 1e-9);
}

// ---------------------------------------------------------------------
// F2′ — overlay: snapshot session τ_snap = 3.0 ms, live session
// τ_sess = 2.5 ms, both measuring the same physical τ_true = 3.0 ms.
//
// Snapshot wire ≡ 0; live wire = −360·f·0.0005. De-rotating the live
// trace by τ_snap − τ_sess = +0.5 ms lands it on the snapshot exactly.
// Kills per-trace-own-delay de-rotation and the sign of the
// cross-session correction.
// ---------------------------------------------------------------------
#[test]
fn f2_prime_snapshot_mode_overlays_two_sessions_of_the_same_system() {
    let freqs = vec![100.0, 250.0, 1_000.0];

    let snap_wire: Vec<f64> = freqs.iter().map(|&f| phi_wire(f, 3.0, 3.0)).collect();
    let live_wire: Vec<f64> = freqs.iter().map(|&f| phi_wire(f, 3.0, 2.5)).collect();
    assert!((live_wire[0] - (-18.0)).abs() < 1e-9, "live wire at 100 Hz");

    // The snapshot is already compensated by its own τ_snap, so it is
    // drawn in session mode (τ_derot = 0).
    let snap = scene(&input(freqs.clone(), snap_wire, 3.0), DerotMode::Session);
    // The live trace takes τ_snap − τ_sess.
    let live = scene(
        &input(freqs.clone(), live_wire, 2.5),
        DerotMode::Snapshot {
            snapshot_delay_ms: 3.0,
        },
    );

    for (i, f) in freqs.iter().enumerate() {
        let (a, b) = (phase_deg_at(&snap, 0, i), phase_deg_at(&live, 0, i));
        assert!(
            (a - b).abs() < 1e-9,
            "traces must overlay at {f} Hz: snapshot {a}, live {b}"
        );
        assert!(a.abs() < 1e-9, "both should sit on 0 for this system");
    }
}

/// The sign of the cross-session correction, isolated: de-rotating the
/// live trace by the *wrong* sign doubles the error instead of nulling
/// it, which is observably different rather than merely inexact.
#[test]
fn f2_prime_wrong_sign_doubles_the_residual_rather_than_cancelling() {
    let f = 100.0;
    let live_wire = phi_wire(f, 3.0, 2.5); // −18°
    let correct = derotate_deg(live_wire, f, 0.5); // → 0
    let wrong = derotate_deg(live_wire, f, -0.5); // → −36
    assert!(correct.abs() < 1e-9);
    assert!((wrong - (-36.0)).abs() < 1e-9);
}

// ---------------------------------------------------------------------
// F3 — coherence 0.9 everywhere except columns 5..9 at 0.3.
// ---------------------------------------------------------------------
#[test]
fn f3_masked_columns_split_the_polyline_and_are_absent_not_zero() {
    let freqs: Vec<f64> = (0..20).map(|i| 100.0 * (i + 1) as f64).collect();
    let mut inp = input(freqs.clone(), vec![0.0; 20], 0.0);
    for c in inp.coherence.iter_mut().take(10).skip(5) {
        *c = 0.3;
    }

    let s = scene(&inp, DerotMode::Session);

    // Exactly two segments, on both panes.
    assert_eq!(s.magnitude.segments.len(), 2);
    assert_eq!(s.phase.segments.len(), 2);
    assert_eq!(s.magnitude.segments[0].len(), 5); // columns 0..4
    assert_eq!(s.magnitude.segments[1].len(), 10); // columns 10..19

    // No vertex exists at a masked column's x — the gap is an absence,
    // not a point on the floor.
    let masked_x: Vec<f64> = (5..10)
        .map(|i| ac_scene::ticks::freq_to_x(freqs[i], FREQ_RANGE.0, FREQ_RANGE.1))
        .collect();
    for seg in s.magnitude.segments.iter().chain(s.phase.segments.iter()) {
        for pt in seg {
            for mx in &masked_x {
                assert!(
                    (pt.0 - mx).abs() > 1e-12,
                    "a masked column produced a vertex at x={mx}"
                );
            }
        }
    }
}

/// Threshold is `< 0.5` masked — a column exactly at 0.5 is kept. Kills
/// the off-by-one in the comparison.
#[test]
fn f3_threshold_is_strictly_below_half() {
    let freqs = vec![100.0, 200.0, 300.0];
    let mut inp = input(freqs, vec![0.0; 3], 0.0);
    inp.coherence = vec![0.5, 0.499_999_9, 0.5];
    let s = scene(&inp, DerotMode::Session);
    assert_eq!(s.magnitude.segments.len(), 2);
    assert_eq!(s.magnitude.segments[0].len(), 1);
    assert_eq!(s.magnitude.segments[1].len(), 1);
}

// ---------------------------------------------------------------------
// F4 — meters.
// ---------------------------------------------------------------------
#[test]
fn f4_meter_heights_latch_and_null_handling() {
    // peak 0.5 ⇒ −6.0206 dBFS ⇒ h = (−6.0206 + 60)/60 = 0.899656…
    assert!((meter_height(Some(-6.0206)) - 0.899_656_666_666_666_6).abs() < 1e-9);
    // peak 1.0 ⇒ 0 dBFS ⇒ h = 1, latch set.
    assert_eq!(meter_height(Some(0.0)), 1.0);

    let mut st = MeterState::default();
    assert!(st.update(Some(0.0), 0.0).clip_latch);

    // null ⇒ h = 0, no latch.
    let mut st = MeterState::default();
    let m = st.update(None, 0.0);
    assert_eq!(m.height, 0.0);
    assert!(!m.clip_latch);
}

/// The calibrated-value leakage check: a frame whose *spectrum* is
/// voltage-calibrated must not move the meter, because the meter reads
/// the raw capture peak and nothing else. Same peak, wildly different
/// spectrum content ⇒ identical meter.
#[test]
fn f4_meters_ignore_everything_except_the_raw_peak() {
    let mut a = input(vec![100.0], vec![0.0], 0.0);
    let mut b = input(vec![100.0], vec![0.0], 0.0);
    a.magnitude_db = vec![-6.0206];
    b.magnitude_db = vec![94.0]; // as if voltage-calibrated into dBV-ish
    a.meas_peak_dbfs = Some(-6.0206);
    b.meas_peak_dbfs = Some(-6.0206);

    let sa = scene(&a, DerotMode::Session);
    let sb = scene(&b, DerotMode::Session);
    assert_eq!(sa.meas_meter, sb.meas_meter);
}

/// An absent field and an explicit `null` must be indistinguishable —
/// there is no code path that can tell an old daemon from a silent
/// channel (no version sniffing).
#[test]
fn f4_absent_and_null_peaks_are_indistinguishable() {
    let with_null = r#"{"meas_peak_dbfs": null, "ref_peak_dbfs": null}"#;
    let absent = r#"{}"#;

    #[derive(serde::Deserialize)]
    struct Peaks {
        #[serde(default)]
        meas_peak_dbfs: Option<f64>,
        #[serde(default)]
        ref_peak_dbfs: Option<f64>,
    }

    let a: Peaks = serde_json::from_str(with_null).unwrap();
    let b: Peaks = serde_json::from_str(absent).unwrap();
    assert_eq!(a.meas_peak_dbfs, b.meas_peak_dbfs);
    assert_eq!(a.ref_peak_dbfs, b.ref_peak_dbfs);
    assert_eq!(
        meter_height(a.meas_peak_dbfs),
        meter_height(b.meas_peak_dbfs)
    );
}

// ─── phase views: unwrapped and group delay (#695) ──────────────────

use ac_scene::transfer::PhaseView;

fn scene_view(inp: &TransferInput, view: PhaseView) -> TransferScene {
    let mut meters = (MeterState::default(), MeterState::default());
    TransferScene::from_input(
        inp,
        DisplayModes::new(DerotMode::Raw, Smoothing::Off).with_phase_view(view),
        FREQ_RANGE,
        DB_RANGE,
        &mut meters,
        &mut FaultState::default(),
        0.0,
    )
}

/// A pure 1 ms delay: phase −360·f·τ, wrapped on the wire. Its group delay
/// is 1 ms at every frequency. The rejected implementation — a difference
/// of the wrapped phase — is computed here too: it spikes at each wrap.
#[test]
fn group_delay_of_a_pure_delay_is_flat_where_the_wrapped_difference_spikes() {
    let freqs: Vec<f64> = (1..=60).map(|k| 100.0 * k as f64).collect();
    let wrapped: Vec<f64> = freqs
        .iter()
        .map(|&f| {
            let p = phi_raw(f, 1.0);
            let r = (p + 180.0).rem_euclid(360.0) - 180.0;
            if r == -180.0 {
                180.0
            } else {
                r
            }
        })
        .collect();
    let inp = input(freqs.clone(), wrapped.clone(), 0.0);

    let naive: Vec<f64> = (1..freqs.len())
        .map(|i| -(wrapped[i] - wrapped[i - 1]) / (360.0 * (freqs[i] - freqs[i - 1])) * 1000.0)
        .collect();
    assert!(
        naive.iter().any(|g| (g - 1.0).abs() > 1.0),
        "the wrapped difference was meant to spike"
    );

    let gd = scene_view(&inp, PhaseView::GroupDelay);
    let (lo, hi) = gd.phase_span.expect("a span");
    assert!(
        (lo - 1.0).abs() < 1e-9 && (hi - 1.0).abs() < 1e-9,
        "span {lo}..{hi}"
    );
    // A flat curve: every point on one y, and the axis labels it 1 ms.
    let ys: Vec<f64> = gd.phase.segments[0].iter().map(|p| p.1).collect();
    assert!(ys.iter().all(|y| (y - ys[0]).abs() < 1e-9), "{ys:?}");
    assert!(gd.phase_axis.ticks.iter().any(|t| t.label == "1.0 ms"));
    assert_eq!(
        gd.phase_view_readout,
        Some("group delay \u{b7} relative to the phase reference")
    );
}

/// Unwrapped phase of the same delay falls monotonically, with no jump.
#[test]
fn unwrapped_phase_has_no_wraps() {
    let freqs: Vec<f64> = (1..=60).map(|k| 100.0 * k as f64).collect();
    let wrapped: Vec<f64> = freqs
        .iter()
        .map(|&f| (phi_raw(f, 1.0) + 180.0).rem_euclid(360.0) - 180.0)
        .collect();
    let s = scene_view(&input(freqs, wrapped, 0.0), PhaseView::Unwrapped);
    let ys: Vec<f64> = s.phase.segments[0].iter().map(|p| p.1).collect();
    assert!(ys.windows(2).all(|w| w[1] < w[0]), "not monotonic: {ys:?}");
    let (lo, hi) = s.phase_span.unwrap();
    assert!(hi - lo > 5.0 * 360.0, "span {lo}..{hi} still wrapped");
}

/// A masked gap restarts the unwrapping: the run after it starts from its
/// own wrapped value, not from one extrapolated across the gap.
#[test]
fn a_masked_gap_restarts_the_unwrapping() {
    let freqs: Vec<f64> = (1..=8).map(|k| 1000.0 * k as f64).collect();
    let phase = vec![0.0, -90.0, 180.0, 90.0, 0.0, -90.0, 180.0, 90.0];
    let mut inp = input(freqs, phase, 0.0);
    inp.coherence = vec![0.9, 0.9, 0.9, 0.1, 0.9, 0.9, 0.9, 0.9];
    let s = scene_view(&inp, PhaseView::Unwrapped);
    assert_eq!(s.phase.segments.len(), 2);
    // Run 1: 0, −90, −180; run 2 starts again at 0: 0, −90, −180, −270.
    let axis = &s.phase_axis;
    assert!(!axis.ticks.is_empty());
    let first_y = |seg: usize| s.phase.segments[seg][0].1;
    assert!(
        (first_y(0) - first_y(1)).abs() < 1e-9,
        "run 2 did not restart at its own start"
    );
}

/// A shared range fixes the axis whatever the trace spans.
#[test]
fn a_shared_phase_range_holds_the_axis() {
    let freqs: Vec<f64> = (1..=10).map(|k| 1000.0 * k as f64).collect();
    let inp = input(freqs, vec![0.0; 10], 0.0);
    let mut meters = (MeterState::default(), MeterState::default());
    let s = TransferScene::from_input(
        &inp,
        DisplayModes::new(DerotMode::Raw, Smoothing::Off)
            .with_phase_view(PhaseView::GroupDelay)
            .with_phase_range((-2.0, 6.0)),
        FREQ_RANGE,
        DB_RANGE,
        &mut meters,
        &mut FaultState::default(),
        0.0,
    );
    let labels: Vec<&str> = s
        .phase_axis
        .ticks
        .iter()
        .map(|t| t.label.as_str())
        .collect();
    assert_eq!(labels.first(), Some(&"-2 ms"));
    assert_eq!(labels.last(), Some(&"6 ms"));
    assert_eq!(
        s.phase_span,
        Some((0.0, 0.0)),
        "the span is the trace's own"
    );
}

/// Codex review: columns outside the frequency view do not set the span.
#[test]
fn off_screen_columns_do_not_set_the_phase_span() {
    let freqs: Vec<f64> = vec![100.0, 200.0, 1000.0, 1500.0, 2000.0, 9000.0];
    // Flat 1 ms group delay in view; huge slopes only outside it.
    let phase: Vec<f64> = freqs
        .iter()
        .map(|&f| {
            if (1000.0..=2000.0).contains(&f) {
                -0.36 * f
            } else {
                -50.0 * f
            }
        })
        .collect();
    let mut meters = (MeterState::default(), MeterState::default());
    let s = TransferScene::from_input(
        &input(freqs, phase, 0.0),
        DisplayModes::new(DerotMode::Raw, Smoothing::Off).with_phase_view(PhaseView::Unwrapped),
        (900.0, 2100.0),
        DB_RANGE,
        &mut meters,
        &mut FaultState::default(),
        0.0,
    );
    // In view the phase falls 0.36°/Hz over 1 kHz: a 360° span. Counting
    // the off-screen columns (50°/Hz over 9 kHz) would make it ~450 000°.
    let (lo, hi) = s.phase_span.unwrap();
    assert!(
        hi - lo <= 400.0,
        "span {lo}..{hi} set by off-screen columns"
    );
}

/// Codex recheck: a segment crossing the whole view with neither end in it
/// still sets the span, so the visible part is not clipped to ±1.
#[test]
fn a_segment_spanning_the_view_sets_the_span() {
    let mut meters = (MeterState::default(), MeterState::default());
    let s = TransferScene::from_input(
        &input(vec![100.0, 1000.0], vec![0.0, -90.0], 0.0),
        DisplayModes::new(DerotMode::Raw, Smoothing::Off).with_phase_view(PhaseView::Unwrapped),
        (300.0, 500.0),
        DB_RANGE,
        &mut meters,
        &mut FaultState::default(),
        0.0,
    );
    assert_eq!(s.phase_span, Some((-90.0, 0.0)));
}

// ─── invert and offset (Smaart's trace controls) ─────────────────────

fn scene_with(inp: &TransferInput, invert: bool, offset_db: f64) -> TransferScene {
    let mut meters = (MeterState::default(), MeterState::default());
    TransferScene::from_input(
        inp,
        DisplayModes::new(DerotMode::Session, Smoothing::Off).with_invert_offset(invert, offset_db),
        FREQ_RANGE,
        DB_RANGE,
        &mut meters,
        &mut FaultState::default(),
        0.0,
    )
}

/// y of a dB value in this file's range, the inverse of nothing: the
/// same mapping the scene uses, rebuilt from its documented affine form.
fn y_of_db(db: f64) -> f64 {
    (db - DB_RANGE.0) / (DB_RANGE.1 - DB_RANGE.0)
}

/// Inverting negates magnitude (dB) and phase; the offset adds dB after
/// it. The caption says so, and nothing else about the scene moves.
#[test]
fn invert_and_offset_transform_the_drawn_trace_and_say_so() {
    let freqs = vec![100.0, 1000.0, 10_000.0];
    let inp = input(freqs, vec![30.0, -120.0, 170.0], 0.0);
    let plain = scene_with(&inp, false, 0.0);
    let flipped = scene_with(&inp, true, 0.0);
    let moved = scene_with(&inp, true, 3.0);

    let mag_y = |s: &TransferScene| s.magnitude.segments[0][1].1;
    assert!((mag_y(&plain) - y_of_db(-6.0206)).abs() < 1e-9);
    assert!((mag_y(&flipped) - y_of_db(6.0206)).abs() < 1e-9);
    assert!((mag_y(&moved) - y_of_db(9.0206)).abs() < 1e-9);

    for (i, want) in [(0, -30.0), (1, 120.0), (2, -170.0)] {
        let got = phase_deg_at(&flipped, 0, i);
        assert!((got - want).abs() < 1e-6, "phase {i}: {got} vs {want}");
    }

    assert_eq!(plain.invert_offset_readout, None);
    assert_eq!(flipped.invert_offset_readout.as_deref(), Some("inverted"));
    assert_eq!(
        moved.invert_offset_readout.as_deref(),
        Some("inverted \u{b7} offset +3.0 dB")
    );
    assert_eq!(
        scene_with(&inp, false, -6.0)
            .invert_offset_readout
            .as_deref(),
        Some("offset -6.0 dB")
    );
    // Display only: the readouts that describe the measurement stay put.
    assert_eq!(flipped.delay_readout, plain.delay_readout);
    assert_eq!(
        flipped.magnitude.segments.len(),
        plain.magnitude.segments.len()
    );
}

/// The coherence mask gaps an inverted trace exactly where it gaps the
/// measured one: inverting must not draw a column the mask hides.
#[test]
fn an_inverted_trace_keeps_the_measured_trace_gaps() {
    let mut inp = input(vec![100.0, 1000.0, 10_000.0], vec![0.0; 3], 0.0);
    inp.coherence = vec![0.9, 0.1, 0.9];
    let plain = scene_with(&inp, false, 0.0);
    let flipped = scene_with(&inp, true, 12.0);
    let lens = |s: &TransferScene| {
        s.magnitude
            .segments
            .iter()
            .map(Vec::len)
            .collect::<Vec<_>>()
    };
    assert_eq!(lens(&flipped), lens(&plain));
    assert_eq!(lens(&plain), vec![1, 1]);
}

/// The `Shift+P` crash (operator, 2026-09-28: "index out of bounds: the
/// len is 0 but the index is 0" at the phase-view span): a column the
/// coherence mask leaves standing alone is a one-point run, which has no
/// slope. Group delay returned nothing for it and the span loop indexed
/// it. Both views, an isolated column, a run of two and a longer run.
#[test]
fn an_isolated_unmasked_column_does_not_panic_either_phase_view() {
    let freqs: Vec<f64> = (1..=12).map(|k| 100.0 * k as f64).collect();
    let phase: Vec<f64> = freqs.iter().map(|f| -0.36 * f).collect();
    let mut inp = input(freqs, phase, 0.0);
    // Kept: 0 (alone), 2–3 (a pair), 6–11 (a run). Masked: the rest.
    inp.coherence = vec![0.9, 0.1, 0.9, 0.9, 0.1, 0.1, 0.9, 0.9, 0.9, 0.9, 0.9, 0.9];
    for view in [PhaseView::Unwrapped, PhaseView::GroupDelay] {
        let s = scene_view(&inp, view);
        assert!(!s.phase.segments.is_empty(), "{view:?} drew nothing");
        assert!(s.phase_span.is_some(), "{view:?} fitted no range");
    }
}

// ─── motion easing (#716) ───────────────────────────────────────────

use ac_scene::tween::{Tween, TweenOptions, TWEEN_S};

const EASE: TweenOptions = TweenOptions {
    ease_phase: true,
    coherence_mask: 0.3,
};

fn est(mag: f64, phase: f64, coh: f64, delay_ms: f64) -> TransferInput {
    let mut i = input(vec![100.0, 1000.0], vec![phase; 2], delay_ms);
    i.magnitude_db = vec![mag; 2];
    i.coherence = vec![coh; 2];
    i
}

/// The curve eases from the drawn estimate to the new one over TWEEN_S,
/// passing through the midpoint, and is exactly the new one from then on.
/// The first estimate is drawn as is.
#[test]
fn a_new_estimate_is_reached_over_the_tween_time() {
    let mut tw = Tween::default();
    assert_eq!(
        tw.sample(&est(0.0, 0.0, 0.9, 1.0), 0.0, EASE).magnitude_db[0],
        0.0
    );
    let to = est(10.0, 40.0, 0.9, 1.0);
    let mid = tw.sample(&to, 1.0 + 0.0, EASE);
    assert_eq!(mid.magnitude_db[0], 0.0, "moved before any time passed");
    let half = tw.sample(&to, 1.0 + TWEEN_S / 2.0, EASE);
    assert!((half.magnitude_db[0] - 5.0).abs() < 1e-9);
    assert!((half.phase_deg[0] - 20.0).abs() < 1e-9);
    for t in [TWEEN_S, TWEEN_S * 3.0] {
        let s = tw.sample(&to, 1.0 + t, EASE);
        assert_eq!(s.magnitude_db, to.magnitude_db);
        assert_eq!(s.phase_deg, to.phase_deg);
    }
}

/// Phase takes the shorter arc: 170° to −170° passes through 180°, not 0°
/// — the rejected straight interpolation (computed) would sweep the pane.
#[test]
fn phase_eases_along_the_shorter_arc() {
    let mut tw = Tween::default();
    tw.sample(&est(0.0, 170.0, 0.9, 1.0), 0.0, EASE);
    tw.sample(&est(0.0, -170.0, 0.9, 1.0), 1.0, EASE); // arrives
    let half = tw.sample(&est(0.0, -170.0, 0.9, 1.0), 1.0 + TWEEN_S / 2.0, EASE);
    let p = half.phase_deg[0];
    assert!((p.abs() - 180.0).abs() < 1e-9, "went the long way: {p}");
    let straight: f64 = (170.0 + -170.0) / 2.0;
    assert!(straight.abs() < 1e-9, "the rejected path's midpoint is 0°");
}

/// A new estimate arriving mid-ease starts from where the curve is drawn,
/// not from the old target: no jump.
#[test]
fn an_estimate_mid_ease_starts_from_the_drawn_curve() {
    let mut tw = Tween::default();
    tw.sample(&est(0.0, 0.0, 0.9, 1.0), 0.0, EASE);
    tw.sample(&est(10.0, 0.0, 0.9, 1.0), 0.0, EASE);
    let drawn = tw
        .sample(&est(10.0, 0.0, 0.9, 1.0), TWEEN_S / 2.0, EASE)
        .magnitude_db[0];
    let next = tw
        .sample(&est(20.0, 0.0, 0.9, 1.0), TWEEN_S / 2.0, EASE)
        .magnitude_db[0];
    assert!((drawn - 5.0).abs() < 1e-9);
    assert!((next - drawn).abs() < 1e-9, "jumped from {drawn} to {next}");
}

/// Incomparable estimates snap: a new delay, or a new column set.
/// Coherence is never eased — the mask is the latest estimate's.
#[test]
fn a_changed_alignment_or_grid_snaps_and_coherence_is_never_eased() {
    let mut tw = Tween::default();
    tw.sample(&est(0.0, 0.0, 0.2, 1.0), 0.0, EASE);
    let s = tw.sample(&est(10.0, 0.0, 0.9, 1.0), TWEEN_S / 2.0, EASE);
    assert_eq!(s.coherence, vec![0.9; 2], "coherence was eased");
    let snapped = tw.sample(&est(30.0, 0.0, 0.9, 2.0), TWEEN_S / 2.0, EASE);
    assert_eq!(snapped.magnitude_db[0], 30.0, "eased across a delay change");
    let mut other = est(40.0, 0.0, 0.9, 2.0);
    other.freqs = vec![100.0, 2000.0];
    assert_eq!(tw.sample(&other, TWEEN_S / 2.0, EASE).magnitude_db[0], 40.0);
}

/// Codex review of #716: a column the previous estimate masked appears at
/// its new value, not easing in from the rejected one; and with phase
/// easing off (the unwrapped and group-delay views) phase is the latest
/// while magnitude still eases.
#[test]
fn an_unmasked_column_appears_at_once_and_phase_can_be_left_uneased() {
    let mut tw = Tween::default();
    let mut first = est(-80.0, 0.0, 0.9, 1.0);
    first.coherence = vec![0.9, 0.1]; // column 1 masked
    first.magnitude_db = vec![0.0, -80.0];
    tw.sample(&first, 0.0, EASE);
    let mut next = est(10.0, 90.0, 0.9, 1.0);
    next.magnitude_db = vec![10.0, 0.0];
    tw.sample(&next, 1.0, EASE);
    let half = tw.sample(&next, 1.0 + TWEEN_S / 2.0, EASE);
    assert!(
        (half.magnitude_db[0] - 5.0).abs() < 1e-9,
        "drawn column eases"
    );
    assert_eq!(half.magnitude_db[1], 0.0, "masked column eased in from -80");

    let no_phase = TweenOptions {
        ease_phase: false,
        ..EASE
    };
    let mut tw = Tween::default();
    tw.sample(&est(0.0, 0.0, 0.9, 1.0), 0.0, no_phase);
    tw.sample(&est(10.0, 90.0, 0.9, 1.0), 1.0, no_phase);
    let half = tw.sample(&est(10.0, 90.0, 0.9, 1.0), 1.0 + TWEEN_S / 2.0, no_phase);
    assert_eq!(half.phase_deg[0], 90.0);
    assert!((half.magnitude_db[0] - 5.0).abs() < 1e-9);
}
