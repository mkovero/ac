use crate::client::AcClient;
use crate::parse::CommandKind;

/// Resolve the channel list, defaulting to the configured `input_channel`,
/// and print a one-liner so it's obvious which channel(s) the UI will
/// monitor (was a silent default before; users hit it via "why is ac-ui
/// showing a different view than ac monitor?" — they were on different
/// implicit defaults).
fn resolve_channels_or_default(
    explicit: Option<Vec<u32>>,
    cfg: &ac_core::config::Config,
    cmd_label: &str,
) -> Vec<u32> {
    match explicit {
        Some(chs) => {
            let pretty = chs.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
            eprintln!("  {cmd_label}: channels {pretty}  (explicit)");
            chs
        }
        None => {
            let ch = cfg.input_channel;
            eprintln!(
                "  {cmd_label}: channel {ch}  (configured input — `ac setup input <N>` to change, or pass an explicit channel spec)"
            );
            vec![ch]
        }
    }
}

pub fn run(
    cmd: &CommandKind,
    cfg: &ac_core::config::Config,
) {
    let channels = match cmd {
        CommandKind::Monitor { channels, .. } => channels.clone(),
        _ => unreachable!(),
    };
    let channels = resolve_channels_or_default(channels, cfg, "ac monitor");

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
    let channels = resolve_channels_or_default(channels, cfg, "ac monitor cwt");

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
    let channels = resolve_channels_or_default(channels, cfg, "ac monitor cqt");

    let ack = client.send_cmd(
        &serde_json::json!({"cmd": "set_analysis_mode", "mode": "cqt"}),
        None,
    );
    super::check_ack(ack, "set_analysis_mode cqt");

    super::plot::launch_ui("spectrum", cfg, Some(&channels));
}

/// `ac monitor reassigned` — symmetric to `run_cwt`/`run_cqt`. Switches
/// the server analysis mode to `reassigned`, then launches the monitor UI.
pub fn run_reassigned(
    cmd: &CommandKind,
    cfg: &ac_core::config::Config,
    client: &mut AcClient,
) {
    let channels = match cmd {
        CommandKind::MonitorReassigned { channels, .. } => channels.clone(),
        _ => unreachable!(),
    };
    let channels = resolve_channels_or_default(channels, cfg, "ac monitor reassigned");

    let ack = client.send_cmd(
        &serde_json::json!({"cmd": "set_analysis_mode", "mode": "reassigned"}),
        None,
    );
    super::check_ack(ack, "set_analysis_mode reassigned");

    super::plot::launch_ui("spectrum", cfg, Some(&channels));
}

