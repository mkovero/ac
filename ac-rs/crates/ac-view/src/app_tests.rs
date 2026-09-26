//! Unit tests for [`crate::app`].
//!
//! Split out of `app.rs` purely for size — this is still the
//! `app::tests` module (attached by `#[path]`), not a separate
//! integration test, so it keeps direct access to the private
//! methods it drives: the parse-failure streak, the status line,
//! and the scene rebuild. Kept under `src/` so the
//! `computes_nothing` forbidden-token scan still covers it.

use super::*;
use crate::keys::Action;
use crate::stimulus::StimState;

/// Stimulus ceiling for tests that construct through
/// `new_transfer`. Named and explicit because it used to come from
/// whatever `drive_max_dbfs` the developer's real config held —
/// these tests read the user's config file until the ceiling became
/// a parameter, so a local config change could move them.
const TEST_DRIVE_MAX_DBFS: f64 = -10.0;

fn transfer_app() -> AcViewApp {
    let mut app = AcViewApp::new(Endpoint {
        host: "localhost".into(),
        ctrl_port: 0,
        data_port: 0,
    });
    // Ceiling −10, start −20; no session (sends still record).
    app.view = ViewKind::Transfer(TransferViewState::new(-10.0, -20.0));
    app
}

/// A spectrum-view app holding one frame, with the scene already
/// built and `last_scene_ranges` primed — the state the range gate
/// in `rebuild_scenes` actually runs against.
fn spectrum_app_with_a_frame() -> AcViewApp {
    let mut app = AcViewApp::new(Endpoint {
        host: "localhost".into(),
        ctrl_port: 0,
        data_port: 0,
    });
    app.view = ViewKind::Spectrum(SpectrumViewState::default());
    app.ingest_frame_for_test(transfer_frame(), 0.0);
    assert!(app.scene.is_some(), "fixture must start with a built scene");
    app
}

/// #193-era regression, testable for the first time now that the
/// paint pass and the test hook share `rebuild_scenes`: a zoom/pan
/// with **no new frame** must rebuild from the held frame, or the
/// display appears frozen on a paused or slow stream until the next
/// frame happens to arrive. The rebuilt scene must reflect the new
/// range, not merely exist — a rebuild that reused the old ranges
/// would leave the axis unmoved and look identical to no rebuild.
#[test]
fn a_range_change_alone_rebuilds_the_spectrum_scene_from_the_held_frame() {
    let mut app = spectrum_app_with_a_frame();
    let before = app.current_scene().expect("scene").freq_axis.clone();

    app.zoom_freq(0.5);
    app.rebuild_scenes(false, 0.0);

    let after = &app.current_scene().expect("scene").freq_axis;
    assert_ne!(
        &before, after,
        "a zoom with no new frame left the frequency axis untouched — \
         the range gate did not fire and the display is frozen"
    );
}

/// The other half of the same gate: with the ranges unmoved and no
/// new frame there is nothing to rebuild, so the scene must be left
/// exactly as it was. Asserted by clearing `scene` first — if the
/// gate wrongly fired it would be rebuilt and come back `Some`,
/// which is the only way this pass/fail is observable from outside.
#[test]
fn an_unchanged_range_with_no_new_frame_skips_the_spectrum_rebuild() {
    let mut app = spectrum_app_with_a_frame();

    app.scene = None;
    app.rebuild_scenes(false, 0.0);

    assert!(
        app.scene.is_none(),
        "rebuilt the spectrum scene with no new frame and no range change"
    );
}

fn stim_state(app: &AcViewApp) -> StimState {
    match &app.view {
        ViewKind::Transfer(t) => t.stimulus.state(),
        _ => panic!("not transfer view"),
    }
}

fn drive(app: &mut AcViewApp) {
    app.handle_action(Action::StimulusArmOrStop, false); // Idle -> Armed
    app.handle_action(Action::StimulusFireOrPause, false); // Armed -> Driving
    assert_eq!(stim_state(app), StimState::Driving);
}

// The fix's primary guarantee: opening settings while Driving stops
// the drive first — it never stays on under the menu.
#[test]
fn opening_settings_while_driving_auto_stops_the_drive() {
    let mut app = transfer_app();
    drive(&mut app);
    app.sent_drive.clear();

    app.handle_action(Action::OpenSettings, false);

    assert_eq!(
        stim_state(&app),
        StimState::Idle,
        "drive not stopped on open"
    );
    assert!(app.settings.is_some(), "overlay did not open");
    let last = app.sent_drive.last().expect("an off must be relayed");
    assert!(
        !last.on,
        "opening settings while driving must relay set_drive off"
    );
}

// The structural invariant: even with a modal open (simulating a
// future modal that does NOT auto-stop), the panic cluster stops a
// live machine before the modal sees the key. This is the AC "panic
// works from Driving regardless of UI modal state" through the adapter.
#[test]
fn panic_first_stops_a_live_machine_even_with_a_modal_open() {
    let mut app = transfer_app();
    drive(&mut app);
    // Force the overlay open WITHOUT auto-stop — a future modal might.
    app.settings = Some(crate::settings::SettingsOverlay::from_config(
        &ac_core::config::Config::default(),
        -20.0,
    ));
    app.sent_drive.clear();

    // Esc arrives. panic_first must consume it and stop the drive —
    // not let the overlay's Esc-cancel swallow it.
    let consumed = app.panic_first(false, false, true);

    assert!(consumed, "panic key must be consumed by the stop path");
    assert_eq!(stim_state(&app), StimState::Idle, "drive not stopped");
    let last = app.sent_drive.last().expect("an off must be relayed");
    assert!(!last.on, "panic must relay set_drive off");
}

// Each panic key (Space/Esc) stops from Driving through the adapter —
// the machine's own test proves the transition; this proves the app
// relays the off for each. Enter is not a stop since #256: it pauses the
// live trace (`enter_pauses_while_driving_and_leaves_the_drive_on`).
#[test]
fn every_panic_key_relays_off_from_driving() {
    for key in ["space", "esc"] {
        let mut app = transfer_app();
        drive(&mut app);
        app.sent_drive.clear();
        let consumed = app.panic_first(key == "space", key == "enter", key == "esc");
        assert!(consumed, "{key} not consumed");
        assert_eq!(stim_state(&app), StimState::Idle, "{key} did not stop");
        assert!(!app.sent_drive.last().unwrap().on, "{key} relayed no off");
    }
}

// Drive the machine directly to Driving at a controlled instant, so
// the keepalive cadence can be exercised on logical time.
fn drive_at(app: &mut AcViewApp, t0: std::time::Instant) {
    if let ViewKind::Transfer(t) = &mut app.view {
        t.stimulus.press_space(t0);
        t.stimulus.press_enter(t0); // last_send = t0
    }
    assert_eq!(stim_state(app), StimState::Driving);
    app.sent_drive.clear();
}

// Keepalive backstop, happy path: while Driving and reachable, a tick
// past the 250 ms interval relays set_drive on.
#[test]
fn keepalive_relays_on_while_driving_and_reachable() {
    let mut app = transfer_app();
    let t0 = std::time::Instant::now();
    drive_at(&mut app, t0);
    assert!(app.panic_reachable());

    app.keepalive_tick(t0 + std::time::Duration::from_millis(300));

    let last = app
        .sent_drive
        .last()
        .expect("keepalive must relay while reachable");
    assert!(last.on, "reachable keepalive must assert the drive");
}

// Keepalive backstop, the hazard that matters: when the panic path is
// obstructed, the keepalive stays SILENT — no set_drive on — so the
// daemon's dead-man takes over instead of the UI's tick keeping an
// un-stoppable drive alive.
#[test]
fn keepalive_stays_silent_when_panic_is_obstructed() {
    let mut app = transfer_app();
    let t0 = std::time::Instant::now();
    drive_at(&mut app, t0);

    app.panic_keys_obstructed = true; // a future capturing modal
    assert!(!app.panic_reachable());

    // Even well past the keepalive interval, nothing is relayed.
    app.keepalive_tick(t0 + std::time::Duration::from_millis(300));
    app.keepalive_tick(t0 + std::time::Duration::from_millis(600));

    assert!(
        app.sent_drive.is_empty(),
        "obstructed keepalive must not assert the drive — got {:?}",
        app.sent_drive
    );
}

// panic_first is a no-op when Idle (does not consume keys the normal
// dispatch needs — e.g. Enter/Esc/Space in the settings overlay).
#[test]
fn panic_first_is_a_noop_when_idle() {
    let mut app = transfer_app();
    assert_eq!(stim_state(&app), StimState::Idle);
    assert!(
        !app.panic_first(true, true, true),
        "idle must not consume panic keys"
    );
    assert!(app.sent_drive.is_empty());
}

#[test]
fn missing_reference_channel_is_a_fatal_error_with_the_setup_hint() {
    let cfg = ac_core::config::Config {
        input_channel: 2,
        reference_channel: None,
        ..ac_core::config::Config::default()
    };
    let err = resolve_transfer_channels(&cfg).unwrap_err();
    assert!(
        err.contains("ac setup reference"),
        "error must carry the fix hint: {err}"
    );
}

#[test]
fn configured_channels_resolve_to_input_and_reference() {
    let cfg = ac_core::config::Config {
        input_channel: 2,
        reference_channel: Some(5),
        ..ac_core::config::Config::default()
    };
    assert_eq!(resolve_transfer_channels(&cfg).unwrap(), (2, 5));
}

fn transfer_frame() -> ac_core::wire::TransferFrame {
    serde_json::from_value(transfer_frame_json()).expect("wire frame")
}

/// The raw wire JSON `transfer_frame` parses — split out so a test can
/// mutate it (e.g. drop a required field) before it reaches
/// `serde_json::from_value`, exercising the same boundary the app's
/// ingest path does.
fn transfer_frame_json() -> serde_json::Value {
    // A mis-estimated delay so the wire carries non-zero phase and
    // Session (τ_derot 0) differs from the other modes — otherwise
    // cycling would be a no-op and the test could not fail on the bug
    // it names.
    // Built separately: one `json!` deep enough to hold the stage
    // table blows the macro's recursion limit.
    let stages = serde_json::Value::Array(vec![
        serde_json::json!({"decim": 1, "rate": 96000.0, "df": 23.4375, "window_s": 0.042666666666666665, "hop_s": 0.021333333333333333, "f_valid": 1623.0, "settling_s": 0.10666666666666667}),
        serde_json::json!({"decim": 8, "rate": 12000.0, "df": 2.9296875, "window_s": 0.3413333333333333, "hop_s": 0.17066666666666666, "f_valid": 202.88, "settling_s": 0.8533333333333333}),
        serde_json::json!({"decim": 24, "rate": 4000.0, "df": 0.9765625, "window_s": 1.024, "hop_s": 0.512, "f_valid": 67.63, "settling_s": 2.56}),
    ]);
    let mtw = serde_json::json!({
        "freqs": [100.0, 1000.0, 10000.0],
        "magnitude_db": [-6.0, -6.0, -6.0],
        "phase_deg": [-18.0, -180.0, 60.0],
        "coherence": [0.9, 0.9, 0.9],
        "df": [0.9765625, 2.9296875, 23.4375],
        "window_s": [1.024, 0.3413333333333333, 0.042666666666666665],
        "n": [4, 4, 4],
        "stage": [2, 1, 0],
        "blend": [0.0, 0.0, 0.0],
        "bins": [1, 3, 21],
        "ppo": 48.0,
        "n_blocks": 4,
        "stages": stages,
    });
    let json = serde_json::json!({
        "type": "transfer_stream",
        "sr": 48000,
        "meas_channel": 0,
        "ref_channel": 1,
        "spec_freqs": [100.0, 1000.0, 10000.0],
        "meas_spectrum": [0.1, 0.1, 0.1],
        "ref_spectrum": [0.1, 0.1, 0.1],
        "spl": null,
        "spl_weighting": "Z",
        "spl_integration": "fast",
        // Full-rate Welch arrays. Still on the wire, deliberately NOT
        // the display's source since the three-stage switch — kept here
        // so this fixture stays a realistic frame, and so a regression
        // that started reading them again would show up as these values
        // appearing on screen instead of the `mtw` ones below.
        "freqs": [100.0, 1000.0, 10000.0],
        "magnitude_db": [-99.0, -99.0, -99.0],
        "phase_deg": [11.0, 22.0, 33.0],
        "coherence": [0.9, 0.9, 0.9],
        "delay_samples": 96,
        "delay_ms": 2.0,
        "meas_peak_dbfs": -6.0,
        "ref_peak_dbfs": -12.0,
        // What the display actually draws (built above).
        "mtw": mtw
    });
    json
}

// Scene-accessor AC (no shape scraping): a derot keypress must change
// the BUILT transfer scene's phase segments — not merely the
// `derot_mode()` state field. Closes the hole a state-only assertion
// cannot: a mode change that fails to reach the scene.
#[test]
fn cycling_derot_changes_the_built_transfer_scene_phase() {
    let mut app = AcViewApp::new_transfer(
        Endpoint {
            host: "localhost".into(),
            ctrl_port: 0,
            data_port: 0,
        },
        TEST_DRIVE_MAX_DBFS,
    );
    app.ingest_frame_for_test(transfer_frame(), 0.0);

    let before: Vec<Vec<(f64, f64)>> = app
        .current_transfer_scene()
        .expect("scene built")
        .phase
        .segments
        .clone();

    app.press_for_test(Action::CycleDerotReference, 0.1);

    let after = &app
        .current_transfer_scene()
        .expect("scene rebuilt")
        .phase
        .segments;
    assert_ne!(
        &before, after,
        "cycling de-rotation reference did not change the built phase pane"
    );
}

// The magnitude pane must NOT move when only the de-rotation
// reference changes — de-rotation is a phase-only operation.
#[test]
fn cycling_derot_leaves_the_magnitude_pane_unchanged() {
    let mut app = AcViewApp::new_transfer(
        Endpoint {
            host: "localhost".into(),
            ctrl_port: 0,
            data_port: 0,
        },
        TEST_DRIVE_MAX_DBFS,
    );
    app.ingest_frame_for_test(transfer_frame(), 0.0);
    let before = app
        .current_transfer_scene()
        .unwrap()
        .magnitude
        .segments
        .clone();
    app.press_for_test(Action::CycleDerotReference, 0.1);
    let after = &app.current_transfer_scene().unwrap().magnitude.segments;
    assert_eq!(&before, after, "de-rotation moved the magnitude pane");
}

/// A frame whose columns are close enough together for a smoothing
/// window to hold more than one of them (#229). `transfer_frame`'s three
/// decade-apart columns cannot exercise smoothing: at 1/24 octave each is
/// alone in its own window, so a real bug would pass.
fn dense_transfer_frame() -> ac_core::wire::TransferFrame {
    let n = 24;
    // 1/48-octave spacing, stepped by repeated multiplication with the
    // ratio written out as a literal. Raising two to a fractional power
    // here would trip this crate's own AC1 guard, which scans `src/`
    // including test code and is right to — the exception would be the
    // crack that lets real arithmetic back in.
    const RATIO: f64 = 1.014_545_334_9; // 2^(1/48)
    let freqs: Vec<f64> = (0..n)
        .scan(1000.0, |f, _| {
            let out = *f;
            *f *= RATIO;
            Some(out)
        })
        .collect();
    let mag: Vec<f64> = (0..n)
        .map(|i| if i % 2 == 0 { -14.0 } else { -26.0 })
        .collect();
    let mtw = serde_json::json!({
        "freqs": freqs,
        "magnitude_db": mag,
        "phase_deg": vec![0.0_f64; n],
        "coherence": vec![0.9_f64; n],
        "df": vec![1.0_f64; n],
        "window_s": vec![1.0_f64; n],
        "n": vec![4_u32; n],
        "stage": vec![0_usize; n],
        "blend": vec![0.0_f64; n],
        "bins": vec![1_u32; n],
        "ppo": 48.0,
        "n_blocks": 4,
        "stages": [],
    });
    let mut f = transfer_frame();
    f.mtw = serde_json::from_value(mtw).expect("mtw columns");
    f
}

// Scene-accessor AC, the same rule the derot keys are held to: the
// smoothing key must change the BUILT magnitude pane, not merely the
// state field.
#[test]
fn cycling_smoothing_changes_the_built_transfer_magnitude() {
    let mut app = AcViewApp::new_transfer(
        Endpoint {
            host: "localhost".into(),
            ctrl_port: 0,
            data_port: 0,
        },
        TEST_DRIVE_MAX_DBFS,
    );
    app.ingest_frame_for_test(dense_transfer_frame(), 0.0);

    let before = app
        .current_transfer_scene()
        .expect("scene built")
        .magnitude
        .segments
        .clone();
    assert_eq!(
        app.current_transfer_scene().unwrap().smoothing_readout,
        None,
        "a session must open unsmoothed"
    );

    app.press_for_test(Action::CycleSmoothing, 0.1);

    let after = app.current_transfer_scene().expect("scene rebuilt");
    assert_ne!(
        &before, &after.magnitude.segments,
        "cycling smoothing did not change the built magnitude pane"
    );
    assert_eq!(
        after.smoothing_readout,
        Some("smoothing 1/24 octave"),
        "the smoothed trace must say so on screen"
    );
}

/// A real `PairDerivation` cheap enough for a unit test — the same
/// `derive_pair` path `open_stored_transfer_run` uses, just fed
/// samples directly instead of via a written `.acsnap` (no fixture
/// file needed for what this test is about: dispatch, not decoding).
/// A deterministic pseudo-noise sequence, not a sine — `sin` is one
/// of `computes_nothing`'s forbidden tokens, enforced over all of
/// `src/` including test code, so a tone fixture cannot live here
/// (`it_trace_comparison.rs`, in `tests/`, is outside that scan and
/// uses a real one).
fn fixture_derivation() -> ac_core::visualize::pair_derivation::PairDerivation {
    let sr = 48_000u32;
    let n = sr as usize;
    let samples: Vec<f32> = (0..n).map(|i| (i % 97) as f32 / 97.0 - 0.5).collect();
    ac_core::visualize::pair_derivation::derive_pair(
        &samples,
        &samples,
        sr,
        0,
        None,
        None,
        WeightingCurve::Z,
    )
}

fn loaded_run(label: &str, captured_at_utc: &str) -> crate::view::LoadedRun {
    crate::view::LoadedRun::new(
        label.to_string(),
        captured_at_utc.to_string(),
        fixture_derivation(),
        "meas_0".to_string(),
        48_000,
    )
}

fn focus_of(app: &AcViewApp) -> crate::view::Focus {
    match &app.view {
        ViewKind::Transfer(state) => state.focus,
        ViewKind::Spectrum(_) => panic!("not transfer view"),
    }
}

// QA #336, test coverage gap: `Action::CycleFocus` and
// `Action::CloseFocusedRun` are wired in `handle_action` (#321) but
// were exercised only by calling `TransferViewState::cycle_focus` /
// `close_focused_stored_run` directly (`it_trace_comparison.rs`),
// never through the actual keypress dispatch path. This drives both
// through `handle_action`, the same entry point a real `Tab`/`X`
// press reaches.
#[test]
fn cycle_focus_and_close_focused_run_reach_transfer_view_state_through_dispatch() {
    let mut app = transfer_app();
    if let ViewKind::Transfer(state) = &mut app.view {
        state.add_loaded_run(loaded_run("a.acsnap", "2026-01-01T00:00:00Z"));
        state.add_loaded_run(loaded_run("b.acsnap", "2026-01-02T00:00:00Z"));
    }
    assert_eq!(
        focus_of(&app),
        crate::view::Focus::Stored(1),
        "load-order focus, established elsewhere — the starting point here"
    );

    // `Tab`, through dispatch: Stored(1) is the last run, so this
    // wraps to Live.
    app.handle_action(Action::CycleFocus, false);
    assert_eq!(
        focus_of(&app),
        crate::view::Focus::Live,
        "Action::CycleFocus did not reach TransferViewState::cycle_focus"
    );

    // `Tab` again: Live -> Stored(0).
    app.handle_action(Action::CycleFocus, false);
    assert_eq!(focus_of(&app), crate::view::Focus::Stored(0));

    // `X`, through dispatch: closes the focused run (a.acsnap) and
    // leaves b.acsnap as the sole remaining run.
    app.handle_action(Action::CloseFocusedRun, false);
    match &app.view {
        ViewKind::Transfer(state) => {
            assert_eq!(
                state.loaded.len(),
                1,
                "Action::CloseFocusedRun did not reach \
                 TransferViewState::close_focused_stored_run"
            );
            assert_eq!(state.loaded[0].label, "b.acsnap");
        }
        ViewKind::Spectrum(_) => panic!("not transfer view"),
    }
}

/// A driving frame with no delay yet, built from the healthy fixture so
/// only the fields the indicator reads differ.
fn unaligned_frame() -> ac_core::wire::TransferFrame {
    let mut f = transfer_frame();
    f.drive = Some(ac_core::wire::WireDrive {
        on: true,
        level_dbfs: Some(-30.0),
        drivable: true,
    });
    f.delay_locked = Some(false);
    f.delay_attempts = 0;
    f
}

/// The fixture with a held delay of 400 whose live IR peaks 12 later.
fn found_frame() -> ac_core::wire::TransferFrame {
    let mut f = unaligned_frame();
    f.delay_locked = Some(true);
    f.delay_attempts = 1;
    f.delay_samples = 400;
    f.delay_residual = Some(12);
    f
}

/// A delay arriving shows no banner (#256): the readout says it.
#[test]
fn a_found_delay_shows_no_banner() {
    let mut app = transfer_app();
    app.ingest_frame_for_test(unaligned_frame(), 0.0);
    app.ingest_frame_for_test(found_frame(), 1.0);
    assert_eq!(app.current_transfer_scene().unwrap().fault, None);
}

/// `E` inserts what Find reads — the held delay plus the residual, as the
/// scene computed it — and Shift+E asks the daemon to find again.
#[test]
fn e_inserts_the_found_delay_and_shift_e_finds_again() {
    let mut app = transfer_app();
    app.ingest_frame_for_test(found_frame(), 0.0);
    app.handle_action(Action::InsertDelay, false);
    app.handle_action(Action::InsertDelay, true);
    assert_eq!(
        app.sent_delay,
        vec![
            serde_json::json!({"cmd": "set_delay", "samples": 412}),
            serde_json::json!({"cmd": "set_delay", "samples": null}),
        ]
    );
}

/// `,` and `.` step the held delay by one sample — relative, so two presses
/// between frames are two steps, not the same value sent twice.
#[test]
fn arrows_step_the_live_delay_by_one_or_ten() {
    let mut app = transfer_app();
    app.ingest_frame_for_test(found_frame(), 0.0);
    app.handle_action(Action::NudgeDelayEarlier, false);
    app.handle_action(Action::NudgeDelayLater, false);
    app.handle_action(Action::NudgeDelayLater, true);
    assert_eq!(
        app.sent_delay,
        vec![
            serde_json::json!({"cmd": "set_delay", "step": -1}),
            serde_json::json!({"cmd": "set_delay", "step": 1}),
            serde_json::json!({"cmd": "set_delay", "step": 10}),
        ]
    );
}

/// With a slot selected, `←`/`→` move that slot's delay and nothing else
/// (#256): the daemon hears nothing, and the slot's readout follows.
#[test]
fn arrows_move_the_selected_slot_not_live() {
    let mut app = transfer_app();
    app.ingest_frame_for_test(found_frame(), 0.0);
    app.finish_capture_for_test(Ok(crate::capture::Captured {
        slot: 1,
        opened: false,
        path: std::path::PathBuf::from("/c/slot1.acsnap"),
        run: loaded_run("slot 1", "2026-09-26T14:00:00Z"),
    }));
    app.handle_action(Action::CycleFocus, false); // slot 1
    app.rebuild_scenes(true, 0.0);
    let before = app.current_loaded_scenes()[0].delay_readout.clone();
    app.handle_action(Action::NudgeDelayLater, true);
    app.handle_action(Action::NudgeDelayEarlier, false);
    assert!(app.sent_delay.is_empty(), "a slot nudge reached the daemon");
    let ViewKind::Transfer(t) = &app.view else {
        panic!("not transfer view")
    };
    assert_eq!(t.loaded[0].delay_offset_samples, 9);
    let recorded = ac_scene::TransferInput::stored_delay_ms(&t.loaded[0].derivation);
    // The Snapshot de-rotation reference is the slot as drawn, nudge
    // included (Codex review).
    app.with_transfer(|t| t.derot = crate::view::DerotChoice::Snapshot);
    let ViewKind::Transfer(t) = &app.view else {
        panic!("not transfer view")
    };
    assert_eq!(
        t.derot_mode(),
        ac_scene::DerotMode::Snapshot {
            snapshot_delay_ms: recorded + 9.0 * 1000.0 / 48_000.0
        }
    );
    app.with_transfer(|t| t.derot = crate::view::DerotChoice::Session);
    let phase_before = app.current_loaded_scenes()[0].phase.clone();
    app.rebuild_scenes(true, 1.0);
    let after = &app.current_loaded_scenes()[0];
    assert_ne!(after.delay_readout, before);
    assert_ne!(after.phase, phase_before, "the slot's phase did not move");
}

/// Without a delay there is nothing to insert or nudge, and a guessed
/// value would be worse than none.
#[test]
fn no_delay_keys_send_anything_before_a_delay_exists() {
    let mut app = transfer_app();
    app.ingest_frame_for_test(unaligned_frame(), 0.0);
    app.handle_action(Action::InsertDelay, false);
    app.handle_action(Action::NudgeDelayEarlier, false);
    app.handle_action(Action::NudgeDelayLater, false);
    assert!(app.sent_delay.is_empty(), "sent {:?}", app.sent_delay);
}

/// `T` opens the entry; digits edit; `T` applies; an empty entry and Esc
/// cancel without sending.
#[test]
fn a_typed_delay_applies_on_t_and_cancels_when_empty_or_on_esc() {
    let mut app = transfer_app();
    app.handle_action(Action::TypeDelay, false);
    app.handle_delay_entry_keys("-25", false, false, false);
    app.handle_delay_entry_keys("", true, false, false);
    app.handle_delay_entry_keys("0", false, true, false);
    let typed = vec![serde_json::json!({"cmd": "set_delay", "samples": -20})];
    assert_eq!(app.sent_delay, typed);
    assert!(app.delay_entry.is_none());

    app.handle_action(Action::TypeDelay, false);
    app.handle_delay_entry_keys("", false, true, false);
    app.handle_action(Action::TypeDelay, false);
    app.handle_delay_entry_keys("99", false, false, true);
    assert_eq!(app.sent_delay, typed, "a cancel sent a delay");
    assert!(app.delay_entry.is_none());
}

/// The #225 session in one test: driving, reference leg dead, and the
/// screen says which leg rather than leaving the operator to infer it
/// from a wrong-looking top end.
#[test]
fn a_dead_reference_leg_names_itself_on_the_transfer_scene() {
    let mut app = AcViewApp::new_transfer(
        Endpoint {
            host: "localhost".into(),
            ctrl_port: 0,
            data_port: 0,
        },
        TEST_DRIVE_MAX_DBFS,
    );
    let mut frame = found_frame();
    frame.ref_peak_dbfs = None;
    app.ingest_frame_for_test(frame, 0.0);
    assert_eq!(
        app.current_transfer_scene().unwrap().fault,
        Some(ac_scene::Fault::NoReference)
    );
}

/// Today's daemon sends neither field. The indicator must stay silent
/// rather than read absent levels as silence.
#[test]
fn a_frame_without_drive_state_shows_no_indicator() {
    let mut app = AcViewApp::new_transfer(
        Endpoint {
            host: "localhost".into(),
            ctrl_port: 0,
            data_port: 0,
        },
        TEST_DRIVE_MAX_DBFS,
    );
    app.ingest_frame_for_test(transfer_frame(), 0.0);
    assert_eq!(app.current_transfer_scene().unwrap().fault, None);
}

// #193: the status line must say `malformed`, with a count, once a run
// of frames that fail the `TransferFrame` schema clears the grace window —
// driven through `ingest_raw_frame` (the raw-JSON boundary), not
// `ingest_frame_for_test`, so the test exercises the same
// `serde_json::from_value` failure #192's blank-but-"live" view hid.
#[test]
fn a_sustained_run_of_malformed_frames_flips_status_to_malformed_with_a_count() {
    let mut app = AcViewApp::new(Endpoint {
        host: "localhost".into(),
        ctrl_port: 5556,
        data_port: 5557,
    });
    let t0 = Instant::now();
    // Missing `sr`, a required field — fails to deserialize into
    // `TransferFrame` rather than being silently dropped and forgotten.
    let mut bad = transfer_frame_json();
    bad.as_object_mut().unwrap().remove("sr");

    for _ in 0..7 {
        assert!(
            !app.ingest_raw_frame(bad.clone(), t0),
            "a frame missing `sr` must fail to parse"
        );
    }
    assert_eq!(app.frame_parse_failures, 7);

    // Before the grace window clears, the status must still read
    // `live` — a run of bad frames must not out-race the grace period.
    assert_eq!(
        app.status_for_state(
            ConnectionState::Live,
            t0 + MALFORMED_GRACE - Duration::from_millis(1)
        ),
        "live — localhost:5556",
        "status flipped before the grace window cleared"
    );

    // Once the grace window clears, `malformed` replaces `live` and
    // carries the streak count — `live` must not appear while every
    // frame is being dropped (acceptance criterion, verbatim).
    let status = app.status_for_state(ConnectionState::Live, t0 + MALFORMED_GRACE);
    assert_eq!(
        status,
        "malformed — localhost:5556 — 7 consecutive frames dropped, not rendering"
    );
}

// A single dropped frame in an otherwise-healthy stream must not
// flicker the status: one bad frame followed by a good one, well
// inside the grace window, must never read `malformed` — the good
// frame clears the streak before the grace gate ever gets to fire.
#[test]
fn a_single_malformed_frame_followed_by_a_good_one_never_flips_the_status() {
    let mut app = AcViewApp::new(Endpoint {
        host: "localhost".into(),
        ctrl_port: 5556,
        data_port: 5557,
    });
    let t0 = Instant::now();
    let mut bad = transfer_frame_json();
    bad.as_object_mut().unwrap().remove("sr");

    app.ingest_raw_frame(bad, t0);
    assert!(app.ingest_raw_frame(transfer_frame_json(), t0 + Duration::from_millis(50)));

    // Checked at every point up to and past the grace window: the
    // streak was cleared by the good frame, so it never fires.
    for elapsed in [
        Duration::from_millis(50),
        MALFORMED_GRACE,
        MALFORMED_GRACE * 10,
    ] {
        assert_eq!(
            app.status_for_state(ConnectionState::Live, t0 + elapsed),
            "live — localhost:5556",
            "a single glitch flickered the status at t0+{elapsed:?}"
        );
    }
}

// The happy path (AC): a run of good frames reports live-and-rendering
// with no false `malformed` indicator, and a parse success clears a
// prior streak instead of leaving a stale failure count behind it.
#[test]
fn good_frames_report_live_and_clear_a_prior_malformed_streak() {
    let mut app = AcViewApp::new(Endpoint {
        host: "localhost".into(),
        ctrl_port: 5556,
        data_port: 5557,
    });
    let t0 = Instant::now();
    let mut bad = transfer_frame_json();
    bad.as_object_mut().unwrap().remove("sr");
    for _ in 0..3 {
        app.ingest_raw_frame(bad.clone(), t0);
    }
    assert_eq!(app.frame_parse_failures, 3);

    assert!(app.ingest_raw_frame(transfer_frame_json(), t0 + MALFORMED_GRACE));
    assert_eq!(
        app.frame_parse_failures, 0,
        "a good parse must reset the streak"
    );
    assert_eq!(
        app.status_for_state(ConnectionState::Live, t0 + MALFORMED_GRACE),
        "live — localhost:5556",
        "status must not stay malformed after a good frame"
    );
}

// connected-but-no-frames (the third AC state) is `Disconnected`,
// already distinct from both `live` and the new `malformed` — this
// pins that a malformed streak never masks it, since `Disconnected`
// only happens once the raw socket itself has gone quiet.
#[test]
fn disconnected_state_is_unaffected_by_a_malformed_streak() {
    let mut app = AcViewApp::new(Endpoint {
        host: "localhost".into(),
        ctrl_port: 5556,
        data_port: 5557,
    });
    let t0 = Instant::now();
    let mut bad = transfer_frame_json();
    bad.as_object_mut().unwrap().remove("sr");
    for _ in 0..3 {
        app.ingest_raw_frame(bad.clone(), t0);
    }
    assert_eq!(
        app.status_for_state(ConnectionState::Disconnected, t0 + MALFORMED_GRACE),
        "disconnected — localhost:5556 not responding"
    );
}

// A real disconnect must not let a stale streak fast-path the grace
// window on the next session: a malformed streak from before an outage
// must not survive it, and the first post-reconnect frame must not
// skip MALFORMED_GRACE (#301 review).
#[test]
fn a_streak_does_not_survive_a_real_disconnect() {
    let mut app = AcViewApp::new(Endpoint {
        host: "localhost".into(),
        ctrl_port: 5556,
        data_port: 5557,
    });
    let t0 = Instant::now();
    let mut bad = transfer_frame_json();
    bad.as_object_mut().unwrap().remove("sr");

    // Streak builds and clears the grace window before the outage.
    for _ in 0..5 {
        app.ingest_raw_frame(bad.clone(), t0);
    }
    assert_eq!(
        app.status_for_state(ConnectionState::Live, t0 + MALFORMED_GRACE),
        "malformed — localhost:5556 — 5 consecutive frames dropped, not rendering"
    );

    // The daemon actually goes away — real disconnect, no frames at all.
    let t_reconnect = t0 + MALFORMED_GRACE + Duration::from_secs(15);
    assert_eq!(
        app.status_for_state(ConnectionState::Disconnected, t_reconnect),
        "disconnected — localhost:5556 not responding"
    );

    // Session resumes; first frame back is bad again. This is a *new*
    // run — it must get its own grace window, not inherit the old one.
    assert!(!app.ingest_raw_frame(bad, t_reconnect));
    assert_eq!(
        app.status_for_state(
            ConnectionState::Live,
            t_reconnect + Duration::from_millis(1)
        ),
        "live — localhost:5556",
        "post-reconnect streak reused the pre-outage grace timer"
    );
}

// ---------------------------------------------------------------
// Wire-version refusal (#112)
// ---------------------------------------------------------------

fn with_wire_version(mut v: serde_json::Value, version: u64) -> serde_json::Value {
    v["wire_version"] = serde_json::json!(version);
    v
}

fn localhost_transfer_app() -> AcViewApp {
    AcViewApp::new_transfer(
        Endpoint {
            host: "localhost".into(),
            ctrl_port: 5556,
            data_port: 5557,
        },
        0.0,
    )
}

// The state, both versions and the count, in the sibling states' shape. The
// range collapses to `v1` while MIN_WIRE_VERSION == WIRE_VERSION.
#[test]
fn a_refused_frame_reports_version_mismatch_naming_both_versions() {
    let mut app = localhost_transfer_app();
    let t0 = Instant::now();
    assert!(!app.ingest_raw_frame(with_wire_version(transfer_frame_json(), 2), t0));
    assert_eq!(
        app.status_for_state(ConnectionState::Live, t0),
        "version mismatch — localhost:5556 — daemon sends wire v2, ac-view reads v1 \
         — 1 frames refused, not rendering"
    );
}

// The count is what separates "wrong version, alive" from a dead stream, so
// it has to move with every refused frame — sidecars included.
#[test]
fn the_refused_count_rises_with_every_refused_frame() {
    let mut app = localhost_transfer_app();
    let t0 = Instant::now();
    for _ in 0..3 {
        app.ingest_raw_frame(with_wire_version(transfer_frame_json(), 2), t0);
    }
    app.ingest_raw_ir_frame(with_wire_version(ir_frame_json(), 2));
    assert!(app
        .status_for_state(ConnectionState::Live, t0)
        .ends_with("— 4 frames refused, not rendering"));
}

// A refusal is not a parse failure: no malformed streak, no grace window, and
// the refusal wins the status line even once the grace window has passed.
#[test]
fn a_refused_frame_does_not_advance_the_malformed_streak() {
    let mut app = localhost_transfer_app();
    let t0 = Instant::now();
    for _ in 0..5 {
        app.ingest_raw_frame(with_wire_version(transfer_frame_json(), 0), t0);
    }
    assert_eq!(app.frame_parse_failures, 0);
    assert!(app.first_malformed_since.is_none());
    assert!(app
        .status_for_state(ConnectionState::Live, t0 + MALFORMED_GRACE)
        .starts_with("version mismatch — localhost:5556 — daemon sends wire v0, ac-view reads v1"));
}

// Nothing drawn from before the refusal may stay up: those frames came from
// a daemon build this client no longer reads, and would look current.
#[test]
fn a_refusal_clears_the_held_frames_and_their_scenes() {
    let mut app = localhost_transfer_app();
    app.handle_action(Action::ToggleIrPanel, false);
    let t0 = Instant::now();
    assert!(app.ingest_raw_frame(transfer_frame_json(), t0));
    app.ingest_raw_ir_frame(ir_frame_json());
    app.rebuild_scenes(true, 0.0);
    assert!(app.current_transfer_scene().is_some());
    assert!(app.current_ir_scene().is_some());

    assert!(!app.ingest_raw_frame(with_wire_version(transfer_frame_json(), 2), t0));
    app.rebuild_scenes(true, 0.1);
    assert!(app.last_frame.is_none());
    assert!(app.current_transfer_scene().is_none());
    assert!(app.last_ir_frame.is_none());
    assert!(app.current_ir_scene().is_none());
}

// A refused sidecar alone also drops the IR panel's held frame.
#[test]
fn a_refused_ir_frame_is_not_held() {
    let mut app = localhost_transfer_app();
    app.ingest_raw_ir_frame(with_wire_version(ir_frame_json(), 2));
    assert!(app.last_ir_frame.is_none());
}

// An accepted frame — the daemon restarted on a matching build — ends the
// state and resets the count; so does a real disconnect.
#[test]
fn an_accepted_frame_or_a_disconnect_ends_the_refusal() {
    let mut app = localhost_transfer_app();
    let t0 = Instant::now();
    app.ingest_raw_frame(with_wire_version(transfer_frame_json(), 2), t0);
    assert!(app.ingest_raw_frame(with_wire_version(transfer_frame_json(), 1), t0));
    assert_eq!(
        app.status_for_state(ConnectionState::Live, t0),
        "live — localhost:5556"
    );

    app.ingest_raw_frame(with_wire_version(transfer_frame_json(), 2), t0);
    assert_eq!(
        app.status_for_state(ConnectionState::Disconnected, t0),
        "disconnected — localhost:5556 not responding"
    );
    assert_eq!(
        app.status_for_state(ConnectionState::Live, t0),
        "live — localhost:5556"
    );
    // A new refusal after that counts from one again.
    app.ingest_raw_frame(with_wire_version(transfer_frame_json(), 2), t0);
    assert!(app
        .status_for_state(ConnectionState::Live, t0)
        .ends_with("— 1 frames refused, not rendering"));
}

// A frame without the field is a daemon predating it: accepted as v1.
#[test]
fn an_absent_wire_version_is_accepted() {
    let mut app = localhost_transfer_app();
    let v = transfer_frame_json();
    assert!(v.get("wire_version").is_none());
    assert!(app.ingest_raw_frame(v, Instant::now()));
}

// ---------------------------------------------------------------
// IR panel (#286)
// ---------------------------------------------------------------

fn ir_frame() -> ac_core::wire::IrFrame {
    serde_json::from_value(ir_frame_json()).expect("ir wire frame")
}

/// The raw wire JSON `ir_frame` parses — split out so the drain test
/// (#219) can stamp and serialize it as an injected sidecar frame.
fn ir_frame_json() -> serde_json::Value {
    serde_json::json!({
        "samples": [0.0, 1.0, -0.5, 0.0],
        "sr": 48000,
        "stride": 24,
        "dt_ms": 250.0,
        "t_origin_ms": -500.0,
        "ref_channel": 1,
        "meas_channel": 0,
        "delay_samples": 231,
        "delay_ms": 4.82,
        "delay_locked": true
    })
}

fn ir_app() -> AcViewApp {
    AcViewApp::new_transfer(
        Endpoint {
            host: "localhost".into(),
            ctrl_port: 0,
            data_port: 0,
        },
        TEST_DRIVE_MAX_DBFS,
    )
}

// The panel is closed by default: a received sidecar frame alone
// must not build a scene the view never asked for.
#[test]
fn ir_scene_stays_none_while_the_panel_is_closed() {
    let mut app = ir_app();
    app.ingest_ir_frame_for_test(ir_frame());
    assert!(app.current_ir_scene().is_none());
}

// Opening the panel (`H`) with a frame already held builds the scene
// immediately — it does not wait for the next sidecar frame to
// arrive, matching the toggle-then-frame ordering test below.
#[test]
fn opening_the_panel_builds_the_scene_from_the_held_frame() {
    let mut app = ir_app();
    app.ingest_ir_frame_for_test(ir_frame());
    app.press_for_test(Action::ToggleIrPanel, 0.0);

    let scene = app.current_ir_scene().expect("scene built on open");
    assert_eq!(scene.header, ac_scene::IR_HEADER);
    assert!(!scene.trace.segments.is_empty());
}

// The order the operator is more likely to hit in practice: panel
// opened first (nothing to show yet), a frame arrives after.
#[test]
fn a_frame_arriving_after_the_panel_opens_still_builds_the_scene() {
    let mut app = ir_app();
    app.press_for_test(Action::ToggleIrPanel, 0.0);
    assert!(app.current_ir_scene().is_none(), "no frame held yet");

    app.ingest_ir_frame_for_test(ir_frame());
    assert!(app.current_ir_scene().is_some());
}

// Closing the panel again clears the built scene, not just the state
// flag — the same scene-accessor discipline the derot/smoothing keys
// are held to elsewhere in this module.
#[test]
fn closing_the_panel_clears_the_built_scene() {
    let mut app = ir_app();
    app.ingest_ir_frame_for_test(ir_frame());
    app.press_for_test(Action::ToggleIrPanel, 0.0);
    assert!(app.current_ir_scene().is_some());

    app.press_for_test(Action::ToggleIrPanel, 0.0);
    assert!(app.current_ir_scene().is_none());
}

// The toggle only affects the transfer view's own panel — pressing it
// in the spectrum view (where the binding isn't even offered) must
// not fabricate an IR scene.
#[test]
fn toggle_ir_panel_is_a_noop_outside_the_transfer_view() {
    let mut app = AcViewApp::new(Endpoint {
        host: "localhost".into(),
        ctrl_port: 0,
        data_port: 0,
    });
    app.ingest_ir_frame_for_test(ir_frame());
    app.press_for_test(Action::ToggleIrPanel, 0.0);
    assert!(app.current_ir_scene().is_none());
}

// ---------------------------------------------------------------
// Drain to newest over an injected DATA stream (#219 Part B)
// ---------------------------------------------------------------

/// The M2 captured `transfer_stream` frame (the file
/// `ac-scene/tests/it_fixtures.rs` reads) — the injected payload, so
/// the drain test runs on a real wire frame rather than a hand-built one.
const M2_TRANSFER_FRAME: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../tests/fixtures/transfer-frame-v2.json"
));

/// Transfer frames (and their IR sidecars) in the injected backlog.
const DRAIN_BACKLOG: i64 = 6;

/// One `<topic> <json>` DATA payload, as the daemon puts it on the wire.
fn wire(topic: &str, payload: &serde_json::Value) -> Vec<u8> {
    format!("{topic} {payload}").into_bytes()
}

/// A mixed DATA backlog shaped like a live `transfer_stream` session
/// publishes it (the Part A capture on #219): each transfer frame
/// followed by its `visualize/ir` sidecar, with `keepalive` frames
/// scattered through. Frame *k* and sidecar *k* carry
/// `delay_samples = k`, so which one survived a drain is readable off
/// the held frame. Also carries, mid-stream so an early stop is
/// visible: a `keepalive` directly after transfer frame 0, a `data`
/// frame of a type `ac-view` does not consume, and two malformed
/// frames (no separator; bad JSON). Returns the raw payloads and the
/// number of malformed ones among them.
fn mixed_data_backlog() -> (Vec<Vec<u8>>, u64) {
    let transfer: serde_json::Value =
        serde_json::from_str(M2_TRANSFER_FRAME).expect("M2 fixture is json");
    assert_eq!(
        transfer["type"], "transfer_stream",
        "M2 fixture must be a transfer_stream frame"
    );
    let mut ir = ir_frame_json();
    ir["type"] = serde_json::json!("visualize/ir");
    let keepalive = serde_json::json!({"type": "keepalive"});

    let mut frames = Vec::new();
    let mut malformed = 0;
    for k in 0..DRAIN_BACKLOG {
        let mut t = transfer.clone();
        t["delay_samples"] = serde_json::json!(k);
        frames.push(wire("data", &t));
        if k == 0 {
            frames.push(wire("keepalive", &keepalive));
        }
        let mut s = ir.clone();
        s["delay_samples"] = serde_json::json!(k);
        frames.push(wire("data", &s));
        if k == 2 {
            frames.push(b"data-with-no-separator".to_vec());
            frames.push(b"data {not json".to_vec());
            malformed += 2;
            // Any `data` type this crate has no consumer for.
            frames.push(wire("data", &serde_json::json!({"type": "not_consumed"})));
        }
        if k == 4 {
            frames.push(wire("keepalive", &keepalive));
        }
    }
    (frames, malformed)
}

// One drain pass over a backlog must leave the newest transfer frame and
// the newest IR sidecar held, and nothing pending — the property the
// load-bearing comment in `drain_frames` claims. Driven through the same
// `parse_frame` → `poll_next` → `collect_drained` → `ingest_drained`
// path the live loop runs, with the socket replaced by a queue: no real
// socket, no timing, so it cannot flake on fill state the way the
// reverted live-socket version did. Mutation-checked at birth against
// keep-oldest ingest, stop-at-first-skipped-frame, and
// stop-at-first-malformed-frame (see the PR for #219 Part B).
#[test]
fn one_drain_pass_over_a_mixed_backlog_keeps_the_newest_frames() {
    let (frames, malformed_injected) = mixed_data_backlog();
    let mut queue: std::collections::VecDeque<crate::zmq_client::Recv> = frames
        .iter()
        .map(|b| crate::zmq_client::parse_frame(b))
        .collect();
    // Not vacuous: two frames per transfer tick, plus the extras.
    assert_eq!(queue.len(), frames.len());
    assert!(queue.len() > 2 * DRAIN_BACKLOG as usize);
    assert_eq!(malformed_injected, 2);

    let mut app = ir_app();
    let mut malformed_counted = 0u64;
    let (drained, drained_ir) = collect_drained(|| {
        crate::session::poll_next(
            || queue.pop_front().unwrap_or(crate::zmq_client::Recv::Empty),
            &mut malformed_counted,
        )
    });
    assert!(
        queue.is_empty(),
        "drain left {} frames pending",
        queue.len()
    );
    assert_eq!(drained.len(), DRAIN_BACKLOG as usize);
    assert_eq!(drained_ir.len(), DRAIN_BACKLOG as usize);
    assert_eq!(malformed_counted, malformed_injected);

    let got_new_frame = app.ingest_drained(drained, drained_ir, std::time::Instant::now());
    assert!(got_new_frame, "no injected transfer frame was accepted");
    let held = app.last_frame.as_ref().expect("a transfer frame is held");
    assert_eq!(held.delay_samples, DRAIN_BACKLOG - 1);
    let held_ir = app.last_ir_frame.as_ref().expect("an IR frame is held");
    assert_eq!(held_ir.delay_samples, DRAIN_BACKLOG - 1);
}

// ---- #256: capture, pause, compare ----

/// `Ctrl`+digit with no session answers on screen instead of doing nothing.
#[test]
fn a_slot_store_without_a_session_says_so() {
    let mut app = transfer_app();
    app.store_slot_request(1, std::time::Instant::now());
    assert_eq!(
        app.toast_text(),
        Some("no session \u{2014} nothing to store")
    );
}

/// Storing needs live to roll (#256): while paused it refuses and says how
/// to resume, and starts nothing.
#[test]
fn a_slot_store_while_paused_is_refused() {
    let mut app = transfer_app();
    app.handle_action(Action::StimulusFireOrPause, false); // pause (idle)
    app.store_slot_request(5, std::time::Instant::now());
    assert_eq!(
        app.toast_text(),
        Some("live is paused \u{2014} press Enter to resume, then store slot 5")
    );
    assert!(app.capture_slot.is_none());
}

/// A stored slot is overlaid and reported; storing the same slot again
/// replaces it in place; slots sit in number order.
#[test]
fn slots_replace_in_place_and_sit_in_order() {
    let mut app = transfer_app();
    let captured = |slot: u8, t: &str| {
        Ok(crate::capture::Captured {
            slot,
            opened: false,
            path: std::path::PathBuf::from(format!("/c/slot{slot}.acsnap")),
            run: loaded_run(&format!("slot {slot}"), t),
        })
    };
    app.finish_capture_for_test(captured(5, "2026-09-26T14:00:00Z"));
    assert_eq!(
        app.toast_text(),
        Some("slot 5 stored \u{2014} /c/slot5.acsnap")
    );
    app.finish_capture_for_test(captured(1, "2026-09-26T14:01:00Z"));
    app.finish_capture_for_test(captured(5, "2026-09-26T14:02:00Z"));
    let ViewKind::Transfer(t) = &app.view else {
        panic!("not transfer view")
    };
    assert_eq!(
        t.loaded
            .iter()
            .map(|r| (r.slot, r.captured_at_utc.as_str(), r.color_slot))
            .collect::<Vec<_>>(),
        [
            (Some(1), "2026-09-26T14:01:00Z", 0),
            (Some(5), "2026-09-26T14:02:00Z", 4)
        ]
    );
    app.finish_capture_for_test(Err("no transfer_stream session running".into()));
    assert_eq!(
        app.toast_text(),
        Some("storing failed \u{2014} no transfer_stream session running")
    );
}

/// Enter pauses unless armed, and never stops a running drive (#256):
/// Space and Esc are the stops.
#[test]
fn enter_pauses_while_driving_and_leaves_the_drive_on() {
    let mut app = transfer_app();
    app.handle_action(Action::StimulusArmOrStop, false); // armed
    app.handle_action(Action::StimulusFireOrPause, false); // fires
    let ViewKind::Transfer(t) = &app.view else {
        panic!("not transfer view")
    };
    assert_eq!(t.stimulus.state(), crate::stimulus::StimState::Driving);
    assert!(!t.paused);
    // Through panic-first, as a real Enter press arrives.
    assert!(
        !app.panic_first(false, true, false),
        "Enter taken as a stop"
    );
    app.handle_action(Action::StimulusFireOrPause, false);
    let ViewKind::Transfer(t) = &app.view else {
        panic!("not transfer view")
    };
    assert_eq!(t.stimulus.state(), crate::stimulus::StimState::Driving);
    assert!(t.paused);
}

/// Enter hides the live trace (#256): the scene keeps rolling — the
/// readouts and meters follow the newest frame — and only the drawing
/// leaves the live trace out, so the slots are compared on their own.
#[test]
fn enter_hides_the_live_trace_and_keeps_the_scene_rolling() {
    let mut app = transfer_app();
    let mut a = transfer_frame();
    a.delay_ms = 1.0;
    let mut b = transfer_frame();
    b.delay_ms = 2.0;
    let now = std::time::Instant::now();
    assert!(app.ingest_raw_frame(serde_json::to_value(&a).unwrap(), now));
    app.handle_action(Action::StimulusFireOrPause, false);
    assert!(app.ingest_raw_frame(serde_json::to_value(&b).unwrap(), now));
    app.rebuild_scenes(true, 1.0);
    assert_eq!(
        app.current_transfer_scene().unwrap().delay_readout,
        "2.00 ms"
    );
    let ViewKind::Transfer(t) = &app.view else {
        panic!("not transfer view")
    };
    assert!(t.paused && !t.live_trace_shown());
    app.handle_action(Action::StimulusFireOrPause, false);
    let ViewKind::Transfer(t) = &app.view else {
        panic!("not transfer view")
    };
    assert!(t.live_trace_shown());
}

/// `V` hides the focused trace — live or stored — and `Shift+V` shows all.
/// Colours stay with their run when another is removed.
#[test]
fn v_hides_the_focused_trace_and_colours_stay_put() {
    let mut app = transfer_app();
    app.with_transfer(|t| {
        t.add_run(loaded_run("a.acsnap", "2026-09-26T14:00:00Z"));
        t.add_run(loaded_run("b.acsnap", "2026-09-26T14:01:00Z"));
        t.add_run(loaded_run("c.acsnap", "2026-09-26T14:02:00Z"));
    });
    app.handle_action(Action::ToggleTraceVisible, false); // live
    app.handle_action(Action::CycleFocus, false);
    app.handle_action(Action::CycleFocus, false); // b
    app.handle_action(Action::ToggleTraceVisible, false);
    {
        let ViewKind::Transfer(t) = &app.view else {
            panic!("not transfer view")
        };
        assert!(!t.live_visible);
        assert_eq!(
            t.loaded.iter().map(|r| r.visible).collect::<Vec<_>>(),
            [true, false, true]
        );
    }
    // Hiding b handed the selection to live; show all and select b again.
    app.handle_action(Action::ToggleTraceVisible, true);
    app.handle_action(Action::CycleFocus, false);
    app.handle_action(Action::CycleFocus, false); // b
    app.handle_action(Action::CloseFocusedRun, false); // removes b
    let ViewKind::Transfer(t) = &app.view else {
        panic!("not transfer view")
    };
    assert!(t.live_visible && t.loaded.iter().all(|r| r.visible));
    assert_eq!(
        t.loaded.iter().map(|r| r.color_slot).collect::<Vec<_>>(),
        [9, 11]
    );
}

/// Opening runs from disk colours them too (Codex review): every way a
/// run enters the comparison goes through the one colour assignment.
#[test]
fn opened_runs_get_distinct_colours() {
    let mut app = transfer_app();
    app.with_transfer(|t| {
        t.add_loaded_run(loaded_run("a.acsnap", "2026-09-26T14:00:00Z"));
        t.add_loaded_run(loaded_run("b.acsnap", "2026-09-26T14:01:00Z"));
    });
    let ViewKind::Transfer(t) = &app.view else {
        panic!("not transfer view")
    };
    assert_eq!(
        t.loaded.iter().map(|r| r.color_slot).collect::<Vec<_>>(),
        [9, 10],
        "file runs take colours after the nine slots"
    );
}

/// Settings never apply under a live stimulus (Codex review, #256): with
/// Enter no longer a stop while driving, it can reach an open overlay, and
/// applying relaunches the session. It refuses and says what stops it.
#[test]
fn settings_refuse_to_apply_while_driving() {
    let mut app = transfer_app();
    drive(&mut app);
    app.settings = Some(crate::settings::SettingsOverlay::from_config(
        &ac_core::config::Config::default(),
        -30.0,
    ));
    app.handle_settings_keys(SettingsKeys {
        enter: true,
        ..Default::default()
    });
    assert!(
        app.settings.is_some(),
        "settings applied under a live drive"
    );
    assert_eq!(stim_state(&app), StimState::Driving);
    assert_eq!(
        app.toast_text(),
        Some("stop the stimulus (Space or Esc) before applying settings")
    );
}

/// A bare slot digit shows or hides that slot (#256); an empty slot says
/// how to fill it.
#[test]
fn a_bare_digit_toggles_its_slot() {
    let mut app = transfer_app();
    let now = std::time::Instant::now();
    app.toggle_slot(3, now);
    assert_eq!(
        app.toast_text(),
        Some("slot 3 is empty \u{2014} Ctrl+3 stores the live trace there")
    );
    app.finish_capture_for_test(Ok(crate::capture::Captured {
        slot: 3,
        opened: false,
        path: std::path::PathBuf::from("/c/slot3.acsnap"),
        run: loaded_run("slot 3", "2026-09-26T14:00:00Z"),
    }));
    let visible = |app: &AcViewApp| match &app.view {
        ViewKind::Transfer(t) => t.loaded[0].visible,
        ViewKind::Spectrum(_) => panic!("not transfer view"),
    };
    app.toggle_slot(3, now);
    assert!(!visible(&app));
    app.toggle_slot(3, now);
    assert!(visible(&app));
}

/// One frame of input through the real dispatch path: `key` pressed with
/// `event_ctrl` on its event, the frame's modifiers at `frame_ctrl` (they
/// differ when Ctrl is released before the frame is processed), and the
/// digit's text event, as a real keyboard sends it.
fn press_key(app: &mut AcViewApp, key: egui::Key, event_ctrl: bool, frame_ctrl: bool) {
    let mods = |ctrl: bool| egui::Modifiers {
        ctrl,
        command: ctrl,
        ..Default::default()
    };
    let mut events = vec![egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: mods(event_ctrl),
    }];
    if !event_ctrl {
        events.push(egui::Event::Text(key.symbol_or_name().to_string()));
    }
    let input = egui::RawInput {
        events,
        modifiers: mods(frame_ctrl),
        ..Default::default()
    };
    let ctx = egui::Context::default();
    ctx.begin_pass(input);
    app.dispatch_input(&ctx);
    let _ = ctx.end_pass();
}

/// Through dispatch (Codex review): a bare digit toggles, `Ctrl`+digit
/// stores — here refused for want of a session, which proves the route —
/// even when Ctrl was released before the frame; and a digit typed into
/// the delay entry lands in the entry, not on the slots.
#[test]
fn digits_reach_the_slots_through_dispatch() {
    let mut app = transfer_app();
    app.finish_capture_for_test(Ok(crate::capture::Captured {
        slot: 2,
        opened: false,
        path: std::path::PathBuf::from("/c/slot2.acsnap"),
        run: loaded_run("slot 2", "2026-09-26T14:00:00Z"),
    }));
    let visible = |app: &AcViewApp| match &app.view {
        ViewKind::Transfer(t) => t.loaded[0].visible,
        ViewKind::Spectrum(_) => panic!("not transfer view"),
    };
    press_key(&mut app, egui::Key::Num2, false, false);
    assert!(!visible(&app), "bare 2 did not hide slot 2");
    press_key(&mut app, egui::Key::Num2, true, false);
    assert!(
        !visible(&app),
        "Ctrl+2 (Ctrl released by frame time) toggled"
    );
    assert_eq!(
        app.toast_text(),
        Some("no session \u{2014} nothing to store")
    );

    app.handle_action(Action::TypeDelay, false);
    press_key(&mut app, egui::Key::Num2, false, false);
    assert!(
        !visible(&app),
        "a digit typed into the entry toggled slot 2"
    );
    assert_eq!(app.delay_entry.as_ref().map(|e| e.text()), Some("2"));
}

/// `Q` exits (#256 feedback): the drive goes off, the session stops, and
/// the window is asked to close — not merely left sitting there.
#[test]
fn q_asks_the_window_to_close() {
    let mut app = transfer_app();
    drive(&mut app);
    app.handle_action(Action::Quit, false);
    assert!(app.quit_requested);
    assert!(!app.sent_drive.last().expect("drive off relayed").on);
}

fn temp_captures(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ac-view-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `F` with nothing saved says where it looked; with files it opens the
/// list, and a digit starts loading the selected file into that slot.
#[test]
fn f_lists_saved_captures_and_a_digit_loads_into_that_slot() {
    let mut app = transfer_app();
    let dir = temp_captures("f");
    app.captures_dir = dir.clone();
    app.handle_action(Action::OpenSnapshot, false);
    assert!(app.file_list.is_none());
    assert_eq!(
        app.toast_text().map(str::to_string),
        Some(format!("no saved captures in {}", dir.display()))
    );

    std::fs::write(
        dir.join("slot1-2026-09-26T14-00-00Z.acsnap"),
        b"not a real snapshot",
    )
    .unwrap();
    app.handle_action(Action::OpenSnapshot, false);
    assert_eq!(app.file_list.as_ref().map(|l| l.entries().len()), Some(1));
    app.load_selected_into_slot(2, std::time::Instant::now());
    assert!(app.file_list.is_none(), "the list stays open after a load");
    assert_eq!(app.capture_slot, Some(2));
    assert_eq!(
        app.toast_text(),
        Some("loading slot1-2026-09-26T14-00-00Z.acsnap into slot 2\u{2026}")
    );
    // The file is not a snapshot: the load fails and says so.
    let result = app.capture_rx.take().unwrap().recv().unwrap();
    app.capture_slot = None;
    app.finish_capture_for_test(result);
    assert!(
        app.toast_text().unwrap().starts_with("storing failed"),
        "{:?}",
        app.toast_text()
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// A load reports where from; a capture still reports where to.
#[test]
fn a_loaded_slot_says_where_it_came_from() {
    let mut app = transfer_app();
    app.finish_capture_for_test(Ok(crate::capture::Captured {
        slot: 4,
        opened: true,
        path: std::path::PathBuf::from("/c/old.acsnap"),
        run: loaded_run("slot 4", "2026-09-26T14:00:00Z"),
    }));
    assert_eq!(app.toast_text(), Some("slot 4 loaded from /c/old.acsnap"));
}

/// `C` writes the selected trace — live or a slot, the slot with its
/// nudge — and says where.
#[test]
fn c_writes_the_selected_trace_to_csv() {
    let mut app = transfer_app();
    let dir = temp_captures("c");
    app.captures_dir = dir.clone();
    app.handle_action(Action::ExportCsv, false);
    assert_eq!(app.toast_text(), Some("no live trace to export"));

    app.ingest_frame_for_test(found_frame(), 0.0);
    app.handle_action(Action::ExportCsv, false);
    assert!(
        app.toast_text().unwrap().starts_with("live written"),
        "{:?}",
        app.toast_text()
    );

    app.finish_capture_for_test(Ok(crate::capture::Captured {
        slot: 1,
        opened: false,
        path: std::path::PathBuf::from("/c/slot1.acsnap"),
        run: loaded_run("slot 1", "2026-09-26T14:00:00Z"),
    }));
    app.handle_action(Action::CycleFocus, false);
    app.handle_action(Action::NudgeDelayLater, true);
    app.handle_action(Action::ExportCsv, false);
    let written: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    let slot = written
        .iter()
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("slot1-")
        })
        .expect("slot CSV written");
    let csv = std::fs::read_to_string(slot).unwrap();
    assert!(csv.starts_with("# ac transfer trace: slot 1\n"), "{csv}");
    let ViewKind::Transfer(t) = &app.view else {
        panic!("not transfer view")
    };
    let want = ac_scene::TransferInput::stored_delay_ms(&t.loaded[0].derivation)
        + 10.0 * 1000.0 / 48_000.0;
    assert!(csv.contains(&format!("# delay_ms: {want:.6}\n")), "{csv}");
    assert!(csv.lines().count() > 6);
    // A second export in the same second never overwrites the first.
    let before = std::fs::read_dir(&dir).unwrap().count();
    app.handle_action(Action::ExportCsv, false);
    app.handle_action(Action::ExportCsv, false);
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), before + 2);
    std::fs::remove_dir_all(&dir).ok();
}

/// In the list, a bare digit loads and Ctrl+digit does nothing (Codex
/// review): Ctrl+digit means "store live", which the list must not turn
/// into a load.
#[test]
fn ctrl_digit_in_the_list_does_not_load() {
    let mut app = transfer_app();
    let dir = temp_captures("ctrl");
    app.captures_dir = dir.clone();
    std::fs::write(dir.join("a.acsnap"), b"x").unwrap();
    app.handle_action(Action::OpenSnapshot, false);
    press_key(&mut app, egui::Key::Num2, true, true);
    assert!(app.capture_slot.is_none() && app.file_list.is_some());
    press_key(&mut app, egui::Key::Num2, false, false);
    assert_eq!(app.capture_slot, Some(2));
    std::fs::remove_dir_all(&dir).ok();
}

/// Recalling a slot selects it; hiding the selected slot hands the
/// selection back to live; a slot loaded with `F` is selected; a live
/// store is not (#256).
#[test]
fn a_recalled_slot_is_selected() {
    let mut app = transfer_app();
    let captured = |slot: u8, opened: bool| {
        Ok(crate::capture::Captured {
            slot,
            opened,
            path: std::path::PathBuf::from(format!("/c/slot{slot}.acsnap")),
            run: loaded_run(&format!("slot {slot}"), "2026-09-26T14:00:00Z"),
        })
    };
    app.finish_capture_for_test(captured(2, false));
    app.finish_capture_for_test(captured(5, false));
    assert_eq!(
        focus_of(&app),
        crate::view::Focus::Live,
        "a live store moved the selection"
    );

    let now = std::time::Instant::now();
    app.toggle_slot(5, now); // hide
    app.toggle_slot(5, now); // recall
    assert_eq!(focus_of(&app), crate::view::Focus::Stored(1));
    app.toggle_slot(5, now); // hide the selected one
    assert_eq!(focus_of(&app), crate::view::Focus::Live);

    app.finish_capture_for_test(captured(1, true)); // loaded with F
    assert_eq!(focus_of(&app), crate::view::Focus::Stored(0));
}

/// The selection never rests on a hidden slot (Codex review): `V` on the
/// selected slot hands it to live; `F` into a hidden slot shows it; `X`
/// never lands on a hidden one; `Tab` skips hidden slots.
#[test]
fn the_selection_never_rests_on_a_hidden_slot() {
    let mut app = transfer_app();
    let captured = |slot: u8, opened: bool| {
        Ok(crate::capture::Captured {
            slot,
            opened,
            path: std::path::PathBuf::from(format!("/c/slot{slot}.acsnap")),
            run: loaded_run(&format!("slot {slot}"), "2026-09-26T14:00:00Z"),
        })
    };
    let now = std::time::Instant::now();
    for n in [1, 2, 3] {
        app.finish_capture_for_test(captured(n, false));
    }
    app.toggle_slot(2, now); // hide 2 (selection stays live)
    app.handle_action(Action::CycleFocus, false);
    assert_eq!(focus_of(&app), crate::view::Focus::Stored(0));
    app.handle_action(Action::CycleFocus, false);
    assert_eq!(
        focus_of(&app),
        crate::view::Focus::Stored(2),
        "Tab landed on hidden 2"
    );

    app.handle_action(Action::ToggleTraceVisible, false); // V hides 3
    assert_eq!(focus_of(&app), crate::view::Focus::Live);

    app.handle_action(Action::CycleFocus, false); // 1
    app.handle_action(Action::CloseFocusedRun, false); // X: next is hidden 2
    assert_eq!(focus_of(&app), crate::view::Focus::Live);

    app.finish_capture_for_test(captured(2, true)); // F into hidden 2
    let ViewKind::Transfer(t) = &app.view else {
        panic!("not transfer view")
    };
    let i = t.loaded.iter().position(|r| r.slot == Some(2)).unwrap();
    assert!(t.loaded[i].visible);
    assert_eq!(t.focus, crate::view::Focus::Stored(i));
}
