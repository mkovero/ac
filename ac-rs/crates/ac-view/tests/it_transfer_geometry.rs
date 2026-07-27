//! M4b AC: masked columns render as polyline gaps — a geometry test
//! asserting NO vertex exists at a masked column's x, run through the
//! real `view::draw_view` transfer path (egui_kittest shapes, no GPU),
//! not a hand-rolled substitute. The complement of ac-scene's
//! segment-split unit test: this proves the split survives to the
//! painted polyline.

use ac_scene::{DerotMode, MeterState, Source, TransferInput, TransferScene};
use ac_view::view::{draw_view, TransferViewState, ViewKind};
use egui_kittest::Harness;

const FREQ_RANGE: (f64, f64) = (20.0, 20_000.0);
const DB_RANGE: (f64, f64) = (-80.0, 20.0);
const N: usize = 20;
// Columns [MASK_LO, MASK_HI) are masked (coherence 0.3 < 0.5).
const MASK_LO: usize = 5;
const MASK_HI: usize = 10;

fn freqs() -> Vec<f64> {
    (0..N).map(|i| 100.0 * (i + 1) as f64).collect()
}

fn masked_scene() -> TransferScene {
    let mut coherence = vec![0.9; N];
    for c in coherence.iter_mut().take(MASK_HI).skip(MASK_LO) {
        *c = 0.3;
    }
    let inp = TransferInput {
        freqs: freqs(),
        magnitude_db: vec![0.0; N],
        phase_deg: vec![0.0; N],
        coherence,
        delay_ms: 0.0,
        meas_peak_dbfs: None,
        ref_peak_dbfs: None,
        channel_role: "meas_0".to_string(),
        source: Source::Live,
        sr: 48_000,
        // Welch-derived fixture: no per-column provenance to carry.
        column_df: Vec::new(),
        column_window_s: Vec::new(),
        n_effective: None,
    };
    let mut meters = (MeterState::default(), MeterState::default());
    TransferScene::from_input(
        &inp,
        DerotMode::Session,
        FREQ_RANGE,
        DB_RANGE,
        &mut meters,
        0.0,
    )
}

fn extract_line_points(shapes: &[egui::epaint::ClippedShape]) -> Vec<Vec<egui::Pos2>> {
    shapes
        .iter()
        .filter_map(|cs| match &cs.shape {
            egui::Shape::Path(path) if path.points.len() > 1 => Some(path.points.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn masked_columns_leave_a_gap_in_the_painted_transfer_polyline() {
    let scene = masked_scene();
    let view = ViewKind::Transfer(TransferViewState::default());

    let mut harness = Harness::new_ui(|ui| {
        ui.set_min_size(egui::vec2(400.0, 300.0));
        draw_view(&view, ui, None, Some(&scene));
    });
    harness.run();

    let lines = extract_line_points(&harness.output().shapes);
    // Magnitude + phase panes, each split into two segments by the mask
    // ⇒ at least four polylines. Exact count depends on tessellation.
    assert!(
        lines.len() >= 4,
        "expected split polylines, got {}",
        lines.len()
    );

    // All painted vertex x's. The extremes correspond to the first
    // (col 0) and last (col N-1) live columns, which lets us map any
    // column's normalized x onto screen x without reconstructing the
    // viewport transform — robust to pane origin/width.
    let xs: Vec<f32> = lines.iter().flatten().map(|p| p.x).collect();
    let min_x = xs.iter().cloned().fold(f32::INFINITY, f32::min);
    let max_x = xs.iter().cloned().fold(f32::NEG_INFINITY, f32::max);

    let f = freqs();
    let norm = |i: usize| ac_scene::ticks::freq_to_x(f[i], FREQ_RANGE.0, FREQ_RANGE.1) as f32;
    let (n0, n_last) = (norm(0), norm(N - 1));
    let screen_x = |i: usize| min_x + (norm(i) - n0) / (n_last - n0) * (max_x - min_x);

    // No painted vertex may sit at any masked column's screen x.
    for i in MASK_LO..MASK_HI {
        let sx = screen_x(i);
        for line in &lines {
            for p in line {
                assert!(
                    (p.x - sx).abs() > 1.0,
                    "vertex painted at masked column {i}'s x ({sx}) — the coherence \
                     gap did not survive to the polyline"
                );
            }
        }
    }

    // And the gap is real: there must be a horizontal span (between the
    // last live column before the mask and the first after) with no
    // vertices — i.e. the two segments do not visually bridge the mask.
    let gap_lo = screen_x(MASK_LO - 1);
    let gap_hi = screen_x(MASK_HI);
    let vertices_in_gap = xs
        .iter()
        .filter(|&&x| x > gap_lo + 1.0 && x < gap_hi - 1.0)
        .count();
    assert_eq!(
        vertices_in_gap, 0,
        "the mask span contains painted vertices"
    );
}
