use super::{check_ack, get_cal, level_to_dbfs, print_level_clamp, print_level_clamp_range};
use crate::client::AcClient;
use crate::parse::CommandKind;

pub fn run_level(cmd: &CommandKind, client: &mut AcClient) {
    let (start, stop, freq, duration) = match cmd {
        CommandKind::SweepLevel {
            start,
            stop,
            freq,
            duration,
        } => (start, stop, *freq, *duration),
        _ => unreachable!(),
    };

    let cal = get_cal(client);
    let start_db = level_to_dbfs(start, cal.as_ref());
    let stop_db = level_to_dbfs(stop, cal.as_ref());

    println!(
        "\n  Sweep: {start_db:.1} \u{2192} {stop_db:.1} dBFS  |  {freq:.0} Hz  |  {duration:.1}s"
    );

    let ack = check_ack(
        client.send_cmd(
            &serde_json::json!({
                "cmd": "sweep_level",
                "freq_hz": freq,
                "start_dbfs": start_db,
                "stop_dbfs": stop_db,
                "duration": duration,
            }),
            None,
        ),
        "sweep_level",
    );
    let start_applied = ack
        .get("start_dbfs")
        .and_then(|v| v.as_f64())
        .unwrap_or(start_db);
    let stop_applied = ack
        .get("stop_dbfs")
        .and_then(|v| v.as_f64())
        .unwrap_or(stop_db);
    print_level_clamp_range(start_db, stop_db, start_applied, stop_applied);
    if let Some(p) = ack.get("out_port").and_then(|v| v.as_str()) {
        println!("  Output: {p}");
    }
    println!("  Sweeping... Ctrl+C or q to stop.\n");

    super::generate::wait_for_stop(client, "sweep_level");
}

pub fn run_frequency(cmd: &CommandKind, cfg: &ac_core::config::Config, client: &mut AcClient) {
    let (start, stop, level, duration) = match cmd {
        CommandKind::SweepFrequency {
            start,
            stop,
            level,
            duration,
        } => (*start, *stop, level, *duration),
        _ => unreachable!(),
    };

    let cal = get_cal(client);
    let level_db = level_to_dbfs(level, cal.as_ref());
    let start_hz = start.unwrap_or(cfg.range_start_hz);
    let stop_hz = stop.unwrap_or(cfg.range_stop_hz);

    println!(
        "\n  Sweep: {start_hz:.0} \u{2192} {stop_hz:.0} Hz  |  {level_db:.1} dBFS  |  {duration:.1}s"
    );

    let ack = check_ack(
        client.send_cmd(
            &serde_json::json!({
                "cmd": "sweep_frequency",
                "start_hz": start_hz,
                "stop_hz": stop_hz,
                "level_dbfs": level_db,
                "duration": duration,
            }),
            None,
        ),
        "sweep_frequency",
    );
    let applied_db = ack
        .get("level_dbfs")
        .and_then(|v| v.as_f64())
        .unwrap_or(level_db);
    print_level_clamp(level_db, applied_db);
    if let Some(p) = ack.get("out_port").and_then(|v| v.as_str()) {
        println!("  Output: {p}");
    }
    println!("  Sweeping... Ctrl+C or q to stop.\n");

    super::generate::wait_for_stop(client, "sweep_frequency");
}
