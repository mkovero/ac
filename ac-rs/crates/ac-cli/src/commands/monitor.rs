use crate::client::AcClient;
use crate::parse::CommandKind;

pub fn run(
    cmd: &CommandKind,
    cfg: &ac_core::config::Config,
) {
    let channels = match cmd {
        CommandKind::Monitor { channels, .. } => channels.clone(),
        _ => unreachable!(),
    };
    let channels = channels.unwrap_or_else(|| vec![cfg.input_channel]);

    super::plot::launch_ui("spectrum", cfg, Some(&channels));
}

/// `ac monitor cwt` — pre-step: switch the server analysis mode to
/// `cwt` via REQ/REP, then launch the existing monitor UI (which
/// sends `monitor_spectrum` on its own). Matches the multi-step
/// pattern in `commands/calibrate.rs`.
pub fn run_cwt(
    cmd: &CommandKind,
    cfg: &ac_core::config::Config,
    client: &mut AcClient,
) {
    let channels = match cmd {
        CommandKind::MonitorCwt { channels, .. } => channels.clone(),
        _ => unreachable!(),
    };
    let channels = channels.unwrap_or_else(|| vec![cfg.input_channel]);

    let ack = client.send_cmd(
        &serde_json::json!({"cmd": "set_analysis_mode", "mode": "cwt"}),
        None,
    );
    super::check_ack(ack, "set_analysis_mode cwt");

    super::plot::launch_ui("spectrum", cfg, Some(&channels));
}

/// `ac monitor cqt` — symmetric to `run_cwt`. Switches the server
/// analysis mode to `cqt`, then launches the monitor UI.
pub fn run_cqt(
    cmd: &CommandKind,
    cfg: &ac_core::config::Config,
    client: &mut AcClient,
) {
    let channels = match cmd {
        CommandKind::MonitorCqt { channels, .. } => channels.clone(),
        _ => unreachable!(),
    };
    let channels = channels.unwrap_or_else(|| vec![cfg.input_channel]);

    let ack = client.send_cmd(
        &serde_json::json!({"cmd": "set_analysis_mode", "mode": "cqt"}),
        None,
    );
    super::check_ack(ack, "set_analysis_mode cqt");

    super::plot::launch_ui("spectrum", cfg, Some(&channels));
}

pub fn run_not_implemented(technique: &str) {
    eprintln!("ac monitor {technique}: not yet implemented (tracked in ARCHITECTURE.md)");
    std::process::exit(1);
}
