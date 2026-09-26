//! Tests for [`super`] — the transfer view's fault indicator (#228).
//!
//! Kept as one `mod tests` via `#[path]`, the same pattern
//! `ac-view/tests/support.rs` is included with, so `super::*` still reaches
//! the private items the state machine is tested through.

use super::*;

fn driving() -> FaultFrame {
    FaultFrame {
        drive: DriveState {
            on: true,
            drivable: true,
        },
        delay_locked: Some(true),
    }
}

/// A driving session with both legs live and a delay held — the healthy
/// baseline every case below perturbs one field of. The peaks are the
/// rig's own, 15 dB apart.
fn healthy(coherence: &[f64]) -> FaultInput<'_> {
    FaultInput {
        frame: Some(driving()),
        meas_peak_dbfs: Some(-30.0),
        ref_peak_dbfs: Some(-14.5),
        coherence,
    }
}

/// The rig's own healthy coherence: stage 0 reverberation-limited at 0.755,
/// the lower rungs well above it.
const LIVE_COH: [f64; 2] = [0.755, 0.92];

/// Every column below the display's mask — two legs carrying unrelated
/// sources, with nothing drawable.
const DEAD_COH: [f64; 3] = [0.1, 0.08, 0.12];

/// A drivable session sitting idle: nothing driven, no delay yet.
fn idle() -> FaultFrame {
    FaultFrame {
        drive: DriveState {
            on: false,
            drivable: true,
        },
        delay_locked: None,
    }
}

/// A fully passive external-DUT session. It never drives, so the level rows
/// have no ground to stand on.
fn passive(delay_locked: Option<bool>) -> FaultFrame {
    FaultFrame {
        drive: DriveState {
            on: false,
            drivable: false,
        },
        delay_locked,
    }
}

/// A pair still warming up: no delay held yet.
fn warming() -> FaultFrame {
    FaultFrame {
        delay_locked: Some(false),
        ..driving()
    }
}

/// One frame against the healthy peaks.
fn frame_in(frame: FaultFrame, coherence: &[f64]) -> FaultInput<'_> {
    FaultInput {
        frame: Some(frame),
        ..healthy(coherence)
    }
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
        ..frame_in(passive(Some(false)), &[])
    };
    assert_eq!(st.update(&inp, 0.0), None);
}

/// Two live legs with no delay yet is warmup, never a fault: the daemon's
/// Find always has a peak on live legs (#669), so nothing is refusing.
#[test]
fn warmup_paints_nothing_however_long_it_takes() {
    let mut st = FaultState::default();
    for t in [0.0, 5.0, 30.0, 300.0] {
        assert_eq!(st.update(&frame_in(warming(), &[]), t), None);
    }
}

/// The daemon finding the delay is confirmed, briefly.
#[test]
fn a_found_delay_shows_a_transient_confirmation() {
    let mut st = FaultState::default();
    assert_eq!(st.update(&frame_in(warming(), &[]), 0.0), None);
    assert_eq!(st.update(&healthy(&LIVE_COH), 1.0), Some(Fault::DelayFound));
    assert_eq!(
        st.update(&healthy(&LIVE_COH), 1.0 + DELAY_FOUND_HOLD_S - 0.1),
        Some(Fault::DelayFound)
    );
    assert_eq!(
        st.update(&healthy(&LIVE_COH), 1.0 + DELAY_FOUND_HOLD_S),
        None
    );
    assert_eq!(Fault::DelayFound.severity(), Severity::Confirmation);
    assert_eq!(Fault::CheckRouting.severity(), Severity::Fault);
}

/// A session first seen already holding a delay has nothing to confirm.
#[test]
fn a_delay_held_from_the_first_frame_is_not_a_find() {
    let mut st = FaultState::default();
    assert_eq!(st.update(&healthy(&LIVE_COH), 0.0), None);
    assert_eq!(st.update(&healthy(&LIVE_COH), 1.0), None);
}

/// A passive session gets the both-legs-live rows: neither reads drive.
#[test]
fn a_non_drivable_session_still_gets_check_routing_and_the_confirmation() {
    let mut st = FaultState::default();
    let inp = frame_in(passive(Some(true)), &DEAD_COH);
    assert_eq!(st.update(&inp, 0.0), Some(Fault::CheckRouting));

    let mut st = FaultState::default();
    let unaligned = frame_in(passive(Some(false)), &[]);
    let found = frame_in(passive(Some(true)), &LIVE_COH);
    assert_eq!(st.update(&unaligned, 0.0), None);
    assert_eq!(st.update(&found, 1.0), Some(Fault::DelayFound));
}

/// A fault outranks the confirmation: unrelated legs get a delay (their IR
/// has a peak) and then a dead ladder, and the operator must read the fault.
#[test]
fn check_routing_outranks_the_confirmation() {
    let mut st = FaultState::default();
    assert_eq!(st.update(&frame_in(warming(), &[]), 0.0), None);
    assert_eq!(
        st.update(&healthy(&DEAD_COH), 1.0),
        Some(Fault::CheckRouting)
    );
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

/// A daemon predating #227 sends no `delay_locked`. Every other row still
/// works, and absence is never a find.
#[test]
fn a_daemon_without_delay_locked_gets_no_confirmation() {
    let mut st = FaultState::default();
    let none = |coh| {
        frame_in(
            FaultFrame {
                delay_locked: None,
                ..driving()
            },
            coh,
        )
    };
    assert_eq!(st.update(&none(&DEAD_COH), 0.0), Some(Fault::CheckRouting));
    assert_eq!(st.update(&none(&LIVE_COH), 1.0), None);
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

/// A frame with no ladder contributes no coherence columns even when it
/// carries a full Welch array — the threshold was measured against the
/// ladder's columns, and quietly feeding it the other array would keep the
/// name while changing the test.
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
    let frame: TransferFrame = serde_json::from_str(json).expect("deserialize");
    assert_eq!(frame.coherence.len(), 4, "the Welch array is on the frame");
    let inp = FaultInput::from_wire_frame(&frame);
    assert!(
        inp.coherence.is_empty(),
        "the Welch array reached the indicator: {:?}",
        inp.coherence
    );
    let mut st = FaultState::default();
    assert_eq!(st.update(&inp, 0.0), None);
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
#[test]
fn reads_a_live_frame_end_to_end() {
    let json = r#"{
        "type": "transfer_stream",
        "delay_ms": 5.9,
        "delay_locked": true,
        "delay_attempts": 1,
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
            "coherence": [0.1, 0.08, 0.054]
        }
    }"#;
    let frame: TransferFrame = serde_json::from_str(json).expect("deserialize");
    let inp = FaultInput::from_wire_frame(&frame);
    let f = inp.frame.expect("drive state present");
    assert_eq!(f.delay_locked, Some(true));
    assert!(f.drive.on);
    assert_eq!(inp.coherence.len(), 3);
    let mut st = FaultState::default();
    assert_eq!(st.update(&inp, 0.0), Some(Fault::CheckRouting));
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
    let frame: TransferFrame = serde_json::from_str(json).expect("deserialize");
    assert!(frame.drive.is_none());
    assert!(frame.delay_locked.is_none());
    let inp = FaultInput::from_wire_frame(&frame);
    assert!(inp.frame.is_none());
    let mut st = FaultState::default();
    assert_eq!(st.update(&inp, 0.0), None);
}
