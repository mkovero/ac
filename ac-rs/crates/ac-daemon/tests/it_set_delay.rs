//! ZMQ integration tests for `set_delay` (#669) and the drive off→on flush
//! (#226). `set_delay` with `samples: null` is the re-find `relock` used to be.
//!
//! `fake_correlated_pair` (see `it_set_drive.rs`'s doc for why it, not the
//! plain idle tone) gives a deterministic delay to lock onto — the fake
//! backend's correlated-pair generator is a seeded pseudorandom stream
//! (`audio/fake/stimulus.rs::correlated_source_at`), so re-estimating
//! against the same configured `delay_samples` reliably reproduces the same
//! lock. That determinism is what lets these tests assert "the new lag
//! equals the old" rather than merely "some lag exists" — a plain idle-tone
//! pair could not distinguish a re-lock that worked from one that silently
//! failed and defaulted to 0, since a digital loopback's own genuine delay
//! is also 0. These sessions are drivable, so their outputs are open and a
//! `set_drive on` is heard: the fake adds the generator's noise to the
//! pair rather than replacing it. That noise is seeded per channel, so it
//! is uncorrelated between meas and ref — a drive lowers the pair's
//! coherence but leaves its correlated component (the configured
//! `delay_samples` and `gain`) unchanged, and every lock, drive on or off,
//! must find `LOCK_DELAY_SAMPLES`.

use std::thread;
use std::time::Duration;

use serde_json::{json, Value};

#[path = "common/mod.rs"]
mod common;

use common::{Client, Daemon};

const CEILING_DBFS: f64 = -20.0;
/// The configured `fake_correlated_pair` delay most tests lock onto.
const LOCK_DELAY_SAMPLES: i64 = 400;
/// Generous — a throttled CI runner's FFT/rayon fan-out can fall well
/// behind realtime under load; these tests care about correctness of the
/// transition, not its latency.
const LOCK_TIMEOUT: Duration = Duration::from_secs(20);

fn start_correlated(c: &Client, drivable: bool, delay_samples: i64, gain: f64) -> Value {
    c.call(json!({
        "cmd": "transfer_stream", "meas_channel": 0, "ref_channel": 1,
        "weighting": "Z", "integration": "fast",
        "drivable": drivable,
        "fake_correlated_pair": {"gain": gain, "delay_samples": delay_samples},
    }))
}

fn locked(f: &Value) -> bool {
    f["delay_locked"] == json!(true)
}

fn attempts(f: &Value) -> u64 {
    f["delay_attempts"].as_u64().expect("delay_attempts")
}

fn delay_samples(f: &Value) -> i64 {
    f["delay_samples"].as_i64().expect("delay_samples")
}

fn mtw_present(f: &Value) -> bool {
    !f["mtw"].is_null()
}

// ---------------------------------------------------------------------
// 6. No session running.
// ---------------------------------------------------------------------

#[test]
fn set_delay_with_no_session_running_errors_instead_of_panicking() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let r = c.call(json!({"cmd": "set_delay", "samples": null}));
    assert_eq!(r["ok"], json!(false), "{r}");
    assert_eq!(r["error"], json!("no transfer_stream session running"));

    // The daemon is still alive and answering afterwards.
    assert_eq!(c.call(json!({"cmd": "status"}))["ok"], json!(true));
}

// ---------------------------------------------------------------------
// 2. delay_locked cycles false→true and the new lag equals the old.
// ---------------------------------------------------------------------

#[test]
fn refind_unlocks_and_refinds_the_same_lag_on_a_static_fake_path() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    assert_eq!(
        start_correlated(&c, false, LOCK_DELAY_SAMPLES, 0.6)["ok"],
        json!(true)
    );

    let before = c.frame_matching(LOCK_TIMEOUT, locked);
    assert_eq!(delay_samples(&before), LOCK_DELAY_SAMPLES);
    let attempts_before = attempts(&before);

    assert_eq!(
        c.call(json!({"cmd": "set_delay", "samples": null}))["ok"],
        json!(true)
    );

    // `flush_pair` clears `next_delay_attempt` precisely so the retry runs
    // on the SAME worker tick as the flush, not up to `FIND_RETRY` later
    // — and on a clean deterministic signal that retry succeeds
    // immediately, so the unlocked intermediate is internal state
    // (`pair_delays[i] = None`) that never survives to a published wire
    // frame. `delay_attempts` strictly increasing is what's actually
    // observable here, and is the flush's real fingerprint — see test
    // `delay_attempts_never_resets_across_refinds_and_drive_edges`
    // for the property this stands in for.
    let after = c.frame_matching(LOCK_TIMEOUT, |f| locked(f) && attempts(f) > attempts_before);
    assert_eq!(
        delay_samples(&after),
        LOCK_DELAY_SAMPLES,
        "re-lock against the same deterministic signal found a different lag"
    );
}

// ---------------------------------------------------------------------
// 3. mtw columns disappear on the flush and return after the re-lock.
// ---------------------------------------------------------------------

#[test]
fn refind_drops_and_rebuilds_the_mtw_ladder() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    assert_eq!(
        start_correlated(&c, false, LOCK_DELAY_SAMPLES, 0.6)["ok"],
        json!(true)
    );

    // Wait for the ladder to produce at least one column before flushing —
    // otherwise "mtw disappeared" would be indistinguishable from "mtw
    // never appeared yet".
    let settled = c.frame_matching(LOCK_TIMEOUT, |f| locked(f) && mtw_present(f));
    let attempts_before = attempts(&settled);

    assert_eq!(
        c.call(json!({"cmd": "set_delay", "samples": null}))["ok"],
        json!(true)
    );

    // The delay lock itself can re-settle within the same worker tick as
    // the flush on a clean deterministic signal (see
    // `refind_unlocks_and_refinds_the_same_lag_on_a_static_fake_path`),
    // so `!locked` is not a reliable flush signal here. The ladder is:
    // `mtw[i] = None` is followed by a *fresh* `MtwPair` built the same
    // tick the new lock lands, and a fresh ladder needs several ticks of
    // pushes before `columns()` has anything to report — so `locked &&
    // !mtw_present` is the window this flush actually produces.
    let flushed = c.frame_matching(LOCK_TIMEOUT, |f| {
        locked(f) && attempts(f) > attempts_before && !mtw_present(f)
    });
    assert!(
        !mtw_present(&flushed),
        "mtw columns survived the flush: {flushed}"
    );

    let rebuilt = c.frame_matching(LOCK_TIMEOUT, |f| locked(f) && mtw_present(f));
    assert_eq!(delay_samples(&rebuilt), LOCK_DELAY_SAMPLES);
}

// ---------------------------------------------------------------------
// 4. A lock taken with the drive off is discarded on the first
//    `set_drive on`.
// ---------------------------------------------------------------------

#[test]
fn a_lock_taken_with_drive_off_is_discarded_on_the_first_drive_on() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    assert_eq!(
        start_correlated(&c, true, LOCK_DELAY_SAMPLES, 0.6)["ok"],
        json!(true)
    );

    // Locked while the session's drive is still off (no set_drive sent
    // yet) — `lock.driving` must be false.
    let before = c.frame_matching(LOCK_TIMEOUT, locked);
    let attempts_before = attempts(&before);

    assert_eq!(
        c.call(json!({"cmd": "set_drive", "on": true, "level_dbfs": CEILING_DBFS}))["ok"],
        json!(true)
    );

    // The edge must flush: the attempt count strictly increases once it
    // relocks. (Flush and re-lock can land in the same worker tick on a
    // clean deterministic signal — see
    // `refind_unlocks_and_refinds_the_same_lag_on_a_static_fake_path`
    // — so `attempts` increasing, not an observed `!locked` frame, is
    // the flush's reliable wire fingerprint.)
    let after = c.frame_matching(LOCK_TIMEOUT, |f| locked(f) && attempts(f) > attempts_before);
    assert!(
        attempts(&after) > attempts_before,
        "drive-on edge did not trigger a new attempt: before {attempts_before}, after {}",
        attempts(&after)
    );
    // This re-lock both starts and completes under drive, so it pins that
    // the drive's contribution stays uncorrelated at lag 0.
    assert_eq!(delay_samples(&after), LOCK_DELAY_SAMPLES);
}

// ---------------------------------------------------------------------
// 5. A lock taken WHILE driving survives a dead-man expiry and the
//    subsequent `set_drive on` — the thrash case, and the one a bare
//    edge (no provenance qualifier) gets wrong.
// ---------------------------------------------------------------------

#[test]
fn a_lock_taken_while_driving_survives_dead_man_expiry_and_resume() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    assert_eq!(
        start_correlated(&c, true, LOCK_DELAY_SAMPLES, 0.6)["ok"],
        json!(true)
    );

    // Drive on BEFORE the pair locks, so the lock's provenance is
    // `driving: true`.
    assert_eq!(
        c.call(json!({"cmd": "set_drive", "on": true, "level_dbfs": CEILING_DBFS}))["ok"],
        json!(true)
    );
    let locked_frame = c.frame_matching(LOCK_TIMEOUT, locked);
    let attempts_at_lock = attempts(&locked_frame);
    assert_eq!(delay_samples(&locked_frame), LOCK_DELAY_SAMPLES);

    // No CTRL traffic at all for longer than the 1.5 s dead-man window —
    // drive drops (on→off), which this issue deliberately excludes from
    // the flush regardless of provenance.
    thread::sleep(Duration::from_millis(1_700));
    let after_expiry = c.frame_matching(LOCK_TIMEOUT, |f| f["drive"]["on"] == json!(false));
    assert!(
        locked(&after_expiry),
        "dead-man expiry (on→off) flushed a held lock: {after_expiry}"
    );
    assert_eq!(delay_samples(&after_expiry), LOCK_DELAY_SAMPLES);
    assert_eq!(attempts(&after_expiry), attempts_at_lock);

    // Resume — off→on, the edge this issue watches. Provenance says this
    // lock was acquired while driving, so it must survive untouched: no
    // unlocked frame, no new attempt, same lag.
    assert_eq!(
        c.call(json!({"cmd": "set_drive", "on": true, "level_dbfs": CEILING_DBFS}))["ok"],
        json!(true)
    );
    for _ in 0..10 {
        let f = c
            .next_frame(Duration::from_secs(3))
            .expect("no frame after resume");
        assert!(locked(&f), "resume flushed a lock taken while driving: {f}");
        assert_eq!(delay_samples(&f), LOCK_DELAY_SAMPLES);
        assert_eq!(
            attempts(&f),
            attempts_at_lock,
            "resume re-attempted a lock that should have survived untouched"
        );
    }
}

// ---------------------------------------------------------------------
// 7. A stop and a restart the OPERATOR asked for do not compose into a
//    firing edge either — distinct path from test 5's dead-man case.
// ---------------------------------------------------------------------

#[test]
fn an_operator_stop_and_restart_do_not_flush_a_lock_taken_while_driving() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    assert_eq!(
        start_correlated(&c, true, LOCK_DELAY_SAMPLES, 0.6)["ok"],
        json!(true)
    );
    assert_eq!(
        c.call(json!({"cmd": "set_drive", "on": true, "level_dbfs": CEILING_DBFS}))["ok"],
        json!(true)
    );
    let locked_frame = c.frame_matching(LOCK_TIMEOUT, locked);
    let attempts_at_lock = attempts(&locked_frame);
    let mtw_at_lock = mtw_present(&locked_frame);

    // Operator-requested off — `on: true → false` never discards a lock.
    assert_eq!(
        c.call(json!({"cmd": "set_drive", "on": false, "level_dbfs": CEILING_DBFS}))["ok"],
        json!(true)
    );
    let after_off = c.frame_matching(LOCK_TIMEOUT, |f| f["drive"]["on"] == json!(false));
    assert!(locked(&after_off), "operator off flushed a held lock");
    assert_eq!(delay_samples(&after_off), LOCK_DELAY_SAMPLES);

    // Operator-requested restart — off→on, but this lock's provenance is
    // `driving: true`, so it survives.
    assert_eq!(
        c.call(json!({"cmd": "set_drive", "on": true, "level_dbfs": CEILING_DBFS}))["ok"],
        json!(true)
    );
    for _ in 0..10 {
        let f = c
            .next_frame(Duration::from_secs(3))
            .expect("no frame after restart");
        assert!(
            locked(&f),
            "operator restart flushed a lock taken while driving: {f}"
        );
        assert_eq!(delay_samples(&f), LOCK_DELAY_SAMPLES);
        assert_eq!(attempts(&f), attempts_at_lock);
        if mtw_at_lock {
            assert!(
                mtw_present(&f),
                "mtw columns dropped across a stop/restart that should not flush"
            );
        }
    }
}

// ---------------------------------------------------------------------
// 1. delay_attempts strictly increases and never resets across either
//    trigger (manual key, then the drive edge).
// ---------------------------------------------------------------------

#[test]
fn delay_attempts_never_resets_across_refinds_and_drive_edges() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    assert_eq!(
        start_correlated(&c, true, LOCK_DELAY_SAMPLES, 0.6)["ok"],
        json!(true)
    );

    // Locked with drive off — `lock.driving == false`.
    let f0 = c.frame_matching(LOCK_TIMEOUT, locked);
    let mut high_water = attempts(&f0);
    assert!(high_water >= 1);

    // Trigger 1: the manual key. (Flush and re-lock can land in the same
    // worker tick on a clean deterministic signal, so `attempts`
    // increasing — not an observed `!locked` frame — is the flush's
    // reliable wire fingerprint; see
    // `refind_unlocks_and_refinds_the_same_lag_on_a_static_fake_path`.)
    assert_eq!(
        c.call(json!({"cmd": "set_delay", "samples": null}))["ok"],
        json!(true)
    );
    let f1 = c.frame_matching(LOCK_TIMEOUT, |f| locked(f) && attempts(f) > high_water);
    assert!(
        attempts(&f1) > high_water,
        "manual re-find did not advance attempts"
    );
    high_water = attempts(&f1);

    // Trigger 2: the drive off→on edge (this lock was taken with drive
    // off, so the edge flushes it).
    assert_eq!(
        c.call(json!({"cmd": "set_drive", "on": true, "level_dbfs": CEILING_DBFS}))["ok"],
        json!(true)
    );
    let f2 = c.frame_matching(LOCK_TIMEOUT, |f| locked(f) && attempts(f) > high_water);
    assert!(
        attempts(&f2) > high_water,
        "drive-edge re-find did not advance attempts"
    );
    high_water = attempts(&f2);

    // A third manual press on a now-driving-acquired lock must not lose
    // ground either.
    assert_eq!(
        c.call(json!({"cmd": "set_delay", "samples": null}))["ok"],
        json!(true)
    );
    let f3 = c.frame_matching(LOCK_TIMEOUT, |f| locked(f) && attempts(f) > high_water);
    assert!(
        attempts(&f3) > high_water,
        "attempts went backwards or stalled across a third re-find: {} -> {}",
        high_water,
        attempts(&f3)
    );
}

// ---------------------------------------------------------------------
// #669: the operator owns the delay.
// ---------------------------------------------------------------------

fn residual(f: &Value) -> Option<i64> {
    f["delay_residual"].as_i64()
}

/// A typed delay is held as typed and marked operator-set, and the frame's
/// residual is what is left over: the path's 400 against a setting of 390
/// reads +10. Find → Insert is then `set_delay(delay_samples + residual)`,
/// after which the residual reads 0.
#[test]
fn a_typed_delay_is_held_and_the_residual_reads_the_difference() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    assert_eq!(
        start_correlated(&c, false, LOCK_DELAY_SAMPLES, 0.6)["ok"],
        json!(true)
    );
    let found = c.frame_matching(LOCK_TIMEOUT, |f| locked(f) && residual(f).is_some());
    assert_eq!(delay_samples(&found), LOCK_DELAY_SAMPLES);
    assert_eq!(residual(&found), Some(0));
    assert_eq!(found["delay_operator"], json!(false));

    let r = c.call(json!({"cmd": "set_delay", "samples": LOCK_DELAY_SAMPLES - 10}));
    assert_eq!(r["ok"], json!(true), "{r}");
    let typed = c.frame_matching(LOCK_TIMEOUT, |f| {
        delay_samples(f) == LOCK_DELAY_SAMPLES - 10 && residual(f).is_some()
    });
    assert_eq!(typed["delay_operator"], json!(true));
    assert_eq!(residual(&typed), Some(10));

    let insert = delay_samples(&typed) + residual(&typed).unwrap();
    assert_eq!(
        c.call(json!({"cmd": "set_delay", "samples": insert, "pair": 0}))["ok"],
        json!(true)
    );
    let inserted = c.frame_matching(LOCK_TIMEOUT, |f| {
        delay_samples(f) == LOCK_DELAY_SAMPLES && f["delay_operator"] == json!(true)
    });
    assert_eq!(residual(&inserted), Some(0));
}

/// A delay the operator set while the drive was off survives the drive
/// coming on: nothing the daemon does by itself replaces it.
#[test]
fn an_operator_delay_survives_the_drive_edge() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    assert_eq!(
        start_correlated(&c, true, LOCK_DELAY_SAMPLES, 0.6)["ok"],
        json!(true)
    );
    c.frame_matching(LOCK_TIMEOUT, locked);
    assert_eq!(
        c.call(json!({"cmd": "set_delay", "samples": 123}))["ok"],
        json!(true)
    );
    let typed = c.frame_matching(LOCK_TIMEOUT, |f| delay_samples(f) == 123);
    let attempts_at_set = attempts(&typed);
    assert_eq!(
        c.call(json!({"cmd": "set_drive", "on": true, "level_dbfs": CEILING_DBFS}))["ok"],
        json!(true)
    );
    let driving = c.frame_matching(LOCK_TIMEOUT, |f| f["drive"]["on"] == json!(true));
    for f in std::iter::once(driving).chain((0..10).map(|_| {
        c.next_frame(Duration::from_secs(3))
            .expect("no frame after drive on")
    })) {
        assert_eq!(
            delay_samples(&f),
            123,
            "the drive edge replaced a typed delay: {f}"
        );
        assert_eq!(attempts(&f), attempts_at_set);
    }
}

/// Malformed requests are refused with a reason, never coerced: a delay
/// that silently became 0 would look exactly like a digital loopback.
#[test]
fn set_delay_refuses_malformed_requests() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    assert_eq!(
        start_correlated(&c, false, LOCK_DELAY_SAMPLES, 0.6)["ok"],
        json!(true)
    );
    for (req, why) in [
        (json!({"cmd": "set_delay"}), "missing samples"),
        (
            json!({"cmd": "set_delay", "samples": 1.5}),
            "fractional samples",
        ),
        (
            json!({"cmd": "set_delay", "samples": "400"}),
            "string samples",
        ),
        (
            json!({"cmd": "set_delay", "samples": 0, "pair": -1}),
            "negative pair",
        ),
        (
            json!({"cmd": "set_delay", "samples": 0, "pair": 1}),
            "pair out of range",
        ),
    ] {
        let r = c.call(req);
        assert_eq!(r["ok"], json!(false), "{why}: {r}");
        assert!(r["error"].is_string(), "{why}: no reason given: {r}");
    }
}
