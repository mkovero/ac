//! M4b AC: masked columns render as polyline gaps — a geometry test
//! asserting NO vertex exists at a masked column's x, run through the
//! real `view::draw_view` transfer path (egui_kittest shapes, no GPU),
//! not a hand-rolled substitute. The complement of ac-scene's
//! segment-split unit test: this proves the split survives to the
//! painted polyline.

use ac_scene::{
    DerotMode, DisplayModes, FaultState, MeterState, Smoothing, Source, TransferInput,
    TransferScene,
};
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
        delay_locked: Some(true),
        speed_of_sound_m_s: None,
        meas_peak_dbfs: None,
        ref_peak_dbfs: None,
        channel_role: "meas_0".to_string(),
        source: Source::Live,
        sr: 48_000,
        // Welch-derived fixture: no per-column provenance to carry.
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

/// Every text string painted by the transfer view.
fn extract_texts(shapes: &[egui::epaint::ClippedShape]) -> Vec<String> {
    shapes
        .iter()
        .filter_map(|cs| match &cs.shape {
            egui::Shape::Text(t) => Some(t.galley.text().to_string()),
            _ => None,
        })
        .collect()
}

fn scene_with(fault: Option<ac_scene::fault::FaultFrame>, now_s: f64) -> TransferScene {
    let inp = TransferInput {
        freqs: freqs(),
        magnitude_db: vec![0.0; N],
        phase_deg: vec![0.0; N],
        coherence: vec![0.9; N],
        delay_ms: 0.0,
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
        fault,
    };
    let mut meters = (MeterState::default(), MeterState::default());
    TransferScene::from_input(
        &inp,
        DisplayModes::new(DerotMode::Session, Smoothing::Off),
        FREQ_RANGE,
        DB_RANGE,
        &mut meters,
        &mut FaultState::default(),
        now_s,
    )
}

fn painted_texts(scene: &TransferScene) -> Vec<String> {
    let view = ViewKind::Transfer(TransferViewState::default());
    let mut harness = Harness::new_ui(|ui| {
        ui.set_min_size(egui::vec2(400.0, 300.0));
        draw_view(&view, ui, None, Some(scene));
    });
    harness.run();
    extract_texts(&harness.output().shapes)
}

/// #228 AC: the indicator's label reaches the painted output verbatim.
///
/// The scene-level tests prove the state machine picks the right row; this
/// proves the row survives to the screen. Without it, a scene field that
/// nothing draws would pass every other test in the tree — which is the
/// same hole the derot scene-accessor test exists to close, one layer up.
#[test]
fn a_fault_row_is_painted_verbatim_from_ac_scene() {
    let refusing = ac_scene::fault::FaultFrame {
        drive: ac_scene::fault::DriveState {
            on: true,
            drivable: true,
        },
        delay_locked: Some(false),
        // The field case #238 unblocked: no lock, so no ladder, so never
        // settled — the state the display used to paint nothing for.
        settled: false,
        estimator_attempted: true,
    };
    // Never locked in this session, so the transient row is NO LOCK rather
    // than LOST LOCK: nothing was lost.
    let scene = scene_with(Some(refusing), 0.0);
    assert_eq!(scene.fault, Some(ac_scene::Fault::NoLockYet));

    let texts = painted_texts(&scene);
    assert!(
        texts
            .iter()
            .any(|t| t == ac_scene::Fault::NoLockYet.label()),
        "the fault label was not painted; texts on screen: {texts:?}"
    );
}

/// The persistent row paints its instruction too — the whole point of
/// separating it from the transient one is that the operator is told to
/// move something.
#[test]
fn the_persistent_row_paints_its_instruction() {
    let refusing = ac_scene::fault::FaultFrame {
        drive: ac_scene::fault::DriveState {
            on: true,
            drivable: true,
        },
        delay_locked: Some(false),
        // The field case #238 unblocked: no lock, so no ladder, so never
        // settled — the state the display used to paint nothing for.
        settled: false,
        estimator_attempted: true,
    };
    // Never locked in this session, so the transient row is NO LOCK rather
    // than LOST LOCK: nothing was lost.
    // One FaultState, two frames: the clock has to run for the row to
    // change, so this cannot be built from a single scene call.
    let inp_fault = Some(refusing);
    let mut meters = (MeterState::default(), MeterState::default());
    let mut fault = FaultState::default();
    let build = |now_s: f64, meters: &mut (MeterState, MeterState), fault: &mut FaultState| {
        let inp = TransferInput {
            freqs: freqs(),
            magnitude_db: vec![0.0; N],
            phase_deg: vec![0.0; N],
            coherence: vec![0.9; N],
            delay_ms: 0.0,
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
            fault: inp_fault,
        };
        TransferScene::from_input(
            &inp,
            DisplayModes::new(DerotMode::Session, Smoothing::Off),
            FREQ_RANGE,
            DB_RANGE,
            meters,
            fault,
            now_s,
        )
    };
    assert_eq!(
        build(0.0, &mut meters, &mut fault).fault,
        Some(ac_scene::Fault::NoLockYet)
    );
    let scene = build(
        ac_scene::fault::PERSISTENT_REFUSAL_S,
        &mut meters,
        &mut fault,
    );
    assert_eq!(scene.fault, Some(ac_scene::Fault::NoLock));

    let texts = painted_texts(&scene);
    let detail = ac_scene::Fault::NoLock
        .detail()
        .expect("has an instruction");
    assert!(
        texts.iter().any(|t| t == ac_scene::Fault::NoLock.label()),
        "label missing; texts on screen: {texts:?}"
    );
    assert!(
        texts.iter().any(|t| t == detail),
        "instruction missing; texts on screen: {texts:?}"
    );
}

/// A healthy session paints no indicator at all. An indicator that is
/// always on screen is one the operator stops reading.
#[test]
fn a_healthy_session_paints_no_indicator() {
    let healthy = ac_scene::fault::FaultFrame {
        drive: ac_scene::fault::DriveState {
            on: true,
            drivable: true,
        },
        delay_locked: Some(true),
        settled: true,
        estimator_attempted: true,
    };
    let scene = scene_with(Some(healthy), 0.0);
    assert_eq!(scene.fault, None);

    let texts = painted_texts(&scene);
    for row in [
        ac_scene::Fault::NoReference,
        ac_scene::Fault::NoSignal,
        ac_scene::Fault::CheckRouting,
        ac_scene::Fault::LostLock,
        ac_scene::Fault::NoLockYet,
        ac_scene::Fault::NoLock,
        ac_scene::Fault::LockAcquired,
    ] {
        assert!(
            !texts.iter().any(|t| t == row.label()),
            "{} painted on a healthy session; texts: {texts:?}",
            row.label()
        );
    }
}

/// Every text shape painted by the transfer view, with the horizontal
/// centre of its galley — `Align2::CENTER_TOP` puts the shape's `pos` at
/// the galley's top-left, so the centre has to be recovered from the
/// laid-out width.
fn painted_text_centres(scene: &TransferScene) -> Vec<(String, f32)> {
    let view = ViewKind::Transfer(TransferViewState::default());
    let mut harness = Harness::new_ui(|ui| {
        ui.set_min_size(egui::vec2(960.0, 420.0));
        draw_view(&view, ui, None, Some(scene));
    });
    harness.run();
    harness
        .output()
        .shapes
        .iter()
        .filter_map(|cs| match &cs.shape {
            egui::Shape::Text(t) => Some((
                t.galley.text().to_string(),
                t.pos.x + t.galley.size().x / 2.0,
            )),
            _ => None,
        })
        .collect()
}

/// Every text shape painted by the transfer view, with its laid-out
/// rectangle — the whole rectangle, because a collision is an overlap of
/// areas and a centre cannot show one.
fn painted_text_rects(scene: &TransferScene) -> Vec<(String, egui::Rect)> {
    let view = ViewKind::Transfer(TransferViewState::default());
    let mut harness = Harness::new_ui(|ui| {
        ui.set_min_size(egui::vec2(960.0, 420.0));
        draw_view(&view, ui, None, Some(scene));
    });
    harness.run();
    harness
        .output()
        .shapes
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

/// A scene whose ladder is the real 96 kHz one — the rate the ratified
/// figures (0.98 / 2.93 / 23.4 Hz) are quoted at.
///
/// `delay_ms` is a parameter because the delay readout's laid-out width is
/// what the label row has to clear, and that width is a function of the
/// number's digits. A fixture fixed at 0.00 ms is the friendly case.
fn scene_with_bands(delay_ms: f64, smoothing: Smoothing) -> TransferScene {
    let stages = ac_core::visualize::mtw::ladder::layout(96_000)
        .expect("layout")
        .stages
        .iter()
        .map(|s| ac_scene::MtwStage {
            decim: s.decim,
            rate: s.rate,
            df: s.df,
            window_s: s.window_s,
            hop_s: s.hop_s,
            f_valid: s.f_valid,
            settling_s: ac_core::visualize::mtw::settling_seconds(s, 4),
        })
        .collect();
    let inp = TransferInput {
        freqs: freqs(),
        magnitude_db: vec![0.0; N],
        phase_deg: vec![0.0; N],
        coherence: vec![0.9; N],
        delay_ms,
        // Locked, because the collision this fixture reproduces is between
        // the band row and the readout at its *widest* — and the metres
        // figure is what makes it wide. An unlocked fixture would silently
        // narrow the string and the layout test would stop proving anything.
        delay_locked: Some(true),
        speed_of_sound_m_s: None,
        meas_peak_dbfs: Some(-30.0),
        ref_peak_dbfs: Some(-14.5),
        channel_role: "meas_0".to_string(),
        source: Source::Live,
        sr: 96_000,
        column_df: Vec::new(),
        column_window_s: Vec::new(),
        column_n: Vec::new(),
        column_bins: Vec::new(),
        stages,
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

/// #224 AC: the three band labels reach the painted output verbatim, and
/// at the scene's positions rather than at eyeballed ones.
///
/// The pane origin and width are not reconstructed here: the check is that
/// the painted centres are an affine image of the scene's normalized
/// positions, which is exactly the mapping the renderer is allowed to
/// apply and nothing more. A renderer that placed the labels at equal
/// spacing — the obvious wrong implementation, and the one a screenshot
/// would not obviously contradict — fails it, because the scene's
/// positions are not equally spaced.
#[test]
fn band_labels_are_painted_verbatim_at_the_scenes_band_centres() {
    let scene = scene_with_bands(0.0, Smoothing::Off);
    assert_eq!(scene.band_labels.len(), 3, "{:?}", scene.band_labels);

    let painted = painted_text_centres(&scene);
    let mut centres = Vec::new();
    for band in &scene.band_labels {
        let hit = painted
            .iter()
            .find(|(text, _)| text == &band.text)
            .unwrap_or_else(|| {
                panic!(
                    "band label {:?} was not painted; texts on screen: {:?}",
                    band.text,
                    painted.iter().map(|(t, _)| t).collect::<Vec<_>>()
                )
            });
        centres.push(hit.1 as f64);
    }

    // Two-point affine fit from the outer two labels, then the middle one
    // is a prediction, not an input.
    let (p0, p2) = (scene.band_labels[0].position, scene.band_labels[2].position);
    let (c0, c2) = (centres[0], centres[2]);
    let predicted = c0 + (scene.band_labels[1].position - p0) / (p2 - p0) * (c2 - c0);
    assert!(
        (centres[1] - predicted).abs() < 2.0,
        "middle band painted at {} px, the log axis puts it at {predicted} px",
        centres[1]
    );

    // The rejected placement, computed here rather than assumed: the
    // band's *arithmetic* centre. Only the geometric centre is the
    // midpoint of a span on a log axis — over the middle band's
    // 202.9–1623 Hz the two are two thirds of an octave apart, and the
    // arithmetic one would sit to the right of the frequencies it labels.
    // Both are "the centre of the band" in prose, which is why this is
    // measured rather than left to review.
    let l = ac_core::visualize::mtw::ladder::layout(96_000).expect("layout");
    let (lo, hi) = (l.stages[1].f_valid, l.stages[0].f_valid);
    let arithmetic = ac_scene::ticks::freq_to_x((lo + hi) / 2.0, FREQ_RANGE.0, FREQ_RANGE.1);
    let arithmetic_px = c0 + (arithmetic - p0) / (p2 - p0) * (c2 - c0);
    assert!(
        (arithmetic_px - predicted).abs() > 10.0,
        "the geometric and arithmetic centres are indistinguishable at this \
         width ({predicted} vs {arithmetic_px} px) — the test proves nothing"
    );
    assert!(
        (centres[1] - arithmetic_px).abs() > 10.0,
        "the middle band was painted at its arithmetic centre ({arithmetic_px} px), \
         not its geometric one ({predicted} px)"
    );
}

/// The collision this layout exists to remove, pinned at a delay that shows
/// it.
///
/// #224 originally painted the delay readout on the band-label row and
/// checked the clearance with `delay_ms = 0.0`, where the gap is ~8 px and
/// the arrangement looks fine. It is not: the readout's laid-out width is a
/// function of the number's digits, and at three digits it runs under the
/// deepest band label. 123.45 ms is 42 m of flight — an ordinary far-mic
/// distance in a live room, and less than a misrouted loopback produces.
///
/// The x-overlap is asserted first, so this test cannot pass by accident on
/// a build where the strings happen to be narrow: it proves the two would
/// collide on one row, and then proves the rows are what keeps them apart.
#[test]
fn the_delay_readout_never_shares_a_row_with_a_band_label() {
    for smoothing in [Smoothing::Off, Smoothing::Oct6] {
        for delay_ms in [0.0, 12.34, 123.45] {
            let scene = scene_with_bands(delay_ms, smoothing);
            let painted = painted_text_rects(&scene);
            let find = |want: &str| -> egui::Rect {
                painted
                    .iter()
                    .find(|(text, _)| text == want)
                    .unwrap_or_else(|| panic!("{want:?} was not painted"))
                    .1
            };

            let delay = find(&scene.delay_readout);
            for band in &scene.band_labels {
                let label = find(&band.text);
                assert!(
                    !delay.intersects(label),
                    "{delay_ms} ms, {smoothing:?}: delay readout {delay:?} overlaps \
                     band label {:?} at {label:?}",
                    band.text
                );
                assert!(
                    delay.min.y >= label.max.y,
                    "{delay_ms} ms, {smoothing:?}: delay readout is not below the \
                     band row ({:?} against {:?})",
                    delay.min.y,
                    label.max.y
                );
            }

            // The caption, when present, sits between them — the two
            // resolution statements adjacent, the measured value below.
            if let Some(caption) = scene.smoothing_readout {
                let caption = find(caption);
                let deepest = find(&scene.band_labels.last().expect("bands").text);
                assert!(
                    caption.min.y >= deepest.max.y && delay.min.y >= caption.max.y,
                    "{delay_ms} ms: rows out of order — bands {:?}, caption {:?}, \
                     delay {:?}",
                    deepest,
                    caption,
                    delay
                );
            }
        }
    }

    // The rejected layout, measured here rather than argued: on one shared
    // row these two DO overlap horizontally at a 3-digit delay, so the row
    // separation above is what is holding them apart, not luck with widths.
    let scene = scene_with_bands(123.45, Smoothing::Off);
    let painted = painted_text_rects(&scene);
    let x_range = |want: &str| -> (f32, f32) {
        let r = painted
            .iter()
            .find(|(text, _)| text == want)
            .expect("painted")
            .1;
        (r.min.x, r.max.x)
    };
    let (delay_x0, delay_x1) = x_range(&scene.delay_readout);
    let (band_x0, band_x1) = x_range(&scene.band_labels.last().expect("bands").text);
    assert!(
        delay_x1 > band_x0 && band_x1 > delay_x0,
        "the fixture no longer reproduces the collision: delay spans \
         {delay_x0}–{delay_x1} px, deepest band label {band_x0}–{band_x1} px — \
         at these widths a shared row would pass and prove nothing"
    );
}
