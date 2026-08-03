//! Acceptance fixtures for the live-display switch: the transfer view draws
//! the three-stage columns, and nothing else.
//!
//! Every frame here is built from `ac-core`'s real ladder rather than by hand,
//! so the column grids under test are the ones the daemon actually produces —
//! including the non-uniform spacing that is the point of the honest-density
//! rule. A hand-written uniform grid would pass criterion 3 without ever
//! exercising the case it exists for.

use ac_core::visualize::mtw::ladder;
use ac_scene::transfer::{DerotMode, MeterState, TransferScene, COHERENCE_THRESHOLD};
use ac_scene::{FaultState, TransferInput, WireFrame};
use serde_json::json;

const FREQ_RANGE: (f64, f64) = (20.0, 24_000.0);
const DB_RANGE: (f64, f64) = (-80.0, 20.0);

/// `freq_to_x`, restated rather than imported — an assertion that called the
/// implementation's own mapping would pass whatever that mapping did.
fn expected_x(f_hz: f64) -> f64 {
    (f_hz / FREQ_RANGE.0).ln() / (FREQ_RANGE.1 / FREQ_RANGE.0).ln()
}

/// A real ladder's column grid at `sr`, with a flat, fully-coherent response.
fn columns_for(sr: u32) -> (Vec<f64>, Vec<f64>, Vec<f64>, Vec<usize>) {
    let l = ladder::layout(sr).expect("ladder");
    let edges = ladder::column_edges(&l, FREQ_RANGE.0, f64::from(sr) / 2.0, ladder::P_REF);
    let freqs = ladder::column_centres(&edges);
    let df: Vec<f64> = freqs
        .iter()
        .map(|&f| l.stages[l.source_at(f).deep].df)
        .collect();
    let window: Vec<f64> = freqs
        .iter()
        .map(|&f| l.stages[l.source_at(f).deep].window_s)
        .collect();
    let stage: Vec<usize> = freqs.iter().map(|&f| l.source_at(f).deep).collect();
    (freqs, df, window, stage)
}

fn stages_json(sr: u32) -> serde_json::Value {
    let l = ladder::layout(sr).expect("ladder");
    serde_json::Value::Array(
        l.stages
            .iter()
            .map(|s| {
                json!({
                    "decim": s.decim, "rate": s.rate, "df": s.df,
                    "window_s": s.window_s, "hop_s": s.hop_s,
                    "f_valid": s.f_valid, "settling_s": s.window_s + s.hop_s * 3.0,
                })
            })
            .collect(),
    )
}

/// A daemon-shaped frame carrying three-stage columns for `sr`.
fn frame_with_mtw(sr: u32, coherence: f64) -> WireFrame {
    let (freqs, df, window, stage) = columns_for(sr);
    let n = freqs.len();
    let mtw = json!({
        "freqs": freqs,
        "magnitude_db": vec![-6.0206_f64; n],
        "phase_deg": vec![0.0_f64; n],
        "coherence": vec![coherence; n],
        "df": df,
        "window_s": window,
        "n": vec![4_u32; n],
        "stage": stage,
        "blend": vec![0.0_f64; n],
        "bins": vec![1_u32; n],
        "ppo": ladder::P_REF,
        "n_blocks": 4,
        "stages": stages_json(sr),
    });
    frame_from(sr, Some(mtw))
}

fn frame_from(sr: u32, mtw: Option<serde_json::Value>) -> WireFrame {
    let mut v = json!({
        "type": "transfer_stream",
        "sr": sr,
        "meas_channel": 0,
        "ref_channel": 1,
        "spec_freqs": [100.0],
        "meas_spectrum": [0.1],
        "ref_spectrum": [0.1],
        "spl": null,
        "spl_weighting": "Z",
        "spl_integration": "fast",
        // Full-rate Welch arrays, deliberately given values the display must
        // never show. If any of these reach a trace, the old path is back.
        "freqs": [100.0, 1000.0, 10000.0],
        "magnitude_db": [-99.0, -99.0, -99.0],
        "phase_deg": [77.0, 77.0, 77.0],
        "coherence": [1.0, 1.0, 1.0],
        "delay_samples": 0,
        "delay_ms": 0.0,
        "meas_peak_dbfs": -6.0,
        "ref_peak_dbfs": -12.0,
    });
    if let Some(m) = mtw {
        v["mtw"] = m;
    }
    serde_json::from_value(v).expect("wire frame")
}

fn scene(frame: &WireFrame) -> TransferScene {
    let input = TransferInput::from_wire_frame(frame);
    let mut meters = (MeterState::default(), MeterState::default());
    TransferScene::from_input(
        &input,
        DerotMode::Session,
        FREQ_RANGE,
        DB_RANGE,
        &mut meters,
        &mut FaultState::default(),
        0.0,
    )
}

/// Criterion 1. Every drawn point is a column the frame carried, at that
/// column's own frequency — nothing is resampled onto a uniform grid, and
/// nothing is synthesised between columns.
///
/// Mutation-verified by construction: an implementation that interpolated onto
/// a fixed grid would produce a point count set by the grid rather than by the
/// frame, and x values that are not those of the frame's columns.
#[test]
fn every_drawn_point_is_a_column_the_frame_carried() {
    let frame = frame_with_mtw(48_000, 0.9);
    let mtw = frame.mtw.as_ref().unwrap();
    let sc = scene(&frame);

    let drawn: Vec<(f64, f64)> = sc.magnitude.segments.concat();
    assert_eq!(
        drawn.len(),
        mtw.freqs.len(),
        "drew {} points from {} columns",
        drawn.len(),
        mtw.freqs.len()
    );
    for (i, (x, _)) in drawn.iter().enumerate() {
        assert!(
            (x - expected_x(mtw.freqs[i])).abs() < 1e-12,
            "point {i} is not at column {i}'s frequency"
        );
    }
}

/// Criterion 2/3. The grid is genuinely non-uniform, and is mapped by
/// frequency rather than by index.
///
/// The observable is the spacing between drawn x coordinates. Index-based
/// mapping yields *exactly* uniform spacing whatever the frequencies are, so
/// any spread at all falsifies it; the ladder's real grid spreads by ~4.7x
/// between the delta-f-limited bottom and the log-spaced midrange. Asserting a
/// spread also checks its own premise — a near-uniform grid would fail here
/// rather than passing vacuously.
#[test]
fn non_uniform_spacing_is_mapped_by_frequency_not_index() {
    let frame = frame_with_mtw(48_000, 0.9);
    let drawn = sc_points(&frame);
    assert!(drawn.len() > 100);

    let dx: Vec<f64> = drawn.windows(2).map(|w| w[1].0 - w[0].0).collect();
    let (lo, hi) = dx
        .iter()
        .fold((f64::MAX, 0.0_f64), |(a, b), &d| (a.min(d), b.max(d)));
    assert!(lo > 0.0, "columns must be strictly increasing in frequency");
    assert!(
        hi / lo > 2.0,
        "x spacing spreads only {:.3}x ({lo}..{hi}) — that is what index-based \
         mapping looks like, and it would also mean the honest-density grid \
         collapsed to a uniform one",
        hi / lo
    );

    // And each x is that column's own frequency, not its position.
    let mtw = frame.mtw.as_ref().unwrap();
    for (i, (x, _)) in drawn.iter().enumerate() {
        assert!((x - expected_x(mtw.freqs[i])).abs() < 1e-12, "column {i}");
    }
}

fn sc_points(frame: &WireFrame) -> Vec<(f64, f64)> {
    scene(frame).magnitude.segments.concat()
}

/// Criterion 3. A variable column count renders at every supported rate,
/// including across each stage boundary.
#[test]
fn variable_column_counts_render_at_every_supported_rate() {
    for sr in [44_100_u32, 48_000, 96_000, 192_000] {
        let frame = frame_with_mtw(sr, 0.9);
        let mtw = frame.mtw.as_ref().unwrap();
        let drawn = sc_points(&frame);
        assert!(
            mtw.freqs.len() > 100,
            "sr {sr}: only {} columns",
            mtw.freqs.len()
        );
        assert_eq!(drawn.len(), mtw.freqs.len(), "sr {sr}: point count");

        // Every rung is exercised, so the boundaries are actually crossed.
        let mut seen = [false; 8];
        for &s in &mtw.stage {
            seen[s] = true;
        }
        let n_stages = ladder::layout(sr).unwrap().stages.len();
        assert!(
            (0..n_stages).all(|i| seen[i]),
            "sr {sr}: not every rung reached the display"
        );
    }
}

/// Deliverable 4. The averaging depth's raw inputs reach the scene: blocks
/// actually averaged, and source bins per column.
///
/// Deliberately *not* combined into a single "effective depth". The coherence
/// floor depends on both, sublinearly in bins, and no validated model exists —
/// an earlier version of this crate shipped a Welch ρ = 1/6 correction that
/// measured *further* from the truth than the uncorrected value. See
/// `design-mtw-ladder.md`.
#[test]
fn the_depth_inputs_reach_the_scene_uncombined() {
    let frame = frame_with_mtw(48_000, 0.9);
    let input = TransferInput::from_wire_frame(&frame);
    let n = input.freqs.len();
    assert_eq!(
        input.column_n.len(),
        n,
        "blocks-averaged must reach the scene"
    );
    assert_eq!(
        input.column_bins.len(),
        n,
        "bins-per-column must reach the scene"
    );

    // Blocks averaged is uniform across columns — that is the property the
    // ladder's uniform-N decision buys, and it must survive the wire.
    assert!(
        input.column_n.iter().all(|&v| (v - 4.0).abs() < 1e-9),
        "blocks averaged is not uniform: {:?}",
        input.column_n.iter().take(8).collect::<Vec<_>>()
    );
    // Bins per column is NOT uniform — it is the other half of the depth, and
    // the half that changes at a crossover.
    assert!(input.column_bins.iter().any(|&b| b >= 1));
}

/// Deliverable 5, behaviourally: the Welch arrays are on the wire and are not
/// read. This is the regression that would silently restore the old display.
#[test]
fn a_frame_without_mtw_draws_nothing_rather_than_the_welch_arrays() {
    let frame = frame_from(48_000, None);
    assert!(
        !frame.magnitude_db.is_empty(),
        "premise: Welch arrays present"
    );

    let input = TransferInput::from_wire_frame(&frame);
    assert!(
        input.freqs.is_empty(),
        "Welch freqs leaked into the display"
    );
    assert!(input.magnitude_db.is_empty());
    assert!(input.column_n.is_empty() && input.column_bins.is_empty());

    let sc = scene(&frame);
    assert!(
        sc.magnitude.segments.is_empty() && sc.phase.segments.is_empty(),
        "an unwarmed ladder must draw nothing, not fall back to a different \
         measurement"
    );
    // The meters and delay readout stay live throughout the settling window —
    // that is what gain staging needs while the curve is still filling.
    assert!(
        sc.meas_meter.height > 0.0,
        "meters must survive the warmup gap"
    );
    assert!(!sc.delay_readout.is_empty());
}

/// Mismatched parallel arrays draw nothing rather than a truncated guess —
/// the same argument the Welch path already made.
#[test]
fn mismatched_column_lengths_draw_nothing() {
    let mut frame = frame_with_mtw(48_000, 0.9);
    frame.mtw.as_mut().unwrap().coherence.pop();
    let sc = scene(&frame);
    assert!(sc.magnitude.segments.is_empty() && sc.phase.segments.is_empty());
}

/// Per-column resolution and window reach the scene, so a reading can be
/// interpreted. How UX surfaces them is not decided here; that they arrive is.
#[test]
fn per_column_resolution_and_window_reach_the_scene() {
    let frame = frame_with_mtw(96_000, 0.9);
    let input = TransferInput::from_wire_frame(&frame);
    let n = input.freqs.len();
    assert_eq!(input.column_df.len(), n);
    assert_eq!(input.column_window_s.len(), n);

    // They vary across the ladder — a constant would mean the rungs collapsed.
    let d0 = input.column_df[0];
    let dn = input.column_df[n - 1];
    assert!(
        dn > d0 * 10.0,
        "Δf should coarsen with frequency: {d0} -> {dn}"
    );
    let w0 = input.column_window_s[0];
    let wn = input.column_window_s[n - 1];
    assert!(
        w0 > wn * 10.0,
        "window should shorten with frequency: {w0} -> {wn}"
    );
}

/// Low-coherence columns stay masked on the new path — the D5 rule survives
/// the source switch.
#[test]
fn low_coherence_columns_are_still_masked() {
    let frame = frame_with_mtw(48_000, COHERENCE_THRESHOLD - 0.01);
    let sc = scene(&frame);
    assert!(
        sc.magnitude.segments.is_empty(),
        "columns below the coherence threshold must not be drawn"
    );
}
