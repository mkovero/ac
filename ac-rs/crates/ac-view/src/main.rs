//! `ac-view` binary — thin wrapper. All logic lives in the library
//! (`src/lib.rs` and its modules) so it's testable without a window;
//! this file only parses args and hands off to `eframe::run_native`.

use ac_core::visualize::weighting_curves::WeightingCurve;
use ac_view::app::connect_and_launch;
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

    let app = connect_and_launch(endpoint, 0, 1, WeightingCurve::Z, "fast").unwrap_or_else(|e| {
        eprintln!("ac-view: failed to connect/launch session: {e}");
        std::process::exit(1);
    });

    let options = eframe::NativeOptions::default();
    eframe::run_native("ac-view", options, Box::new(|_cc| Ok(Box::new(app))))
}
