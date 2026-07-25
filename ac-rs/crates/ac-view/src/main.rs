//! `ac-view` binary — thin wrapper. All logic lives in the library
//! (`src/lib.rs` and its modules) so it's testable without a window;
//! this file only parses args and hands off to `eframe::run_native`.

use ac_core::visualize::weighting_curves::WeightingCurve;
use ac_view::app::{connect_and_launch, resolve_transfer_channels};
use ac_view::zmq_client::Endpoint;

fn main() -> eframe::Result<()> {
    let mut args = std::env::args().skip(1);
    let host = args.next().unwrap_or_else(|| "127.0.0.1".to_string());
    let ctrl_port: u16 = args.next().and_then(|s| s.parse().ok()).unwrap_or(5556);
    let data_port: u16 = args.next().and_then(|s| s.parse().ok()).unwrap_or(5557);

    let endpoint = Endpoint {
        host,
        ctrl_port,
        data_port,
    };

    // Channels come from config, not a hardcoded 0/1 (M4c). A missing
    // reference channel is fatal with the exact fix — never a silent
    // fallback that would measure against the wrong port.
    let cfg = ac_core::config::load(None).unwrap_or_default();
    let (meas_channel, ref_channel) = resolve_transfer_channels(&cfg).unwrap_or_else(|e| {
        eprintln!("ac-view: {e}");
        std::process::exit(1);
    });

    let app = connect_and_launch(
        endpoint,
        meas_channel,
        ref_channel,
        WeightingCurve::Z,
        "fast",
    )
    .unwrap_or_else(|e| {
        eprintln!("ac-view: failed to connect/launch session: {e}");
        std::process::exit(1);
    });

    let options = eframe::NativeOptions::default();
    eframe::run_native(
        "ac-view",
        options,
        Box::new(|cc| {
            ac_view::fonts::install(&cc.egui_ctx);
            Ok(Box::new(app))
        }),
    )
}
