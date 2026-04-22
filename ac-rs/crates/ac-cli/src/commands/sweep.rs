use crate::client::AcClient;
use crate::parse::CommandKind;
use super::{check_ack, get_cal, level_to_dbfs};

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
    if let Some(p) = ack.get("out_port").and_then(|v| v.as_str()) {
        println!("  Output: {p}");
    }
    println!("  Sweeping... Ctrl+C or q to stop.\n");

    super::generate::wait_for_stop(client, "sweep_level");
}

pub fn run_ir(cmd: &CommandKind, client: &mut AcClient) {
    let (f1, f2, duration, level) = match cmd {
        CommandKind::SweepIr { f1, f2, duration, level } => (*f1, *f2, *duration, level),
        _ => unreachable!(),
    };
    let cal = get_cal(client);
    let level_db = level_to_dbfs(level, cal.as_ref());

    println!(
        "\n  IR sweep: {f1:.0} \u{2192} {f2:.0} Hz  |  {level_db:.1} dBFS  |  {duration:.1}s"
    );

    let ack = check_ack(
        client.send_cmd(
            &serde_json::json!({
                "cmd": "sweep_ir",
                "f1_hz": f1,
                "f2_hz": f2,
                "duration": duration,
                "level_dbfs": level_db,
            }),
            None,
        ),
        "sweep_ir",
    );
    if let Some(p) = ack.get("out_port").and_then(|v| v.as_str()) {
        println!("  Output: {p}");
    }
    println!("  Running IR measurement...\n");

    super::generate::wait_for_stop(client, "sweep_ir");
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
    if let Some(p) = ack.get("out_port").and_then(|v| v.as_str()) {
        println!("  Output: {p}");
    }
    println!("  Sweeping... Ctrl+C or q to stop.\n");

    super::generate::wait_for_stop(client, "sweep_frequency");
}
