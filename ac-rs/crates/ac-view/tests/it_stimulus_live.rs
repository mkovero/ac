//! M4c closure evidence (#182, PR #197): the stimulus path against a
//! **real** fake-audio daemon over real ZMQ, at real wall-clock cadence —
//! the daemon-timing face that headless logical-time tests cannot prove.
//!
//! Drives the real `StimulusMachine` and relays its `DriveCmd`s via the
//! real `Session::set_drive`, exactly as the app does, and observes the
//! daemon's response through the published `meas_peak_dbfs` (fake-audio
//! derives capture from its stimulus, so drive-on lifts the peak from the
//! idle tone's ≈ −20 dBFS to the driven ≈ −10 dBFS).
//!
//! Four properties. **Three of them are no longer uniquely this test's** —
//! `ac-daemon/tests/it_set_drive.rs` covers the dead-man against a real daemon
//! over real ZMQ, in the default suite, across five tests. The claim below
//! used to read "none observable headless" and was true when written; it is
//! kept accurate here rather than deleted, because which property this test is
//! *for* decides whether it is worth a trip to the box.
//!
//! 1. fire → the drive actually reaches the daemon over ZMQ
//!    (also `it_set_drive`);
//! 2. **the residue, and the reason this test still exists**: `ac-view`'s own
//!    250 ms keepalive, driven by the real `StimulusMachine`, holds the drive
//!    up across seconds without ever gapping past the 1.5 s dead-man **under
//!    real scheduling**. That is a claim about the *client's* timing, and
//!    `it_set_drive` exercises the daemon's side of the contract with a test
//!    harness's timing, not the app's;
//! 3. panic-stop latency through real REQ/REP is bounded;
//! 4. the dead-man is a real backstop — a frozen client (no keepalive) has its
//!    drive dropped within ~1.5 s (also `it_set_drive`,
//!    `dead_man_drops_drive_after_keepalive_silence_but_keeps_the_session`).
//!
//! `#[ignore]`: needs a real daemon; run on the box (192.168.9.25):
//! `cargo test -p ac-view --test it_stimulus_live -- --ignored --nocapture`

#[path = "support.rs"]
mod support;

use std::time::{Duration, Instant};

use ac_core::visualize::weighting_curves::WeightingCurve;
use ac_view::session::{PolledFrame, Session};
use ac_view::stimulus::StimulusMachine;
use ac_view::zmq_client::{Client, Endpoint};
use support::DaemonProcess;

const CEILING: f64 = -10.0;
const IDLE_DBFS: f64 = -20.0; // fake idle tone (0.1 amplitude)

fn endpoint(d: &DaemonProcess) -> Endpoint {
    Endpoint {
        host: "127.0.0.1".to_string(),
        ctrl_port: d.ctrl_port,
        data_port: d.data_port,
    }
}

/// Next published meas peak, draining to the freshest frame within
/// `timeout`. `None` on a null/absent peak or timeout.
fn next_peak(session: &mut Session, timeout: Duration) -> Option<f64> {
    let deadline = Instant::now() + timeout;
    let mut latest = None;
    while Instant::now() < deadline {
        match session.poll_frame(Duration::from_millis(80)) {
            Some(PolledFrame::Transfer(f)) => {
                if let Some(p) = f.get("meas_peak_dbfs").and_then(|v| v.as_f64()) {
                    latest = Some(p);
                }
            }
            // The IR sidecar carries no peak — not a gap in the stream,
            // just not the frame this helper reads.
            Some(PolledFrame::Ir(_)) => {}
            None => {
                if latest.is_some() {
                    break;
                }
            }
        }
    }
    latest
}

/// Wait until the peak crosses a threshold in the given direction, or the
/// deadline. Returns the elapsed time to the crossing (for latency
/// assertions), or `None` if it never crossed.
fn wait_for_peak(
    session: &mut Session,
    want_above: bool,
    threshold: f64,
    within: Duration,
) -> Option<Duration> {
    let start = Instant::now();
    while start.elapsed() < within {
        if let Some(p) = next_peak(session, Duration::from_millis(300)) {
            let crossed = if want_above {
                p > threshold
            } else {
                p < threshold
            };
            if crossed {
                return Some(start.elapsed());
            }
        }
    }
    None
}

#[test]
#[ignore = "real daemon; run on 192.168.9.25 — M4c daemon-timing closure"]
fn stimulus_arm_fire_keepalive_panic_and_deadman_over_real_zmq() {
    let daemon = DaemonProcess::spawn();
    let mut session = Session::new(Client::connect(&endpoint(&daemon)).expect("connect"));
    session
        .launch(0, 1, WeightingCurve::A, "fast")
        .expect("launch transfer_stream");

    // Settle to the idle level.
    let idle = next_peak(&mut session, Duration::from_secs(3)).expect("no idle frame");
    assert!(
        (idle - IDLE_DBFS).abs() < 3.0,
        "idle peak {idle} not near {IDLE_DBFS}"
    );

    let mut m = StimulusMachine::new(CEILING, CEILING);
    let relay = |session: &Session, cmd| {
        if let Some(c) = cmd {
            let c: ac_view::stimulus::DriveCmd = c;
            session.set_drive(c.on, c.level_dbfs);
        }
    };

    // --- 1. arm → fire: the drive reaches the daemon over ZMQ ---
    let t = Instant::now();
    m.press_space(t);
    relay(&session, m.press_enter(t)); // fire → set_drive on
    let up = wait_for_peak(&mut session, true, IDLE_DBFS + 5.0, Duration::from_secs(3));
    assert!(
        up.is_some(),
        "fire did not drive the daemon (peak never rose)"
    );

    // --- 2. keepalive holds the drive up across seconds, real cadence,
    //        never gapping past the 1.5 s dead-man ---
    let hold_until = Instant::now() + Duration::from_secs(3);
    let mut samples = 0;
    while Instant::now() < hold_until {
        relay(&session, m.tick(Instant::now())); // 250 ms keepalive
        if let Some(p) = next_peak(&mut session, Duration::from_millis(200)) {
            assert!(
                p > IDLE_DBFS + 5.0,
                "drive dropped mid-keepalive (peak {p}) — cadence gapped past the dead-man"
            );
            samples += 1;
        }
    }
    assert!(samples >= 3, "too few keepalive-window samples ({samples})");

    // --- 3. panic (Esc) → stop, bounded latency over real REQ/REP ---
    relay(&session, m.press_esc(Instant::now())); // set_drive off
    let stop_latency = wait_for_peak(&mut session, false, IDLE_DBFS + 3.0, Duration::from_secs(2))
        .expect("panic did not stop the drive");
    assert!(
        stop_latency < Duration::from_secs(1),
        "panic-stop latency {stop_latency:?} exceeded 1 s over ZMQ"
    );

    // --- 4. dead-man backstop: a frozen client (no keepalive) has its
    //        drive dropped by the daemon within ~1.5 s ---
    let t = Instant::now();
    m.press_space(t);
    relay(&session, m.press_enter(t)); // drive on again
    assert!(
        wait_for_peak(&mut session, true, IDLE_DBFS + 5.0, Duration::from_secs(3)).is_some(),
        "re-fire did not drive"
    );
    // Now FREEZE: send no keepalives at all. The daemon's dead-man must
    // drop the drive on its own within ~1.5 s (+ margin).
    let dropped = wait_for_peak(&mut session, false, IDLE_DBFS + 3.0, Duration::from_secs(3));
    assert!(
        dropped.is_some(),
        "dead-man did not drop a frozen drive — the UI's absence must not keep it alive"
    );
    let d = dropped.unwrap();
    assert!(
        d >= Duration::from_millis(1_200) && d < Duration::from_millis(2_500),
        "dead-man drop at {d:?} — expected ≈1.5 s"
    );

    session.stop();
}
