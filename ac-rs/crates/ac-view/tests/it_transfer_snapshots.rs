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
//! automatically (#337). Last regenerated 2026-08-24 on 192.168.9.25 (RTX
//! 2070), `issue-391` at `92d3d42`, immediately followed by a plain
//! (non-update) run in the same session per the acceptance check in #337
//! — built on the dev VM (`cargo test --no-run`) and the resulting binary
//! copied across, not built on the rig itself (its host has no headroom
//! to spare for a build). `#391` removed
//! the delay readout's ms → m conversion entirely (the calibration/warning
//! rows #356 added on top of it went with it): `spectrum_ref_trace_off.png`
//! / `spectrum_ref_trace_on.png` came out byte-identical (spectrum view,
//! untouched by this change); the 5 transfer-view files
//! (`transfer_armed_banner.png`, `transfer_driving_banner.png`,
//! `transfer_ir_panel.png`, `transfer_live_masked_gap.png`,
//! `transfer_stored_comparison_no_live.png`) all moved — the masked-gap
//! reference shrank the most (2 calibration/warning rows gone). See
//! `TESTING.md` → "A3 snapshot reference currency" for the checklist a
//! `draw_view`/pane change must satisfy before merge.
//!
//! **Re-verified 2026-08-31** on the same box for the `view.rs` module
//! split (PR #418), same build-on-the-dev-VM-and-copy method: all 7 pass
//! against these references unchanged. Two facts worth recording from
//! that run, because they are cheap to re-derive wrongly:
//!
//! * Regenerating produced byte-identical PNGs for the 5 transfer files
//!   but a **±1 LSB** difference on ~400–750 scattered pixels in the two
//!   spectrum files. It is not this crate's: a binary built from `main`
//!   (`c2aa26fe`) regenerated *the same bytes* as the refactor binary for
//!   all 7, and two runs of one binary are byte-identical, so the drift
//!   is in the box's graphics stack since 2026-08-24 (its NVIDIA
//!   userspace is now 610.57 against a 610.43 kernel module), not in any
//!   `ac` commit. The references were therefore **not** regenerated —
//!   they pass the gate, and replacing them would commit environment
//!   drift as if it were a rendering change.
//! * The gate is `threshold` 0.6 with `failed_pixel_count_threshold` 0
//!   (egui_kittest defaults, no `kittest.toml` in this workspace): no
//!   pixel may differ perceptibly, but a sub-threshold LSB shift passes.
//!   A pass is therefore evidence of visual identity, not of byte
//!   identity — regenerate into a scratch directory and hash if the
//!   stronger claim is what a review needs.

use ac_scene::{
    DerotMode, DisplayModes, FaultState, IrInput, IrScene, MeterState, Scene, SceneInput,
    Smoothing, Source, TransferInput, TransferScene,
};
use ac_view::view::{
    draw_view, Focus, SpectrumViewState, StimState, StoredTrace, TransferViewState, ViewKind,
};
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
        // Locked. #391 removed the ms → m conversion entirely, so this
        // reference image's readout is ms-only — "2.50 ms". See this
        // file's module doc: references need regenerating on the real
        // adapter after this change.
        delay_locked: Some(true),
        meas_channel: 0,
        ref_channel: 1,
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
    let stored = vec![
        StoredTrace {
            label: "run.acsnap",
            captured_at_utc: "2026-08-01T00:00:00Z",
            scene: &run_a,
            focused: true,
        },
        StoredTrace {
            label: "run.acsnap",
            captured_at_utc: "2026-08-02T00:00:00Z",
            scene: &run_b,
            focused: false,
        },
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
