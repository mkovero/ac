//! #321 paint-level coverage for the display-truth gate (QA #336): the
//! comparison feature changes what `draw_view`'s transfer branch paints —
//! new dashed stored-trace polylines, a legend text band — and none of
//! that painted content had a harness assertion before this file.
//! `it_trace_comparison.rs` covers the `TransferViewState`/`LoadedRun`
//! state; this covers the actual `draw_view` paint call, egui_kittest
//! shapes, no GPU, no pixels — the same discipline `it_geometry.rs` and
//! `it_transfer_geometry.rs` already hold the rest of this crate to.

use ac_scene::{
    DerotMode, DisplayModes, FaultState, MeterState, Smoothing, Source, TransferInput,
    TransferScene,
};
use ac_view::view::{draw_view, Focus, TransferViewState, ViewKind};
use egui_kittest::Harness;

const FREQ_RANGE: (f64, f64) = (20.0, 20_000.0);
const DB_RANGE: (f64, f64) = (-80.0, 20.0);
const N: usize = 8;

fn freqs() -> Vec<f64> {
    (0..N).map(|i| 100.0 * (i + 1) as f64).collect()
}

/// A minimal but real transfer scene, built the same way
/// `it_transfer_geometry.rs`'s fixtures are — through `TransferScene`'s
/// actual `from_input` path, not a hand-rolled struct literal.
fn scene(smoothing: Smoothing) -> TransferScene {
    let inp = TransferInput {
        freqs: freqs(),
        magnitude_db: vec![0.0; N],
        phase_deg: vec![0.0; N],
        coherence: vec![0.9; N],
        delay_ms: 1.5,
        delay_locked: Some(true),
        speed_of_sound_m_s: None,
        meas_peak_dbfs: None,
        ref_peak_dbfs: None,
        channel_role: "meas_0".to_string(),
        source: Source::Snapshot,
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
        DisplayModes::new(DerotMode::Session, smoothing),
        FREQ_RANGE,
        DB_RANGE,
        &mut meters,
        &mut FaultState::default(),
        0.0,
    )
}

/// Count of "a line got painted" shapes, counting both a continuous
/// `Shape::Path` (the live/solid case) and each individual
/// `Shape::LineSegment` — what `Shape::dashed_line_many` actually emits
/// per dash, per `it_trace_distinction.rs`'s reading of epaint's
/// `dashes_from_line` source. Every stored run here paints dashed, so a
/// `Path`-only filter would find nothing and pass for the wrong reason.
fn line_like_shape_count(shapes: &[egui::epaint::ClippedShape]) -> usize {
    shapes
        .iter()
        .filter(|cs| {
            matches!(&cs.shape, egui::Shape::Path(p) if p.points.len() > 1)
                || matches!(&cs.shape, egui::Shape::LineSegment { .. })
        })
        .count()
}

fn extract_texts(shapes: &[egui::epaint::ClippedShape]) -> Vec<String> {
    shapes
        .iter()
        .filter_map(|cs| match &cs.shape {
            egui::Shape::Text(t) => Some(t.galley.text().to_string()),
            _ => None,
        })
        .collect()
}

/// Correctness issue 1 (QA #336): a viewer with one or more stored runs
/// loaded, but no live frame yet, must still see them — the comparison
/// feature this issue exists to add was unreachable in exactly that
/// state before this fix (`draw_transfer` bailed on `scene: None` before
/// any stored-run code ran).
#[test]
fn stored_runs_paint_without_a_live_scene() {
    let fixture = scene(Smoothing::Off);
    let mut state = TransferViewState::new(-10.0, -30.0);
    // Focus lands on the stored run once something is loaded — mirrors
    // `TransferViewState::add_loaded_run`'s "just arrived" precedence,
    // no live scene involved.
    state.focus = Focus::Stored(0);
    let view = ViewKind::Transfer(state);
    let stored: Vec<(&str, &str, &TransferScene, bool)> =
        vec![("fixture.acsnap", "2026-08-18T00:00:00Z", &fixture, true)];

    let mut harness = Harness::new_ui(|ui| {
        ui.set_min_size(egui::vec2(400.0, 300.0));
        draw_view(&view, ui, None, None, &stored, None);
    });
    harness.run();

    let shapes = &harness.output().shapes;
    assert!(
        line_like_shape_count(shapes) > 0,
        "no line painted for the stored run with no live scene"
    );

    let texts = extract_texts(shapes);
    assert!(
        texts.iter().any(|t| t.contains("fixture.acsnap")),
        "stored run legend must paint even with no live frame; painted texts: {texts:?}"
    );
    assert!(
        !texts.iter().any(|t| t.contains("no session")),
        "the empty-session placeholder must not paint when a stored run is loaded"
    );
}

/// Correctness issue 2 (QA #336): two loaded runs sharing a basename (the
/// same file opened twice, or two files named identically from different
/// session directories — `label` is `path.file_name()`, not the full
/// path) must stay distinguishable in the legend. `captured_at_utc` is
/// what disambiguates them.
#[test]
fn legend_rows_distinguish_same_named_runs_by_timestamp() {
    let scene_a = scene(Smoothing::Off);
    let scene_b = scene(Smoothing::Off);
    let mut state = TransferViewState::new(-10.0, -30.0);
    state.focus = Focus::Stored(0);
    let view = ViewKind::Transfer(state);
    let stored: Vec<(&str, &str, &TransferScene, bool)> = vec![
        ("run.acsnap", "2026-08-01T00:00:00Z", &scene_a, true),
        ("run.acsnap", "2026-08-02T00:00:00Z", &scene_b, false),
    ];

    let mut harness = Harness::new_ui(|ui| {
        ui.set_min_size(egui::vec2(400.0, 300.0));
        draw_view(&view, ui, None, None, &stored, None);
    });
    harness.run();

    let texts = extract_texts(&harness.output().shapes);
    let row_a = texts
        .iter()
        .find(|t| t.contains("run.acsnap") && t.contains("2026-08-01"));
    let row_b = texts
        .iter()
        .find(|t| t.contains("run.acsnap") && t.contains("2026-08-02"));
    assert!(
        row_a.is_some() && row_b.is_some(),
        "two same-named runs must each paint their own timestamp; painted texts: {texts:?}"
    );
    assert_ne!(
        row_a, row_b,
        "same-named runs painted identical legend rows — no way to tell them apart"
    );
}
