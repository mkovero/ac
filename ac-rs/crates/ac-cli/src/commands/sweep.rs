use super::{
    await_session_check, check_ack, consumes_voltage, get_cal, level_to_dbfs, level_unit,
    print_consumer_check, print_level, print_level_range,
};
use crate::client::AcClient;
use crate::parse::CommandKind;

pub fn run_level(cmd: &CommandKind, client: &mut AcClient) {
    let (start, stop, level_defaulted, freq, duration) = match cmd {
        CommandKind::SweepLevel {
            start,
            stop,
            level_defaulted,
            freq,
            duration,
        } => (start, stop, *level_defaulted, *freq, *duration),
        _ => unreachable!(),
    };

    let mut cal = get_cal(client);
    let start_db = level_to_dbfs(start, cal.as_ref());
    let stop_db = level_to_dbfs(stop, cal.as_ref());
    // A physical endpoint makes the whole ramp physical.
    let typed = if level_unit(start) != "dbfs" {
        start
    } else {
        stop
    };
    let consumes = consumes_voltage(cal.as_ref(), Some(typed));

    println!("\n  Sweep: {freq:.0} Hz  |  {duration:.1}s");

    let ack = check_ack(
        client.send_cmd(
            &serde_json::json!({
                "cmd": "sweep_level",
                "freq_hz": freq,
                "start_dbfs": start_db,
                "stop_dbfs": stop_db,
                "duration": duration,
                "level_unit": level_unit(typed),
            }),
            None,
        ),
        "sweep_level",
    );
    // #466: the level block waits for the session check.
    let wait = await_session_check(client, "sweep_level", &ack, 30_000);
    if print_consumer_check(wait, &mut cal, Some(typed), consumes).is_err() {
        std::process::exit(1);
    }
    print_level_range(
        ack.get("start_dbfs").and_then(|v| v.as_f64()),
        ack.get("stop_dbfs").and_then(|v| v.as_f64()),
        level_defaulted,
        ack.get("max_dbfs").and_then(|v| v.as_f64()),
        cal.as_ref(),
    );
    if let Some(p) = ack.get("out_port").and_then(|v| v.as_str()) {
        println!("  Output: {p}");
    }
    println!("  Sweeping... Ctrl+C or q to stop.\n");

    super::generate::wait_for_stop(client, "sweep_level");
}

pub fn run_frequency(cmd: &CommandKind, cfg: &ac_core::config::Config, client: &mut AcClient) {
    let (start, stop, level, level_defaulted, duration) = match cmd {
        CommandKind::SweepFrequency {
            start,
            stop,
            level,
            level_defaulted,
            duration,
        } => (*start, *stop, level, *level_defaulted, *duration),
        _ => unreachable!(),
    };

    let mut cal = get_cal(client);
    let level_db = level_to_dbfs(level, cal.as_ref());
    let consumes = consumes_voltage(cal.as_ref(), Some(level));
    let start_hz = start.unwrap_or(cfg.range_start_hz);
    let stop_hz = stop.unwrap_or(cfg.range_stop_hz);

    println!("\n  Sweep: {start_hz:.0} \u{2192} {stop_hz:.0} Hz  |  {duration:.1}s");

    let ack = check_ack(
        client.send_cmd(
            &serde_json::json!({
                "cmd": "sweep_frequency",
                "start_hz": start_hz,
                "stop_hz": stop_hz,
                "level_dbfs": level_db,
                "duration": duration,
                "level_unit": level_unit(level),
            }),
            None,
        ),
        "sweep_frequency",
    );
    let wait = await_session_check(client, "sweep_frequency", &ack, 30_000);
    if print_consumer_check(wait, &mut cal, Some(level), consumes).is_err() {
        std::process::exit(1);
    }
    print_level(
        ack.get("level_dbfs").and_then(|v| v.as_f64()),
        level_defaulted,
        ack.get("max_dbfs").and_then(|v| v.as_f64()),
        cal.as_ref(),
        true,
    );
    if let Some(p) = ack.get("out_port").and_then(|v| v.as_str()) {
        println!("  Output: {p}");
    }
    println!("  Sweeping... Ctrl+C or q to stop.\n");

    super::generate::wait_for_stop(client, "sweep_frequency");
}
