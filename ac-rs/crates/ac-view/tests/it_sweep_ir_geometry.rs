//! ac-view-layer coverage for Frame C (#308), mirroring
//! `it_ir_geometry.rs`'s pattern: shape assertions against the real
//! `view::draw_sweep_ir_panel` paint path (`egui_kittest`, no GPU), not
//! a hand-rolled substitute. Not reachable through `draw_view` yet (the
//! file-open UI is #256's), so this calls the pub drawing function
//! directly — the same shape `report_flow::open_sweep_ir` will feed it
//! once that picker exists.

use ac_core::measurement::report::{
    GateParams, IntegrationParams, MeasurementData, MeasurementMethod, MeasurementPayload,
    MeasurementReport, ProcessingChain, StimulusParams, SCHEMA_VERSION,
};
use ac_scene::SweepIrFault;
use ac_view::view::draw_sweep_ir_panel;
use egui_kittest::Harness;

fn base_report() -> MeasurementReport {
    MeasurementReport {
        schema_version: SCHEMA_VERSION,
        ac_version: "0.1.0".into(),
        timestamp_utc: "2026-08-16T00:00:00Z".into(),
        method: MeasurementMethod::SweptSine {
            f1_hz: 20.0,
            f2_hz: 20_000.0,
            duration_s: 1.0,
        },
        stimulus: StimulusParams {
            sample_rate_hz: 4_000,
            f_start_hz: 20.0,
            f_stop_hz: 20_000.0,
            level_dbfs: -6.0,
            n_points: 0,
        },
        integration: IntegrationParams {
            duration_s: 1.0,
            window: "farina-inverse".into(),
            n_averages: None,
        },
        calibration: None,
        position: None,
        interface_latency: None,
        data: vec![],
        notes: None,
        processing_chain: ProcessingChain::default(),
    }
}

/// Five samples at sr=4kHz (dt=0.25ms), peak at index 1 (+1.0) and
/// trough at index 3 (-0.5) — known vertical extremes, same fixture
/// discipline `it_ir_geometry.rs::ir_scene` uses.
fn gated_report() -> MeasurementReport {
    let mut r = base_report();
    r.data.push(MeasurementPayload {
        data: MeasurementData::ImpulseResponse {
            sample_rate_hz: 4_000,
            f1_hz: 20.0,
            f2_hz: 20_000.0,
            duration_s: 1.0,
            linear_ir: vec![0.0, 1.0, 0.0, -0.5, 0.0],
            harmonics: vec![],
            noise_tail_start_s: None,
        },
        standard: vec![],
        gate: Some(GateParams {
            gate_start_s: -0.000625,
            gate_length_s: 0.00125,
            window_kind: "rectangular".into(),
            f_low_hz: 800.0,
        }),
    });
    r
}

/// Count of "a line got painted" shapes: a continuous `Shape::Path`
/// (unused here — the sweep-derived trace is always stored-source, so
/// it always dashes) plus each individual `Shape::LineSegment`, which is
/// what `Shape::dashed_line_many` actually emits per dash and what the
/// arrival marker's own vertical line is. Mirrors
/// `it_trace_distinction.rs`'s `line_like_shape_count`.
fn line_like_shape_count(shapes: &[egui::epaint::ClippedShape]) -> usize {
    shapes
        .iter()
        .filter(|cs| {
            matches!(&cs.shape, egui::Shape::Path(p) if p.points.len() > 1)
                || matches!(&cs.shape, egui::Shape::LineSegment { .. })
        })
        .count()
}

fn painted_texts(shapes: &[egui::epaint::ClippedShape]) -> Vec<String> {
    shapes
        .iter()
        .filter_map(|cs| match &cs.shape {
            egui::Shape::Text(t) => Some(t.galley.text().to_string()),
            _ => None,
        })
        .collect()
}

/// A gated report paints its header, its dashed (stored-source) h(t)
/// polyline, and its verbatim arrival readout — not the live panel's
/// solid line, and not either fault string.
#[test]
fn a_gated_report_paints_the_header_trace_and_arrival_readout() {
    let report = gated_report();
    let scene = ac_scene::SweepIrScene::from_report(&report).expect("gated report builds a scene");

    let mut harness = Harness::new_ui(|ui| {
        ui.set_min_size(egui::vec2(400.0, 300.0));
        let rect = ui.available_rect_before_wrap();
        draw_sweep_ir_panel(ui.painter(), rect, Ok(&scene));
    });
    harness.run();

    // The h(t) trace is always stored-source (#308: a sweep-derived IR
    // is never live), so it dashes into several `LineSegment` shapes
    // rather than one continuous `Path` — plus one more for the arrival
    // marker's own vertical line.
    let line_count = line_like_shape_count(&harness.output().shapes);
    assert!(
        line_count > 1,
        "expected the dashed h(t) trace plus the arrival marker line; got {line_count} line-like shapes"
    );

    let texts = painted_texts(&harness.output().shapes);
    assert!(
        texts.iter().any(|t| t == &scene.header),
        "header not painted; texts on screen: {texts:?}"
    );
    assert!(
        texts.iter().any(|t| t == &scene.arrival.text),
        "arrival readout not painted; texts on screen: {texts:?}"
    );
}

/// The two failure modes paint distinct header and detail strings —
/// never a merged "cannot open" message, and never the success frame's
/// trace.
#[test]
fn a_load_failure_paints_its_own_header_and_detail_not_a_trace() {
    for fault in [SweepIrFault::NotASweepDerivedIr, SweepIrFault::NoGate] {
        let mut harness = Harness::new_ui(|ui| {
            ui.set_min_size(egui::vec2(400.0, 300.0));
            let rect = ui.available_rect_before_wrap();
            draw_sweep_ir_panel(ui.painter(), rect, Err(fault));
        });
        harness.run();

        let texts = painted_texts(&harness.output().shapes);
        assert!(
            texts.iter().any(|t| t == fault.header()),
            "{fault:?}'s header not painted; texts on screen: {texts:?}"
        );
        assert!(
            texts.iter().any(|t| t == fault.detail()),
            "{fault:?}'s detail not painted; texts on screen: {texts:?}"
        );
        let line_count = line_like_shape_count(&harness.output().shapes);
        assert_eq!(
            line_count, 0,
            "{fault:?} painted a trace or marker line; expected header/detail text only"
        );
    }
}

/// The two failure modes are visually distinguishable from each other —
/// same requirement the UX comment states for "cannot open" collapsing
/// into one message.
#[test]
fn the_two_failure_modes_paint_different_headers() {
    assert_ne!(
        SweepIrFault::NotASweepDerivedIr.header(),
        SweepIrFault::NoGate.header()
    );
}
