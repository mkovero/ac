//! AC2: the geometry test, at the shape level (egui's test harness via
//! `egui_kittest`, no GPU adapter, no pixels — asserting on painted
//! `epaint::Shape`s) rather than on the pure `geometry::scene_to_screen`
//! function directly. This is the anti-Y-mirror test at the exact line
//! the old Ember bug lived on, run through the actual paint call
//! (`view::draw_view`), not a hand-rolled substitute for it.

use ac_scene::{Scene, Source};
use ac_view::view::{draw_view, SpectrumViewState, ViewKind};
use egui_kittest::Harness;

/// A minimal `Scene` with two points at known, distinct scene
/// coordinates — constructed directly (not via `Scene::from_input`,
/// which isn't `pub` outside the `SceneInput` funnel) using the
/// crate's public `Trace`/`Provenance` types, so this test only
/// depends on `ac-scene`'s public contract.
fn scene_with_known_points() -> Scene {
    // ac-scene's Scene fields are all `pub` except the two private
    // cursor-lookup arrays populated only via `from_input`; this test
    // doesn't need cursor lookups, so a direct construction covering
    // just `traces`/`freq_axis`/`db_axis`/`readouts` is enough for a
    // paint-only check. We reuse `Scene::from_input` via the public
    // `SceneInput` path instead, to stay entirely within the crate's
    // real contract rather than hand-building a struct literal that
    // could drift from it.
    let input = ac_scene::SceneInput {
        spec_freqs: vec![100.0, 1_000.0, 10_000.0],
        // Chosen so the low-freq point is the QUIET one and the
        // high-freq point is the LOUD one — a mirrored map would put
        // the loud point below the quiet one on screen instead of
        // above it, which is exactly what this test must catch.
        meas_spectrum: vec![0.01, 0.1, 0.9],
        ref_spectrum: vec![0.01, 0.1, 0.9],
        spl: None,
        spl_weighting: ac_core::visualize::weighting_curves::WeightingCurve::Z,
        spl_integration: None,
        meas_role: "meas_0".to_string(),
        ref_role: "ref".to_string(),
        source: Source::Live,
        sr: 48_000,
    };
    Scene::from_input(input, (20.0, 20_000.0), (-80.0, 0.0))
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
fn geometry_orientation_holds_through_the_actual_paint_call() {
    let scene = scene_with_known_points();
    let state = SpectrumViewState::default();
    let view = ViewKind::Spectrum(state);

    let mut harness = Harness::new_ui(|ui| {
        ui.set_min_size(egui::vec2(400.0, 300.0));
        draw_view(&view, ui, Some(&scene));
    });
    harness.run();

    let lines = extract_line_points(&harness.output().shapes);
    assert!(!lines.is_empty(), "no polyline shapes were painted");

    // The meas trace's polyline: 3 points, x ascending with frequency
    // (100Hz, 1kHz, 10kHz) and — the actual anti-mirror assertion —
    // the loudest point (10kHz, amp 0.9) must have the *smallest*
    // screen y (highest on screen), the quietest point (100Hz, amp
    // 0.01) the *largest* screen y (lowest on screen).
    let meas_line = &lines[0];
    assert_eq!(meas_line.len(), 3, "expected 3 points on the meas trace");
    assert!(
        meas_line[0].x < meas_line[1].x && meas_line[1].x < meas_line[2].x,
        "screen x must increase with frequency: {meas_line:?}"
    );
    assert!(
        meas_line[2].y < meas_line[0].y,
        "the loud (10kHz) point must render higher on screen (smaller y) than \
         the quiet (100Hz) point — orientation/anti-mirror check: {meas_line:?}"
    );
}
