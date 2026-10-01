//! `ac-view` binary — thin wrapper. All logic lives in the library
//! (`src/lib.rs` and its modules) so it's testable without a window;
//! this file only parses args and hands off to `eframe::run_native`.

use ac_core::visualize::weighting_curves::WeightingCurve;
use ac_view::app::{connect_and_launch_transfer, parse_meas_list, resolve_transfer_channels};
use ac_view::zmq_client::Endpoint;

fn main() -> eframe::Result<()> {
    // Positional: host ctrl_port data_port. Flag: `--transfer` selects the
    // transfer view; without it the window is the live input monitor. There is deliberately NO drive
    // flag — the CLI cannot ask a launch to come up driving; drive only
    // ever starts through the in-app arm→fire machine.
    let raw: Vec<String> = std::env::args().skip(1).collect();
    // `--report <file>` (#665): a window on one report, no daemon —
    // what `ac plot ir` opens once its report is written.
    if let Some(i) = raw.iter().position(|a| a == "--report") {
        let Some(path) = raw.get(i + 1) else {
            eprintln!("ac-view: --report needs a file");
            std::process::exit(1);
        };
        let view = ac_view::report_view::ReportView::open(std::path::Path::new(path));
        let title = view.title();
        return eframe::run_native(
            &title,
            eframe::NativeOptions::default(),
            Box::new(|cc| {
                ac_view::fonts::install(&cc.egui_ctx);
                Ok(Box::new(view))
            }),
        );
    }
    let transfer = raw.iter().any(|a| a == "--transfer");
    // `--meas <N>[,<N>…]` overrides the measurement channel(s) from the
    // CLI (an explicit `ac monitor`/`ac transfer` channel spec); each is
    // measured against the configured reference (#685). A value that does
    // not parse is refused, never ignored.
    let meas_override: Option<Vec<u32>> = raw.iter().position(|a| a == "--meas").map(|i| {
        parse_meas_list(raw.get(i + 1).map(String::as_str).unwrap_or("")).unwrap_or_else(|e| {
            eprintln!("ac-view: --meas: {e}");
            std::process::exit(1);
        })
    });
    // Positional args = everything that isn't a `--flag` or the value
    // consumed by `--meas`.
    let mut positional: Vec<&String> = Vec::new();
    let mut it = raw.iter();
    while let Some(a) = it.next() {
        if a == "--meas" {
            it.next(); // skip its value
        } else if !a.starts_with("--") {
            positional.push(a);
        }
    }
    let mut pos = positional.into_iter();
    let host = pos
        .next()
        .cloned()
        .unwrap_or_else(|| "127.0.0.1".to_string());
    let ctrl_port: u16 = pos.next().and_then(|s| s.parse().ok()).unwrap_or(5556);
    let data_port: u16 = pos.next().and_then(|s| s.parse().ok()).unwrap_or(5557);

    let endpoint = Endpoint {
        host,
        ctrl_port,
        data_port,
    };

    let cfg = ac_core::config::load(None).unwrap_or_default();

    // The live input monitor: every `--meas` channel (default the
    // configured input), no reference leg and no output path, so it needs
    // no reference channel configured.
    if !transfer {
        let channels = meas_override.unwrap_or_else(|| vec![cfg.input_channel]);
        let view =
            ac_view::monitor_view::MonitorView::launch(endpoint, &channels).unwrap_or_else(|e| {
                eprintln!("ac-view: failed to start the monitor: {e:#}");
                std::process::exit(1);
            });
        let title = view.title();
        return eframe::run_native(
            &title,
            eframe::NativeOptions::default(),
            Box::new(|cc| {
                ac_view::fonts::install(&cc.egui_ctx);
                Ok(Box::new(view))
            }),
        );
    }

    // Channels come from config, not a hardcoded 0/1 (M4c). A missing
    // reference channel is fatal with the exact fix — never a silent
    // fallback that would measure against the wrong port. `--meas`
    // overrides just the measurement leg.
    let (cfg_meas, ref_channel) = resolve_transfer_channels(&cfg).unwrap_or_else(|e| {
        eprintln!("ac-view: {e}");
        std::process::exit(1);
    });
    let meas_channels = meas_override.unwrap_or_else(|| vec![cfg_meas]);

    let pairs: Vec<(u32, u32)> = meas_channels.iter().map(|&m| (m, ref_channel)).collect();
    let launched = connect_and_launch_transfer(
        endpoint,
        &pairs,
        WeightingCurve::Z,
        "fast",
        ac_core::shared::emission_level::MAX_EMISSION_DBFS,
    );
    let app = launched.unwrap_or_else(|e| {
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
