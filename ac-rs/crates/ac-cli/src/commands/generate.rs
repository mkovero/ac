use crate::client::AcClient;
use crate::parse::{CommandKind, LevelSpec};
use super::{check_ack, get_cal, level_to_dbfs};

pub fn run_sine(cmd: &CommandKind, client: &mut AcClient) {
    let (level, freq, ch_spec) = match cmd {
        CommandKind::GenerateSine {
            level,
            freq,
            channels,
        } => (level, *freq, channels),
        _ => unreachable!(),
    };

    let level = resolve_level(level, client);
    let channels = resolve_channels(ch_spec, client);

    println!();
    let mut first_dbfs = None;
    for &ch in &channels {
        let cal = get_cal_for_channel(client, ch);
        let dbfs = level_to_dbfs(&level, cal.as_ref());
        if first_dbfs.is_none() {
            first_dbfs = Some(dbfs);
        }
        print_channel_info(ch, Some(freq), dbfs, &cal);
    }

    let dbfs = first_dbfs.unwrap_or(-12.0);
    let ack = check_ack(
        client.send_cmd(
            &serde_json::json!({
                "cmd": "generate",
                "freq_hz": freq,
                "level_dbfs": dbfs,
                "channels": channels,
            }),
            None,
        ),
        "generate",
    );
    if let Some(ports) = ack.get("out_ports").and_then(|v| v.as_array()) {
        for p in ports {
            if let Some(s) = p.as_str() {
                println!("  -> {s}");
            }
        }
    }
    let n = channels.len();
    println!("\n  Playing {n} channel(s)... Ctrl+C or q to stop.\n");

    wait_for_stop(client, "generate");
}

pub fn run_pink(cmd: &CommandKind, client: &mut AcClient) {
    let (level, ch_spec) = match cmd {
        CommandKind::GeneratePink { level, channels } => (level, channels),
        _ => unreachable!(),
    };

    let level = resolve_level(level, client);
    let channels = resolve_channels(ch_spec, client);

    println!();
    let mut first_dbfs = None;
    for &ch in &channels {
        let cal = get_cal_for_channel(client, ch);
        let dbfs = level_to_dbfs(&level, cal.as_ref());
        if first_dbfs.is_none() {
            first_dbfs = Some(dbfs);
        }
        print_channel_info(ch, None, dbfs, &cal);
    }

    let dbfs = first_dbfs.unwrap_or(-12.0);
    let ack = check_ack(
        client.send_cmd(
            &serde_json::json!({
                "cmd": "generate_pink",
                "level_dbfs": dbfs,
                "channels": channels,
            }),
            None,
        ),
        "generate_pink",
    );
    if let Some(ports) = ack.get("out_ports").and_then(|v| v.as_array()) {
        for p in ports {
            if let Some(s) = p.as_str() {
                println!("  -> {s}");
            }
        }
    }
    let n = channels.len();
    println!("\n  Playing pink noise on {n} channel(s)... Ctrl+C or q to stop.\n");

    wait_for_stop(client, "generate_pink");
}

fn resolve_level(level: &Option<LevelSpec>, client: &mut AcClient) -> LevelSpec {
    match level {
        Some(l) => l.clone(),
        None => {
            let cal = super::get_cal(client);
            if cal.is_some() {
                LevelSpec::Dbu(0.0)
            } else {
                LevelSpec::Dbfs(-20.0)
            }
        }
    }
}

fn resolve_channels(ch_spec: &Option<String>, client: &mut AcClient) -> Vec<u32> {
    if let Some(spec) = ch_spec {
        crate::parse::parse_channels(spec).unwrap_or_else(|_| vec![0])
    } else {
        let ack = client.send_cmd(&serde_json::json!({"cmd": "setup", "update": {}}), None);
        let ch = ack
            .as_ref()
            .and_then(|a| a.get("config"))
            .and_then(|c| c.get("output_channel"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        vec![ch]
    }
}

fn get_cal_for_channel(
    client: &mut AcClient,
    ch: u32,
) -> Option<serde_json::Value> {
    let reply = client.send_cmd(
        &serde_json::json!({"cmd": "get_calibration", "output_channel": ch}),
        None,
    )?;
    if reply.get("found").and_then(|v| v.as_bool()) == Some(true) {
        Some(reply)
    } else {
        None
    }
}

fn print_channel_info(ch: u32, freq: Option<f64>, dbfs: f64, cal: &Option<serde_json::Value>) {
    let v_out = cal
        .as_ref()
        .and_then(|c| c.get("vrms_at_0dbfs_out"))
        .and_then(|v| v.as_f64());

    let (vrms_s, cal_tag) = if let Some(ref_vrms) = v_out {
        let vrms = ref_vrms * 10.0_f64.powf(dbfs / 20.0);
        let dbu = ac_core::conversions::vrms_to_dbu(vrms);
        (ac_core::conversions::fmt_vrms(vrms), format!("{dbu:+.2} dBu"))
    } else {
        ("  -".to_string(), format!("{dbfs:.1} dBFS (uncal)"))
    };

    match freq {
        Some(f) => println!("  ch {ch:>3}  {f:.0} Hz  {vrms_s:>14}  {cal_tag}"),
        None => println!("  ch {ch:>3}  pink noise  {vrms_s:>14}  {cal_tag}"),
    }
}

pub(crate) fn wait_for_stop(client: &mut AcClient, cmd_name: &str) {
    crossterm::terminal::enable_raw_mode().ok();
    let result = wait_loop(client, cmd_name);
    crossterm::terminal::disable_raw_mode().ok();
    if let Err(reason) = result {
        println!("\n  {reason}");
    }
}

fn wait_loop(client: &mut AcClient, cmd_name: &str) -> Result<(), String> {
    loop {
        if crossterm::event::poll(std::time::Duration::from_millis(50)).unwrap_or(false) {
            if let Ok(crossterm::event::Event::Key(key)) = crossterm::event::read() {
                use crossterm::event::KeyCode;
                match key.code {
                    KeyCode::Char('q') | KeyCode::Char('Q') => {
                        crossterm::terminal::disable_raw_mode().ok();
                        client.send_cmd(
                            &serde_json::json!({"cmd": "stop", "name": cmd_name}),
                            None,
                        );
                        return Err("Stopped.".into());
                    }
                    KeyCode::Char('c')
                        if key
                            .modifiers
                            .contains(crossterm::event::KeyModifiers::CONTROL) =>
                    {
                        crossterm::terminal::disable_raw_mode().ok();
                        client.send_cmd(
                            &serde_json::json!({"cmd": "stop", "name": cmd_name}),
                            None,
                        );
                        return Err("Stopped.".into());
                    }
                    _ => {}
                }
            }
        }

        if let Some((topic, frame)) = client.recv_data(100) {
            let frame_cmd = frame.get("cmd").and_then(|v| v.as_str()).unwrap_or("");
            if topic == "error" && (frame_cmd.is_empty() || frame_cmd == cmd_name) {
                let msg = frame
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("error");
                return Err(format!("error: {msg}"));
            }
            if topic == "done" && (frame_cmd.is_empty() || frame_cmd == cmd_name) {
                return Ok(());
            }
        }
    }
}
