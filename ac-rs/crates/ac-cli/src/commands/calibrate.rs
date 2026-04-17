use std::io::{self, Write};

use crate::client::AcClient;
use crate::parse::{CommandKind, LevelSpec};
use super::{check_ack, get_cal, level_to_dbfs};

pub fn run(cmd: &CommandKind, client: &mut AcClient) {
    let (level, out_ch, in_ch) = match cmd {
        CommandKind::Calibrate {
            level,
            output_channel,
            input_channel,
        } => (level, output_channel, input_channel),
        _ => unreachable!(),
    };

    let cal_info = get_cal(client);
    let ref_dbfs = match level {
        LevelSpec::Dbfs(v) => *v,
        other => {
            if let Some(ref cal) = cal_info {
                level_to_dbfs(other, Some(cal))
            } else {
                -10.0
            }
        }
    };

    let mut cmd_json = serde_json::json!({"cmd": "calibrate", "ref_dbfs": ref_dbfs});
    if let Some(ch) = out_ch {
        cmd_json["output_channel"] = (*ch).into();
    }
    if let Some(ch) = in_ch {
        cmd_json["input_channel"] = (*ch).into();
    }

    check_ack(client.send_cmd(&cmd_json, Some(5000)), "calibrate");
    println!("  Calibration started: 1 kHz  |  {ref_dbfs:.1} dBFS");
    println!("  Press Ctrl+C or type q to cancel.\n");

    loop {
        let frame = match client.recv_data(120000) {
            Some(f) => f,
            None => {
                eprintln!("  error: calibration timed out");
                return;
            }
        };
        let (topic, data) = frame;

        if topic == "cal_prompt" {
            let text = data.get("text").and_then(|v| v.as_str()).unwrap_or("");
            println!("\n  {text}\n");

            let dmm_vrms = data.get("dmm_vrms").and_then(|v| v.as_f64());

            let vrms = if let Some(dmm) = dmm_vrms {
                let hint = format!("{:.4} mVrms", dmm * 1000.0);
                print!("  Enter to accept ({hint}), or override (q to cancel): ");
                io::stdout().flush().ok();
                let raw = read_line();
                if raw.trim().eq_ignore_ascii_case("q") {
                    println!("  Calibration cancelled.");
                    client.send_cmd(&serde_json::json!({"cmd": "stop"}), None);
                    return;
                }
                if raw.trim().is_empty() {
                    Some(dmm)
                } else {
                    parse_vrms(raw.trim())
                }
            } else {
                loop {
                    print!("  DMM reading (e.g. 245mV or 0.245, Enter to skip, q to cancel): ");
                    io::stdout().flush().ok();
                    let raw = read_line();
                    if raw.trim().eq_ignore_ascii_case("q") {
                        println!("  Calibration cancelled.");
                        client.send_cmd(&serde_json::json!({"cmd": "stop"}), None);
                        return;
                    }
                    if raw.trim().is_empty() {
                        break None;
                    }
                    match parse_vrms(raw.trim()) {
                        Some(v) => break Some(v),
                        None => println!("  Try:  0.245  or  245mV"),
                    }
                }
            };

            let reply_val: serde_json::Value = match vrms {
                Some(v) => v.into(),
                None => serde_json::Value::Null,
            };
            client.send_cmd(
                &serde_json::json!({"cmd": "cal_reply", "vrms": reply_val}),
                None,
            );
        } else if topic == "cal_done" {
            let key = data.get("key").and_then(|v| v.as_str()).unwrap_or("?");
            println!("\n  Calibration saved: [{key}]");
            if let Some(v) = data.get("vrms_at_0dbfs_out").and_then(|v| v.as_f64()) {
                let dbu = ac_core::conversions::vrms_to_dbu(v);
                println!(
                    "  Output: 0 dBFS = {:>14}  =  {dbu:+.2} dBu",
                    ac_core::conversions::fmt_vrms(v)
                );
            }
            if let Some(v) = data.get("vrms_at_0dbfs_in").and_then(|v| v.as_f64()) {
                let dbu = ac_core::conversions::vrms_to_dbu(v);
                println!(
                    "  Input:  0 dBFS = {:>14}  =  {dbu:+.2} dBu",
                    ac_core::conversions::fmt_vrms(v)
                );
            }
            if let Some(err) = data.get("error").and_then(|v| v.as_str()) {
                println!("  Note: {err}");
            }
            println!();
            return;
        } else if topic == "error" {
            let msg = data
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("error");
            eprintln!("  error: {msg}");
            return;
        }
    }
}

pub fn run_show(client: &mut AcClient) {
    let ack = check_ack(
        client.send_cmd(&serde_json::json!({"cmd": "list_calibrations"}), None),
        "list_calibrations",
    );
    let cals = ack
        .get("calibrations")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let cal_path = ac_core::calibration::default_cal_path();
    if cals.is_empty() {
        println!("\n  No calibrations stored  ({})\n", cal_path.display());
        return;
    }

    println!("\n  Stored calibrations  ({})\n", cal_path.display());
    for c in &cals {
        let key = c.get("key").and_then(|v| v.as_str()).unwrap_or("?");
        println!("  [{key}]");
        match c.get("vrms_at_0dbfs_out").and_then(|v| v.as_f64()) {
            Some(v) => {
                let dbu = ac_core::conversions::vrms_to_dbu(v);
                println!(
                    "    Output: 0 dBFS = {:>14}  =  {dbu:+.2} dBu",
                    ac_core::conversions::fmt_vrms(v)
                );
            }
            None => println!("    Output: not calibrated"),
        }
        match c.get("vrms_at_0dbfs_in").and_then(|v| v.as_f64()) {
            Some(v) => {
                let dbu = ac_core::conversions::vrms_to_dbu(v);
                println!(
                    "    Input:  0 dBFS = {:>14}  =  {dbu:+.2} dBu",
                    ac_core::conversions::fmt_vrms(v)
                );
            }
            None => println!("    Input:  not calibrated"),
        }
        println!();
    }
}

fn read_line() -> String {
    let mut line = String::new();
    io::stdin().read_line(&mut line).ok();
    line
}

fn parse_vrms(raw: &str) -> Option<f64> {
    let s = raw.to_lowercase().replace(' ', "");
    if s.is_empty() {
        return None;
    }
    if let Some(rest) = s.strip_suffix("mv") {
        return rest.parse::<f64>().ok().map(|v| v / 1000.0);
    }
    if let Some(rest) = s.strip_suffix('m') {
        return rest.parse::<f64>().ok().map(|v| v / 1000.0);
    }
    if let Some(rest) = s.strip_suffix('v') {
        return rest.parse::<f64>().ok();
    }
    s.parse::<f64>().ok()
}
