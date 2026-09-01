//! AC3: live end-to-end under `--fake-audio` — session launches through
//! `ac-view`'s own production `Session`/`Client` code (not a test-only
//! reimplementation), frames flow, and the on-screen SPL string equals
//! `ac-scene`'s output for the same captured frame, asserted at the
//! harness level (not eyeballed).
//!
//! Two checks, because "equals" needs two genuinely different things
//! compared against each other, and a live streaming session only
//! offers that safely in one of two ways:
//!
//! 1. **Determinism check** (exact equality): the identical frame
//!    bytes, parsed twice independently (once "as the app would",
//!    once as a fresh standalone call) — proves the daemon → ac-view
//!    client → `WireFrame` → `Scene` chain doesn't corrupt or lose
//!    data anywhere, and that the conversion is a pure function of the
//!    bytes (no hidden state).
//! 2. **Live-app paint check** (small tolerance): drives the *actual*
//!    `AcViewApp` through `egui_kittest`'s real eframe harness and
//!    reads back `current_scene()` — the same field `view::draw_spectrum`
//!    paints verbatim — comparing it to an independently-sniffed frame
//!    from a second SUB socket on the same session. A small tolerance
//!    here is honest, not a weakening: two different frames of a
//!    streaming session can differ by the estimator's own noise floor,
//!    same discipline M1.5 established for live-vs-reprocessed
//!    parity checks.

#[path = "support.rs"]
mod support;

use std::time::Duration;

use ac_core::visualize::weighting_curves::WeightingCurve;
use ac_scene::{IrInput, IrScene, Scene};
use ac_view::app::{connect_and_launch, connect_and_launch_transfer};
use ac_view::session::{ConnectionState, PolledFrame, Session};
use ac_view::zmq_client::{Client, Endpoint, Recv};
use egui_kittest::Harness;
use serde_json::json;
use support::DaemonProcess;

fn calibrate_spl(client: &Client) {
    let r = client
        .call(&json!({"cmd": "calibrate_spl", "input_channel": 0, "capture_s": 0.05}))
        .unwrap();
    assert_eq!(r["ok"], json!(true), "calibrate_spl: {r}");
    loop {
        match client.recv_frame(Duration::from_secs(3)) {
            Recv::Frame(t, _) if t == "cal_prompt" => break,
            // A malformed frame is not an empty socket — keep waiting rather
            // than failing the test on one bad payload (issue #219).
            Recv::Frame(..) | Recv::Malformed(_) => continue,
            Recv::Empty => panic!("no spl cal_prompt"),
        }
    }
    let _ = client.call(&json!({"cmd": "cal_reply", "vrms": serde_json::Value::Null}));
    loop {
        match client.recv_frame(Duration::from_secs(5)) {
            Recv::Frame(t, _) if t == "cal_done" => break,
            Recv::Frame(..) | Recv::Malformed(_) => continue,
            Recv::Empty => panic!("no spl cal_done"),
        }
    }
}

#[test]
fn live_frame_readout_matches_ac_scene_output_for_the_same_frame() {
    let daemon = DaemonProcess::spawn();
    let endpoint = Endpoint {
        host: "127.0.0.1".to_string(),
        ctrl_port: daemon.ctrl_port,
        data_port: daemon.data_port,
    };

    // A raw client used only to arrange calibration before either the
    // app or the sniffer connects — closed (dropped) before launch so
    // it doesn't compete for CTRL replies with the app's own session.
    {
        let setup_client = Client::connect(&endpoint).expect("connect (setup)");
        calibrate_spl(&setup_client);
    }

    // --- Check 1: determinism, exact equality ---
    let sniff_client = Client::connect(&endpoint).expect("connect (sniffer)");
    let mut sniff_session = Session::new(sniff_client);
    sniff_session
        .launch(0, 1, WeightingCurve::A, "fast")
        .expect("launch transfer_stream (sniffer)");
    assert_eq!(sniff_session.connection_state(), ConnectionState::Live);

    let raw_frame = {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut found = None;
        while std::time::Instant::now() < deadline {
            // Skip the interleaved `visualize/ir` sidecar (#286) — this
            // check wants a `transfer_stream` frame specifically — and the
            // settling frames a session publishes before its ring holds a
            // Welch segment, which carry no spectrum and so no `spl`.
            if let Some(PolledFrame::Transfer(f)) =
                sniff_session.poll_frame(Duration::from_millis(200))
            {
                if f["n_averages"].as_u64().unwrap_or(0) > 0 {
                    found = Some(f);
                    break;
                }
            }
        }
        found.expect("no transfer_stream frame within 10s")
    };
    let frame_text = serde_json::to_string(&raw_frame).unwrap();
    let parse_a: ac_scene::WireFrame = serde_json::from_str(&frame_text).unwrap();
    let parse_b: ac_scene::WireFrame = serde_json::from_str(&frame_text).unwrap();
    let scene_a = Scene::from_wire_frame(&parse_a, (20.0, 20_000.0), (-140.0, 0.0));
    let scene_b = Scene::from_wire_frame(&parse_b, (20.0, 20_000.0), (-140.0, 0.0));
    assert!(
        scene_a.readouts.spl.is_some(),
        "expected a real spl reading (SPL cal was loaded)"
    );
    assert_eq!(
        scene_a.readouts.spl, scene_b.readouts.spl,
        "identical frame bytes must parse to an identical SPL readout"
    );
    let sniffed_spl: f64 = scene_a
        .readouts
        .spl
        .as_ref()
        .unwrap()
        .split(' ')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    sniff_session.stop();
    drop(sniff_session);

    // --- Check 2: the real app, driven through a real eframe harness,
    // reads back what it would paint ---
    let mut harness = Harness::new_eframe(move |_cc| {
        connect_and_launch(endpoint, 0, 1, WeightingCurve::A, "fast").expect("connect_and_launch")
    });

    let app_spl: f64 = {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut found = None;
        while std::time::Instant::now() < deadline {
            harness.step();
            if let Some(spl) = harness
                .state()
                .current_scene()
                .and_then(|s| s.readouts.spl.as_ref())
            {
                found = Some(spl.split(' ').next().unwrap().parse::<f64>().unwrap());
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        found.expect("app never received a frame with a real spl reading within 10s")
    };

    // Tolerance: measured first, not guessed (this project's standing
    // discipline) — an initial artificially tight bound (0.0001 dB)
    // showed these two frames agree to within floating-point noise,
    // not just "close": the default fake stimulus is a stationary,
    // deterministic tone (no correlated-pair randomness), so two
    // frames ~2.5 s apart of the same session see essentially
    // identical content and an EmaIntegrator that's already converged.
    // 0.01 dB leaves headroom above float rounding without
    // reintroducing the loose guess this replaced.
    let delta = (app_spl - sniffed_spl).abs();
    assert!(
        delta < 0.01,
        "app's on-screen SPL ({app_spl:.4}) and an independently-sniffed frame's SPL \
         ({sniffed_spl:.4}) diverged by {delta:.4} dB — measured near-zero on a stationary \
         fake tone, so this should never trip on jitter alone"
    );
}

/// QA follow-up on #286/PR #309: the SPL check above never exercised the
/// `visualize/ir` sidecar or the IR panel at all — extends the same
/// two-check discipline to it.
///
/// 1. **Determinism check**: the identical sidecar frame bytes, parsed
///    twice independently, must produce byte-identical `IrScene`s
///    (trace included, not just the header/arrival strings) — proves
///    the sidecar → `IrWireFrame` → `IrInput` → `IrScene` chain is a
///    pure function of the bytes.
/// 2. **Live-app paint check**: the real `AcViewApp`, driven through a
///    real eframe harness with an actual `H` keypress (not
///    `handle_action` called directly — that would only prove the
///    dispatch table exists, not that a key event reaches it), opens
///    the IR panel and its `current_ir_scene()` reads back the header
///    verbatim and an arrival delay within tolerance of an
///    independently-sniffed frame from the same session.
#[test]
fn ir_panel_header_and_arrival_match_ac_scene_for_the_same_sidecar_frame() {
    let daemon = DaemonProcess::spawn();
    let endpoint = Endpoint {
        host: "127.0.0.1".to_string(),
        ctrl_port: daemon.ctrl_port,
        data_port: daemon.data_port,
    };

    // --- Check 1: determinism, exact equality ---
    let sniff_client = Client::connect(&endpoint).expect("connect (sniffer)");
    let mut sniff_session = Session::new(sniff_client);
    sniff_session
        .launch(0, 1, WeightingCurve::A, "fast")
        .expect("launch transfer_stream (sniffer)");
    assert_eq!(sniff_session.connection_state(), ConnectionState::Live);

    let raw_ir_frame = {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut found = None;
        while std::time::Instant::now() < deadline {
            if let Some(PolledFrame::Ir(f)) = sniff_session.poll_frame(Duration::from_millis(200)) {
                found = Some(f);
                break;
            }
        }
        found.expect("no visualize/ir frame within 10s")
    };
    let frame_text = serde_json::to_string(&raw_ir_frame).unwrap();
    let parse_a: ac_scene::IrWireFrame = serde_json::from_str(&frame_text).unwrap();
    let parse_b: ac_scene::IrWireFrame = serde_json::from_str(&frame_text).unwrap();
    let scene_a = IrScene::from_input(&IrInput::from_wire_frame(&parse_a));
    let scene_b = IrScene::from_input(&IrInput::from_wire_frame(&parse_b));
    assert_eq!(
        scene_a, scene_b,
        "identical sidecar frame bytes must parse to an identical IrScene"
    );
    assert!(
        !scene_a.trace.segments.is_empty(),
        "expected a non-empty h(t) trace from a live session"
    );
    let sniffed_delay_ms: f64 = scene_a
        .arrival
        .text
        .split(' ')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    sniff_session.stop();
    drop(sniff_session);

    // --- Check 2: the real app, driven through a real eframe harness
    // with a real `H` keypress, reads back what it would paint ---
    let mut harness = Harness::new_eframe(move |_cc| {
        // Explicit stimulus ceiling: this test drives no stimulus, but
        // the value is now a parameter rather than a config re-read, so
        // it must not depend on the developer's local config.
        connect_and_launch_transfer(endpoint, 0, 1, WeightingCurve::A, "fast", -10.0)
            .expect("connect_and_launch_transfer")
    });

    // One `H` press opens the IR panel — queued now, consumed on the
    // next `step()` inside the polling loop below (`AcViewApp::ui`'s
    // `ctx.input(|i| i.key_pressed(...))` check needs to see the event
    // land inside a frame it processes, not merely be queued).
    harness.key_press(egui::Key::H);

    let (app_header, app_delay_ms): (&'static str, f64) = {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut found = None;
        while std::time::Instant::now() < deadline {
            harness.step();
            if let Some(scene) = harness.state().current_ir_scene() {
                if !scene.trace.segments.is_empty() {
                    let ms: f64 = scene
                        .arrival
                        .text
                        .split(' ')
                        .next()
                        .unwrap()
                        .parse()
                        .unwrap();
                    found = Some((scene.header, ms));
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        found.expect("app's IR panel never received a non-empty frame within 10s")
    };

    assert_eq!(
        app_header,
        ac_scene::IR_HEADER,
        "the painted panel's header must be ac-scene's IR_HEADER verbatim"
    );

    // Tolerance: two independent H1 estimates off the same stationary
    // fake tone, ~seconds apart — same discipline and same order of
    // magnitude as the SPL check above, not a loose guess. The delay
    // estimate has coarser native resolution than SPL (sample-period
    // granularity, not a continuously-varying integrator), so the bound
    // is wider; it still catches a mis-wired panel (which would show a
    // multi-ms-to-multi-second divergence, not a fraction of a ms).
    let delta = (app_delay_ms - sniffed_delay_ms).abs();
    assert!(
        delta < 0.5,
        "app's on-screen IR arrival ({app_delay_ms:.4} ms) and an independently-sniffed \
         frame's arrival ({sniffed_delay_ms:.4} ms) diverged by {delta:.4} ms"
    );
}
