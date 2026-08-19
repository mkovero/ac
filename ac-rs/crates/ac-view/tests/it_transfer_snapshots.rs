//! Pixel-level snapshot evidence for the transfer view (M4b, A3 gate).
//!
//! These render the real `view::draw_view` output through the wgpu
//! backend and pixel-diff it against committed reference PNGs in
//! `tests/snapshots/`. They are the permanent regression equipment the
//! A3 gate asks for: a future banner/layout/meter change is caught by a
//! pixel diff rather than re-argued in review.
//!
//! `#[ignore]` by policy — the same reason the rest of the A3 harness is
//! real-adapter-gated: sandbox lavapipe segfaults on wgpu, and a
//! reference PNG rendered on one adapter will not pixel-match another.
//! Run on the real adapter (192.168.9.25, RTX 2070). Generate references
//! with `UPDATE_SNAPSHOTS=1`, then re-run to pixel-diff. Use
//! `--test-threads=1`: several wgpu device contexts spun up in parallel
//! contend and hang the process at teardown, so the snapshots run one at
//! a time.
//!
//! ```text
//! UPDATE_SNAPSHOTS=1 cargo test -p ac-view --test it_transfer_snapshots -- --ignored --test-threads=1
//! cargo test -p ac-view --test it_transfer_snapshots -- --ignored --test-threads=1
//! ```
//!
//! The committed `tests/snapshots/*.png` are also the human-viewable
//! evidence attached to the PR: live transfer (masked gap, meters, delay
//! readout), the ARMED and DRIVING banners, and the ref-trace toggle on
//! versus off.
//!
//! **Reference currency.** These references are only as current as the
//! rig run that produced them — nothing re-checks them against `draw_view`
//! automatically (#337). Last regenerated 2026-08-18 on 192.168.9.25 (RTX
//! 2070), `main` at `3c05c03`, immediately followed by a plain (non-update)
//! run in the same session per the acceptance check in #337.
//! `transfer_ir_panel.png` and `transfer_stored_comparison_no_live.png`
//! came out byte-identical to the pre-regeneration commit, so only the
//! other 5 files in this directory moved. See `TESTING.md` → "A3 snapshot
//! reference currency" for the checklist a `draw_view`/pane change must
//! satisfy before merge.

use ac_scene::{
    DerotMode, DisplayModes, FaultState, IrInput, IrScene, MeterState, Scene, SceneInput,
    Smoothing, Source, TransferInput, TransferScene,
};
use ac_view::view::{draw_view, Focus, SpectrumViewState, StimState, TransferViewState, ViewKind};
use egui_kittest::Harness;

const FREQ_RANGE: (f64, f64) = (20.0, 20_000.0);
const DB_RANGE: (f64, f64) = (-80.0, 20.0);
// A representative field-laptop content width — wide enough that the
// DRIVING banner (the longest, largest string) fits without clipping. On
// a much narrower window a long banner can still overflow; the banner
// stays top-center and readable at the widths this instrument is used at.
const SIZE: egui::Vec2 = egui::vec2(960.0, 420.0);

/// Transfer scene over 24 columns with a coherence gap at 8..14 and a
/// sloped magnitude, so the gap and the pane shapes are both visible.
fn transfer_scene() -> TransferScene {
    let n = 24;
    let freqs: Vec<f64> = (0..n).map(|i| 40.0 * 1.3f64.powi(i as i32)).collect();
    let magnitude_db: Vec<f64> = (0..n).map(|i| -6.0 + (i as f64 - 12.0) * 0.8).collect();
    let phase_deg: Vec<f64> = (0..n)
        .map(|i| ((i as f64) * 25.0 - 180.0).rem_euclid(360.0) - 180.0)
        .collect();
    let mut coherence = vec![0.9; n];
    for c in coherence.iter_mut().take(14).skip(8) {
        *c = 0.3;
    }
    let inp = TransferInput {
        freqs,
        magnitude_db,
        phase_deg,
        coherence,
        delay_ms: 2.5,
        // Locked, no frame-supplied speed and no stored distance
        // calibration — #243 changed this reference image's own readout
        // from "2.50 ms  (0.86 m)" to ms-only plus a "not calibrated"
        // line, since a metres figure with no per-pair constant is exactly
        // the defect #243 fixes. See this file's module doc: references
        // need regenerating on the real adapter after this change.
        delay_locked: Some(true),
        speed_of_sound_m_s: None,
        meas_channel: 0,
        ref_channel: 1,
        distance_cal: None,
        distance_plausible_max_m: None,
        meas_peak_dbfs: Some(-6.0),
        ref_peak_dbfs: Some(-14.0),
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

/// IR panel fixture (#286, QA follow-up on PR #309: the A3 gate's human-
/// viewable evidence was missing for this panel specifically). 200
/// samples of a decaying sinusoid so the trace looks like an actual
/// arrival rather than the geometry tests' 5-point fixtures — this one
/// is for a person to look at, not for a coordinate assertion.
fn ir_scene() -> IrScene {
    let n = 200;
    let dt_ms = 0.5;
    let t_origin_ms = -(n as f64 / 2.0) * dt_ms;
    let samples: Vec<f32> = (0..n)
        .map(|i| {
            let t = i as f64 * dt_ms + t_origin_ms;
            let env = (-((t - 10.0).abs() / 8.0).max(0.0)).exp();
            (env * (t * 0.3).sin()) as f32
        })
        .collect();
    let input = IrInput {
        samples,
        dt_ms,
        t_origin_ms,
        delay_ms: 10.0,
        delay_locked: Some(true),
        speed_of_sound_m_s: None,
        channel_role: "meas_0".to_string(),
        source: Source::Live,
        sr: 48_000,
    };
    IrScene::from_input(&input)
}

fn spectrum_scene() -> Scene {
    let input = SceneInput {
        spec_freqs: vec![100.0, 1_000.0, 10_000.0],
        meas_spectrum: vec![0.01, 0.1, 0.9],
        ref_spectrum: vec![0.02, 0.2, 0.5],
        spl: None,
        spl_weighting: ac_core::visualize::weighting_curves::WeightingCurve::Z,
        spl_integration: None,
        meas_role: "meas_0".to_string(),
        ref_role: "ref".to_string(),
        source: Source::Live,
        sr: 48_000,
    };
    Scene::from_input(input, FREQ_RANGE, (-80.0, 0.0))
}

fn transfer_state(target: StimState) -> TransferViewState {
    // Drive the real machine to the target state via key presses (start
    // level −20, ceiling −10). Idle = untouched; Armed = Space; Driving =
    // Space then Enter.
    let mut s = TransferViewState::new(-10.0, -20.0);
    let now = std::time::Instant::now();
    match target {
        StimState::Idle => {}
        StimState::Armed => {
            s.stimulus.press_space(now);
        }
        StimState::Driving => {
            s.stimulus.press_space(now);
            s.stimulus.press_enter(now);
        }
    }
    s
}

#[test]
#[ignore = "real-adapter only (wgpu); run on 192.168.9.25 per A3 policy"]
fn snapshot_transfer_live_masked_gap() {
    let scene = transfer_scene();
    let view = ViewKind::Transfer(transfer_state(StimState::Idle));
    let mut h = Harness::builder()
        .with_size(SIZE)
        .wgpu()
        .build_ui(|ui| draw_view(&view, ui, None, Some(&scene), &[], None));
    ac_view::fonts::install(&h.ctx);
    h.run();
    h.snapshot("transfer_live_masked_gap");
}

#[test]
#[ignore = "real-adapter only (wgpu); run on 192.168.9.25 per A3 policy"]
fn snapshot_transfer_armed_banner() {
    let scene = transfer_scene();
    let view = ViewKind::Transfer(transfer_state(StimState::Armed));
    let mut h = Harness::builder()
        .with_size(SIZE)
        .wgpu()
        .build_ui(|ui| draw_view(&view, ui, None, Some(&scene), &[], None));
    ac_view::fonts::install(&h.ctx);
    h.run();
    h.snapshot("transfer_armed_banner");
}

#[test]
#[ignore = "real-adapter only (wgpu); run on 192.168.9.25 per A3 policy"]
fn snapshot_transfer_driving_banner() {
    let scene = transfer_scene();
    let view = ViewKind::Transfer(transfer_state(StimState::Driving));
    let mut h = Harness::builder()
        .with_size(SIZE)
        .wgpu()
        .build_ui(|ui| draw_view(&view, ui, None, Some(&scene), &[], None));
    ac_view::fonts::install(&h.ctx);
    h.run();
    h.snapshot("transfer_driving_banner");
}

#[test]
#[ignore = "real-adapter only (wgpu); run on 192.168.9.25 per A3 policy"]
fn snapshot_transfer_ir_panel() {
    let transfer = transfer_scene();
    let ir = ir_scene();
    let mut state = transfer_state(StimState::Idle);
    state.toggle_ir_panel();
    let view = ViewKind::Transfer(state);
    let mut h = Harness::builder()
        .with_size(SIZE)
        .wgpu()
        .build_ui(|ui| draw_view(&view, ui, None, Some(&transfer), &[], Some(&ir)));
    ac_view::fonts::install(&h.ctx);
    h.run();
    h.snapshot("transfer_ir_panel");
}

/// QA #336's outstanding blocker on this PR (#321): the two fixed
/// correctness issues (stored runs invisible with no live scene; two
/// same-named runs indistinguishable in the legend) painted through the
/// real wgpu adapter, not just `it_trace_comparison_paint.rs`'s no-GPU
/// kittest harness — this is the human-viewable A3 evidence the earlier
/// review round asked for and the dev's session couldn't reach. Both
/// stored runs share the label `run.acsnap` on purpose, so the legend's
/// two timestamp-suffixed rows are themselves the evidence for issue 2;
/// `transfer: None` (no live scene at all) is the evidence for issue 1.
#[test]
#[ignore = "real-adapter only (wgpu); run on 192.168.9.25 per A3 policy"]
fn snapshot_transfer_stored_comparison_no_live() {
    let run_a = transfer_scene();
    let run_b = transfer_scene();
    let mut state = TransferViewState::new(-10.0, -20.0);
    state.focus = Focus::Stored(0);
    let view = ViewKind::Transfer(state);
    let stored: Vec<(&str, &str, &TransferScene, bool)> = vec![
        ("run.acsnap", "2026-08-01T00:00:00Z", &run_a, true),
        ("run.acsnap", "2026-08-02T00:00:00Z", &run_b, false),
    ];
    let mut h = Harness::builder()
        .with_size(SIZE)
        .wgpu()
        .build_ui(|ui| draw_view(&view, ui, None, None, &stored, None));
    ac_view::fonts::install(&h.ctx);
    h.run();
    h.snapshot("transfer_stored_comparison_no_live");
}

#[test]
#[ignore = "real-adapter only (wgpu); run on 192.168.9.25 per A3 policy"]
fn snapshot_spectrum_ref_trace_on() {
    let scene = spectrum_scene();
    let view = ViewKind::Spectrum(SpectrumViewState {
        ref_trace_visible: true,
        ..SpectrumViewState::default()
    });
    let mut h = Harness::builder()
        .with_size(SIZE)
        .wgpu()
        .build_ui(|ui| draw_view(&view, ui, Some(&scene), None, &[], None));
    ac_view::fonts::install(&h.ctx);
    h.run();
    h.snapshot("spectrum_ref_trace_on");
}

#[test]
#[ignore = "real-adapter only (wgpu); run on 192.168.9.25 per A3 policy"]
fn snapshot_spectrum_ref_trace_off() {
    let scene = spectrum_scene();
    let view = ViewKind::Spectrum(SpectrumViewState {
        ref_trace_visible: false,
        ..SpectrumViewState::default()
    });
    let mut h = Harness::builder()
        .with_size(SIZE)
        .wgpu()
        .build_ui(|ui| draw_view(&view, ui, Some(&scene), None, &[], None));
    ac_view::fonts::install(&h.ctx);
    h.run();
    h.snapshot("spectrum_ref_trace_off");
}
