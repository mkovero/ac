//! `ac monitor`'s window under `--fake-audio`: every requested channel
//! streams, at the monitor cadence, through the production `MonitorView`
//! driven by `egui_kittest`'s eframe harness.

#[path = "support.rs"]
mod support;

use std::time::{Duration, Instant};

use ac_view::monitor_view::{MonitorView, MONITOR_INTERVAL_S};
use ac_view::zmq_client::Endpoint;
use egui_kittest::Harness;
use support::DaemonProcess;

#[test]
fn every_requested_channel_streams_at_the_monitor_cadence() {
    let daemon = DaemonProcess::spawn();
    let endpoint = Endpoint {
        host: "127.0.0.1".into(),
        ctrl_port: daemon.ctrl_port,
        data_port: daemon.data_port,
    };
    let channels = [0u32, 1];
    let mut harness = Harness::new_eframe(move |_cc| {
        MonitorView::launch(endpoint, &channels).expect("monitor_spectrum launch")
    });

    // Wait for both channels' first frame, then count for a fixed span.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        harness.step();
        let live = harness
            .state()
            .scene()
            .is_some_and(|s| s.channels.iter().all(|c| c.trace.is_some()));
        if live {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "not every channel delivered a frame within 10 s; fault: {:?}",
            harness.state().fault()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let start_frames = harness.state().frames_received();
    let span = Duration::from_secs(2);
    let t0 = Instant::now();
    while t0.elapsed() < span {
        harness.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    let per_channel_hz = (harness.state().frames_received() - start_frames) as f64
        / channels.len() as f64
        / span.as_secs_f64();
    // Nominal 1 / MONITOR_INTERVAL_S = 20 Hz. Half of it is the bar: the
    // transfer-session spectrum this window replaced changed about twice a
    // second, so anything near it fails here, while a loaded test host
    // still passes.
    let nominal = 1.0 / MONITOR_INTERVAL_S;
    eprintln!("monitor: {per_channel_hz:.1} frames/s per channel (nominal {nominal:.0})");
    assert!(
        per_channel_hz >= nominal / 2.0,
        "{per_channel_hz:.1} frames/s per channel, want >= {:.1}",
        nominal / 2.0
    );

    let scene = harness.state().scene().expect("scene");
    for ch in &scene.channels {
        assert!(ch.trace.is_some(), "channel {} has no trace", ch.channel);
        assert!(
            ch.readout.contains("dBFS"),
            "channel {} readout has no level: {}",
            ch.channel,
            ch.readout
        );
        assert!(
            ch.meter.height > 0.0,
            "channel {} meter is empty",
            ch.channel
        );
    }
    assert!(harness.state().fault().is_none());
}
