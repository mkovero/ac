//! #321 AC4: adding, changing, or removing one stored trace must not
//! silently alter what another trace shows. Two runs are opened from the
//! same `.acsnap` fixture (independent `PairDerivation`s — `derive_pair`
//! is re-run per open, D8's "no reimplementation" applies the same way it
//! does to every other snapshot-reprocessing path), loaded into one
//! `TransferViewState`, and only one is ever touched.

use ac_core::shared::calibration::Calibration;
use ac_core::snapshot::{write_acsnap, ChannelMeta, SessionMeta, SnapshotMeta, FORMAT_VERSION};
use ac_scene::Smoothing;
use ac_view::snapshot_flow::{open_stored_transfer_run, rederive_transfer_scene};
use ac_view::view::{Focus, TransferViewState};

const FREQ_RANGE: (f64, f64) = (20.0, 20_000.0);
const DB_RANGE: (f64, f64) = (-80.0, 20.0);

/// A minimal but real `.acsnap`: two channels, one pair, a flat low-level
/// signal on both — enough for `derive_pair`'s real H1/FFT path to run
/// end to end. This test is about *isolation* between loaded runs, not
/// about what the curve looks like, so the fixture does not need the
/// richer broadband content `ac-core`'s own snapshot tests use.
fn write_fixture(path: &std::path::Path) {
    let sr = 48_000u32;
    let n = sr as usize;
    let samples: Vec<f32> = (0..n)
        .map(|i| (0.2 * (2.0 * std::f64::consts::PI * 300.0 * i as f64 / sr as f64).sin()) as f32)
        .collect();
    let meta = SnapshotMeta {
        format_version: FORMAT_VERSION,
        sr,
        channel_map: vec!["meas_0".to_string(), "ref".to_string()],
        per_channel: vec![
            ChannelMeta {
                role: "meas_0".to_string(),
                input_channel: 0,
                weighting: "Z".to_string(),
                integration: "fast".to_string(),
                calibration: None::<Calibration>,
            },
            ChannelMeta {
                role: "ref".to_string(),
                input_channel: 1,
                weighting: "Z".to_string(),
                integration: "fast".to_string(),
                calibration: None,
            },
        ],
        session: SessionMeta {
            pairs: vec![(0, 1)],
            delay_samples: vec![0],
            nperseg: sr as usize,
        },
        captured_at_utc: "2026-07-16T00:00:00Z".to_string(),
        daemon_version: "test".to_string(),
        ring_duration_s: n as f64 / sr as f64,
    };
    let (bytes, _) = write_acsnap(&meta, &[samples.clone(), samples]).expect("write fixture");
    std::fs::write(path, bytes).expect("write .acsnap");
}

fn fixture_path(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ac-view-trace-comparison-{}-{tag}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir.join("fixture.acsnap")
}

#[test]
fn changing_one_runs_smoothing_leaves_the_other_runs_scene_byte_identical() {
    let path = fixture_path("smoothing");
    write_fixture(&path);

    let run_a = open_stored_transfer_run(&path, 0).expect("open run a");
    let run_b = open_stored_transfer_run(&path, 0).expect("open run b");

    let mut state = TransferViewState::new(-10.0, -30.0);
    state.add_loaded_run(run_a);
    state.add_loaded_run(run_b);
    assert_eq!(
        state.focus,
        Focus::Stored(1),
        "load-order focus, not asserted elsewhere"
    );

    let scene_before = |state: &TransferViewState, idx: usize| {
        let run = &state.loaded[idx];
        rederive_transfer_scene(
            &run.derivation,
            &run.channel_role,
            run.sr,
            ac_scene::DisplayModes::new(ac_scene::DerotMode::Session, run.smoothing),
            FREQ_RANGE,
            DB_RANGE,
        )
    };
    let b_before = scene_before(&state, 1);
    assert_eq!(b_before.smoothing_readout, None, "b must open unsmoothed");

    // Focus A and change ONLY its smoothing.
    state.focus = Focus::Stored(0);
    state.cycle_smoothing();
    assert_eq!(
        state.loaded[0].smoothing,
        Smoothing::Oct24,
        "a's field must change"
    );
    assert_eq!(
        state.loaded[1].smoothing,
        Smoothing::Off,
        "b's field must NOT change — one runs's control edited another's state"
    );

    let a_after = scene_before(&state, 0);
    let b_after = scene_before(&state, 1);

    assert_eq!(
        a_after.smoothing_readout,
        Some("smoothing 1/24 octave"),
        "a's own change must reach its own rebuilt scene"
    );
    assert_eq!(
        b_before, b_after,
        "b's rebuilt scene changed after only a's smoothing was edited — \
         acceptance criterion 4 (isolation) violated"
    );

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn closing_the_focused_stored_run_leaves_the_remaining_run_untouched() {
    let path = fixture_path("close");
    write_fixture(&path);

    let run_a = open_stored_transfer_run(&path, 0).expect("open run a");
    let run_b = open_stored_transfer_run(&path, 0).expect("open run b");

    let mut state = TransferViewState::new(-10.0, -30.0);
    state.add_loaded_run(run_a);
    state.add_loaded_run(run_b);
    // Give b a distinguishing smoothing setting before closing a, so a
    // silent index-shift bug (closing index 0 without adjusting b's
    // position) would show up as the wrong run's smoothing surviving.
    state.focus = Focus::Stored(1);
    state.cycle_smoothing();
    assert_eq!(state.loaded[1].smoothing, Smoothing::Oct24);

    state.focus = Focus::Stored(0);
    state.close_focused_stored_run();

    assert_eq!(state.loaded.len(), 1, "exactly one run must remain");
    assert_eq!(
        state.loaded[0].smoothing,
        Smoothing::Oct24,
        "the remaining run must still be what was b, unaltered by closing a"
    );
    assert_eq!(state.focus, Focus::Stored(0));

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}
