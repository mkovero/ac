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
use ac_view::view::{draw_view, Focus, StoredTrace, TransferViewState, ViewKind};
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
        delay_control: None,
        meas_channel: 0,
        ref_channel: 1,
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
        estimator: ac_scene::transfer::Estimator::Welch {
            nperseg: ac_core::visualize::transfer::h1_nperseg(48_000),
        },
        fault: None,
        calibration: None,
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
    let stored = vec![StoredTrace {
        label: "fixture.acsnap",
        captured_at_utc: "2026-08-18T00:00:00Z",
        scene: &fixture,
        focused: true,
        visible: true,
        color_slot: 0,
        slot: None,
    }];

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
    let stored = vec![
        StoredTrace {
            label: "run.acsnap",
            captured_at_utc: "2026-08-01T00:00:00Z",
            scene: &scene_a,
            focused: true,
            visible: true,
            color_slot: 0,
            slot: None,
        },
        StoredTrace {
            label: "run.acsnap",
            captured_at_utc: "2026-08-02T00:00:00Z",
            scene: &scene_b,
            focused: false,
            visible: true,
            color_slot: 0,
            slot: None,
        },
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

/// #221: a stored run derived by the Welch H₁ carries the scene's "not the
/// live ladder" statement in its legend row, drawn verbatim as its own span —
/// not folded into the identity text — and a stored run whose ladder was
/// replayed draws none.
#[test]
fn legend_draws_the_estimator_readout_for_a_welch_run_only() {
    let welch = scene(Smoothing::Off);
    let readout = welch
        .estimator_readout
        .clone()
        .expect("a Welch-tagged snapshot scene carries the statement");
    assert_eq!(readout, "H₁ Welch 1.00 Hz flat — not the live ladder");

    let paint = |scene: &TransferScene| {
        let mut state = TransferViewState::new(-10.0, -30.0);
        state.focus = Focus::Stored(0);
        let view = ViewKind::Transfer(state);
        let stored = vec![StoredTrace {
            label: "run.acsnap",
            captured_at_utc: "2026-09-24T13:02:11Z",
            scene,
            focused: true,
            visible: true,
            color_slot: 0,
            slot: None,
        }];
        let mut harness = Harness::new_ui(|ui| {
            ui.set_min_size(egui::vec2(900.0, 300.0));
            draw_view(&view, ui, None, None, &stored, None);
        });
        harness.run();
        extract_texts(&harness.output().shapes)
    };

    let texts = paint(&welch);
    assert!(
        texts.contains(&readout),
        "the estimator statement must paint as its own span; painted texts: {texts:?}"
    );
    assert!(
        texts
            .iter()
            .any(|t| t.contains("run.acsnap") && !t.contains("not the live ladder")),
        "the identity span must not carry the statement; painted texts: {texts:?}"
    );

    let mut ladder = welch.clone();
    ladder.estimator_readout = None;
    let texts = paint(&ladder);
    assert!(
        !texts.iter().any(|t| t.contains("not the live ladder")),
        "a replayed-ladder run must not claim to differ from the live view; painted texts: {texts:?}"
    );
}

/// #256: slot runs show as a strip of boxes — `live` and 1…9 — not as
/// timestamp rows; a stored-and-shown slot is a filled box.
#[test]
fn slot_runs_paint_as_a_box_strip_not_timestamp_rows() {
    let fixture = scene(Smoothing::Off);
    let view = ViewKind::Transfer(TransferViewState::new(-10.0, -30.0));
    let stored = vec![StoredTrace {
        label: "slot 3",
        captured_at_utc: "2026-09-26T14:00:00Z",
        scene: &fixture,
        focused: false,
        visible: true,
        color_slot: 2,
        slot: Some(3),
    }];
    let mut harness = Harness::new_ui(|ui| {
        ui.set_min_size(egui::vec2(640.0, 360.0));
        draw_view(&view, ui, None, None, &stored, None);
    });
    harness.run();
    let shapes = &harness.output().shapes;
    let texts = extract_texts(shapes);
    for want in ["live", "1", "3", "9"] {
        assert!(texts.iter().any(|t| t == want), "{want} missing: {texts:?}");
    }
    assert!(
        !texts.iter().any(|t| t.contains("2026-09-26T14:00:00Z")),
        "slot shown as a timestamp row: {texts:?}"
    );
    let slot3 = ac_view::view::palette::compare_color(2);
    assert!(
        shapes.iter().any(|cs| matches!(
            &cs.shape,
            egui::Shape::Rect(r) if r.fill == slot3
        )),
        "slot 3 not drawn as a filled box in its colour"
    );
    // No live scene: the `live` box must not claim a curve (Codex review).
    let live = ac_view::view::palette::COLOR_STRUCTURAL;
    assert!(
        !shapes.iter().any(|cs| matches!(
            &cs.shape,
            egui::Shape::Rect(r) if r.fill == live
        )),
        "live box filled with no live frame"
    );
}
