//! Per-tick session behaviour, driven directly rather than through a
//! daemon.
//!
//! Everything here was previously reachable only from a live ZMQ session:
//! the warmup gate, the block count the frame reports, the two lock
//! flushes and the refusal retry timer all lived inside the worker
//! closure. `it_relock` covers three of them end to end and is the right
//! test for the protocol, but it cannot advance the clock, so the retry
//! interval below had no test at all — a `RELOCK_RETRY` of zero, or of an
//! hour, would both have stayed green.

use super::*;

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
        mtw_stages: Value::Null,
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
        relock_requested: false,
        mc_enabled: false,
    }
}

fn drive_msg(on: bool) -> Value {
    json!({"on": on, "level_dbfs": if on { json!(-20.0) } else { Value::Null }, "drivable": true})
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

/// Uncorrelated legs: the estimator has nothing to lock to and must
/// refuse rather than pick the tallest noise peak (#227).
fn run_uncorrelated(
    s: &mut SessionState,
    ticks: &[std::time::Instant],
    ev: TickEvents,
) -> Vec<Value> {
    let mut out = Vec::new();
    for (k, &now) in ticks.iter().enumerate() {
        let meas = noise(CHUNK, 0x1000 + k as u32);
        let refb = noise(CHUNK, 0x9000 + k as u32);
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
        held["magnitude_db"], fresh.magnitude_db,
        "the frame's magnitude is not what the ring says now"
    );
    assert_eq!(held["coherence"], fresh.coherence);
    assert_eq!(held["meas_spectrum"], fresh.meas_spectrum);
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

    // Move the lock without moving the ring — the drive edge and
    // `relock` both do this in the middle of a hop.
    s.pairs[0].delay = Some(Lock {
        samples: 1200,
        driving: false,
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

/// A refused estimate must not be retried on the very next tick: each
/// attempt is the same full-ring FFT+IFFT the delay cache exists to
/// avoid, and its inputs only turn over on the ring's own timescale.
///
/// The clock is a parameter, so this asserts the interval itself. A
/// live session could only assert it by sleeping, which is why
/// `RELOCK_RETRY` had no test before: any value at all was green.
#[test]
fn a_refused_delay_waits_out_the_retry_interval_before_trying_again() {
    let mut s = session();
    let t0 = std::time::Instant::now();
    // Fill the ring, then hold the clock still: every tick after the
    // first attempt is inside the retry window.
    let warm: Vec<std::time::Instant> = (0..20)
        .map(|k| t0 + std::time::Duration::from_millis(50 * k))
        .collect();
    let frames = run_uncorrelated(&mut s, &warm, events(true));
    let first = frames.last().expect("a frame once the segment is in");
    assert_eq!(
        first["delay_locked"],
        json!(false),
        "uncorrelated legs must not lock"
    );
    assert_eq!(
        first["delay_attempts"],
        json!(1),
        "expected exactly one attempt"
    );

    // Well inside RELOCK_RETRY: no second attempt.
    let held: Vec<std::time::Instant> = (0..5)
        .map(|k| t0 + std::time::Duration::from_millis(1000 + 50 * k))
        .collect();
    let frames = run_uncorrelated(&mut s, &held, events(true));
    assert_eq!(
        frames.last().unwrap()["delay_attempts"],
        json!(1),
        "retried before the interval elapsed"
    );

    // Past it: exactly one more.
    let after = vec![t0 + RELOCK_RETRY + std::time::Duration::from_millis(1500)];
    let frames = run_uncorrelated(&mut s, &after, events(true));
    assert_eq!(
        frames.last().unwrap()["delay_attempts"],
        json!(2),
        "did not retry after the interval elapsed"
    );
}

/// `relock` (#226) discards the held lock, and the attempt counter
/// stays monotone across it — a pair that locked and then started
/// refusing must not read as one never asked (`ac-scene::fault`).
#[test]
fn a_relock_request_drops_the_lock_and_leaves_the_attempt_count_monotone() {
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

    let ev = TickEvents {
        relock_requested: true,
        ..events(true)
    };
    // The flush lands before this tick's own acquisition, so the pair
    // re-locks within the same tick — what changes is the attempt
    // count, which must have gone up rather than reset.
    let after = run_correlated(&mut s, 1, 480, ev, t0 + std::time::Duration::from_secs(5));
    let f = after.last().unwrap();
    assert!(
        f["delay_attempts"].as_u64().unwrap() > attempts_before,
        "relock did not cause a new attempt"
    );
}

/// The drive off→on edge discards a lock taken against silence and
/// keeps one taken while driving (#226). `it_relock` covers both over
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
    silent.flush_locks_taken_against_silence();
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
    driving.flush_locks_taken_against_silence();
    assert_eq!(
        driving.pairs[0].delay.map(|l| l.samples),
        held.map(|l| l.samples),
        "a lock taken while driving was discarded by a later drive edge"
    );
}
