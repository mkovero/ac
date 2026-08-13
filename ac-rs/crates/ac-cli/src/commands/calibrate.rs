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

            let (prompt, try_hint) = if let Some(dmm) = dmm_vrms {
                let hint = format!("{:.4} mVrms", dmm * 1000.0);
                (
                    format!(
                        "  Enter to accept ({hint}), or override \
                         (skip to keep stored, clear to erase, q to cancel): "
                    ),
                    "  Try:  0.245  or  245mV  (or skip / clear)",
                )
            } else {
                (
                    "  DMM reading (e.g. 245mV or 0.245; Enter or skip keeps the stored \
                     value, clear erases it, q to cancel): "
                        .to_string(),
                    "  Try:  0.245  or  245mV  (or clear)",
                )
            };

            let reply = loop {
                print!("{prompt}");
                io::stdout().flush().ok();
                let raw = read_line();
                match classify_entry(&raw, dmm_vrms) {
                    Entry::Cancel => {
                        println!("  Calibration cancelled.");
                        client.send_cmd(&serde_json::json!({"cmd": "stop"}), None);
                        return;
                    }
                    Entry::Reply(r) => break r,
                    Entry::Unparsed => println!("{try_hint}"),
                }
            };

            client.send_cmd(&reply.to_cmd(), None);
        } else if topic == "cal_done" {
            let key = data.get("key").and_then(|v| v.as_str()).unwrap_or("?");
            println!("\n  Calibration saved: [{key}]");
            print_cal_leg("Output", &data, "vrms_at_0dbfs_out", "out_state");
            print_cal_leg("Input", &data, "vrms_at_0dbfs_in", "in_state");
            print_tau_leg(&data);
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
#[derive(Debug, PartialEq)]
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

/// One line of operator input, resolved to an intent. Split out of the
/// prompt loop so the keystroke -> intent mapping is testable without a
/// terminal — getting that mapping wrong is exactly what #279 was: Enter
/// meant "skip" to the user and `None` meant "erase" to the daemon.
///
/// `dmm` is the pre-filled reading the daemon offered, if any. It is the
/// only thing that changes what an empty line means: accept the offered
/// reading when there is one, keep the stored value when there is not.
#[derive(Debug, PartialEq)]
enum Entry {
    Cancel,
    Reply(Reply),
    Unparsed,
}

fn classify_entry(input: &str, dmm: Option<f64>) -> Entry {
    let t = input.trim();
    if t.eq_ignore_ascii_case("q") {
        Entry::Cancel
    } else if t.is_empty() {
        match dmm {
            Some(v) => Entry::Reply(Reply::Value(v)),
            None => Entry::Reply(Reply::Skip),
        }
    } else if t.eq_ignore_ascii_case("skip") {
        Entry::Reply(Reply::Skip)
    } else if t.eq_ignore_ascii_case("clear") {
        Entry::Reply(Reply::Clear)
    } else {
        match parse_vrms(t) {
            Some(v) => Entry::Reply(Reply::Value(v)),
            None => Entry::Unparsed,
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

/// Render the `Delay:` leg of a `cal_done` frame (#281) — third leg of the
/// same block, same `{label:<8}` alignment as `print_cal_leg`'s
/// Output/Input rows.
///
/// τ has no `(unchanged)` state — unlike the voltage legs, it is not
/// prompt-driven, so a skipped voltage prompt never touches it (see
/// ZMQ.md's `cal_done` schema). The sample rate + period size are printed
/// alongside the value on purpose: without them the number is
/// unfalsifiable a year and three `-p` changes later, which is the exact
/// failure #281 exists to close (device/backend/port identity is already
/// implied by the session and the `[out_in]` key on the line above, so
/// repeating those here would be redundant, not missing).
fn print_tau_leg(data: &serde_json::Value) {
    let state = data.get("tau_state").and_then(|v| v.as_str()).unwrap_or("");
    let sample_rate = data.get("tau_sample_rate").and_then(|v| v.as_u64());
    let period_size = data.get("tau_period_size").and_then(|v| v.as_u64());
    let conditions = match (sample_rate, period_size) {
        (Some(sr), Some(p)) => format!("{sr} Hz, period {p}"),
        (Some(sr), None) => format!("{sr} Hz"),
        (None, _) => String::new(),
    };
    match state {
        "measured" => match data.get("tau_s").and_then(|v| v.as_f64()) {
            Some(tau_s) => {
                println!(
                    "  {:<8}{:.4} ms   (measured, {conditions})",
                    "Delay:",
                    tau_s * 1000.0
                );
            }
            None => println!("  {:<8}not measured", "Delay:"),
        },
        "error" => {
            // A real measurement failure with a loopback present — state
            // the observed cause, unlike the no-loopback case below where
            // the daemon has no cause to report, only an observation.
            let msg = data
                .get("tau_error")
                .and_then(|v| v.as_str())
                .unwrap_or("measurement failed");
            println!("  {:<8}not measured ({msg})", "Delay:");
        }
        // "not_measured_no_loopback" and anything unrecognised (older
        // daemon without this field): state the observation, not an
        // inferred cause — `is_loopback` is what the daemon saw, not a
        // claim about physical wiring the instrument cannot verify.
        _ => println!(
            "  {:<8}not measured (loopback not detected this run)",
            "Delay:"
        ),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// #279: Enter with no DMM reading offered must resolve to `Skip`,
    /// never to something that erases the stored value.
    #[test]
    fn enter_keeps_the_stored_value_when_no_reading_is_offered() {
        assert_eq!(classify_entry("", None), Entry::Reply(Reply::Skip));
        assert_eq!(classify_entry("   ", None), Entry::Reply(Reply::Skip));
        assert_eq!(classify_entry("skip", None), Entry::Reply(Reply::Skip));
        assert_eq!(classify_entry("SKIP", None), Entry::Reply(Reply::Skip));
    }

    #[test]
    fn enter_accepts_the_offered_reading_but_skip_still_skips() {
        assert_eq!(
            classify_entry("", Some(0.245)),
            Entry::Reply(Reply::Value(0.245))
        );
        assert_eq!(
            classify_entry("skip", Some(0.245)),
            Entry::Reply(Reply::Skip)
        );
    }

    #[test]
    fn only_the_clear_word_erases() {
        assert_eq!(classify_entry("clear", None), Entry::Reply(Reply::Clear));
        assert_eq!(
            classify_entry("Clear", Some(0.245)),
            Entry::Reply(Reply::Clear)
        );
    }

    #[test]
    fn q_cancels_from_either_branch() {
        assert_eq!(classify_entry("q", None), Entry::Cancel);
        assert_eq!(classify_entry("Q", Some(0.245)), Entry::Cancel);
    }

    #[test]
    fn unparseable_input_reprompts_and_sends_nothing() {
        assert_eq!(classify_entry("banana", None), Entry::Unparsed);
        assert_eq!(classify_entry("banana", Some(0.245)), Entry::Unparsed);
    }

    /// The wire encoding is the other half of #279: a skip must not carry
    /// `clear`, or the daemon reads "I did not measure this" as "erase it".
    #[test]
    fn skip_and_clear_encode_to_distinct_wire_frames() {
        let skip = Reply::Skip.to_cmd();
        assert_eq!(skip["vrms"], serde_json::Value::Null);
        assert!(
            skip.get("clear").is_none(),
            "a skip must not carry `clear`: {skip}"
        );

        let clear = Reply::Clear.to_cmd();
        assert_eq!(clear["clear"], serde_json::json!(true));

        let value = Reply::Value(0.245).to_cmd();
        assert_eq!(value["vrms"], serde_json::json!(0.245));
        assert!(value.get("clear").is_none());
    }
}
