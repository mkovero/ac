//! Acceptance fixtures for fractional-octave smoothing of the transfer view
//! (#229).
//!
//! The column grid comes from `ac-core`'s real ladder, so the windows under
//! test slide over the spacing the daemon actually produces — including the
//! places where the grid thins because a rung cannot resolve the requested
//! density. A uniform hand-written grid would let an index-based window pass.

use ac_core::visualize::mtw::ladder;
use ac_scene::transfer::{
    DerotMode, DisplayModes, MeterState, Smoothing, TransferScene, COHERENCE_THRESHOLD,
};
use ac_scene::{FaultState, TransferInput, WireFrame};
use serde_json::json;

const FREQ_RANGE: (f64, f64) = (20.0, 24_000.0);
const DB_RANGE: (f64, f64) = (-80.0, 20.0);
const SR: u32 = 48_000;

/// Inverse of `db_to_y`, restated rather than imported — an assertion that
/// called the implementation's own mapping would pass whatever it did.
fn y_to_db(y: f64) -> f64 {
    DB_RANGE.0 + y * (DB_RANGE.1 - DB_RANGE.0)
}

/// Inverse of `phase_to_y` over the fixed ±180° pane.
fn y_to_deg(y: f64) -> f64 {
    (y - 0.5) * 360.0
}

fn column_freqs() -> Vec<f64> {
    let l = ladder::layout(SR).expect("ladder");
    let edges = ladder::column_edges(&l, FREQ_RANGE.0, f64::from(SR) / 2.0, ladder::P_REF);
    ladder::column_centres(&edges)
}

/// A daemon-shaped frame over the real column grid, with per-column
/// magnitude, phase and coherence supplied by the caller.
fn frame(magnitude_db: Vec<f64>, phase_deg: Vec<f64>, coherence: Vec<f64>) -> WireFrame {
    let freqs = column_freqs();
    let n = freqs.len();
    assert_eq!(magnitude_db.len(), n);
    let mtw = json!({
        "freqs": freqs,
        "magnitude_db": magnitude_db,
        "phase_deg": phase_deg,
        "coherence": coherence,
        "df": vec![1.0_f64; n],
        "window_s": vec![1.0_f64; n],
        "n": vec![4_u32; n],
        "stage": vec![0_usize; n],
        "blend": vec![0.0_f64; n],
        "bins": vec![1_u32; n],
        "ppo": ladder::P_REF,
        "n_blocks": 4,
        "stages": [],
    });
    let v = json!({
        "type": "transfer_stream",
        "sr": SR,
        "meas_channel": 0,
        "ref_channel": 1,
        "spec_freqs": [100.0],
        "meas_spectrum": [0.1],
        "ref_spectrum": [0.1],
        "spl": null,
        "spl_weighting": "Z",
        "spl_integration": "fast",
        "freqs": [100.0],
        "magnitude_db": [-99.0],
        "phase_deg": [77.0],
        "coherence": [1.0],
        "delay_samples": 0,
        "delay_ms": 0.0,
        "meas_peak_dbfs": -6.0,
        "ref_peak_dbfs": -12.0,
        "mtw": mtw,
    });
    serde_json::from_value(v).expect("wire frame")
}

fn scene(f: &WireFrame, smoothing: Smoothing) -> TransferScene {
    let input = TransferInput::from_wire_frame(f);
    let mut meters = (MeterState::default(), MeterState::default());
    TransferScene::from_input(
        &input,
        DisplayModes::new(DerotMode::Session, smoothing),
        FREQ_RANGE,
        DB_RANGE,
        &mut meters,
        &mut FaultState::default(),
        0.0,
    )
}

/// Column-to-column excursion of the drawn magnitude, in dB.
fn ripple_db(s: &TransferScene) -> f64 {
    s.magnitude
        .segments
        .iter()
        .flat_map(|seg| seg.windows(2))
        .map(|w| (y_to_db(w[1].1) - y_to_db(w[0].1)).abs())
        .fold(0.0, f64::max)
}

/// A ±6 dB column-to-column ripple on a −20 dB curve — the readability
/// problem smoothing exists for.
fn ripple_fixture(coherence: Vec<f64>) -> WireFrame {
    let n = column_freqs().len();
    let mag: Vec<f64> = (0..n)
        .map(|i| if i % 2 == 0 { -14.0 } else { -26.0 })
        .collect();
    frame(mag, vec![0.0; n], coherence)
}

#[test]
fn off_leaves_the_trace_exactly_as_measured() {
    let n = column_freqs().len();
    let f = ripple_fixture(vec![0.9; n]);
    let s = scene(&f, Smoothing::Off);
    assert!(
        (ripple_db(&s) - 12.0).abs() < 1e-9,
        "off altered the trace: ripple {} dB",
        ripple_db(&s)
    );
    assert_eq!(
        s.smoothing_readout, None,
        "an unaltered trace must carry no smoothing caption"
    );
}

#[test]
fn wider_smoothing_flattens_the_trace_monotonically() {
    let n = column_freqs().len();
    let f = ripple_fixture(vec![0.9; n]);
    let mut prev = ripple_db(&scene(&f, Smoothing::Off));
    for s in [
        Smoothing::Oct24,
        Smoothing::Oct12,
        Smoothing::Oct6,
        Smoothing::Oct3,
        Smoothing::Oct1,
    ] {
        let r = ripple_db(&scene(&f, s));
        assert!(
            r <= prev + 1e-9,
            "{s:?} roughened the trace: {r} dB against {prev} dB"
        );
        prev = r;
    }
    // And it is a real reduction, not a rounding-scale one. The bound is
    // stated as a ratio rather than an absolute dB figure because the ladder
    // grid is NOT uniformly 1/48 octave: at 48 kHz its column spacing runs
    // 1.010 to 1.048, so a 1/6-octave window holds anywhere from 2 to 8
    // columns and flattens a ±6 dB alternation by correspondingly different
    // amounts along the axis. An absolute threshold here would be a claim
    // about the densest part of the grid, asserted over all of it.
    let off = ripple_db(&scene(&f, Smoothing::Off));
    let oct6 = ripple_db(&scene(&f, Smoothing::Oct6));
    assert!(
        oct6 <= off / 2.0,
        "1/6 octave barely moved the ripple: {oct6} dB against {off} dB"
    );
}

#[test]
fn smoothing_does_not_move_the_coherence_gaps() {
    // Coherence is not smoothed, so the masked band gaps the trace in exactly
    // the same place at every setting. If smoothing ever reached the mask, a
    // heavy setting would close the gap — an untrusted column drawn as
    // measurement, which is the one direction this control must not fail in.
    let n = column_freqs().len();
    let mut coh = vec![0.9; n];
    for c in coh.iter_mut().take(n / 2 + 6).skip(n / 2) {
        *c = COHERENCE_THRESHOLD - 0.2;
    }
    let f = ripple_fixture(coh);

    let off = scene(&f, Smoothing::Off);
    let shape = |s: &TransferScene| -> Vec<usize> {
        s.magnitude.segments.iter().map(|seg| seg.len()).collect()
    };
    let xs = |s: &TransferScene| -> Vec<f64> {
        s.magnitude
            .segments
            .iter()
            .flat_map(|seg| seg.iter().map(|p| p.0))
            .collect()
    };
    assert_eq!(shape(&off).len(), 2, "fixture must gap the trace");

    for sm in [Smoothing::Oct24, Smoothing::Oct6, Smoothing::Oct1] {
        let s = scene(&f, sm);
        assert_eq!(shape(&s), shape(&off), "{sm:?} changed the segment layout");
        assert_eq!(xs(&s), xs(&off), "{sm:?} moved a column's frequency");
    }
}

#[test]
fn a_masked_column_cannot_pull_the_drawn_trace() {
    // Two fixtures identical except for the magnitude of columns the display
    // masks out. Smoothing must draw the same trace from both: an untrusted
    // column contributes nothing, rather than being averaged in at reduced
    // weight.
    let n = column_freqs().len();
    let mut coh = vec![0.9; n];
    for c in coh.iter_mut().take(n / 2 + 3).skip(n / 2) {
        *c = COHERENCE_THRESHOLD - 0.2;
    }
    let flat: Vec<f64> = vec![-20.0; n];
    let mut wild = flat.clone();
    for m in wild.iter_mut().take(n / 2 + 3).skip(n / 2) {
        *m = 40.0;
    }

    let a = scene(&frame(flat, vec![0.0; n], coh.clone()), Smoothing::Oct6);
    let b = scene(&frame(wild, vec![0.0; n], coh), Smoothing::Oct6);
    assert_eq!(
        a.magnitude.segments, b.magnitude.segments,
        "a masked column reached the drawn magnitude"
    );
}

#[test]
fn phase_is_smoothed_without_crossing_the_wrap() {
    // Alternating ±170°: the mean of any pair is ±180°, and an average taken
    // on wrapped values would return 0° — a half-turn error, drawn in the
    // middle of the pane where it looks like a well-behaved measurement.
    let n = column_freqs().len();
    let phase: Vec<f64> = (0..n)
        .map(|i| if i % 2 == 0 { 170.0 } else { -170.0 })
        .collect();
    let f = frame(vec![-20.0; n], phase, vec![0.9; n]);

    let s = scene(&f, Smoothing::Oct6);
    for seg in &s.phase.segments {
        for p in seg {
            let deg = y_to_deg(p.1);
            // The failure mode is a collapse toward 0°, half a turn away.
            // The surviving value is not pinned to 180° because the window
            // holds an odd or even number of columns depending on where the
            // ladder grid is dense (2 to 8 columns at 1/6 octave), so the
            // mean of the alternation lands between about ±173° and ±180°.
            assert!(
                deg.abs() > 150.0,
                "smoothed phase collapsed across the wrap: {deg}°"
            );
        }
    }
}

#[test]
fn the_readout_names_the_active_setting() {
    let n = column_freqs().len();
    let f = ripple_fixture(vec![0.9; n]);
    for (sm, expected) in [
        (Smoothing::Off, None),
        (Smoothing::Oct24, Some("smoothing 1/24 octave")),
        (Smoothing::Oct12, Some("smoothing 1/12 octave")),
        (Smoothing::Oct6, Some("smoothing 1/6 octave")),
        (Smoothing::Oct3, Some("smoothing 1/3 octave")),
        (Smoothing::Oct1, Some("smoothing 1/1 octave")),
    ] {
        assert_eq!(scene(&f, sm).smoothing_readout, expected, "{sm:?}");
    }
}

#[test]
fn the_cycle_visits_every_designator_and_returns_to_off() {
    let mut s = Smoothing::Off;
    let mut seen = Vec::new();
    for _ in 0..6 {
        s = s.next();
        seen.push(s.bpo());
    }
    assert_eq!(
        seen,
        vec![Some(24), Some(12), Some(6), Some(3), Some(1), None],
        "cycle order must smooth progressively more, then return to off"
    );
}
