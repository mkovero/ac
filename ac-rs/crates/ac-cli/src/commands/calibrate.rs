use std::io::{self, Write};

use super::{check_ack, get_cal, level_to_dbfs};
use crate::client::AcClient;
use crate::parse::{CommandKind, LevelSpec};

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

            let reply = if let Some(dmm) = dmm_vrms {
                let hint = format!("{:.4} mVrms", dmm * 1000.0);
                loop {
                    print!(
                        "  Enter to accept ({hint}), or override \
                         (skip to keep stored, clear to erase, q to cancel): "
                    );
                    io::stdout().flush().ok();
                    let raw = read_line();
                    let t = raw.trim();
                    if t.eq_ignore_ascii_case("q") {
                        println!("  Calibration cancelled.");
                        client.send_cmd(&serde_json::json!({"cmd": "stop"}), None);
                        return;
                    }
                    if t.is_empty() {
                        break Reply::Value(dmm);
                    }
                    if t.eq_ignore_ascii_case("skip") {
                        break Reply::Skip;
                    }
                    if t.eq_ignore_ascii_case("clear") {
                        break Reply::Clear;
                    }
                    match parse_vrms(t) {
                        Some(v) => break Reply::Value(v),
                        None => println!("  Try:  0.245  or  245mV  (or skip / clear)"),
                    }
                }
            } else {
                loop {
                    print!(
                        "  DMM reading (e.g. 245mV or 0.245; Enter keeps the stored \
                         value, clear erases it, q to cancel): "
                    );
                    io::stdout().flush().ok();
                    let raw = read_line();
                    let t = raw.trim();
                    if t.eq_ignore_ascii_case("q") {
                        println!("  Calibration cancelled.");
                        client.send_cmd(&serde_json::json!({"cmd": "stop"}), None);
                        return;
                    }
                    if t.is_empty() || t.eq_ignore_ascii_case("skip") {
                        break Reply::Skip;
                    }
                    if t.eq_ignore_ascii_case("clear") {
                        break Reply::Clear;
                    }
                    match parse_vrms(t) {
                        Some(v) => break Reply::Value(v),
                        None => println!("  Try:  0.245  or  245mV  (or clear)"),
                    }
                }
            };

            client.send_cmd(&reply.to_cmd(), None);
        } else if topic == "cal_done" {
            let key = data.get("key").and_then(|v| v.as_str()).unwrap_or("?");
            println!("\n  Calibration saved: [{key}]");
            print_cal_leg("Output", &data, "vrms_at_0dbfs_out", "out_state");
            print_cal_leg("Input", &data, "vrms_at_0dbfs_in", "in_state");
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

/// What the user asked the daemon to do with one voltage leg. Kept
/// distinct from `Option<f64>` so "I did not measure this" cannot be
/// mistaken on the wire for "erase it" (#279).
enum Reply {
    Value(f64),
    Skip,
    Clear,
}

impl Reply {
    fn to_cmd(&self) -> serde_json::Value {
        match self {
            Reply::Value(v) => serde_json::json!({"cmd": "cal_reply", "vrms": v}),
            Reply::Skip => serde_json::json!({"cmd": "cal_reply", "vrms": null}),
            Reply::Clear => serde_json::json!({"cmd": "cal_reply", "vrms": null, "clear": true}),
        }
    }
}

/// Render one voltage leg of a `cal_done` frame. The `*_state` word is
/// what separates a value this run measured from one it left alone, and
/// an absent value from either.
fn print_cal_leg(label: &str, data: &serde_json::Value, vrms_key: &str, state_key: &str) {
    let state = data.get(state_key).and_then(|v| v.as_str());
    let label = format!("{label}:");
    match data.get(vrms_key).and_then(|v| v.as_f64()) {
        Some(v) => {
            let dbu = ac_core::shared::conversions::vrms_to_dbu(v);
            let note = match state {
                Some("unchanged") => "   (unchanged)",
                Some("measured") => "   (measured)",
                _ => "",
            };
            println!(
                "  {label:<8}0 dBFS = {:>14}  =  {dbu:+.2} dBu{note}",
                ac_core::shared::conversions::fmt_vrms(v)
            );
        }
        None => println!("  {label:<8}not calibrated"),
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

    let cal_path = ac_core::shared::calibration::default_cal_path();
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
                let dbu = ac_core::shared::conversions::vrms_to_dbu(v);
                println!(
                    "    Output: 0 dBFS = {:>14}  =  {dbu:+.2} dBu",
                    ac_core::shared::conversions::fmt_vrms(v)
                );
            }
            None => println!("    Output: not calibrated"),
        }
        match c.get("vrms_at_0dbfs_in").and_then(|v| v.as_f64()) {
            Some(v) => {
                let dbu = ac_core::shared::conversions::vrms_to_dbu(v);
                println!(
                    "    Input:  0 dBFS = {:>14}  =  {dbu:+.2} dBu",
                    ac_core::shared::conversions::fmt_vrms(v)
                );
            }
            None => println!("    Input:  not calibrated"),
        }
        println!();
    }
}

/// `ac calibrate spl [input N] [output N]` — pistonphone-reference SPL.
/// Sends `calibrate_spl`, prompts the user to seat the calibrator, and
/// passes the keystroke through as `cal_reply` so the daemon's worker
/// proceeds to the audio capture step. The captured dBFS shows up in the
/// `cal_done` frame and is what later `dbfs → dB SPL` conversions use.
pub fn run_spl(cmd: &CommandKind, client: &mut AcClient) {
    let (out_ch, in_ch) = match cmd {
        CommandKind::CalibrateSpl {
            output_channel,
            input_channel,
        } => (output_channel, input_channel),
        _ => unreachable!(),
    };

    let mut cmd_json = serde_json::json!({"cmd": "calibrate_spl"});
    if let Some(ch) = out_ch {
        cmd_json["output_channel"] = (*ch).into();
    }
    if let Some(ch) = in_ch {
        cmd_json["input_channel"] = (*ch).into();
    }

    check_ack(client.send_cmd(&cmd_json, Some(5000)), "calibrate_spl");
    println!("  SPL calibration started.");
    println!("  Press Ctrl+C or type q to cancel.\n");

    loop {
        let frame = match client.recv_data(300_000) {
            Some(f) => f,
            None => {
                eprintln!("  error: SPL calibration timed out");
                return;
            }
        };
        let (topic, data) = frame;

        if topic == "cal_prompt" {
            let text = data.get("text").and_then(|v| v.as_str()).unwrap_or("");
            println!("\n  {text}");
            print!("  Press Enter to capture (q to cancel): ");
            io::stdout().flush().ok();
            let raw = read_line();
            if raw.trim().eq_ignore_ascii_case("q") {
                println!("  Calibration cancelled.");
                client.send_cmd(&serde_json::json!({"cmd": "stop"}), None);
                return;
            }
            // Any non-cancel reply releases the worker. The daemon ignores
            // the value for SPL prompts (it just needs a sync point).
            client.send_cmd(
                &serde_json::json!({"cmd": "cal_reply", "vrms": serde_json::Value::Null}),
                None,
            );
        } else if topic == "cal_done" {
            let key = data.get("key").and_then(|v| v.as_str()).unwrap_or("?");
            let dbfs = data
                .get("mic_sensitivity_dbfs_at_94db_spl")
                .and_then(|v| v.as_f64());
            println!("\n  SPL calibration saved: [{key}]");
            if let Some(d) = dbfs {
                let offset = ac_core::shared::calibration::PISTONPHONE_REF_SPL - d;
                println!("  Mic sensitivity: {d:.2} dBFS @ 94 dB SPL");
                println!("  Offset:          dB SPL = dBFS + {offset:+.2}");
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

/// `ac calibrate mic-curve <path|clear> [input N] [output N]` — parse the
/// .frd / .txt file CLI-side (so bad files fail before the daemon round
/// trip) and upload validated arrays to the daemon. `clear` drops any
/// stored curve.
pub fn run_mic_curve(cmd: &CommandKind, client: &mut AcClient) {
    let (path, out_ch, in_ch) = match cmd {
        CommandKind::CalibrateMicCurve {
            path,
            output_channel,
            input_channel,
        } => (path.clone(), output_channel, input_channel),
        _ => unreachable!(),
    };

    let mut cmd_json = match path {
        None => serde_json::json!({"cmd": "calibrate_mic_curve", "op": "clear"}),
        Some(ref p) => {
            let text = match std::fs::read_to_string(p) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("  error: cannot read {p}: {e}");
                    std::process::exit(1);
                }
            };
            let curve = match ac_core::shared::calibration::parse_mic_curve(&text, Some(p.clone()))
            {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("  error: parsing {p}: {e}");
                    std::process::exit(1);
                }
            };
            serde_json::json!({
                "cmd":         "calibrate_mic_curve",
                "op":          "set",
                "freqs_hz":    curve.freqs_hz,
                "gain_db":     curve.gain_db,
                "source_path": p,
            })
        }
    };
    if let Some(ch) = out_ch {
        cmd_json["output_channel"] = (*ch).into();
    }
    if let Some(ch) = in_ch {
        cmd_json["input_channel"] = (*ch).into();
    }

    let ack = check_ack(
        client.send_cmd(&cmd_json, Some(5000)),
        "calibrate_mic_curve",
    );
    let key = ack.get("key").and_then(|v| v.as_str()).unwrap_or("?");
    let n = ack.get("loaded").and_then(|v| v.as_u64()).unwrap_or(0);
    if n == 0 {
        println!("  Mic curve cleared on [{key}].");
    } else {
        println!("  Mic curve loaded on [{key}]: {n} points.");
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
