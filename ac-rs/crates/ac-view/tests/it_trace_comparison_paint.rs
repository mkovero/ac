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
use ac_view::view::{draw_view, Focus, LiveTrace, StoredTrace, TransferViewState, ViewKind};
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
        delay_tracking: false,
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
        draw_view(&view, ui, None, None, &[], &stored, None, None);
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
        draw_view(&view, ui, None, None, &[], &stored, None, None);
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
            draw_view(&view, ui, None, None, &[], &stored, None, None);
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
        draw_view(&view, ui, None, None, &[], &stored, None, None);
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

/// #256: selecting a trace does not change how its curve is drawn — one
/// width, one colour per trace. The selection is the strip box's border.
#[test]
fn a_selected_slot_draws_like_an_unselected_one() {
    let fixture = scene(Smoothing::Off);
    let view = ViewKind::Transfer(TransferViewState::new(-10.0, -30.0));
    let run = |slot: u8, focused: bool| StoredTrace {
        label: "slot",
        captured_at_utc: "2026-09-26T14:00:00Z",
        scene: &fixture,
        focused,
        visible: true,
        color_slot: usize::from(slot - 1),
        slot: Some(slot),
    };
    let stored = vec![run(1, true), run(2, false)];
    let mut harness = Harness::new_ui(|ui| {
        ui.set_min_size(egui::vec2(640.0, 360.0));
        draw_view(&view, ui, None, None, &[], &stored, None, None);
    });
    harness.run();
    let width_of = |color: egui::Color32| -> Vec<f32> {
        harness
            .output()
            .shapes
            .iter()
            .filter_map(|cs| match &cs.shape {
                egui::Shape::LineSegment { stroke, .. } if stroke.color == color => {
                    Some(stroke.width)
                }
                _ => None,
            })
            .collect()
    };
    let one = width_of(ac_view::view::palette::compare_color(0));
    let two = width_of(ac_view::view::palette::compare_color(1));
    assert!(!one.is_empty() && !two.is_empty(), "both curves drawn");
    assert!(
        one.iter().chain(&two).all(|w| *w == one[0]),
        "selected {one:?} vs unselected {two:?}"
    );
}

/// #685: with two live pairs the strip has one box per pair, each drawn
/// in its own colour, and a fault on the pair that is not selected is
/// painted by name, so a silent second leg cannot hide behind the first.
#[test]
fn two_live_pairs_paint_their_boxes_and_the_other_pairs_fault() {
    let first = scene(Smoothing::Off);
    let mut second = scene(Smoothing::Off);
    second.fault = Some(ac_scene::Fault::NoSignal);
    let mut state = TransferViewState::default();
    state.set_live_count(2);
    let view = ViewKind::Transfer(state);
    let live = [
        LiveTrace {
            label: "live 0".to_string(),
            pair: 0,
            scene: &first,
            selected: true,
        },
        LiveTrace {
            label: "live 4".to_string(),
            pair: 1,
            scene: &second,
            selected: false,
        },
    ];

    let mut harness = Harness::new_ui(|ui| {
        ui.set_min_size(egui::vec2(600.0, 400.0));
        draw_view(&view, ui, None, Some(&first), &live, &[], None, None);
    });
    harness.run();
    let shapes = &harness.output().shapes;
    let texts = extract_texts(shapes);
    for label in ["live 0", "live 4", "live 4  NO SIGNAL"] {
        assert!(
            texts.iter().any(|t| t == label),
            "{label:?} not painted: {texts:?}"
        );
    }
    assert!(
        !texts.iter().any(|t| t == "NO SIGNAL"),
        "the selected pair has no fault, yet the big indicator painted one: {texts:?}"
    );
    let second_colour = ac_view::view::palette::live_color(1);
    assert!(
        shapes.iter().any(|cs| matches!(
            &cs.shape,
            egui::Shape::Path(p) if p.points.len() > 1 && p.stroke.color == egui::epaint::ColorMode::Solid(second_colour)
        )),
        "the second pair's curve is not drawn in its own colour"
    );
}

/// A loaded target curve is painted dashed in the value colour with its
/// caption, on the trace's axes.
#[test]
fn a_target_curve_paints_dashed_with_its_caption() {
    let target =
        ac_scene::target::TargetCurve::parse("house.txt", "20 6\n1000 0\n20000 -3\n").unwrap();
    let drawn = ac_view::view::TargetTrace {
        trace: target.trace(FREQ_RANGE, DB_RANGE),
        caption: target.caption(),
    };
    let first = scene(Smoothing::Off);
    let view = ViewKind::Transfer(TransferViewState::default());
    let mut harness = Harness::new_ui(|ui| {
        ui.set_min_size(egui::vec2(600.0, 400.0));
        draw_view(&view, ui, None, Some(&first), &[], &[], None, Some(&drawn));
    });
    harness.run();
    let shapes = &harness.output().shapes;
    let texts = extract_texts(shapes);
    assert!(texts.iter().any(|t| t == "target: house.txt"), "{texts:?}");
    let value = ac_view::view::palette::COLOR_VALUE;
    assert!(
        shapes.iter().any(|cs| matches!(
            &cs.shape,
            egui::Shape::LineSegment { stroke, .. } if stroke.color == value
        )),
        "no dashed segment in the value colour"
    );
}

/// #707: the caption row follows focus. With a slot focused it names the
/// slot and shows the slot's smoothing — and live's smoothing is not drawn,
/// which is what the operator saw: `N` changed the slot while the caption
/// kept reading live's setting. With live focused, live's caption returns.
#[test]
fn the_smoothing_caption_follows_the_focused_trace() {
    let live = scene(Smoothing::Oct6);
    let slot = scene(Smoothing::Oct3);
    let paint = |focus: Focus| {
        let mut state = TransferViewState::new(-10.0, -30.0);
        state.focus = focus;
        let view = ViewKind::Transfer(state);
        let stored = vec![StoredTrace {
            label: "slot 2",
            captured_at_utc: "2026-09-28T00:00:00Z",
            scene: &slot,
            focused: matches!(focus, Focus::Stored(0)),
            visible: true,
            color_slot: 1,
            slot: Some(2),
        }];
        let lives = [LiveTrace {
            label: "live".to_string(),
            pair: 0,
            scene: &live,
            selected: matches!(focus, Focus::Live),
        }];
        let mut harness = Harness::new_ui(|ui| {
            ui.set_min_size(egui::vec2(900.0, 480.0));
            draw_view(&view, ui, None, Some(&live), &lives, &stored, None, None);
        });
        harness.run();
        extract_texts(&harness.output().shapes)
    };

    let on_slot = paint(Focus::Stored(0));
    assert!(
        on_slot.iter().any(|t| t == "slot 2 \u{b7}"),
        "no owner tag: {on_slot:?}"
    );
    assert!(on_slot.iter().any(|t| t == "smoothing 1/3 octave"));
    assert!(
        !on_slot.iter().any(|t| t == "smoothing 1/6 octave"),
        "live's smoothing drawn while the slot is focused: {on_slot:?}"
    );

    let on_live = paint(Focus::Live);
    assert!(on_live.iter().any(|t| t == "smoothing 1/6 octave"));
    assert!(!on_live.iter().any(|t| t == "slot 2 \u{b7}"));
}

/// The `Shift+P` crash: in the unwrapped and group-delay views the pane's
/// range is fitted to the bulk of the data (2 % trimmed), so a spike — a
/// group-delay peak at a null, a mask edge — runs thousands of pane
/// heights off it. A stored run is dashed, and dashing asks egui for one
/// shape every few pixels of the whole line, visible or not: on real data
/// that ran the view out of memory. Here a trace sits ~150 pane heights
/// off-scale; clipped to the pane first, it paints a handful of shapes.
#[test]
fn an_off_scale_dashed_trace_paints_only_what_is_on_the_pane() {
    let inp = TransferInput {
        freqs: freqs(),
        magnitude_db: vec![0.0; N],
        phase_deg: (0..N)
            .map(|i| if i % 2 == 0 { 170.0 } else { -170.0 })
            .collect(),
        coherence: vec![0.9; N],
        delay_ms: 0.0,
        delay_locked: Some(true),
        delay_control: None,
        delay_tracking: false,
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
    let spiky = TransferScene::from_input(
        &inp,
        DisplayModes::new(DerotMode::Raw, Smoothing::Off)
            .with_phase_view(ac_scene::transfer::PhaseView::Unwrapped)
            .with_phase_range((0.0, 1.0)),
        FREQ_RANGE,
        DB_RANGE,
        &mut meters,
        &mut FaultState::default(),
        0.0,
    );
    let far = spiky
        .phase
        .segments
        .iter()
        .flatten()
        .map(|p| p.1.abs())
        .fold(0.0, f64::max);
    assert!(far > 100.0, "fixture not off-scale: {far}");
    let view = ViewKind::Transfer(TransferViewState::new(-10.0, -30.0));
    let stored = vec![StoredTrace {
        label: "spiky.acsnap",
        captured_at_utc: "2026-09-28T00:00:00Z",
        scene: &spiky,
        focused: false,
        visible: true,
        color_slot: 0,
        slot: None,
    }];
    let mut harness = Harness::new_ui(|ui| {
        ui.set_min_size(egui::vec2(640.0, 360.0));
        draw_view(&view, ui, None, None, &[], &stored, None, None);
    });
    harness.run();
    let segments = harness
        .output()
        .shapes
        .iter()
        .filter(|cs| matches!(&cs.shape, egui::Shape::LineSegment { .. }))
        .count();
    assert!(
        segments < 2_000,
        "{segments} dash segments for an off-scale trace"
    );
}
