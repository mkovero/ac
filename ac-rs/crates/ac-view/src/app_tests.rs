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
    app.handle_action(Action::StimulusFireOrStop, false); // Armed -> Driving
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

// Each panic key (Space/Enter/Esc) stops from Driving through the
// adapter — the machine's own test proves the transition; this proves
// the app relays the off for each.
#[test]
fn every_panic_key_relays_off_from_driving() {
    for key in ["space", "enter", "esc"] {
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

fn transfer_frame() -> ac_scene::WireFrame {
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
fn dense_transfer_frame() -> ac_scene::WireFrame {
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

/// A refusing frame, built from the healthy fixture so only the fields
/// the indicator reads differ.
fn refusing_frame() -> ac_scene::WireFrame {
    refusing_frame_at_attempt(3)
}

/// The same, with the attempt count set: escalation is the later of
/// `PERSISTENT_REFUSAL_S` and `PERSISTENT_REFUSAL_ATTEMPTS` (#247), so a
/// test that advances the clock has to advance the count with it.
fn refusing_frame_at_attempt(attempts: u32) -> ac_scene::WireFrame {
    let mut f = transfer_frame();
    f.drive = Some(ac_scene::WireDrive {
        on: true,
        level_dbfs: Some(-30.0),
        drivable: true,
    });
    f.delay_locked = Some(false);
    // The estimator has answered and refused (#238), so this is a frame a
    // current daemon could publish. It is not what makes the row paint
    // here — `transfer_frame` carries `mtw`, so the settled gate already
    // covers it. The field-reachable no-ladder shape is pinned in
    // `ac-scene::fault` and in `it_transfer_geometry`.
    f.delay_attempts = attempts;
    f
}

/// #228: the refusal clock lives on the app, not on the frame, so a
/// persistent refusal must be reachable by feeding identical frames as
/// scene time advances. A per-frame implementation would show LOST LOCK
/// forever and the operator would never be told to move the mic.
#[test]
fn a_refusal_becomes_persistent_as_scene_time_advances() {
    let mut app = AcViewApp::new_transfer(
        Endpoint {
            host: "localhost".into(),
            ctrl_port: 0,
            data_port: 0,
        },
        TEST_DRIVE_MAX_DBFS,
    );

    app.ingest_frame_for_test(refusing_frame(), 0.0);
    assert_eq!(
        app.current_transfer_scene().expect("scene built").fault,
        // Never locked in this session, so nothing was lost — the
        // transient row says NO LOCK without the instruction.
        Some(ac_scene::Fault::NoLockYet)
    );

    // The same refusal, retried at the daemon's 1 Hz, until both the
    // clock and the attempt count have passed their thresholds. The
    // frames still say nothing new — the row changes because the app
    // carries the state, which is what this test is for.
    let first = 3;
    for n in 1..ac_scene::fault::PERSISTENT_REFUSAL_ATTEMPTS {
        app.ingest_frame_for_test(refusing_frame_at_attempt(first + n), n as f64);
        assert_eq!(
            app.current_transfer_scene().expect("scene rebuilt").fault,
            Some(ac_scene::Fault::NoLockYet),
            "escalated at attempt {n} of the refusal, before either \
             threshold was reached"
        );
    }
    app.ingest_frame_for_test(
        refusing_frame_at_attempt(first + ac_scene::fault::PERSISTENT_REFUSAL_ATTEMPTS),
        ac_scene::fault::PERSISTENT_REFUSAL_S,
    );
    assert_eq!(
        app.current_transfer_scene().expect("scene rebuilt").fault,
        Some(ac_scene::Fault::NoLock)
    );
}

/// And a lock ends it, with the transient confirmation on the way past.
#[test]
fn a_lock_clears_the_refusal_and_confirms_itself() {
    let mut app = AcViewApp::new_transfer(
        Endpoint {
            host: "localhost".into(),
            ctrl_port: 0,
            data_port: 0,
        },
        TEST_DRIVE_MAX_DBFS,
    );
    app.ingest_frame_for_test(refusing_frame(), 0.0);

    let mut locked = refusing_frame();
    locked.delay_locked = Some(true);
    app.ingest_frame_for_test(locked.clone(), 1.0);
    assert_eq!(
        app.current_transfer_scene().unwrap().fault,
        Some(ac_scene::Fault::LockAcquired)
    );

    app.ingest_frame_for_test(locked, 1.0 + ac_scene::fault::LOCK_ACQUIRED_HOLD_S);
    assert_eq!(app.current_transfer_scene().unwrap().fault, None);
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
    let mut frame = refusing_frame();
    frame.delay_locked = Some(true);
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
// of frames that fail the `WireFrame` schema clears the grace window —
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
    // `WireFrame` rather than being silently dropped and forgotten.
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
// IR panel (#286)
// ---------------------------------------------------------------

fn ir_frame() -> ac_scene::IrWireFrame {
    serde_json::from_value(serde_json::json!({
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
    }))
    .expect("ir wire frame")
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
