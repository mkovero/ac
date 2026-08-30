//! Tests for [`super`] — the transfer view's fault indicator (#228).
//!
//! Split out of `fault.rs` because the cases outgrew the module: 43 tests
//! against ~200 lines of logic. Kept as one `mod tests` via `#[path]`, the
//! same pattern `ac-view/tests/support.rs` is included with, so `super::*`
//! still reaches the private items the state machine is tested through.

use super::*;

fn driving() -> FaultFrame {
    FaultFrame {
        drive: DriveState {
            on: true,
            drivable: true,
        },
        delay_locked: Some(true),
        settled: true,
        delay_attempts: 1,
    }
}

/// The daemon's retry interval (`RELOCK_RETRY`, `handlers/transfer.rs`).
/// Duplicated here on purpose: it is not this crate's constant, and the
/// point of #247 is that the module must not depend on its value.
const DAEMON_RETRY_S: f64 = 1.0;

/// Fold a refusing pair forward the way a daemon actually publishes it:
/// a frame every `retry_s`, with `delay_attempts` incrementing on each,
/// starting from attempt 1 at `t = 0`. Returns the indicator from the
/// last frame folded in.
///
/// Tests that want the escalation must go through this rather than
/// jumping the scene clock: `NO LOCK` is now the later of a time and an
/// attempt count, and a clock jump alone models a daemon that stopped
/// retrying — which is a different session and must not escalate.
fn refuse_for(st: &mut FaultState, frame: FaultFrame, retry_s: f64, until_s: f64) -> Option<Fault> {
    let mut out = None;
    let mut attempts = 0u32;
    let mut t = 0.0;
    while t <= until_s + f64::EPSILON {
        attempts += 1;
        let f = FaultFrame {
            delay_attempts: attempts,
            ..frame
        };
        out = st.update(
            &FaultInput {
                frame: Some(f),
                meas_peak_dbfs: Some(-30.0),
                ref_peak_dbfs: Some(-14.5),
                coherence: &[],
            },
            t,
        );
        t += retry_s;
    }
    out
}

/// A driving session with both legs live, settled, and locked — the
/// healthy baseline every case below perturbs one field of. The peaks
/// are the rig's own, 15 dB apart.
fn healthy(coherence: &[f64]) -> FaultInput<'_> {
    FaultInput {
        frame: Some(driving()),
        meas_peak_dbfs: Some(-30.0),
        ref_peak_dbfs: Some(-14.5),
        coherence,
    }
}

/// The rig's own healthy coherence: stage 0 reverberation-limited at 0.755,
/// the lower rungs well above it. Named because almost every case below
/// needs a live ladder and none of them is *about* these numbers.
const LIVE_COH: [f64; 2] = [0.755, 0.92];

/// Every column below the display's mask — two legs carrying unrelated
/// sources, with nothing drawable.
const DEAD_COH: [f64; 3] = [0.1, 0.08, 0.12];

/// A drivable session sitting idle: nothing driven, the estimator never
/// asked, no ladder. Row 1 of the table.
fn idle() -> FaultFrame {
    FaultFrame {
        drive: DriveState {
            on: false,
            drivable: true,
        },
        delay_locked: None,
        settled: false,
        delay_attempts: 0,
    }
}

/// A fully passive external-DUT session. It never drives, so the level rows
/// have no ground to stand on — and the lock rows still do, which is what
/// the `drivable: false` cases exist to pin.
fn passive(delay_locked: Option<bool>) -> FaultFrame {
    FaultFrame {
        drive: DriveState {
            on: false,
            drivable: false,
        },
        delay_locked,
        settled: true,
        delay_attempts: 1,
    }
}

/// [`driving`], with the estimator refusing. The baseline the lock-row
/// cases perturb.
fn refusing_frame() -> FaultFrame {
    FaultFrame {
        delay_locked: Some(false),
        ..driving()
    }
}

/// A pair still warming up: `delay_locked: false` with no estimate yet
/// attempted and no ladder. Indistinguishable from a refusal on the flag
/// alone, which is why nothing may paint from it.
fn warming() -> FaultFrame {
    FaultFrame {
        delay_locked: Some(false),
        settled: false,
        delay_attempts: 0,
        ..driving()
    }
}

/// A refusal reported by a producer that publishes no attempt count
/// (pre-#238): settled, so it reaches the refusal rows through the ladder
/// it kept from a lock it has since lost, with the attempts side
/// unobservable.
///
/// Not a frame a live #226 daemon sends — attempts are monotone and a lock
/// implies at least one — so this shape isolates the `settled` half of the
/// warmup gate rather than reproducing the wire.
fn settled_refusal_without_attempts() -> FaultFrame {
    FaultFrame {
        delay_locked: Some(false),
        settled: true,
        delay_attempts: 0,
        ..driving()
    }
}

/// One frame against the healthy peaks: the shape almost every case wants,
/// where the frame is the subject and the peaks are not.
fn frame_in(frame: FaultFrame, coherence: &[f64]) -> FaultInput<'_> {
    FaultInput {
        frame: Some(frame),
        ..healthy(coherence)
    }
}

/// [`refuse_for`] at the daemon's own pace, folded to the point where both
/// escalation thresholds are met. The pacing is only the *subject* of the
/// two #247 tests; everywhere else it is the backdrop.
fn refuse_to_threshold(frame: FaultFrame) -> Option<Fault> {
    refuse_for(
        &mut FaultState::default(),
        frame,
        DAEMON_RETRY_S,
        PERSISTENT_REFUSAL_S,
    )
}

#[test]
fn a_healthy_settled_session_shows_nothing() {
    // The rig's real figures: stage 0 reverberation-limited at 0.755,
    // the lower rungs well above it.
    let coh = [0.755, 0.92, 0.93];
    let mut st = FaultState::default();
    assert_eq!(st.update(&healthy(&coh), 0.0), None);
}

#[test]
fn a_daemon_without_drive_state_shows_nothing_at_all() {
    let mut st = FaultState::default();
    let inp = FaultInput {
        frame: None,
        // An older daemon has no capture peaks either, which would read
        // as silence on both legs if the indicator ran at all.
        meas_peak_dbfs: None,
        ref_peak_dbfs: None,
        coherence: &[],
    };
    assert_eq!(st.update(&inp, 0.0), None);
}

#[test]
fn a_dead_reference_leg_while_driving_is_no_reference() {
    let mut st = FaultState::default();
    let inp = FaultInput {
        ref_peak_dbfs: None, // digital silence
        ..healthy(&LIVE_COH)
    };
    assert_eq!(st.update(&inp, 0.0), Some(Fault::NoReference));
}

#[test]
fn a_dead_measurement_leg_while_driving_is_no_signal() {
    let mut st = FaultState::default();
    let inp = FaultInput {
        meas_peak_dbfs: Some(-95.0),
        ..healthy(&LIVE_COH)
    };
    assert_eq!(st.update(&inp, 0.0), Some(Fault::NoSignal));
}

#[test]
fn both_legs_dead_names_the_reference_not_the_mic() {
    let mut st = FaultState::default();
    let inp = FaultInput {
        meas_peak_dbfs: None,
        ref_peak_dbfs: None,
        ..healthy(&LIVE_COH)
    };
    assert_eq!(st.update(&inp, 0.0), Some(Fault::NoReference));
}

/// The floor is absolute, never relative. The rig's own 15 dB leg
/// imbalance is a valid session and must show nothing.
#[test]
fn a_fifteen_db_leg_imbalance_is_not_a_fault() {
    let mut st = FaultState::default();
    assert_eq!(st.update(&healthy(&LIVE_COH), 0.0), None);
}

/// A quiet but valid session. −79 dBFS is unusable in practice but it is
/// above the floor, and the floor's job is to catch *nothing at all*.
#[test]
fn a_very_quiet_but_present_leg_is_not_at_the_floor() {
    let coh = [0.755];
    let mut st = FaultState::default();
    let inp = FaultInput {
        meas_peak_dbfs: Some(-79.0),
        ..healthy(&coh)
    };
    assert_eq!(st.update(&inp, 0.0), None);
    // And the boundary itself is inclusive.
    let inp = FaultInput {
        meas_peak_dbfs: Some(SIGNAL_FLOOR_DBFS),
        ..healthy(&coh)
    };
    assert_eq!(st.update(&inp, 0.0), Some(Fault::NoSignal));
}

#[test]
fn silence_on_an_idle_session_shows_nothing() {
    let mut st = FaultState::default();
    let inp = FaultInput {
        meas_peak_dbfs: None,
        ref_peak_dbfs: None,
        ..frame_in(idle(), &[])
    };
    assert_eq!(st.update(&inp, 0.0), None);
}

/// A fully passive external-DUT session never drives, so silence on its
/// inputs says nothing and neither level row may fire.
#[test]
fn a_non_drivable_session_gets_no_level_row() {
    let mut st = FaultState::default();
    let inp = FaultInput {
        meas_peak_dbfs: None,
        ref_peak_dbfs: None,
        // The estimator has run and refused, so the suppression below is
        // the drive gate doing its job, not the warmup gate hiding the
        // case by accident. `settled: false` because a pair that never
        // locked never built a ladder.
        ..frame_in(
            FaultFrame {
                settled: false,
                ..passive(Some(false))
            },
            &[],
        )
    };
    assert_eq!(st.update(&inp, 0.0), None);
}

/// But the lock rows are not about driving. An external-DUT session with
/// both legs live and the estimator refusing is a real fault, and one the
/// operator cannot resolve by starting the stimulus — so suppressing it
/// on `drivable: false` would hide the case that needs it most.
#[test]
fn a_non_drivable_session_still_gets_its_lock_rows() {
    let mut st = FaultState::default();
    let inp = frame_in(passive(Some(false)), &LIVE_COH);
    assert_eq!(st.update(&inp, 0.0), Some(Fault::NoLockYet));
    assert_eq!(
        refuse_to_threshold(passive(Some(false))),
        Some(Fault::NoLock)
    );
}

/// The same for the other two both-legs-live rows: neither reads drive.
#[test]
fn a_non_drivable_session_still_gets_check_routing_and_the_confirmation() {
    let mut st = FaultState::default();
    let inp = frame_in(passive(Some(true)), &DEAD_COH);
    assert_eq!(st.update(&inp, 0.0), Some(Fault::CheckRouting));

    let mut st = FaultState::default();
    let refusing = frame_in(passive(Some(false)), &LIVE_COH);
    let locked = frame_in(passive(Some(true)), &LIVE_COH);
    assert_eq!(st.update(&refusing, 0.0), Some(Fault::NoLockYet));
    assert_eq!(st.update(&locked, 1.0), Some(Fault::LockAcquired));
}

/// The refusal rows must not name a cause. A refusal is equally
/// consistent with an off-axis mic, with unrelated sources, and with a
/// path that has nothing to correlate — which matters more once #227
/// lands and unrelated sources reach the operator through `NO LOCK`
/// rather than through `CHECK ROUTING`.
#[test]
fn the_refusal_rows_name_what_to_check_not_why() {
    let detail = Fault::NoLock.detail().expect("has an instruction");
    for asserted in ["closer", "on-axis", "unrelated", "too far", "unplugged"] {
        assert!(
            !detail.contains(asserted),
            "NO LOCK's detail asserts a cause it cannot know: {detail:?}"
        );
    }
    // It still has to send the operator somewhere.
    assert!(detail.contains("mic") && detail.contains("routing"));
    assert_eq!(Fault::LostLock.detail(), None);
}

/// The whole reason lock states wait: `delay_locked: false` before the
/// estimator has run is warmup, indistinguishable from a refusal on the
/// flag alone, and an indicator that fires on every healthy startup gets
/// ignored.
#[test]
fn warmup_does_not_paint_a_lock_fault() {
    let mut st = FaultState::default();
    let inp = frame_in(warming(), &[]);
    // Well past PERSISTENT_REFUSAL_S in scene time — still nothing,
    // because the clock has not started.
    assert_eq!(st.update(&inp, 0.0), None);
    assert_eq!(st.update(&inp, 30.0), None);
}

/// #238's regression test. A pair that never locks never gets a ladder,
/// so `settled` is false forever — and gating on it alone left `LOST
/// LOCK` and `NO LOCK` unreachable, which is what put a blank window in
/// front of an operator for a whole rig session.
#[test]
fn a_refusal_that_never_settles_still_paints() {
    let mut st = FaultState::default();
    let never_settles = FaultFrame {
        delay_locked: Some(false),
        // No lock, so no ladder, so no columns — for the whole session.
        settled: false,
        delay_attempts: 1,
        ..driving()
    };
    let refusing = frame_in(never_settles, &[]);
    assert_eq!(st.update(&refusing, 0.0), Some(Fault::NoLockYet));
    assert_eq!(refuse_to_threshold(never_settles), Some(Fault::NoLock));
}

/// A daemon predating #238 publishes no attempt count, and absence is not
/// evidence that the estimator ran. Such a session stays as silent as it
/// was — a wrong banner on an old daemon would be worse than the gap.
#[test]
fn a_daemon_without_the_attempt_count_paints_no_refusal() {
    let mut st = FaultState::default();
    let old = frame_in(warming(), &[]);
    assert_eq!(st.update(&old, 0.0), None);
    assert_eq!(st.update(&old, 100.0), None);
}

/// And the refusal clock starts when the estimator first answers, not at
/// session start: a timer from t=0 would fire NO LOCK on a session that
/// was still filling its rings.
#[test]
fn the_refusal_clock_starts_at_the_first_attempt_not_at_session_start() {
    let mut st = FaultState::default();
    let inp = frame_in(warming(), &[]);
    for t in 0..30 {
        assert_eq!(st.update(&inp, t as f64), None);
    }
    // The first frame carrying a completed attempt, at t=30, is where
    // the clock starts — here on a pair that goes on to lock and settle.
    // Attempts advance with it: escalation is the later of ten seconds
    // and ten attempts, so a bare clock jump would prove nothing about
    // the anchor.
    let refusing = |n: u32| {
        frame_in(
            FaultFrame {
                delay_locked: Some(false),
                delay_attempts: n,
                ..driving()
            },
            &LIVE_COH,
        )
    };
    assert_eq!(st.update(&refusing(1), 30.0), Some(Fault::NoLockYet));
    for n in 2..=9 {
        assert_eq!(
            st.update(&refusing(n), 30.0 + n as f64 - 1.0),
            Some(Fault::NoLockYet)
        );
    }
    assert_eq!(st.update(&refusing(10), 39.9), Some(Fault::NoLockYet));
    assert_eq!(st.update(&refusing(10), 40.0), Some(Fault::NoLock));
}

/// The transient row's words follow the history. `LOST LOCK` on a pair
/// that never locked asserts something untrue, and a fault indicator
/// whose words are wrong is what #228 exists to replace.
#[test]
fn a_pair_that_never_locked_is_not_told_it_lost_a_lock() {
    let refusing = |st: &mut FaultState, t: f64| {
        let inp = frame_in(
            FaultFrame {
                delay_locked: Some(false),
                ..driving()
            },
            &LIVE_COH,
        );
        st.update(&inp, t)
    };

    // Never locked: the transient row says NO LOCK, without the
    // instruction the persistent row carries.
    let mut fresh = FaultState::default();
    assert_eq!(refusing(&mut fresh, 0.0), Some(Fault::NoLockYet));
    assert_eq!(Fault::NoLockYet.label(), "NO LOCK");
    assert_eq!(Fault::NoLockYet.detail(), None);
    assert_eq!(
        refuse_to_threshold(FaultFrame {
            delay_locked: Some(false),
            ..driving()
        }),
        Some(Fault::NoLock)
    );

    // Locked earlier, refusing now: LOST LOCK is true, and only here.
    let mut held = FaultState::default();
    assert_eq!(held.update(&healthy(&LIVE_COH), 0.0), None);
    assert_eq!(refusing(&mut held, 1.0), Some(Fault::LostLock));
    assert_eq!(Fault::LostLock.label(), "LOST LOCK");
}

/// A lock earlier in the session is what licenses `LOST LOCK` later. The
/// history is carried, not re-read from the current frame.
#[test]
fn the_words_follow_the_history_not_the_current_frame() {
    let mut st = FaultState::default();
    let refusing = frame_in(refusing_frame(), &LIVE_COH);
    assert_eq!(st.update(&refusing, 0.0), Some(Fault::NoLockYet));
    assert_eq!(
        st.update(&healthy(&LIVE_COH), 1.0),
        Some(Fault::LockAcquired)
    );
    assert_eq!(st.update(&refusing, 5.0), Some(Fault::LostLock));
}

/// The settled gate still stands on its own, for the case the attempt
/// count cannot cover: a pair that locked, settled, and then lost the
/// lock keeps its ladder, and its refusal must still paint.
///
/// `delay_attempts: 0` here is not what a live #226 daemon would send for
/// a pair that has already locked — attempts is monotone and a lock
/// implies at least one — so this frame isolates the `settled` half of
/// the gate from the `estimator_attempted` half rather than reproducing
/// a real wire frame. It is the only test that pins that half in
/// isolation.
#[test]
fn a_settled_pair_that_loses_its_lock_still_paints() {
    let mut st = FaultState::default();
    assert_eq!(st.update(&healthy(&LIVE_COH), 0.0), None);
    let lost = frame_in(settled_refusal_without_attempts(), &LIVE_COH);
    assert_eq!(st.update(&lost, 1.0), Some(Fault::LostLock));
}

#[test]
fn a_persistent_refusal_gets_different_words_than_a_transient_one() {
    let mut st = FaultState::default();
    let refusing = frame_in(refusing_frame(), &LIVE_COH);
    assert_eq!(st.update(&refusing, 0.0), Some(Fault::NoLockYet));
    assert_eq!(
        refuse_to_threshold(FaultFrame {
            delay_locked: Some(false),
            ..driving()
        }),
        Some(Fault::NoLock)
    );
    // The transient one deliberately carries no instruction; the
    // persistent one carries the one the operator needs.
    assert_eq!(Fault::NoLockYet.detail(), None);
    assert!(Fault::NoLock.detail().is_some());
    // The escalation adds the instruction; it does not change the claim,
    // so the two rows share their label on purpose.
    assert_eq!(Fault::NoLockYet.label(), Fault::NoLock.label());
    assert_ne!(Fault::LostLock.label(), Fault::NoLock.label());
}

/// A refusal that spans a stretch of silence is one unbroken refusal —
/// the clock must not restart when a louder row takes the screen.
#[test]
fn a_louder_row_does_not_restart_the_refusal_clock() {
    let mut st = FaultState::default();
    // The estimator keeps attempting through the quiet stretch, so the
    // attempt count advances alongside the clock — both anchors are
    // being tested for continuity, not just the clock.
    let refusing = |n: u32| {
        frame_in(
            FaultFrame {
                delay_locked: Some(false),
                delay_attempts: n,
                ..driving()
            },
            &LIVE_COH,
        )
    };
    assert_eq!(st.update(&refusing(1), 0.0), Some(Fault::NoLockYet));
    // Reference leg drops out, then comes back.
    let dead_ref = |n: u32| FaultInput {
        ref_peak_dbfs: None,
        frame: Some(FaultFrame {
            delay_locked: Some(false),
            delay_attempts: n,
            ..driving()
        }),
        ..healthy(&LIVE_COH)
    };
    assert_eq!(st.update(&dead_ref(3), 2.0), Some(Fault::NoReference));
    assert_eq!(st.update(&dead_ref(6), 5.0), Some(Fault::NoReference));
    // Back on the original clock and the original attempt anchor, not
    // fresh ones.
    assert_eq!(st.update(&refusing(11), 10.0), Some(Fault::NoLock));
}

#[test]
fn a_successful_lock_shows_a_transient_confirmation() {
    let mut st = FaultState::default();
    let refusing = frame_in(refusing_frame(), &LIVE_COH);
    assert_eq!(st.update(&refusing, 0.0), Some(Fault::NoLockYet));
    assert_eq!(
        st.update(&healthy(&LIVE_COH), 1.0),
        Some(Fault::LockAcquired)
    );
    assert_eq!(
        st.update(&healthy(&LIVE_COH), 1.0 + LOCK_ACQUIRED_HOLD_S - 0.1),
        Some(Fault::LockAcquired)
    );
    assert_eq!(
        st.update(&healthy(&LIVE_COH), 1.0 + LOCK_ACQUIRED_HOLD_S),
        None
    );
    assert_eq!(Fault::LockAcquired.severity(), Severity::Confirmation);
    assert_eq!(Fault::NoLock.severity(), Severity::Fault);
}

/// A session that locks on its first settled frame never refused, so
/// there is nothing to confirm.
#[test]
fn locking_without_a_prior_refusal_is_not_an_acquisition() {
    let mut st = FaultState::default();
    assert_eq!(st.update(&healthy(&LIVE_COH), 0.0), None);
    assert_eq!(st.update(&healthy(&LIVE_COH), 1.0), None);
}

#[test]
fn legs_carrying_unrelated_sources_is_check_routing() {
    // Every column below the display's mask — nothing is drawable.
    let mut st = FaultState::default();
    assert_eq!(
        st.update(&healthy(&DEAD_COH), 0.0),
        Some(Fault::CheckRouting)
    );
}

/// The rig's measured bad-lock shape: stage 0 collapsed, stage 2 intact.
/// That is a delay fault, not a routing one, and the coherence row must
/// not claim it. This is also why the row cannot be the lock
/// discriminator it was originally written as.
#[test]
fn a_bad_lock_shape_is_not_check_routing() {
    let coh = [0.054, 0.77, 0.93];
    let mut st = FaultState::default();
    assert_eq!(st.update(&healthy(&coh), 0.0), None);
}

/// With #227 present, unrelated sources make the estimator refuse, and
/// the flag is the more direct statement of the same fact.
#[test]
fn a_refusal_outranks_the_coherence_row() {
    let mut st = FaultState::default();
    let inp = frame_in(refusing_frame(), &DEAD_COH);
    assert_eq!(st.update(&inp, 0.0), Some(Fault::NoLockYet));
}

/// `CHECK ROUTING` is a post-lock state, and #238 does not change that:
/// a refusing pair has no ladder, so it has no coherence columns to
/// evaluate, and the Welch array on the frame is a different measurement
/// with a different bias floor — reading the threshold against it would
/// be a different test under the same name. The operator still gets
/// routing in `NO LOCK`'s instruction.
#[test]
fn a_refusing_session_reaches_routing_through_no_lock() {
    let mut st = FaultState::default();
    let no_ladder = FaultFrame {
        delay_locked: Some(false),
        settled: false,
        delay_attempts: 1,
        ..driving()
    };
    let refusing = FaultInput {
        frame: Some(no_ladder),
        // No ladder, so no columns, whatever the legs are carrying.
        coherence: &[],
        ..healthy(&[])
    };
    assert_eq!(st.update(&refusing, 0.0), Some(Fault::NoLockYet));
    assert_eq!(refuse_to_threshold(no_ladder), Some(Fault::NoLock));
    assert!(Fault::NoLock
        .detail()
        .is_some_and(|d| d.contains("routing")));
}

/// Before #227 lands, `delay_locked` is absent. Every other row still
/// works; no lock state may paint from absence.
#[test]
fn a_daemon_without_delay_locked_reports_no_lock_state() {
    let mut st = FaultState::default();
    let inp = frame_in(
        FaultFrame {
            delay_locked: None,
            ..driving()
        },
        &DEAD_COH,
    );
    // The coherence row still fires...
    assert_eq!(st.update(&inp, 0.0), Some(Fault::CheckRouting));
    // ...and no length of time turns absence into a refusal.
    let inp = frame_in(
        FaultFrame {
            delay_locked: None,
            ..driving()
        },
        &LIVE_COH,
    );
    assert_eq!(st.update(&inp, 100.0), None);
}

/// A malformed frame must not fabricate a fault, the same way a
/// malformed frame draws no trace rather than a guessed one.
#[test]
fn a_nan_peak_is_not_read_as_silence() {
    let mut st = FaultState::default();
    let inp = FaultInput {
        meas_peak_dbfs: Some(f64::NAN),
        ..healthy(&LIVE_COH)
    };
    assert_eq!(st.update(&inp, 0.0), None);
}

#[test]
fn negative_infinity_is_at_the_floor() {
    assert!(at_floor(Some(f64::NEG_INFINITY)));
    assert!(at_floor(None));
    assert!(!at_floor(Some(-79.999)));
}

/// An empty coherence array is an unsettled ladder, not a dead one.
#[test]
fn no_columns_is_not_check_routing() {
    assert!(!coherence_dead(&[]));
}

/// Rig session 3 (`audit/rig-session-3/silence-ceiling.md`): the first
/// attempt clearing each candidate admission threshold, worst case over
/// the two distant positions (A at 3.000 m, B at 3.2 m).
///
/// A **correct** measurement needs this many attempts before it locks, so
/// it is the quantity the refusal threshold has to stay clear of.
const RIG_WORST_ATTEMPT_TO_FIRST_LOCK: &[(f64, u32)] =
    &[(12.0, 3), (14.0, 5), (16.0, 18), (18.0, 23)];

/// The coupling test #247 asks for, and the reason both constants are
/// worth their doc comments.
///
/// `NOISE_FLOOR_PROMINENCE` (ac-core, admission) decides how long a
/// correct distant measurement takes to lock.
/// [`PERSISTENT_REFUSAL_ATTEMPTS`] (this crate, operator advice) decides
/// when the display stops waiting and tells the operator to check the
/// rig. Nothing connects them in code, they sit in different crates, and
/// they are set by different reasoning — so raising admission silently
/// converts a correct measurement into "check mic placement and routing"
/// eight seconds before it locks.
///
/// This is the third defect of that exact shape in this project —
/// `settled` versus the ladder (#238), `MIN_PROMINENCE` versus
/// `DIRECT_PEAK_FRACTION` (#246), and this one. The test is what stops
/// the fourth: it fails if either side moves.
#[test]
fn the_admission_constant_leaves_room_before_the_advice_fires() {
    let admission = ac_core::visualize::transfer::NOISE_FLOOR_PROMINENCE;
    let Some(&(_, worst_attempt)) = RIG_WORST_ATTEMPT_TO_FIRST_LOCK
        .iter()
        .find(|(p, _)| (p - admission).abs() < 1e-9)
    else {
        panic!(
            "admission moved to {admission}, which no rig capture has scored. \
             Score it against audit/rig-session-3/ and add the row: the \
             threshold below is only correct for a measured time-to-lock, \
             and guessing it is what puts 'check mic placement' in front of \
             a working setup."
        );
    };
    assert!(
        worst_attempt < PERSISTENT_REFUSAL_ATTEMPTS,
        "at admission {admission} the worst measured first lock is attempt \
         {worst_attempt}, but the advice fires at attempt \
         {PERSISTENT_REFUSAL_ATTEMPTS} — a correct measurement would be told \
         to move the microphone and then lock anyway. Either constant may \
         have moved; both are the fix."
    );
}

/// The unit is attempts, so slow retries must not escalate on the clock
/// alone. This is the failure #247 names: `RELOCK_RETRY` is the daemon's,
/// this crate cannot see it, and a build that paces retries by ring
/// refill instead of a 1 s timer moves the advice with no edit here.
///
/// At a 4 s retry the threshold in seconds lands after 3 attempts — one
/// less than the estimator needs at 3 m on a bad night, and exactly the
/// case that must not be advised.
#[test]
fn a_slow_retry_does_not_escalate_before_the_estimator_has_had_its_chances() {
    let refusing = FaultFrame {
        delay_locked: Some(false),
        settled: false,
        delay_attempts: 1,
        ..driving()
    };

    // Same wall-clock instant, two retry paces. Only the 1 Hz one has
    // given the estimator ten chances by now.
    assert_eq!(
        refuse_for(
            &mut FaultState::default(),
            refusing,
            4.0,
            PERSISTENT_REFUSAL_S
        ),
        Some(Fault::NoLockYet),
        "escalated on the clock after 3 attempts — the threshold is \
         measured in attempts for exactly this reason"
    );
    assert_eq!(
        refuse_for(
            &mut FaultState::default(),
            refusing,
            DAEMON_RETRY_S,
            PERSISTENT_REFUSAL_S
        ),
        Some(Fault::NoLock),
        "at the daemon's 1 Hz retry the two thresholds coincide, and this \
         is the behaviour that must not change"
    );

    // It does escalate once the attempts arrive, however long that takes.
    assert_eq!(
        refuse_for(&mut FaultState::default(), refusing, 4.0, 4.0 * 9.0),
        Some(Fault::NoLock),
        "ten refused attempts is persistent whatever the pacing"
    );
}

/// And the other direction: a fast retry must not race past the seconds
/// threshold. Ten attempts in a second is not ten seconds of a fault an
/// operator has had a chance to see, and `NO LOCK` carries an
/// instruction — it must not flash up on a transient.
#[test]
fn a_fast_retry_does_not_escalate_before_the_operator_has_seen_it() {
    let refusing = FaultFrame {
        delay_locked: Some(false),
        settled: false,
        delay_attempts: 1,
        ..driving()
    };
    assert_eq!(
        refuse_for(&mut FaultState::default(), refusing, 0.1, 1.5),
        Some(Fault::NoLockYet),
        "16 attempts in 1.5 s escalated — the later of the two thresholds \
         wins, and the clock has not reached it"
    );
}

/// A producer that reports no attempt count leaves the attempts side
/// unobservable, and an unobservable condition must not read as unmet:
/// that would make `NO LOCK` unreachable for exactly the daemons whose
/// refusals #238 exists to surface. Such a frame falls back to the clock.
///
/// It reaches the refusal at all only through `settled`, which is the
/// locked-then-lost path (#226).
#[test]
fn a_producer_without_an_attempt_count_still_escalates_on_the_clock() {
    let mut st = FaultState::default();
    // Locked first, so the refusal below is a *lost* lock — the only way
    // a pair reporting no attempts reaches the refusal rows at all.
    assert_eq!(st.update(&healthy(&LIVE_COH), 0.0), None);
    let lost = frame_in(settled_refusal_without_attempts(), &LIVE_COH);
    assert_eq!(st.update(&lost, 1.0), Some(Fault::LostLock));
    assert_eq!(
        st.update(&lost, 1.0 + PERSISTENT_REFUSAL_S),
        Some(Fault::NoLock),
        "an unreported attempt count must fall back to the clock, not \
         suppress the escalation forever"
    );
}

/// The decision that `CHECK ROUTING` stays post-lock, pinned in code
/// rather than in a PR body. A frame with no ladder contributes no
/// coherence columns even when it carries a full Welch array — the
/// threshold was measured against the ladder's columns, and quietly
/// feeding it the other array would keep the name while changing the
/// test.
#[test]
fn no_welch_fallback_fills_the_coherence_columns() {
    let json = r#"{
        "type": "transfer_stream",
        "delay_locked": false,
        "delay_attempts": 3,
        "meas_peak_dbfs": -30.0,
        "ref_peak_dbfs": -14.5,
        "meas_channel": 0,
        "ref_channel": 1,
        "sr": 48000,
        "coherence": [0.02, 0.03, 0.01, 0.04],
        "spec_freqs": [],
        "meas_spectrum": [],
        "ref_spectrum": [],
        "spl": null,
        "spl_weighting": "Z",
        "spl_integration": "fast",
        "drive": {"on": true, "level_dbfs": -30.0, "drivable": true}
    }"#;
    let frame: WireFrame = serde_json::from_str(json).expect("deserialize");
    assert_eq!(frame.coherence.len(), 4, "the Welch array is on the frame");
    let inp = FaultInput::from_wire_frame(&frame);
    assert!(
        inp.coherence.is_empty(),
        "the Welch array reached the indicator: {:?}",
        inp.coherence
    );

    // And the refusal is what the operator gets, not CHECK ROUTING.
    let mut st = FaultState::default();
    assert_eq!(st.update(&inp, 0.0), Some(Fault::NoLockYet));
    assert_eq!(
        refuse_to_threshold(inp.frame.expect("the frame carries drive state")),
        Some(Fault::NoLock)
    );
}

/// #226 adds re-locking. If a re-lock ever reset `delay_attempts`, a
/// pair that locked and then started refusing would read as one that
/// has not been asked yet — and warmup paints nothing, which is the
/// blank window this issue removed.
#[test]
fn a_relocking_pair_never_falls_back_to_warmup() {
    let mut st = FaultState::default();
    assert_eq!(st.update(&healthy(&LIVE_COH), 0.0), None);
    // The wire contract says the count is monotone, so a pair that has
    // locked can only report more attempts, never zero.
    let refusing_after_lock = frame_in(
        FaultFrame {
            delay_locked: Some(false),
            settled: false,
            delay_attempts: 1,
            ..driving()
        },
        &[],
    );
    assert_eq!(st.update(&refusing_after_lock, 1.0), Some(Fault::LostLock));
}

/// The measured case the strict "not one column" rule could not fire on.
///
/// Rig session 2 pointed the two legs at genuinely unrelated sources and
/// 22 of 504 columns still cleared the mask — max 0.844, at 37-71 Hz,
/// where a room and a shared noise floor correlate anything. `CHECK
/// ROUTING` stayed dark for the whole session, on the exact condition it
/// names.
#[test]
fn a_few_low_frequency_columns_do_not_keep_check_routing_dark() {
    // The observed shape: 22 low bins over the mask, the strongest 0.844.
    let mut coh = vec![0.05_f64; 504];
    for c in coh.iter_mut().take(22) {
        *c = 0.6;
    }
    coh[0] = 0.844;
    assert!(
        coherence_dead(&coh),
        "22/504 columns over the mask is still a display drawing nothing"
    );

    // And the other side of the line: a healthy acoustic measurement
    // clears the mask nearly everywhere and must never be called dead.
    let healthy_coh = vec![0.92_f64; 504];
    assert!(!coherence_dead(&healthy_coh));

    // A measurement alive only in part of the band is a measurement.
    let mut partial = vec![0.05_f64; 504];
    for c in partial.iter_mut().take(200) {
        *c = 0.80;
    }
    assert!(!coherence_dead(&partial));
}

/// A whole frame through deserialisation, `FaultInput`, and the clock.
///
/// The `mtw`-present-and-refusing shape is a composite, not a capture:
/// today's daemon builds the ladder from a lock it then caches, so it
/// never emits both together. It is here to exercise the full read path
/// in one test, and it no longer stands in for the `settled` half of the
/// gate — `delay_attempts` is what carries it, as it would on the wire.
#[test]
fn reads_a_live_frame_end_to_end() {
    let json = r#"{
        "type": "transfer_stream",
        "delay_ms": 5.9,
        "delay_locked": false,
        "delay_attempts": 3,
        "meas_peak_dbfs": -30.0,
        "ref_peak_dbfs": -14.5,
        "meas_channel": 0,
        "ref_channel": 1,
        "sr": 48000,
        "spec_freqs": [],
        "meas_spectrum": [],
        "ref_spectrum": [],
        "spl": null,
        "spl_weighting": "Z",
        "spl_integration": "fast",
        "drive": {"on": true, "level_dbfs": -30.0, "drivable": true},
        "mtw": {
            "freqs": [100.0, 1000.0, 10000.0],
            "magnitude_db": [0.0, 0.0, 0.0],
            "phase_deg": [0.0, 0.0, 0.0],
            "coherence": [0.93, 0.77, 0.054]
        }
    }"#;
    let frame: WireFrame = serde_json::from_str(json).expect("deserialize");
    let inp = FaultInput::from_wire_frame(&frame);
    let f = inp.frame.expect("drive state present");
    assert!(f.settled, "mtw columns present means settled");
    assert_eq!(f.delay_locked, Some(false));
    assert!(f.drive.on);
    assert_eq!(inp.coherence.len(), 3);
    let mut st = FaultState::default();
    assert_eq!(st.update(&inp, 0.0), Some(Fault::NoLockYet));
    assert_eq!(
        refuse_to_threshold(f),
        Some(Fault::NoLock),
        "a refusal standing 10 s and 10 attempts past the first refused \
         attempt is persistent"
    );
}

/// A frame from today's daemon — no `delay_locked`, no `drive` — must
/// leave the indicator silent rather than paint from defaults.
#[test]
fn a_pre_228_frame_leaves_the_indicator_silent() {
    let json = r#"{
        "type": "transfer_stream",
        "delay_ms": 5.9,
        "meas_channel": 0,
        "ref_channel": 1,
        "sr": 48000,
        "spec_freqs": [],
        "meas_spectrum": [],
        "ref_spectrum": [],
        "spl": null,
        "spl_weighting": "Z",
        "spl_integration": "fast"
    }"#;
    let frame: WireFrame = serde_json::from_str(json).expect("deserialize");
    assert!(frame.drive.is_none());
    assert!(frame.delay_locked.is_none());
    let inp = FaultInput::from_wire_frame(&frame);
    assert!(inp.frame.is_none());
    let mut st = FaultState::default();
    assert_eq!(st.update(&inp, 0.0), None);
}
