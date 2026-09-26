//! Per-tick session behaviour, driven directly rather than through a
//! daemon.
//!
//! Everything here was previously reachable only from a live ZMQ session:
//! the warmup gate, the block count the frame reports, the delay flushes
//! and the no-peak retry timer all lived inside the worker closure.
//! `it_set_delay` covers the protocol end to end, but it cannot advance the
//! clock, so the retry interval below had no test at all — a `FIND_RETRY`
//! of zero, or of an hour, would both have stayed green.

use super::*;
use crate::workers::{DelayAction, DelayCmd};
use serde_json::json;

/// `set_delay` with `samples: null` for every pair — a re-find.
const REFIND: DelayCmd = DelayCmd {
    pair: None,
    action: DelayAction::Find,
};

const SR: u32 = 48_000;
const CHUNK: usize = (SR as usize) / 20; // 0.05 s, the capture tick

/// Deterministic broadband noise. Fixed-seed LCG rather than an rng
/// dependency, so a failure reproduces across toolchains.
fn noise(n: usize, seed: u32) -> Vec<f32> {
    let mut s = seed | 1;
    (0..n)
        .map(|_| {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (s >> 8) as f32 / (1 << 23) as f32 - 1.0
        })
        .collect()
}

fn statics() -> FrameStatics {
    FrameStatics {
        sr: SR,
        backend: "fake".to_string(),
        spec_f_min: 20.0,
        spec_f_max: SR as f64 / 2.0,
        spec_n_columns: ac_core::visualize::aggregate::transfer_spectrum_n_columns(
            20.0,
            SR as f64 / 2.0,
        ),
        weighting: ac_core::visualize::weighting_curves::WeightingCurve::from_tag("Z").unwrap(),
        integration_tag: "fast".to_string(),
        mtw_ppo: ac_core::visualize::mtw::ladder::P_REF,
        mtw_n_blocks: ac_core::visualize::mtw::average::DEFAULT_N_BLOCKS,
        mtw_stages: Vec::new(),
    }
}

/// One pair, channel 0 measurement against channel 1 reference, no
/// calibration of any kind — `spl` and the cal tags are not what these
/// tests are about.
fn session() -> SessionState {
    SessionState::new(
        statics(),
        Window::new(SR, 4),
        vec![PairCtx {
            pos: 0,
            meas_ch: 0,
            ref_ch: 1,
            mi: 0,
            ri: 1,
            meas_cal: None,
            ref_cal: None,
            meas_curve: None,
            meas_voltage_check: None,
            ref_voltage_check: None,
        }],
        2,
        0.05,
        ac_core::visualize::time_integration::TAU_FAST_S,
    )
}

fn events(engine_on: bool) -> TickEvents {
    TickEvents {
        engine_on,
        drive_edge_on: false,
        mc_enabled: false,
    }
}

fn drive_msg(on: bool) -> ac_core::wire::WireDrive {
    ac_core::wire::WireDrive {
        on,
        level_dbfs: on.then_some(-20.0),
        drivable: true,
    }
}

/// Feed `n` ticks of a correlated pair — measurement is the reference
/// delayed by `delay` samples, which is what the estimator is meant to
/// find — and return every frame published.
fn run_correlated(
    s: &mut SessionState,
    n: usize,
    delay: usize,
    ev: TickEvents,
    t0: std::time::Instant,
) -> Vec<Value> {
    let x = noise(CHUNK * (n + 2) + delay, 0x5eed);
    let mut out = Vec::new();
    for k in 0..n {
        let r0 = delay + k * CHUNK;
        let refb = x[r0..r0 + CHUNK].to_vec();
        let meas = x[r0 - delay..r0 - delay + CHUNK].to_vec();
        let now = t0 + std::time::Duration::from_millis(50 * k as u64);
        out.extend(
            s.tick(&[meas, refb], ev, &drive_msg(ev.engine_on), now)
                .into_iter()
                .filter(|m| m["type"] == json!("transfer_stream")),
        );
    }
    out
}

/// A live measurement leg against a silent reference: H is zero, the live
/// IR has no peak, and the start-up Find has nothing to report.
fn run_silent_ref(
    s: &mut SessionState,
    ticks: &[std::time::Instant],
    ev: TickEvents,
) -> Vec<Value> {
    let mut out = Vec::new();
    for (k, &now) in ticks.iter().enumerate() {
        let meas = noise(CHUNK, 0x1000 + k as u32);
        let refb = vec![0.0; CHUNK];
        out.extend(
            s.tick(&[meas, refb], ev, &drive_msg(ev.engine_on), now)
                .into_iter()
                .filter(|m| m["type"] == json!("transfer_stream")),
        );
    }
    out
}

/// Publication does not wait on the analysis window. Every tick from
/// the first produces a frame; the ones before a ring holds a whole
/// Welch segment say `n_averages: 0` and carry empty analysis arrays,
/// and everything that never depended on the window — the observed
/// drive state, the capture peaks — is there from the start.
///
/// Before this split the loop `continue`d, so for the first second a
/// client could not tell a daemon that had not started from one whose
/// drive had already dead-manned.
#[test]
fn a_frame_ships_from_the_first_tick_and_states_that_it_carries_no_analysis() {
    let mut s = session();
    let t0 = std::time::Instant::now();
    // One segment is `sr` samples = 20 ticks. The 20th completes it.
    let settling = run_correlated(&mut s, 19, 480, events(true), t0);
    assert_eq!(
        settling.len(),
        19,
        "a tick before the segment published nothing"
    );
    for f in &settling {
        assert_eq!(
            f["n_averages"],
            json!(0),
            "settling frame claimed a Welch block"
        );
        for key in [
            "freqs",
            "magnitude_db",
            "phase_deg",
            "coherence",
            "meas_spectrum",
        ] {
            assert_eq!(
                f[key].as_array().map(Vec::len),
                Some(0),
                "{key} was not empty on a settling frame"
            );
        }
        assert_eq!(f["delay_locked"], json!(false));
        assert_eq!(
            f["drive"]["on"],
            json!(true),
            "drive state withheld while settling"
        );
    }
    // Peaks are measured from the tick's own blocks, so they are real
    // numbers on the very first frame — the thing the old gate hid.
    assert!(
        settling[0]["meas_peak_dbfs"].as_f64().is_some(),
        "capture peaks withheld while settling"
    );

    let analysing = run_correlated(&mut s, 1, 480, events(true), t0);
    let f = analysing
        .last()
        .expect("no frame on the tick that completed the segment");
    assert_eq!(f["n_averages"], json!(1));
    assert!(!f["freqs"].as_array().unwrap().is_empty());
}

/// The analysis advances on the ring, not on the loop.
///
/// At 48 kHz the ring's start moves one `step` — 0.5 s — while the
/// loop ticks 20 times, so nine frames in ten repeat the previous
/// estimate exactly. That was true before this cache existed too; the
/// difference is that the repetition was produced by recomputing a
/// 2.5 s Welch pass and a full-resolution IFFT to arrive at the same
/// bytes, and that it was invisible on the wire.
#[test]
fn the_analysis_advances_once_per_welch_hop_not_once_per_tick() {
    let mut s = session();
    let t0 = std::time::Instant::now();
    // Settle first: while the window fills, `n_blocks` changes and
    // every tick legitimately re-analyses.
    run_correlated(&mut s, 60, 480, events(false), t0);
    let frames = run_correlated(&mut s, 60, 480, events(false), t0);

    let seqs: Vec<u64> = frames
        .iter()
        .map(|f| f["analysis_seq"].as_u64().unwrap())
        .collect();
    assert!(
        seqs.windows(2).all(|w| w[1] >= w[0]),
        "analysis_seq went backwards: {seqs:?}"
    );
    let recomputes = seqs.windows(2).filter(|w| w[1] != w[0]).count();
    // 60 ticks of 0.05 s = 3.0 s; the hop is 0.5 s.
    assert_eq!(
        recomputes, 6,
        "expected one recomputation per 0.5 s hop over 3.0 s, got {recomputes}: {seqs:?}"
    );

    // And the repetition is real: same seq means the same numbers.
    for w in frames.windows(2) {
        let same_seq = w[0]["analysis_seq"] == w[1]["analysis_seq"];
        let same_mag = w[0]["magnitude_db"] == w[1]["magnitude_db"];
        assert_eq!(
            same_seq, same_mag,
            "analysis_seq and the arrays disagree about whether the estimate changed"
        );
    }
}

/// The cache must never be stale: what a frame carries has to equal
/// what analysing the ring right now would produce.
///
/// Checked mid-hop, where a stale cache is possible at all — on a
/// boundary tick the two are trivially equal.
#[test]
fn a_held_estimate_equals_one_computed_from_the_ring_as_it_stands() {
    let mut s = session();
    let t0 = std::time::Instant::now();
    run_correlated(&mut s, 60, 480, events(false), t0);
    // Three more ticks: 0.15 s into a 0.5 s hop.
    let frames = run_correlated(&mut s, 3, 480, events(false), t0);
    let held = frames.last().unwrap();

    let key = AnalysisKey {
        dropped: s.dropped,
        n_blocks: s.n_blocks(),
        delay: s.pairs[0].delay.map(|l| l.samples).unwrap_or(0),
        mc_enabled: false,
    };
    let fresh = analyse_pair(&s.ctx[0], &s.pairs[0], &s.statics, &s.rings, key, 0)
        .expect("rings hold both channels");
    assert_eq!(
        held["magnitude_db"],
        json!(fresh.magnitude_db),
        "the frame's magnitude is not what the ring says now"
    );
    assert_eq!(held["coherence"], json!(fresh.coherence));
    assert_eq!(held["meas_spectrum"], json!(fresh.meas_spectrum));
}

/// A lock arriving mid-hop must invalidate the estimate. The held one
/// was computed unaligned, and publishing it until the next boundary
/// would show an alignment the frame simultaneously claims to have.
#[test]
fn a_changed_lock_re_analyses_before_the_next_hop() {
    let mut s = session();
    let t0 = std::time::Instant::now();
    run_correlated(&mut s, 60, 480, events(false), t0);
    let before = run_correlated(&mut s, 1, 480, events(false), t0);
    let before = before.last().unwrap().clone();

    // Move the delay without moving the ring — the drive edge and
    // `set_delay` both do this in the middle of a hop.
    s.pairs[0].delay = Some(Lock {
        samples: 1200,
        driving: false,
        operator: false,
    });
    let after = run_correlated(&mut s, 1, 480, events(false), t0);
    let after = after.last().unwrap();

    assert_ne!(
        before["analysis_seq"], after["analysis_seq"],
        "a changed lock did not re-analyse"
    );
    assert_eq!(after["delay_samples"], json!(1200));
    assert_ne!(
        before["magnitude_db"], after["magnitude_db"],
        "re-analysis at a different alignment produced the same H1"
    );
}

/// A settling frame and an analysis frame must be the same shape. They
/// are built by two different functions, so nothing but this stops one
/// gaining a field the other lacks — and a consumer meeting the
/// difference reads it as a daemon that dropped a field mid-session.
#[test]
fn the_settling_frame_has_the_same_keys_as_an_analysis_frame() {
    fn keys(v: &Value) -> Vec<String> {
        let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        k.sort();
        k
    }
    let mut s = session();
    let t0 = std::time::Instant::now();
    let settling = run_correlated(&mut s, 1, 480, events(false), t0);
    let analysing = run_correlated(&mut s, 20, 480, events(false), t0);
    assert_eq!(
        keys(&settling[0]),
        keys(analysing.last().unwrap()),
        "settling and analysis frames disagree about the frame's shape"
    );
}

/// `n_averages` is the frame's statement about its own coherence bias.
/// It rises from 0 (no segment yet) through the window filling and then
/// stops, because `drain_to_block_lattice` pins the ring inside one
/// `step` of the target (#208).
#[test]
fn n_averages_climbs_to_the_window_depth_and_then_holds() {
    let mut s = session();
    let t0 = std::time::Instant::now();
    let frames = run_correlated(&mut s, 140, 480, events(false), t0);
    let seen: Vec<u64> = frames
        .iter()
        .map(|f| f["n_averages"].as_u64().unwrap())
        .collect();
    assert_eq!(seen.first(), Some(&0), "first frame claimed a Welch block");
    assert_eq!(
        seen.iter().find(|&&n| n > 0),
        Some(&1),
        "the first analysis frame did not report exactly one block"
    );
    assert_eq!(
        seen.last(),
        Some(&4),
        "settled frames do not report the window depth"
    );
    assert!(
        seen.windows(2).all(|w| w[1] >= w[0]),
        "n_averages went backwards: {seen:?}"
    );
    assert!(
        seen.iter().all(|&n| n <= 4),
        "n_averages exceeded the window depth: {seen:?}"
    );
}

/// A Find with no peak must not be retried on the very next tick: its
/// inputs only turn over on the ring's own timescale.
///
/// The clock is a parameter, so this asserts the interval itself. A
/// live session could only assert it by sleeping, which is why the
/// interval had no test before: any value at all was green.
#[test]
fn a_find_with_no_peak_waits_out_the_retry_interval_before_trying_again() {
    let mut s = session();
    let t0 = std::time::Instant::now();
    // Fill the ring, then hold the clock still: every tick after the
    // first attempt is inside the retry window.
    let warm: Vec<std::time::Instant> = (0..20)
        .map(|k| t0 + std::time::Duration::from_millis(50 * k))
        .collect();
    let frames = run_silent_ref(&mut s, &warm, events(true));
    let first = frames.last().expect("a frame once the segment is in");
    assert_eq!(
        first["delay_locked"],
        json!(false),
        "a silent reference must not produce a delay"
    );
    assert_eq!(first["delay_residual"], json!(null));
    assert_eq!(
        first["delay_attempts"],
        json!(1),
        "expected exactly one attempt"
    );

    // Well inside FIND_RETRY: no second attempt.
    let held: Vec<std::time::Instant> = (0..5)
        .map(|k| t0 + std::time::Duration::from_millis(1000 + 50 * k))
        .collect();
    let frames = run_silent_ref(&mut s, &held, events(true));
    assert_eq!(
        frames.last().unwrap()["delay_attempts"],
        json!(1),
        "retried before the interval elapsed"
    );

    // Past it: exactly one more.
    let after = vec![t0 + FIND_RETRY + std::time::Duration::from_millis(1500)];
    let frames = run_silent_ref(&mut s, &after, events(true));
    assert_eq!(
        frames.last().unwrap()["delay_attempts"],
        json!(2),
        "did not retry after the interval elapsed"
    );
}

/// The start-up Find takes the unaligned live IR's peak, and the frame
/// that first claims the delay already carries the estimate aligned to it:
/// residual 0, not the 480 the unaligned estimate read.
#[test]
fn the_start_up_find_locks_to_the_ir_peak_and_publishes_aligned() {
    let mut s = session();
    let frames = run_correlated(&mut s, 25, 480, events(true), std::time::Instant::now());
    let first = frames
        .iter()
        .find(|f| f["delay_locked"] == json!(true))
        .expect("correlated pair found no delay");
    assert_eq!(first["delay_samples"], json!(480));
    assert_eq!(first["delay_residual"], json!(0));
    assert_eq!(first["delay_operator"], json!(false));
}

/// `set_delay` with `samples: null` discards the held delay and finds it
/// again, and the attempt counter stays monotone across it — a pair that
/// found a delay and then went silent must not read as one never asked
/// (`ac-scene::fault`).
#[test]
fn a_refind_drops_the_delay_and_leaves_the_attempt_count_monotone() {
    let mut s = session();
    let t0 = std::time::Instant::now();
    let frames = run_correlated(&mut s, 25, 480, events(true), t0);
    let locked = frames.last().unwrap();
    assert_eq!(
        locked["delay_locked"],
        json!(true),
        "correlated pair failed to lock"
    );
    assert_eq!(locked["delay_samples"], json!(480));
    let attempts_before = locked["delay_attempts"].as_u64().unwrap();

    s.apply_delay_cmd(REFIND, true);
    assert!(s.pairs[0].delay.is_none());
    // The pair finds again within the next tick — what changes is the
    // attempt count, which must have gone up rather than reset.
    let after = run_correlated(
        &mut s,
        1,
        480,
        events(true),
        t0 + std::time::Duration::from_secs(5),
    );
    let f = after.last().unwrap();
    assert!(
        f["delay_attempts"].as_u64().unwrap() > attempts_before,
        "a re-find did not cause a new attempt"
    );
    assert_eq!(f["delay_samples"], json!(480));
}

/// An operator-set delay is held as typed, published as operator-set, and
/// the residual reads what is left over: the true 480 against a setting of
/// 470 leaves +10 — Smaart's Delta Delay (#669).
#[test]
fn a_set_delay_holds_the_value_and_publishes_the_residual() {
    let mut s = session();
    let t0 = std::time::Instant::now();
    run_correlated(&mut s, 25, 480, events(true), t0);
    s.apply_delay_cmd(
        DelayCmd {
            pair: None,
            action: DelayAction::Set(470),
        },
        true,
    );
    let f = run_correlated(
        &mut s,
        1,
        480,
        events(true),
        t0 + std::time::Duration::from_secs(2),
    );
    let f = f.last().unwrap();
    assert_eq!(f["delay_samples"], json!(470));
    assert_eq!(f["delay_operator"], json!(true));
    assert_eq!(f["delay_residual"], json!(10));
}

/// Nothing the daemon does by itself replaces an operator-set delay: not a
/// drive edge, even for a delay set while the drive was off.
#[test]
fn the_drive_edge_keeps_an_operator_set_delay() {
    let mut s = session();
    let t0 = std::time::Instant::now();
    run_correlated(&mut s, 25, 480, events(false), t0);
    s.apply_delay_cmd(
        DelayCmd {
            pair: None,
            action: DelayAction::Set(470),
        },
        false,
    );
    s.flush_locks_taken_against_silence(s.consumed);
    assert_eq!(s.pairs[0].delay.map(|l| l.samples), Some(470));
}

/// Inserting the value already held marks it operator-set without
/// restarting the ladder; a new value rebuilds it at the new offset.
#[test]
fn setting_the_held_value_keeps_the_ladder() {
    let mut s = session();
    let t0 = std::time::Instant::now();
    run_correlated(&mut s, 30, 480, events(true), t0);
    let built = s.mtw_provenance()[0].clone();
    assert!(built.is_some(), "precondition: ladder built");
    let set = |samples| DelayCmd {
        pair: Some(0),
        action: DelayAction::Set(samples),
    };
    s.apply_delay_cmd(set(480), true);
    assert_eq!(
        s.mtw_provenance()[0],
        built,
        "a no-op insert restarted the ladder"
    );
    s.apply_delay_cmd(set(481), true);
    assert!(
        s.mtw_provenance()[0].is_none(),
        "a new offset kept the old ladder"
    );
}

/// Two steps queued between frames both land: the daemon moves the held
/// delay, so a client need not know it (Codex review of #673).
#[test]
fn queued_steps_accumulate() {
    let mut s = session();
    run_correlated(&mut s, 25, 480, events(true), std::time::Instant::now());
    let step = |k| DelayCmd {
        pair: None,
        action: DelayAction::Step(k),
    };
    s.apply_delay_cmd(step(1), true);
    s.apply_delay_cmd(step(1), true);
    assert_eq!(
        s.pairs[0].delay.map(|l| (l.samples, l.operator)),
        Some((482, true))
    );
    s.apply_delay_cmd(step(-3), true);
    assert_eq!(s.pairs[0].delay.map(|l| l.samples), Some(479));
    // Nothing to move before a delay exists.
    let mut fresh = session();
    fresh.apply_delay_cmd(step(1), true);
    assert!(fresh.pairs[0].delay.is_none());
}

/// A request naming another pair leaves this one alone.
#[test]
fn a_set_delay_for_another_pair_is_not_applied_here() {
    let mut s = session();
    run_correlated(&mut s, 25, 480, events(true), std::time::Instant::now());
    s.apply_delay_cmd(
        DelayCmd {
            pair: Some(1),
            action: DelayAction::Set(0),
        },
        true,
    );
    assert_eq!(s.pairs[0].delay.map(|l| l.samples), Some(480));
}

/// The drive off→on edge discards a lock taken against silence and
/// keeps one taken while driving (#226). `it_set_delay` covers both over
/// ZMQ; here they are two assertions on the same held state.
#[test]
fn the_drive_edge_discards_a_lock_taken_against_silence_and_keeps_one_taken_driving() {
    let t0 = std::time::Instant::now();

    let mut silent = session();
    let frames = run_correlated(&mut silent, 25, 480, events(false), t0);
    assert_eq!(frames.last().unwrap()["delay_locked"], json!(true));
    assert!(matches!(
        silent.pairs[0].delay,
        Some(Lock { driving: false, .. })
    ));
    silent.flush_locks_taken_against_silence(silent.consumed);
    assert!(
        silent.pairs[0].delay.is_none(),
        "a lock taken against silence survived the drive edge"
    );
    assert!(
        silent.ladders[0].is_none(),
        "the ladder outlived the lock it was aligned to"
    );

    let mut driving = session();
    let frames = run_correlated(&mut driving, 25, 480, events(true), t0);
    assert_eq!(frames.last().unwrap()["delay_locked"], json!(true));
    let held = driving.pairs[0].delay;
    assert!(matches!(held, Some(Lock { driving: true, .. })));
    driving.flush_locks_taken_against_silence(driving.consumed);
    assert_eq!(
        driving.pairs[0].delay.map(|l| l.samples),
        held.map(|l| l.samples),
        "a lock taken while driving was discarded by a later drive edge"
    );
}

/// Codex review of #673: after the drive comes on, the Find must wait for a
/// ring of driven audio. Tested against the rejected order — re-finding on
/// the edge tick, over a ring still holding the drive-off capture — which
/// takes a noise peak, records it as found while driving, and never finds
/// again.
#[test]
fn the_find_after_a_drive_edge_waits_for_driven_audio() {
    let mut s = session();
    let t0 = std::time::Instant::now();
    // Drive off: two unrelated legs. The Find takes their highest IR peak.
    for k in 0..30u32 {
        let now = t0 + std::time::Duration::from_millis(50 * k as u64);
        let bufs = [noise(CHUNK, 0x1000 + k), noise(CHUNK, 0x9000 + k)];
        s.tick(&bufs, events(false), &drive_msg(false), now);
    }
    assert!(
        matches!(s.pairs[0].delay, Some(Lock { driving: false, .. })),
        "test setup: a delay found against the unrelated legs"
    );

    // The drive comes on; from here the legs are the correlated pair.
    let t1 = t0 + std::time::Duration::from_secs(2);
    let edge = TickEvents {
        drive_edge_on: true,
        ..events(true)
    };
    run_correlated(&mut s, 1, 480, edge, t1);
    assert!(
        s.pairs[0].delay.is_none(),
        "found on the edge tick, over the drive-off ring"
    );
    run_correlated(&mut s, 80, 480, events(true), t1);
    assert_eq!(
        s.pairs[0].delay.map(|l| (l.samples, l.driving)),
        Some((480, true))
    );
}

/// #221: a ladder's recorded origin is the stream sample count *before* the
/// tick that built it — the ladder is built and then fed that tick's `bufs`,
/// so its first input sample is the first sample of that tick. One tick
/// either way replays a different block set, and a pure gain+delay stimulus
/// could not show it; this pins the count directly.
#[test]
fn a_ladder_records_the_sample_count_before_the_tick_that_built_it() {
    let mut s = session();
    let t0 = std::time::Instant::now();
    let x = noise(CHUNK * 60 + 480, 0x5eed);
    let mut built_on: Option<(usize, u64)> = None;
    for k in 0..40 {
        let r0 = 480 + k * CHUNK;
        let refb = x[r0..r0 + CHUNK].to_vec();
        let meas = x[r0 - 480..r0 - 480 + CHUNK].to_vec();
        let before = s.consumed;
        assert_eq!(
            before,
            (k * CHUNK) as u64,
            "consumed counts every tick's bufs"
        );
        let now = t0 + std::time::Duration::from_millis(50 * k as u64);
        s.tick(&[meas, refb], events(true), &drive_msg(true), now);
        if built_on.is_none() && s.ladders[0].is_some() {
            built_on = Some((k, before));
        }
    }
    let (k, before) = built_on.expect("a correlated pair must build its ladder");
    let prov = s.mtw_provenance()[0]
        .clone()
        .expect("a built ladder has provenance");
    assert_eq!(
        prov.origin, before as i64,
        "ladder built on tick {k}: origin must be the count before that tick"
    );
    assert_eq!(
        prov.offset, 480,
        "the ladder's offset is the lock it was built on"
    );
    assert_eq!(s.consumed, (40 * CHUNK) as u64);
}

/// #221: a flush drops the ladder, and its provenance with it, so a snapshot
/// between the flush and the rebuild cannot carry the old origin.
#[test]
fn a_flush_clears_the_ladder_provenance() {
    let mut s = session();
    let t0 = std::time::Instant::now();
    run_correlated(&mut s, 30, 480, events(true), t0);
    assert!(
        s.mtw_provenance()[0].is_some(),
        "precondition: ladder built"
    );
    s.apply_delay_cmd(REFIND, true);
    assert!(
        s.mtw_provenance()[0].is_none(),
        "provenance outlived the ladder it describes"
    );
}

/// A started ring for `session()`'s one pair, as the worker holds it.
fn started_ring() -> std::sync::Mutex<crate::handlers::snapshot::SnapshotRingState> {
    std::sync::Mutex::new(crate::handlers::snapshot::SnapshotRingState::new(
        SR,
        vec![0, 1],
        (SR as usize) * 30,
        vec![(0, 1)],
        "Z".to_string(),
        "fast".to_string(),
        vec![None, None],
    ))
}

/// One correlated tick `k` of the stream `run_correlated` feeds, delay 480.
fn correlated_tick(x: &[f32], k: usize) -> Vec<Vec<f32>> {
    let r0 = 480 + k * CHUNK;
    vec![
        x[r0 - 480..r0 - 480 + CHUNK].to_vec(),
        x[r0..r0 + CHUNK].to_vec(),
    ]
}

/// What `snapshot` would clone that bears on replay: the stream length the
/// tail ends at, and the ladder provenance and locks written beside it.
type Committed = (
    u64,
    Vec<Option<ac_core::visualize::mtw::replay::MtwProvenance>>,
    Vec<Option<i64>>,
);

/// #221 (Codex review of PR #662): the ring's tail and its ladder provenance
/// are committed together. A flush tick is the case that exposed the old
/// two-guard order — samples pushed before `tick`, provenance synced after —
/// because it retires the ladder the previous tick's provenance described.
/// This pins what `commit_tick` writes on that tick — the session's post-tick
/// state, not the retired ladder. It cannot see ordering: the old code ended
/// in the same state. The concurrent test below is the one that does.
#[test]
fn a_flush_tick_commits_its_samples_and_its_provenance_together() {
    let mut s = session();
    let ring = started_ring();
    let t0 = std::time::Instant::now();
    let x = noise(CHUNK * 60 + 480, 0x5eed);
    for k in 0..30 {
        let bufs = correlated_tick(&x, k);
        let now = t0 + std::time::Duration::from_millis(50 * k as u64);
        s.tick(&bufs, events(true), &drive_msg(true), now);
        super::super::worker::commit_tick(&ring, &bufs, &s);
    }
    let before = ring.lock().unwrap().mtw.clone();
    assert!(
        before[0].is_some(),
        "precondition: ladder built and committed"
    );

    let bufs = correlated_tick(&x, 30);
    s.apply_delay_cmd(REFIND, true);
    s.tick(
        &bufs,
        events(true),
        &drive_msg(true),
        t0 + std::time::Duration::from_secs(2),
    );
    super::super::worker::commit_tick(&ring, &bufs, &s);

    let r = ring.lock().unwrap();
    assert_eq!(r.pushed_total(), s.consumed);
    assert_eq!(r.mtw, s.mtw_provenance());
    assert_eq!(r.delay_samples, s.delay_samples());
    assert_ne!(
        r.mtw, before,
        "the flush tick's samples were committed beside the retired ladder"
    );
}

/// #221 (Codex review of PR #662): a reader taking the ring lock the way
/// `snapshot` does never sees a tail and a provenance from different ticks.
/// Every observed state must be one the worker committed whole — the empty
/// start or the session as it stood after some tick — including across
/// repeated flush-and-rebuild cycles.
///
/// Under the old order (push, unlock, tick, lock, sync) the reader can land
/// in the gap and see tick K's length beside tick K−1's ladder; that state
/// is in no committed set. The race is scheduler-dependent, so in principle
/// a regression could pass; with the old order restored in this loop it
/// failed on every run tried (length 48000 beside a flushed `[None]`).
#[test]
fn a_concurrent_reader_only_ever_sees_whole_ticks() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let ring = std::sync::Arc::new(started_ring());
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let reader = {
        let ring = ring.clone();
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut seen: Vec<Committed> = Vec::new();
            while !stop.load(Ordering::Relaxed) {
                let r = ring.lock().unwrap();
                let state = (r.pushed_total(), r.mtw.clone(), r.delay_samples.clone());
                drop(r);
                if seen.last() != Some(&state) {
                    seen.push(state);
                }
            }
            seen
        })
    };

    let mut s = session();
    let t0 = std::time::Instant::now();
    let n = 120;
    let x = noise(CHUNK * (n + 2) + 480, 0x5eed);
    let mut committed: Vec<Committed> = vec![(0, vec![None], vec![None])];
    for k in 0..n {
        let bufs = correlated_tick(&x, k);
        if k > 0 && k % 30 == 0 {
            s.apply_delay_cmd(REFIND, true);
        }
        let now = t0 + std::time::Duration::from_millis(50 * k as u64);
        s.tick(&bufs, events(true), &drive_msg(true), now);
        super::super::worker::commit_tick(&ring, &bufs, &s);
        committed.push((s.consumed, s.mtw_provenance(), s.delay_samples()));
    }
    stop.store(true, Ordering::Relaxed);
    let seen = reader.join().unwrap();

    assert!(
        committed.iter().any(|c| c.1[0].is_some()),
        "precondition: a ladder was built and committed"
    );
    for state in &seen {
        assert!(
            committed.contains(state),
            "reader saw a state no tick committed: length {} beside {:?}",
            state.0,
            state.1
        );
    }
}
