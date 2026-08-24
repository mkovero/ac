//! #245: the shell's connection banner sits on the row directly above the
//! plot, and the topmost y-axis tick label is drawn centred on the pane's
//! top edge — half a line of it above the rect the view was handed. The
//! two collided: the banner's descenders struck through the `20` of the
//! +20 dB tick.
//!
//! The regression test is a composition test, not a layout-constant test:
//! it paints the banner label and the view into one `Ui` exactly as
//! `AcViewApp::update` does, then asserts no text the view paints reaches
//! up into the banner's rect. Asserting on the reserved space itself would
//! pass on any constant, including one too small to clear the glyphs.

use ac_scene::{
    DerotMode, DisplayModes, FaultState, MeterState, Smoothing, Source, TransferInput,
    TransferScene,
};
use ac_view::view::{draw_view, SpectrumViewState, TransferViewState, ViewKind};
use egui_kittest::Harness;

const BANNER: &str = "live — 127.0.0.1:5556";
const FREQ_RANGE: (f64, f64) = (20.0, 20_000.0);
const DB_RANGE: (f64, f64) = (-80.0, 20.0);
const N: usize = 20;

fn freqs() -> Vec<f64> {
    (0..N).map(|i| 100.0 * (i + 1) as f64).collect()
}

fn transfer_scene() -> TransferScene {
    let inp = TransferInput {
        freqs: freqs(),
        magnitude_db: vec![0.0; N],
        phase_deg: vec![0.0; N],
        coherence: vec![0.9; N],
        delay_ms: 0.0,
        delay_locked: Some(true),
        meas_channel: 0,
        ref_channel: 1,
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

fn spectrum_scene() -> ac_scene::Scene {
    let input = ac_scene::SceneInput {
        spec_freqs: vec![100.0, 1_000.0, 10_000.0],
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
    ac_scene::Scene::from_input(input, FREQ_RANGE, (-80.0, 0.0))
}

/// Every painted text as (string, screen rect). A `TextShape`'s `pos` is
/// already the laid-out top-left (the anchor offset is applied when the
/// shape is built), so the galley's size gives the rect it occupies.
fn text_rects(shapes: &[egui::epaint::ClippedShape]) -> Vec<(String, egui::Rect)> {
    shapes
        .iter()
        .filter_map(|cs| match &cs.shape {
            egui::Shape::Text(t) => Some((
                t.galley.text().to_string(),
                egui::Rect::from_min_size(t.pos, t.galley.size()),
            )),
            _ => None,
        })
        .collect()
}

/// Paint the banner and `view` into one `Ui`, then assert nothing the view
/// draws overlaps the banner's rect vertically.
fn assert_view_clears_the_banner(view: ViewKind, scene: Option<&ac_scene::Scene>, kind: &str) {
    let transfer = transfer_scene();
    let transfer_arg = match view {
        ViewKind::Transfer(_) => Some(&transfer),
        ViewKind::Spectrum(_) => None,
    };
    let mut harness = Harness::new_ui(|ui| {
        ui.set_min_size(egui::vec2(400.0, 300.0));
        ui.label(BANNER);
        draw_view(&view, ui, scene, transfer_arg, &[], None);
    });
    harness.run();

    let texts = text_rects(&harness.output().shapes);
    let banner = texts
        .iter()
        .find(|(s, _)| s == BANNER)
        .map(|(_, r)| *r)
        .unwrap_or_else(|| panic!("{kind}: the banner label was not painted"));

    let mut painted = 0usize;
    for (text, rect) in texts.iter().filter(|(s, _)| s != BANNER) {
        painted += 1;
        assert!(
            rect.min.y >= banner.max.y,
            "{kind}: {text:?} is painted at y {} — above the banner's baseline at {} \
             (#245: the view must stay inside its own rect)",
            rect.min.y,
            banner.max.y
        );
    }
    assert!(
        painted > 0,
        "{kind}: the view painted no text — the assertion above proved nothing"
    );
}

#[test]
fn transfer_view_tick_labels_clear_the_connection_banner() {
    assert_view_clears_the_banner(
        ViewKind::Transfer(TransferViewState::default()),
        None,
        "transfer",
    );
}

#[test]
fn spectrum_view_tick_labels_clear_the_connection_banner() {
    let scene = spectrum_scene();
    assert_view_clears_the_banner(
        ViewKind::Spectrum(SpectrumViewState::default()),
        Some(&scene),
        "spectrum",
    );
}
