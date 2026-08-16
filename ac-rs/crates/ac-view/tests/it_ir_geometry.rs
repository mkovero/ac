//! QA follow-up on #286/PR #309: the IR panel had no `ac-view`-layer
//! coverage at all — pure `ac-scene` unit tests cannot see a mis-wired
//! `Viewport` or an off-by-one in `scene_to_screen` usage in
//! `view::draw_ir_panel`. Mirrors `it_transfer_geometry.rs`'s pattern:
//! shape assertions against the real `view::draw_view` paint path
//! (`egui_kittest`, no GPU), not a hand-rolled substitute.

use ac_scene::{
    DerotMode, DisplayModes, FaultState, IrInput, IrScene, MeterState, Smoothing, Source,
    TransferInput, TransferScene,
};
use ac_view::view::{draw_view, TransferViewState, ViewKind};
use egui_kittest::Harness;

const FREQ_RANGE: (f64, f64) = (20.0, 20_000.0);
const DB_RANGE: (f64, f64) = (-80.0, 20.0);

/// A minimal transfer scene — the IR panel replaces the mag/phase panes
/// but the meters still draw off it (`draw_transfer`'s `H`-open branch),
/// so `draw_view` needs a real one, not `None`.
fn transfer_scene() -> TransferScene {
    let inp = TransferInput {
        freqs: vec![100.0, 1_000.0, 10_000.0],
        magnitude_db: vec![0.0; 3],
        phase_deg: vec![0.0; 3],
        coherence: vec![0.9; 3],
        delay_ms: 4.82,
        delay_locked: Some(true),
        speed_of_sound_m_s: None,
        meas_peak_dbfs: Some(-30.0),
        ref_peak_dbfs: Some(-14.5),
        channel_role: "meas_0".to_string(),
        source: Source::Live,
        sr: 48_000,
        column_df: Vec::new(),
        column_window_s: Vec::new(),
        column_n: Vec::new(),
        column_bins: Vec::new(),
        stages: Vec::new(),
        fault: None,
    };
    let mut meters = (MeterState::default(), MeterState::default());
    TransferScene::from_input(
        &inp,
        DisplayModes::new(DerotMode::Session, Smoothing::Off),
        FREQ_RANGE,
        DB_RANGE,
        &mut meters,
        &mut FaultState::default(),
        0.0,
    )
}

/// Five samples at dt_ms=250 span -500..+500 ms — same fixture shape as
/// `ac-scene::ir::tests::locked_input`, extended to include both a +peak
/// and a -peak sample (indices 1 and 2) so the autoscaled trace touches
/// both y=1.0 and y=0.0 — known vertical extremes a test can fit an
/// affine map to, the same way the x axis's first/last columns already
/// give known horizontal extremes (`trace_x_spans_the_frames_own_time_range_exactly`).
fn ir_scene() -> IrScene {
    let input = IrInput {
        samples: vec![0.0, 1.0, -1.0, 0.5, 0.0],
        dt_ms: 250.0,
        t_origin_ms: -500.0,
        delay_ms: 125.0, // a quarter of the way into the span
        delay_locked: Some(true),
        speed_of_sound_m_s: None,
        channel_role: "meas_0".to_string(),
        source: Source::Live,
        sr: 48_000,
    };
    IrScene::from_input(&input)
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

fn extract_line_segments(shapes: &[egui::epaint::ClippedShape]) -> Vec<[egui::Pos2; 2]> {
    shapes
        .iter()
        .filter_map(|cs| match &cs.shape {
            egui::Shape::LineSegment { points, .. } => Some(*points),
            _ => None,
        })
        .collect()
}

fn ir_view() -> ViewKind {
    let mut state = TransferViewState::default();
    state.toggle_ir_panel();
    assert!(state.ir_panel_open());
    ViewKind::Transfer(state)
}

/// The painted h(t) polyline's vertices land where `IrScene::trace`'s
/// normalized coordinates say they should — the same affine
/// `scene_to_screen` mapping `draw_ir_panel` is allowed to apply and
/// nothing more. Run through the real paint call, not
/// `scene_to_screen` called directly a second time in the test (that
/// would only prove the function agrees with itself).
#[test]
fn ir_polyline_paints_where_the_scene_says() {
    let ir = ir_scene();
    let transfer = transfer_scene();
    let view = ir_view();

    let mut harness = Harness::new_ui(|ui| {
        ui.set_min_size(egui::vec2(400.0, 300.0));
        draw_view(&view, ui, None, Some(&transfer), Some(&ir));
    });
    harness.run();

    let lines = extract_line_points(&harness.output().shapes);
    let ir_line = lines
        .iter()
        .find(|l| l.len() == ir.trace.segments[0].len())
        .unwrap_or_else(|| {
            panic!(
                "no painted polyline with {} points; got {lines:?}",
                ir.trace.segments[0].len()
            )
        });

    // Two-point affine fit per axis, from known extremes of the scene's
    // own normalized coordinates — x from the first/last samples (t
    // spans the frame's own range exactly, `trace_x_spans_the_frames_own_time_range_exactly`),
    // y from the +peak/-peak samples the fixture was built to include.
    // The pane origin/width are never reconstructed from the painted
    // output (that would only prove the function agrees with itself);
    // every other point's predicted position is a genuine prediction.
    let scene_pts = &ir.trace.segments[0];
    let (sx0, sx4) = (scene_pts[0].0, scene_pts[4].0);
    let (screen_x0, screen_x4) = (ir_line[0].x as f64, ir_line[4].x as f64);
    let (sy_peak, sy_trough) = (scene_pts[1].1, scene_pts[2].1);
    let (screen_y_peak, screen_y_trough) = (ir_line[1].y as f64, ir_line[2].y as f64);
    assert!((sx4 - sx0).abs() > 1e-9 && (sy_peak - sy_trough).abs() > 1e-9);

    let pred_x = |sx: f64| screen_x0 + (sx - sx0) / (sx4 - sx0) * (screen_x4 - screen_x0);
    let pred_y = |sy: f64| {
        screen_y_trough
            + (sy - sy_trough) / (sy_peak - sy_trough) * (screen_y_peak - screen_y_trough)
    };

    for (i, &(sx, sy)) in scene_pts.iter().enumerate() {
        let (want_x, want_y) = (pred_x(sx) as f32, pred_y(sy) as f32);
        assert!(
            (ir_line[i].x - want_x).abs() < 1.0 && (ir_line[i].y - want_y).abs() < 1.0,
            "point {i}: painted {:?}, affine fit from the scene's own known extremes \
             predicts ({want_x}, {want_y})",
            ir_line[i]
        );
    }

    // Anti-mirror, checked independently of the fit above: the scene's
    // loudest sample (index 1, y=1.0) must paint *above* its quietest
    // neighbour (index 2, y=0.0) — smaller screen y. Same class of check
    // `it_geometry.rs::geometry_orientation_holds_through_the_actual_paint_call`
    // runs for the spectrum view; the IR panel gets its own because it's
    // a different viewport and a different drawing function.
    assert!(
        ir_line[1].y < ir_line[2].y,
        "peak sample did not paint above the trough: {ir_line:?}"
    );
}

/// The arrival marker's vertical line lands at `scene.arrival.position`,
/// mapped through the same viewport the polyline used — not at an
/// eyeballed offset.
#[test]
fn arrival_marker_line_lands_at_the_scenes_position() {
    let ir = ir_scene();
    let transfer = transfer_scene();
    let view = ir_view();

    let mut harness = Harness::new_ui(|ui| {
        ui.set_min_size(egui::vec2(400.0, 300.0));
        draw_view(&view, ui, None, Some(&transfer), Some(&ir));
    });
    harness.run();

    let lines = extract_line_points(&harness.output().shapes);
    let ir_line = lines
        .iter()
        .find(|l| l.len() == ir.trace.segments[0].len())
        .expect("ir polyline");
    let xs: Vec<f32> = ir_line.iter().map(|p| p.x).collect();
    let min_x = xs.iter().cloned().fold(f32::INFINITY, f32::min);
    let max_x = xs.iter().cloned().fold(f32::NEG_INFINITY, f32::max);

    // `IrScene::from_input` puts the arrival at delay_ms=125 over a
    // -500..+500 ms span, i.e. normalized x = 0.625 — distinct from any
    // of the five trace samples (0.0/0.25/0.5/0.75/1.0), so the vertical
    // line under test isn't accidentally coincident with a polyline
    // vertex.
    let expected_norm_x = ac_scene::ticks::time_to_x(125.0, -500.0, 500.0);
    assert!((ir.arrival.position - expected_norm_x).abs() < 1e-9);

    let segments = extract_line_segments(&harness.output().shapes);
    let want_screen_x = min_x + (ir.arrival.position as f32) * (max_x - min_x);
    let hit = segments
        .iter()
        .find(|seg| (seg[0].x - want_screen_x).abs() < 1.5 && (seg[0].x - seg[1].x).abs() < 0.5);
    assert!(
        hit.is_some(),
        "no vertical line segment found near x={want_screen_x} (arrival position {}); \
         segments: {segments:?}",
        ir.arrival.position
    );
}

/// The arrival marker text (`ArrivalMarker::text`, verbatim
/// `format_delay_readout` output) reaches the painted output — not just
/// the position.
#[test]
fn arrival_marker_text_is_painted_verbatim() {
    let ir = ir_scene();
    let transfer = transfer_scene();
    let view = ir_view();

    let mut harness = Harness::new_ui(|ui| {
        ui.set_min_size(egui::vec2(400.0, 300.0));
        draw_view(&view, ui, None, Some(&transfer), Some(&ir));
    });
    harness.run();

    let texts: Vec<String> = harness
        .output()
        .shapes
        .iter()
        .filter_map(|cs| match &cs.shape {
            egui::Shape::Text(t) => Some(t.galley.text().to_string()),
            _ => None,
        })
        .collect();
    assert!(
        texts.iter().any(|t| t == &ir.arrival.text),
        "arrival text {:?} not painted; texts on screen: {texts:?}",
        ir.arrival.text
    );
    assert!(
        texts.iter().any(|t| t == ir.header),
        "IR_HEADER not painted; texts on screen: {texts:?}"
    );
}

/// `H` toggling the panel actually changes what's painted (not just a
/// state bool, same discipline as
/// `it_geometry.rs::ref_trace_toggle_removes_a_polyline_through_the_paint_path`):
/// with the panel open the mag/phase polylines (2 points each on this
/// 3-frequency fixture) are gone, replaced by the IR polyline (5
/// points).
#[test]
fn ir_panel_toggle_replaces_mag_phase_polylines_through_the_paint_path() {
    let transfer = transfer_scene();
    let ir = ir_scene();

    let closed = ViewKind::Transfer(TransferViewState::default());
    let mut h_closed = Harness::new_ui(|ui| {
        ui.set_min_size(egui::vec2(400.0, 300.0));
        draw_view(&closed, ui, None, Some(&transfer), None);
    });
    h_closed.run();
    let closed_lines = extract_line_points(&h_closed.output().shapes);
    // Two panes (mag + phase), 3 points each, no gaps in this fixture.
    assert!(
        closed_lines.iter().any(|l| l.len() == 3),
        "expected a 3-point mag/phase polyline with the panel closed: {closed_lines:?}"
    );
    assert!(
        !closed_lines
            .iter()
            .any(|l| l.len() == ir.trace.segments[0].len()),
        "an IR-shaped polyline painted while the panel was closed"
    );

    let open = ir_view();
    let mut h_open = Harness::new_ui(|ui| {
        ui.set_min_size(egui::vec2(400.0, 300.0));
        draw_view(&open, ui, None, Some(&transfer), Some(&ir));
    });
    h_open.run();
    let open_lines = extract_line_points(&h_open.output().shapes);
    assert!(
        !open_lines.iter().any(|l| l.len() == 3),
        "a mag/phase-shaped polyline still painted with the panel open: {open_lines:?}"
    );
    assert!(
        open_lines
            .iter()
            .any(|l| l.len() == ir.trace.segments[0].len()),
        "no IR-shaped polyline painted with the panel open: {open_lines:?}"
    );
}
