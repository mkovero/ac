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
    derotate_deg, meter_height, DerotMode, MeterState, TransferInput, TransferScene,
};
use ac_scene::Source;

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
        meas_peak_dbfs: Some(-6.0206),
        ref_peak_dbfs: Some(-6.0206),
        channel_role: "meas_0".to_string(),
        source: Source::Live,
        sr: SR,
        // Welch-derived fixture: no per-column provenance to carry.
        column_df: Vec::new(),
        column_window_s: Vec::new(),
        n_effective: None,
    }
}

fn scene(inp: &TransferInput, derot: DerotMode) -> TransferScene {
    let mut meters = (MeterState::default(), MeterState::default());
    TransferScene::from_input(inp, derot, FREQ_RANGE, DB_RANGE, &mut meters, 0.0)
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

    assert_eq!(s.delay_readout, "2.50 ms  (0.86 m)");
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
