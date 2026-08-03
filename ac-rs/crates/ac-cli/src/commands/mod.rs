pub mod calibrate;
pub mod devices;
pub mod dmm;
pub mod generate;
pub mod gpio;
pub mod monitor;
pub mod monitor_tui;
pub mod plot;
pub mod probe;
pub mod report;
pub mod server;
pub mod session;
pub mod setup;
pub mod stop;
pub mod sweep;
pub mod test;
pub mod transfer;

use crate::client::AcClient;
use crate::parse::{CommandKind, LevelSpec, ParsedCommand};

/// Spawn the `ac-view` window (M4d-CLI #185) and wait for it — `ac
/// monitor` (default) and `ac transfer` both launch through here. Host
/// comes from config's `server_host` (localhost when unset); ports are
/// the daemon defaults. `--transfer` selects the transfer view.
///
/// **No drive is ever passed.** The `ac-view` arg surface has no drive
/// option, so a CLI launch cannot bring a session up driving — drive only
/// starts through the in-app arm→fire machine. `meas_override` maps an
/// explicit CLI channel spec onto the measurement leg ("as today").
pub fn spawn_ac_view(cfg: &ac_core::config::Config, transfer: bool, meas_override: Option<u32>) {
    let Some(bin) = crate::spawn::find_binary("ac-view") else {
        eprintln!("  error: ac-view binary not found — build it with `cargo build -p ac-view`");
        return;
    };
    let host = cfg.server_host.as_deref().unwrap_or("localhost");
    let args = ac_view_args(host, transfer, meas_override);
    if let Err(e) = std::process::Command::new(bin).args(&args).status() {
        eprintln!("  error: failed to launch ac-view: {e}");
    }
}

/// Build the `ac-view` argv (pure, testable). Carries host, ports, the
/// view flag, and an optional meas override — and, load-bearingly, **no
/// drive option**: there is no path here to pass a drive/on argument, so
/// the CLI-launch AC (#185, "`ac transfer` never sets launch-time drive")
/// holds by construction of this arg list, not by the UI's separate proof.
fn ac_view_args(host: &str, transfer: bool, meas_override: Option<u32>) -> Vec<String> {
    let mut args = vec![host.to_string(), "5556".to_string(), "5557".to_string()];
    if transfer {
        args.push("--transfer".to_string());
    }
    if let Some(m) = meas_override {
        args.push("--meas".to_string());
        args.push(m.to_string());
    }
    args
}

pub fn dispatch(parsed: ParsedCommand, cfg: &ac_core::config::Config, client: &mut AcClient) {
    let show = parsed.show_plot;
    match parsed.cmd {
        CommandKind::Devices => devices::run(client),
        CommandKind::Setup { .. } => setup::run(&parsed.cmd, client),
        CommandKind::Stop => stop::run(client),
        CommandKind::DmmShow => dmm::run(client),
        CommandKind::ServerEnable => server::enable(client),
        CommandKind::ServerDisable => server::disable(client),
        CommandKind::ServerConnections => server::connections(client),
        CommandKind::Gpio { log } => gpio::run(client, log),

        CommandKind::GenerateSine { .. } => generate::run_sine(&parsed.cmd, client),
        CommandKind::GeneratePink { .. } => generate::run_pink(&parsed.cmd, client),

        CommandKind::Calibrate { .. } => calibrate::run(&parsed.cmd, client),
        CommandKind::CalibrateShow => calibrate::run_show(client),
        CommandKind::CalibrateSpl { .. } => calibrate::run_spl(&parsed.cmd, client),
        CommandKind::CalibrateMicCurve { .. } => calibrate::run_mic_curve(&parsed.cmd, client),

        CommandKind::SweepLevel { .. } => sweep::run_level(&parsed.cmd, client),
        CommandKind::SweepFrequency { .. } => sweep::run_frequency(&parsed.cmd, cfg, client),
        CommandKind::SweepIr { .. } => sweep::run_ir(&parsed.cmd, client),

        CommandKind::Plot { .. } => plot::run(&parsed.cmd, cfg, client, show),
        CommandKind::PlotLevel { .. } => plot::run_level(&parsed.cmd, cfg, client, show),

        CommandKind::Monitor { .. } => monitor::run(&parsed.cmd, cfg),
        CommandKind::Transfer { .. } => transfer::run(&parsed.cmd, cfg),
        CommandKind::MonitorCwt { .. } => monitor::run_cwt(&parsed.cmd, cfg, client),
        CommandKind::MonitorCqt { .. } => monitor::run_cqt(&parsed.cmd, cfg, client),
        CommandKind::MonitorReassigned { .. } => monitor::run_reassigned(&parsed.cmd, cfg, client),

        CommandKind::Probe => probe::run(client),
        CommandKind::TestSoftware => test::run_software(client),
        CommandKind::TestHardware { .. } => test::run_hardware(&parsed.cmd, client),
        CommandKind::TestDut { .. } => test::run_dut(&parsed.cmd, cfg, client),

        // Handled before dispatch in main.rs
        CommandKind::ServerSetHost { .. }
        | CommandKind::SessionNew { .. }
        | CommandKind::SessionList
        | CommandKind::SessionUse { .. }
        | CommandKind::SessionRm { .. }
        | CommandKind::SessionDiff { .. }
        | CommandKind::Report { .. } => unreachable!(),
    }
}

pub fn check_ack(ack: Option<serde_json::Value>, context: &str) -> serde_json::Value {
    match ack {
        None => {
            eprintln!(
                "  error: no response from server{}",
                if context.is_empty() {
                    String::new()
                } else {
                    format!(" ({context})")
                }
            );
            std::process::exit(1);
        }
        Some(v) => {
            if v.get("ok").and_then(|v| v.as_bool()) != Some(true) {
                let err = v
                    .get("error")
                    .and_then(|e| e.as_str())
                    .unwrap_or("unknown error");
                eprintln!("  error: {err}");
                std::process::exit(1);
            }
            // A successful reply may still carry advisories — a config whose
            // meaning changed under it, say (#225). Printed generically here
            // rather than per command, so a handler that adds one does not
            // also have to remember to display it.
            if let Some(ws) = v.get("warnings").and_then(|w| w.as_array()) {
                for w in ws.iter().filter_map(|w| w.as_str()) {
                    eprintln!("  warning: {w}");
                }
            }
            v
        }
    }
}

pub fn level_to_dbfs(level: &LevelSpec, cal: Option<&serde_json::Value>) -> f64 {
    match level {
        LevelSpec::Dbfs(v) => *v,
        LevelSpec::Dbu(dbu) => {
            let vrms_0dbfs = cal
                .and_then(|c| c.get("vrms_at_0dbfs_out"))
                .and_then(|v| v.as_f64());
            match vrms_0dbfs {
                Some(ref_vrms) => {
                    let target_vrms =
                        ac_core::shared::constants::DBU_REF_EXACT * 10.0_f64.powf(*dbu / 20.0);
                    20.0 * (target_vrms / ref_vrms).log10()
                }
                None => {
                    eprintln!("  error: dBu level requires output calibration (run: ac calibrate)");
                    std::process::exit(1);
                }
            }
        }
        LevelSpec::Vrms(vrms) => {
            let vrms_0dbfs = cal
                .and_then(|c| c.get("vrms_at_0dbfs_out"))
                .and_then(|v| v.as_f64());
            match vrms_0dbfs {
                Some(ref_vrms) => 20.0 * (vrms / ref_vrms).log10(),
                None => {
                    eprintln!(
                        "  error: Vrms level requires output calibration (run: ac calibrate)"
                    );
                    std::process::exit(1);
                }
            }
        }
    }
}

pub fn get_cal(client: &mut AcClient) -> Option<serde_json::Value> {
    let reply = client.send_cmd(&serde_json::json!({"cmd": "get_calibration"}), None)?;
    if reply.get("found").and_then(|v| v.as_bool()) == Some(true) {
        Some(reply)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::ac_view_args;

    // The CLI-path drive-off AC (#185), asserted through the CLI's own arg
    // construction — not by reusing ac-view's in-app proof. `ac transfer`
    // spawns ac-view with a view flag and channels only; no argument
    // mentions drive/on, so a session launched this way is always
    // drive-off. If a `--drive` ever gets added here, this trips.
    #[test]
    fn ac_view_launch_args_carry_no_drive_option() {
        for transfer in [true, false] {
            for meas in [None, Some(3u32)] {
                let args = ac_view_args("localhost", transfer, meas);
                let joined = args.join(" ").to_lowercase();
                assert!(
                    !joined.contains("drive") && !joined.contains("--on"),
                    "ac-view launch args must carry no drive option: {args:?}"
                );
            }
        }
    }

    #[test]
    fn transfer_flag_selects_the_transfer_view() {
        assert!(ac_view_args("h", true, None).contains(&"--transfer".to_string()));
        assert!(!ac_view_args("h", false, None).contains(&"--transfer".to_string()));
    }

    #[test]
    fn meas_override_maps_the_channel() {
        let args = ac_view_args("h", true, Some(5));
        let i = args
            .iter()
            .position(|a| a == "--meas")
            .expect("--meas present");
        assert_eq!(args[i + 1], "5");
    }
}
