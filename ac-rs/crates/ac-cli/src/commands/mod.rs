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

use crate::client::AcClient;
use crate::parse::{CommandKind, LevelSpec, ParsedCommand};

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
        CommandKind::MonitorCwt { .. } => monitor::run_cwt(&parsed.cmd, cfg, client),
        CommandKind::MonitorCqt { .. } => monitor::run_cqt(&parsed.cmd, cfg, client),
        CommandKind::MonitorReassigned { .. } => {
            monitor::run_reassigned(&parsed.cmd, cfg, client)
        }

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
            eprintln!("  error: no response from server{}", if context.is_empty() { String::new() } else { format!(" ({context})") });
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
