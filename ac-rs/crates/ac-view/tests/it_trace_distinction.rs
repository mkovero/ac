//! UX review fix verification: meas/ref and live/snapshot traces must
//! be visually distinguished (deliverable 6, D15) — checked at the
//! same shape level as the geometry test, not eyeballed.

use ac_core::visualize::weighting_curves::WeightingCurve;
use ac_scene::{Scene, SceneInput, Source};
use ac_view::view::{draw_view, SpectrumViewState, ViewKind};
use egui::epaint::ColorMode;
use egui_kittest::Harness;

fn scene(source: Source) -> Scene {
    let input = SceneInput {
        spec_freqs: vec![100.0, 1_000.0, 10_000.0],
        meas_spectrum: vec![0.01, 0.1, 0.9],
        ref_spectrum: vec![0.01, 0.1, 0.9],
        spl: None,
        spl_weighting: WeightingCurve::Z,
        spl_integration: None,
        meas_role: "meas_0".to_string(),
        ref_role: "ref".to_string(),
        source,
        sr: 48_000,
    };
    Scene::from_input(input, (20.0, 20_000.0), (-80.0, 0.0))
}

fn path_shapes(shapes: &[egui::epaint::ClippedShape]) -> Vec<&egui::epaint::PathShape> {
    shapes
        .iter()
        .filter_map(|cs| match &cs.shape {
            egui::Shape::Path(p) if p.points.len() > 1 => Some(p),
            _ => None,
        })
        .collect()
}

/// Count of "a line got painted" shapes, counting both a continuous
/// `Shape::Path` (the live/solid case) and each individual
/// `Shape::LineSegment` (what `Shape::dashed_line_many` actually emits
/// per dash — not a `Path`, confirmed by reading epaint's
/// `dashes_from_line` source rather than assumed).
fn line_like_shape_count(shapes: &[egui::epaint::ClippedShape]) -> usize {
    shapes
        .iter()
        .filter(|cs| {
            matches!(&cs.shape, egui::Shape::Path(p) if p.points.len() > 1)
                || matches!(&cs.shape, egui::Shape::LineSegment { .. })
        })
        .count()
}

fn solid_color(stroke: &egui::epaint::PathStroke) -> egui::Color32 {
    match stroke.color {
        ColorMode::Solid(c) => c,
        _ => panic!("expected a solid stroke color"),
    }
}

#[test]
fn meas_and_ref_traces_use_different_colors() {
    let scene = scene(Source::Live);
    let view = ViewKind::Spectrum(SpectrumViewState::default());

    let mut harness = Harness::new_ui(|ui| {
        ui.set_min_size(egui::vec2(400.0, 300.0));
        draw_view(&view, ui, Some(&scene));
    });
    harness.run();

    let paths = path_shapes(&harness.output().shapes);
    assert_eq!(paths.len(), 2, "expected one path per trace (meas, ref)");

    let meas_color = solid_color(&paths[0].stroke);
    let ref_color = solid_color(&paths[1].stroke);
    assert_ne!(
        meas_color, ref_color,
        "meas and ref traces must not share a color (UX review, finding 2)"
    );
    // Neither may be the forbidden green (UX review, finding 1).
    for c in [meas_color, ref_color] {
        assert!(
            !(c.g() > c.r() && c.g() > c.b()),
            "trace color {c:?} looks green-dominant — forbidden by the UX palette rule"
        );
    }
}

#[test]
fn snapshot_traces_paint_as_multiple_dash_segments_live_traces_as_one_solid_path() {
    let live_view = ViewKind::Spectrum(SpectrumViewState::default());
    let live_scene = scene(Source::Live);
    let mut live_harness = Harness::new_ui(|ui| {
        ui.set_min_size(egui::vec2(400.0, 300.0));
        draw_view(&live_view, ui, Some(&live_scene));
    });
    live_harness.run();
    let live_count = line_like_shape_count(&live_harness.output().shapes);
    // One continuous polyline per trace when live.
    assert_eq!(live_count, 2);

    let snap_view = ViewKind::Spectrum(SpectrumViewState::default());
    let snap_scene = scene(Source::Snapshot);
    let mut snap_harness = Harness::new_ui(|ui| {
        ui.set_min_size(egui::vec2(400.0, 300.0));
        draw_view(&snap_view, ui, Some(&snap_scene));
    });
    snap_harness.run();
    let snap_count = line_like_shape_count(&snap_harness.output().shapes);
    // Dashing splits each trace into multiple short segments instead of
    // one continuous path — strictly more line-like shapes than the
    // live case's one-Path-per-trace.
    assert!(
        snap_count > live_count,
        "snapshot-sourced traces must render as dashed (more, shorter line \
         segments) — live={live_count} snapshot={snap_count}"
    );
}
