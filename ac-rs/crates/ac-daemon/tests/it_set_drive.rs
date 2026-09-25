//! ZMQ integration tests for `set_drive` and the §4.2 raw input peaks
//! (#183, M4d-daemon).
//!
//! # Why published peaks are the drive observable
//!
//! The fake backend's capture is derived from its own generator
//! (`audio/fake/stimulus.rs::Synth::block`), and since #204 the generator
//! reaches the capture only while the session has an output port open. A
//! published `meas_peak_dbfs` at the quieter `DRIVE_DBFS` a `set_drive`
//! applies is therefore evidence of two things at once: the worker acted
//! on the drive state, **and** the session drives into a connected output.
//! A drive into nothing — the #203 class — captures zeros and publishes no
//! peak, so every drive test below goes red on it. The drive tests launch
//! `drivable` sessions for that reason: a passive session opens no outputs.
//!
//! An idle generator is heard whatever the routing: the fake's default
//! 0.1-amplitude tone (≈ −20 dBFS), which `set_silence` restores rather
//! than true digital silence. That is why the assertions below are
//! "returned to the idle level", not "went to −inf".
//!
//! # Why the driving level is quieter than idle, not louder (#459)
//!
//! Before #459, `set_drive` clamped a request to a *settable*
//! `drive_max_dbfs` ceiling that defaulted to −10 dBFS — well above the
//! fake backend's ≈ −20 dBFS idle tone, so "drive on" could be
//! demonstrated as a clearly LOUDER peak than idle. The fixtures below
//! retain the quieter −35 dBFS drive chosen by PR #463: revision 3
//! explicitly keeps existing fixture levels, and a peak below the idle
//! tone demonstrates the same dead-man and state-transition mechanics.

use std::fs;
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};

#[path = "common/mod.rs"]
mod common;

use common::{Client, Daemon};

/// The fixed emission maximum (`ac_core::shared::emission_level`), not a
/// config value — nothing above this is ever accepted, by any client.
const MAX_DBFS: f64 = ac_core::shared::emission_level::MAX_EMISSION_DBFS;
/// Idle fake stimulus is a 0.1-amplitude tone ⇒ 20·log10(0.1) ≈ −20 dBFS.
const IDLE_PEAK_DBFS: f64 = -20.0;
/// A driving level clearly below both `MAX_DBFS` and `IDLE_PEAK_DBFS`,
/// used wherever a test needs a peak distinguishable from idle — see the
/// module doc for why "distinguishable" now means quieter, not louder.
const DRIVE_DBFS: f64 = -35.0;

fn start_transfer(c: &Client) -> Value {
    c.call(json!({
        "cmd": "transfer_stream", "meas_channel": 0, "ref_channel": 1,
        "weighting": "Z", "integration": "fast",
    }))
}

/// A session that opens and connects its output ports at launch while
/// staying silent until `set_drive` — the session shape a drive test needs,
/// since only a connected output reaches the fake's capture (#204).
fn start_drivable_transfer(c: &Client) -> Value {
    c.call(json!({
        "cmd": "transfer_stream", "meas_channel": 0, "ref_channel": 1,
        "weighting": "Z", "integration": "fast", "drivable": true,
    }))
}

fn peak(frame: &Value, key: &str) -> Option<f64> {
    match &frame[key] {
        Value::Null => None,
        v => Some(
            v.as_f64()
                .unwrap_or_else(|| panic!("{key} not a number: {v}")),
        ),
    }
}

// ---------------------------------------------------------------------
// Preconditions and schema
// ---------------------------------------------------------------------

#[test]
fn set_drive_without_a_session_is_refused_not_a_panic() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let r = c.call(json!({"cmd": "set_drive", "on": true, "level_dbfs": -20.0}));
    assert_eq!(r["ok"], json!(false), "{r}");
    assert_eq!(r["error"], json!("no transfer_stream session running"));

    // The daemon is still alive and answering afterwards.
    assert_eq!(c.call(json!({"cmd": "status"}))["ok"], json!(true));
}

#[test]
fn set_drive_requires_on_and_a_finite_level() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    assert_eq!(start_transfer(&c)["ok"], json!(true));

    // Missing `on`.
    let r = c.call(json!({"cmd": "set_drive", "level_dbfs": -20.0}));
    assert_eq!(r["ok"], json!(false), "{r}");

    // Missing level — required even though `on` is present, because
    // every message is a full state assertion.
    let r = c.call(json!({"cmd": "set_drive", "on": true}));
    assert_eq!(r["ok"], json!(false), "{r}");

    // Missing level on an OFF request is equally refused.
    let r = c.call(json!({"cmd": "set_drive", "on": false}));
    assert_eq!(r["ok"], json!(false), "{r}");

    // Non-numeric level is a client bug, not something to coerce.
    let r = c.call(json!({"cmd": "set_drive", "on": true, "level_dbfs": "loud"}));
    assert_eq!(r["ok"], json!(false), "{r}");
}

/// #459: a level above the fixed maximum is refused, never clamped — the
/// echo is always exactly what was requested, because a refused request
/// changes nothing. `on: false` is the one exception: it is never
/// checked, so a client silencing a session is never the one request
/// that could itself be rejected.
#[test]
fn set_drive_on_refuses_above_the_maximum_and_passes_through_at_or_below_it() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    assert_eq!(start_transfer(&c)["ok"], json!(true));

    // Above the maximum: refused, and `max_dbfs` rides the refusal.
    let r = c.call(json!({"cmd": "set_drive", "on": true, "level_dbfs": MAX_DBFS + 6.0}));
    assert_eq!(r["ok"], json!(false), "{r}");
    assert_eq!(r["max_dbfs"], json!(MAX_DBFS), "{r}");

    // At or below the maximum: passes through unchanged.
    let r = c.call(json!({"cmd": "set_drive", "on": true, "level_dbfs": MAX_DBFS}));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(r["on"], json!(true));
    assert_eq!(r["level_dbfs"], json!(MAX_DBFS));

    let r = c.call(json!({"cmd": "set_drive", "on": true, "level_dbfs": -25.0}));
    assert_eq!(r["level_dbfs"], json!(-25.0), "{r}");

    // `on: false` is never checked — an operator silencing a session must
    // never be the one request a bad level could leave refused, and drive
    // stuck on.
    let r = c.call(json!({"cmd": "set_drive", "on": false, "level_dbfs": MAX_DBFS + 6.0}));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(r["on"], json!(false));
    assert_eq!(r["level_dbfs"], json!(MAX_DBFS + 6.0));
}

// ---------------------------------------------------------------------
// Busy-guard bypass — the safety finding from #180's architect pass
// ---------------------------------------------------------------------

#[test]
fn set_drive_bypasses_the_busy_guard_it_would_otherwise_contend_with() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    assert_eq!(start_transfer(&c)["ok"], json!(true));

    // A second transfer_stream IS refused by the busy guard — this
    // establishes that the guard is actually engaged for this session,
    // so the set_drive results below are a bypass rather than an idle
    // guard that would have let anything through.
    let busy = start_transfer(&c);
    assert_eq!(busy["ok"], json!(false), "guard not engaged: {busy}");

    // set_drive, targeting that same busy worker, succeeds anyway.
    let on = c.call(json!({"cmd": "set_drive", "on": true, "level_dbfs": -20.0}));
    assert_eq!(on["ok"], json!(true), "{on}");

    // And the panic-stop direction — the one that must never block on
    // the worker it is stopping — succeeds too.
    let off = c.call(json!({"cmd": "set_drive", "on": false, "level_dbfs": -20.0}));
    assert_eq!(off["ok"], json!(true), "{off}");
}

// ---------------------------------------------------------------------
// §4.2 raw input peaks
// ---------------------------------------------------------------------

#[test]
fn frames_carry_raw_input_peaks_for_both_channels() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    assert_eq!(start_transfer(&c)["ok"], json!(true));

    let f = c.frame_after(Duration::from_millis(1_500));

    let m = peak(&f, "meas_peak_dbfs").expect("meas peak present");
    let r = peak(&f, "ref_peak_dbfs").expect("ref peak present");

    // Idle fake stimulus is a 0.1-amplitude tone on both channels.
    for (name, v) in [("meas", m), ("ref", r)] {
        assert!(
            (v - IDLE_PEAK_DBFS).abs() < 1.0,
            "{name} peak {v} not near the idle {IDLE_PEAK_DBFS} dBFS"
        );
        assert!(v <= 0.0, "{name} peak {v} exceeds full scale");
    }
}

/// Swapped-channel detection — the AC this actually kills. Both channels
/// at the idle level (as above) cannot fail on a meas/ref swap, because
/// the swap produces the same co-located peaks. `fake_correlated_pair`
/// makes the channels asymmetric on purpose: ref is the source, meas is
/// `gain × delayed(source)`. With gain 0.5, meas must sit ≈ 6 dB BELOW
/// ref — and the SIGN of that difference is what a swap flips.
#[test]
fn peaks_distinguish_meas_from_ref_so_a_swap_would_fail() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd": "transfer_stream", "meas_channel": 0, "ref_channel": 1,
        "weighting": "Z", "integration": "fast",
        "fake_correlated_pair": {"gain": 0.5, "delay_samples": 120},
    }));
    assert_eq!(r["ok"], json!(true), "{r}");

    let f = c.frame_after(Duration::from_millis(1_500));
    let m = peak(&f, "meas_peak_dbfs").expect("meas peak");
    let rf = peak(&f, "ref_peak_dbfs").expect("ref peak");

    // gain 0.5 ⇒ meas is ~6 dB quieter than ref. Assert the sign and a
    // generous magnitude band (the uniform source's per-block max is not
    // exactly full scale, so the gap is ~6 dB but not to the decimal).
    assert!(
        rf - m > 3.0,
        "ref should be clearly louder than meas (gain 0.5): ref {rf}, meas {m} — \
         a meas/ref swap would invert this"
    );
    assert!(
        rf - m < 9.0,
        "ref−meas {} dB is not the expected ~6 dB for gain 0.5: ref {rf}, meas {m}",
        rf - m
    );
}

/// The peaks must come from the raw capture blocks, before any
/// calibration or aggregation. A voltage-calibrated session moves
/// `meas_spectrum` (and the H1 magnitude) but must leave the meter's
/// peak exactly where an uncalibrated session put it — meters exist to
/// judge gain staging, and a calibrated value hides clipping.
#[test]
fn peaks_are_raw_and_do_not_follow_a_voltage_calibration() {
    fn meas_peak_with_cal(cal_json: Option<&str>) -> f64 {
        let d = Daemon::spawn();
        if let Some(body) = cal_json {
            // Write cal.json directly rather than driving the `calibrate`
            // state machine: that command spawns an Exclusive worker, and
            // the busy guard would then refuse the transfer session this
            // test needs. The daemon reads this file at session start
            // (`Calibration::load` in the transfer handler).
            let dir = d.home.join(".config").join("ac");
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("cal.json"), body).unwrap();
        }
        let c = Client::new(&d);
        assert_eq!(start_transfer(&c)["ok"], json!(true));
        let f = c.frame_after(Duration::from_millis(1_500));
        // A calibrated session must still be a calibrated session — if
        // the fixture silently failed to apply, this test would compare
        // two identical uncalibrated runs and pass for the wrong reason.
        if cal_json.is_some() {
            assert_eq!(
                f["cal_tags"]["meas"]["voltage"],
                json!("on"),
                "voltage calibration did not apply — test would be vacuous"
            );
        }
        peak(&f, "meas_peak_dbfs").expect("meas peak")
    }

    let uncal = meas_peak_with_cal(None);
    // 10 Vrms at 0 dBFS — a 20 dB-scale change to every calibrated
    // value in the frame.
    let cal = meas_peak_with_cal(Some(
        r#"{"out0_in0": {"output_channel": 0, "input_channel": 0, "vrms_at_0dbfs_in": 10.0}}"#,
    ));

    assert!(
        (uncal - cal).abs() < 0.5,
        "peak moved with calibration: uncal {uncal}, cal {cal} — \
         peaks must be taken from raw capture blocks, before calibration"
    );
}

// ---------------------------------------------------------------------
// Drive lifecycle, live level change, dead-man
// ---------------------------------------------------------------------

/// The #203-class guard: a drivable session whose worker opened no output
/// port captures zeros under the fake (#204), so "driving peak" is absent
/// and this fails. Proven by making `drive_out_ports` return no ports.
#[test]
fn drive_on_lowers_the_captured_level_and_off_returns_it_within_one_frame() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    assert_eq!(start_drivable_transfer(&c)["ok"], json!(true));
    let _ = c.frame_after(Duration::from_millis(1_200));

    // On at `DRIVE_DBFS`: amplitude 10^(DRIVE_DBFS/20) ⇒ a peak well
    // below the idle tone (see the module doc for why quieter, not
    // louder, demonstrates the same mechanics under #459's maximum).
    assert_eq!(
        c.call(json!({"cmd": "set_drive", "on": true, "level_dbfs": DRIVE_DBFS}))["ok"],
        json!(true)
    );
    let driving =
        peak(&c.frame_after(Duration::from_millis(900)), "meas_peak_dbfs").expect("driving peak");
    assert!(
        (driving - DRIVE_DBFS).abs() < 2.0,
        "driving peak {driving} not near {DRIVE_DBFS} dBFS"
    );

    // Off: back to the idle stimulus level.
    assert_eq!(
        c.call(json!({"cmd": "set_drive", "on": false, "level_dbfs": DRIVE_DBFS}))["ok"],
        json!(true)
    );
    let stopped =
        peak(&c.frame_after(Duration::from_millis(900)), "meas_peak_dbfs").expect("stopped peak");
    assert!(
        (stopped - IDLE_PEAK_DBFS).abs() < 1.5,
        "peak {stopped} did not return to idle {IDLE_PEAK_DBFS} after set_drive off"
    );
}

#[test]
fn level_changes_take_effect_without_restarting_the_session() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    assert_eq!(start_drivable_transfer(&c)["ok"], json!(true));
    let _ = c.frame_after(Duration::from_millis(1_200));

    c.call(json!({"cmd": "set_drive", "on": true, "level_dbfs": -50.0}));
    let quiet =
        peak(&c.frame_after(Duration::from_millis(900)), "meas_peak_dbfs").expect("quiet peak");

    // Keep driving, just louder — no stop, no session restart. Still at
    // or below the maximum.
    c.call(json!({"cmd": "set_drive", "on": true, "level_dbfs": MAX_DBFS}));
    let loud =
        peak(&c.frame_after(Duration::from_millis(900)), "meas_peak_dbfs").expect("loud peak");

    assert!(
        loud - quiet > 10.0,
        "level change did not follow: {quiet} → {loud} dBFS"
    );
}

/// Dead-man: drive drops after 1.5 s of keepalive silence, and the
/// SESSION KEEPS RUNNING. Both halves matter — a dead-man that killed
/// the session would lose the operator's measurement along with the
/// stimulus.
#[test]
fn dead_man_drops_drive_after_keepalive_silence_but_keeps_the_session() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    assert_eq!(start_drivable_transfer(&c)["ok"], json!(true));
    let _ = c.frame_after(Duration::from_millis(1_200));

    assert_eq!(
        c.call(json!({"cmd": "set_drive", "on": true, "level_dbfs": DRIVE_DBFS}))["ok"],
        json!(true)
    );
    let driving =
        peak(&c.frame_after(Duration::from_millis(900)), "meas_peak_dbfs").expect("driving peak");
    assert!((driving - DRIVE_DBFS).abs() < 2.0, "not driving: {driving}");

    // No CTRL traffic at all for 1.6 s — the timeout must be evaluated
    // on the worker's own poll, not on message arrival.
    thread::sleep(Duration::from_millis(1_600));

    let after = c.frame_after(Duration::from_millis(600));
    let dropped = peak(&after, "meas_peak_dbfs").expect("post-dead-man peak");
    assert!(
        (dropped - IDLE_PEAK_DBFS).abs() < 1.5,
        "drive did not drop after keepalive silence: {dropped} dBFS"
    );

    // Session still publishing, and still answering CTRL.
    assert!(
        after["type"] == json!("transfer_stream"),
        "session stopped publishing"
    );
    assert_eq!(c.call(json!({"cmd": "status"}))["ok"], json!(true));
}

/// Dead-man LOWER bound: drive must still be up well before the 1.5 s
/// window. A timeout accidentally set too short (e.g. 1000 ms) would
/// pass every other test in this file but fail here.
#[test]
fn drive_survives_up_to_the_dead_man_window() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    assert_eq!(start_drivable_transfer(&c)["ok"], json!(true));
    let _ = c.frame_after(Duration::from_millis(1_200));

    assert_eq!(
        c.call(json!({"cmd": "set_drive", "on": true, "level_dbfs": DRIVE_DBFS}))["ok"],
        json!(true)
    );

    // 1.2 s of silence — inside the 1.5 s window, so drive must persist.
    thread::sleep(Duration::from_millis(1_200));

    let still = peak(&c.frame_after(Duration::from_millis(200)), "meas_peak_dbfs")
        .expect("peak before the window");
    assert!(
        (still - DRIVE_DBFS).abs() < 2.0,
        "drive dropped early ({still} dBFS at 1.2 s) — dead-man window too short"
    );
}

/// An idempotent resend keeps drive alive indefinitely — this is what
/// makes the 250 ms resend a keepalive without a second command.
#[test]
fn idempotent_resends_hold_drive_past_the_dead_man_window() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    assert_eq!(start_drivable_transfer(&c)["ok"], json!(true));
    let _ = c.frame_after(Duration::from_millis(1_200));

    let msg = json!({"cmd": "set_drive", "on": true, "level_dbfs": DRIVE_DBFS});
    c.call(msg.clone());

    // Byte-identical resends across a window well past 1.5 s.
    for _ in 0..10 {
        thread::sleep(Duration::from_millis(250));
        let r = c.call(msg.clone());
        assert_eq!(r["ok"], json!(true), "resend refused: {r}");
        assert_eq!(r["level_dbfs"], json!(DRIVE_DBFS));
    }

    let still = peak(&c.frame_after(Duration::from_millis(300)), "meas_peak_dbfs")
        .expect("peak while resending");
    assert!(
        (still - DRIVE_DBFS).abs() < 2.0,
        "resends did not hold drive: {still} dBFS"
    );
}

/// A session launched with the legacy `drive: true` param sends no
/// keepalives — the dead-man must not silence it, or every scripted
/// caller breaks. The dead-man arms on the first `set_drive`.
#[test]
fn legacy_launch_time_drive_is_not_killed_by_the_dead_man() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd": "transfer_stream", "meas_channel": 0, "ref_channel": 1,
        "weighting": "Z", "integration": "fast",
        "drive": true, "level_dbfs": DRIVE_DBFS,
    }));
    assert_eq!(r["ok"], json!(true), "{r}");

    // Well past the dead-man window, with no CTRL traffic at all.
    thread::sleep(Duration::from_millis(2_000));

    let p = peak(&c.frame_after(Duration::from_millis(600)), "meas_peak_dbfs")
        .expect("peak on a launch-driven session");
    assert!(
        (p - DRIVE_DBFS).abs() < 2.0,
        "legacy launch-time drive was silenced by the dead-man: {p} dBFS"
    );
}

/// #459: `transfer_stream`'s own `level_dbfs` seed, used when launched
/// with legacy `drive: true` (or `drivable: true`), goes through the
/// same refusal chokepoint as `set_drive` — a level above the maximum
/// refuses the whole launch rather than silently landing on a lower one.
/// This needs its own test because
/// `legacy_launch_time_drive_is_not_killed_by_the_dead_man` above
/// requests a level already within range, which cannot distinguish a
/// checked seed from an unchecked one.
#[test]
fn legacy_launch_time_drive_refuses_above_the_maximum() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let requested = MAX_DBFS + 6.0;
    let r = c.call(json!({
        "cmd": "transfer_stream", "meas_channel": 0, "ref_channel": 1,
        "weighting": "Z", "integration": "fast",
        "drive": true, "level_dbfs": requested,
    }));
    assert_eq!(r["ok"], json!(false), "{r}");
    assert_eq!(r["max_dbfs"], json!(MAX_DBFS), "{r}");
}

// ---------------------------------------------------------------------
// Observed drive state on the wire (#228)
//
// The indicator #228 builds needs to know whether the daemon is emitting.
// A client's own last `set_drive` is not that: the dead-man can silence a
// drive the client still believes is up, and a refused request must leave
// the engine's state exactly where it was — neither is visible from the
// client's own request. These assert the published state follows the
// engine, not the request.
// ---------------------------------------------------------------------

fn drive_state(frame: &Value) -> &Value {
    &frame["drive"]
}

/// #459: a refused `set_drive` must leave the engine's state untouched —
/// there is no more "applied (clamped) level" to report, because a level
/// above the maximum never reaches the engine at all. The frame after a
/// refusal must still show whatever was true before it.
#[test]
fn frames_report_the_applied_drive_level_unaffected_by_a_refused_request() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd": "transfer_stream", "meas_channel": 0, "ref_channel": 1,
        "weighting": "Z", "integration": "fast", "drivable": true,
    }));
    assert_eq!(r["ok"], json!(true), "{r}");

    let idle = c.frame_after(Duration::from_millis(900));
    assert_eq!(drive_state(&idle)["on"], json!(false));
    assert_eq!(
        drive_state(&idle)["level_dbfs"],
        Value::Null,
        "level must be null while off, not a stale number to misread"
    );
    assert_eq!(
        drive_state(&idle)["drivable"],
        json!(true),
        "a drivable session that is silent must be distinguishable from one \
         that never drives"
    );

    // Turn drive on for real, within range.
    assert_eq!(
        c.call(json!({"cmd": "set_drive", "on": true, "level_dbfs": DRIVE_DBFS}))["ok"],
        json!(true)
    );
    let driving = c.frame_after(Duration::from_millis(900));
    assert_eq!(drive_state(&driving)["on"], json!(true));
    assert_eq!(
        drive_state(&driving)["level_dbfs"].as_f64(),
        Some(DRIVE_DBFS)
    );

    // Refresh the keepalive after waiting for the observation above. The
    // next frame wait is deliberately long enough for a stable sample, and
    // two such waits would otherwise cross the 1.5 s dead-man boundary.
    assert_eq!(
        c.call(json!({"cmd": "set_drive", "on": true, "level_dbfs": DRIVE_DBFS}))["ok"],
        json!(true)
    );

    // Ask for 6 dB above the maximum — refused. The engine's state (and
    // therefore the published frame) must be exactly what it was before
    // this request: still on, still at DRIVE_DBFS, never the refused
    // number.
    let requested = MAX_DBFS + 6.0;
    let refused = c.call(json!({"cmd": "set_drive", "on": true, "level_dbfs": requested}));
    assert_eq!(refused["ok"], json!(false), "{refused}");

    let after = c.frame_after(Duration::from_millis(900));
    assert_eq!(drive_state(&after)["on"], json!(true));
    assert_eq!(
        drive_state(&after)["level_dbfs"].as_f64(),
        Some(DRIVE_DBFS),
        "a refused set_drive must leave the previously-applied level in place, \
         never report the refused request"
    );
}

#[test]
fn the_published_drive_state_follows_the_dead_man_not_the_last_request() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd": "transfer_stream", "meas_channel": 0, "ref_channel": 1,
        "weighting": "Z", "integration": "fast", "drivable": true,
    }));
    assert_eq!(r["ok"], json!(true), "{r}");
    let _ = c.frame_after(Duration::from_millis(900));

    assert_eq!(
        c.call(json!({"cmd": "set_drive", "on": true, "level_dbfs": DRIVE_DBFS}))["ok"],
        json!(true)
    );
    assert_eq!(
        drive_state(&c.frame_after(Duration::from_millis(900)))["on"],
        json!(true)
    );

    // No keepalive for longer than the dead-man window. The client's last
    // request still says "on"; the engine is silent. The frame must say
    // silent, or an indicator built on it reports a drive that is not there.
    thread::sleep(Duration::from_millis(1_600));
    let after = c.frame_after(Duration::from_millis(600));
    assert_eq!(
        drive_state(&after)["on"],
        json!(false),
        "published drive state still says on after the dead-man expired it"
    );
    assert_eq!(drive_state(&after)["level_dbfs"], Value::Null);
}

#[test]
fn a_passive_session_reports_that_it_cannot_drive() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    assert_eq!(start_transfer(&c)["ok"], json!(true));

    let f = c.frame_after(Duration::from_millis(900));
    assert_eq!(
        drive_state(&f)["drivable"],
        json!(false),
        "an external-DUT session opens no output ports; silence from the \
         daemon says nothing about whether signal is present"
    );
    assert_eq!(drive_state(&f)["on"], json!(false));
}
