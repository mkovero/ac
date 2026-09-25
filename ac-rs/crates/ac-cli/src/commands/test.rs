use serde_json::Value;

use super::{
    await_session_check, check_ack, consumes_voltage, get_cal, level_to_dbfs, level_unit,
    print_consumer_check,
};
use crate::client::AcClient;
use crate::io;
use crate::parse::CommandKind;

/// `ac test software` — the daemon-side numeric self-test. Used to also
/// run the ac-ui-hosted display-truth harness (#170) in the same table;
/// that harness was removed along with the ac-ui crate and is pending
/// re-home onto an ac-cli/daemon-side truth harness — not reimplemented
/// here.
pub fn run_software(client: &mut AcClient) {
    let ack = check_ack(
        client.send_cmd(&serde_json::json!({"cmd": "test_software"}), None),
        "test_software",
    );
    for line in software_lines(&ack) {
        println!("{line}");
    }

    let all_pass = ack
        .get("all_pass")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !all_pass {
        std::process::exit(1);
    }
}

/// Full `ac test software` report. Each heading is followed directly by
/// its rows and groups are separated by exactly one blank line — no rule
/// under a heading (#116 layout, #129).
fn software_lines(ack: &Value) -> Vec<String> {
    let mut lines = vec![String::new(), "  Software self-test".to_string()];
    if let Some(results) = ack.get("results").and_then(|v| v.as_array()) {
        lines.extend(result_lines(results));
    }
    lines.push(String::new());
    lines.push("  Display-truth harness (T2/T3, #170)".to_string());
    lines.push("  skip  pending re-home onto ac-cli truth harness".to_string());
    lines.push(String::new());
    lines
}

/// Self-test rows: `pass` / `FAIL`, four wide, then the name; the detail
/// sits on a continuation line indented 8 so name plus detail never share
/// one line. `FAIL` stays in capitals on purpose — it is the only row that
/// makes the command exit 1, and capitals keep it visible with colour gone.
fn result_lines(results: &[Value]) -> Vec<String> {
    let mut lines = Vec::new();
    for r in results {
        let name = r.get("name").and_then(|v| v.as_str()).unwrap_or("?");
        let pass = r.get("pass").and_then(|v| v.as_bool()).unwrap_or(false);
        let mark = if pass { "pass" } else { "FAIL" };
        lines.push(format!("  {mark:<4}  {name}"));
        let detail = r.get("detail").and_then(|v| v.as_str()).unwrap_or("");
        if !detail.is_empty() {
            lines.push(format!("        {detail}"));
        }
    }
    lines
}

pub fn run_hardware(cmd: &CommandKind, client: &mut AcClient) {
    let dmm = match cmd {
        CommandKind::TestHardware { dmm } => *dmm,
        _ => unreachable!(),
    };
    io::print_run_header("test hardware");

    let mut cal = get_cal(client);
    let consumes = consumes_voltage(cal.as_ref(), None);
    let mut json = serde_json::json!({"cmd": "test_hardware"});
    if dmm {
        json["dmm"] = true.into();
    }

    let ack = check_ack(client.send_cmd(&json, None), "test_hardware");
    // #466: the stored output scale feeds the DMM rows; its check prints first.
    let wait = await_session_check(client, "test_hardware", &ack, 30_000);
    if print_consumer_check(wait, &mut cal, None, consumes).is_err() {
        std::process::exit(1);
    }

    io::print_freq_header(false, false);

    loop {
        let frame = match client.recv_data(300_000) {
            Some(f) => f,
            None => {
                eprintln!("  error: timeout");
                return;
            }
        };
        let (topic, data) = frame;

        if topic == "data" {
            if data.get("type").and_then(|v| v.as_str())
                == Some("measurement/frequency_response/point")
            {
                io::print_freq_row(&data, false, false);
            }
        } else if topic == "done" {
            if let Some(xruns) = data.get("xruns").and_then(|v| v.as_u64()) {
                if xruns > 0 {
                    println!("\n  !! {xruns} xrun(s)");
                }
            }
            println!();
            return;
        } else if topic == "error" {
            let msg = data
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("error");
            eprintln!("\n  error: {msg}");
            return;
        }
    }
}

pub fn run_dut(cmd: &CommandKind, cfg: &ac_core::config::Config, client: &mut AcClient) {
    let (compare, level, level_defaulted) = match cmd {
        CommandKind::TestDut {
            compare,
            level,
            level_defaulted,
        } => (*compare, level, *level_defaulted),
        _ => unreachable!(),
    };
    io::print_run_header("test dut");
    // Provenance remains part of parsing and its default tests, but the
    // operator's final #459 ruling exempts self-test level rows.
    let _ = level_defaulted;

    let mut cal = get_cal(client);
    let have_cal = cal.is_some();
    let level_db = level_to_dbfs(level, cal.as_ref());
    let consumes = consumes_voltage(cal.as_ref(), Some(level));

    let mut json = serde_json::json!({
        "cmd": "test_dut",
        "level_dbfs": level_db,
        "level_unit": level_unit(level),
    });
    if compare {
        json["compare"] = true.into();
    }

    let ack = check_ack(client.send_cmd(&json, None), "test_dut");
    let wait = await_session_check(client, "test_dut", &ack, 30_000);
    if print_consumer_check(wait, &mut cal, Some(level), consumes).is_err() {
        std::process::exit(1);
    }

    io::print_freq_header(have_cal, false);

    let mut results = Vec::new();
    loop {
        let frame = match client.recv_data(300_000) {
            Some(f) => f,
            None => {
                eprintln!("  error: timeout");
                break;
            }
        };
        let (topic, data) = frame;

        if topic == "data" {
            if data.get("type").and_then(|v| v.as_str())
                == Some("measurement/frequency_response/point")
            {
                io::print_freq_row(&data, have_cal, false);
                results.push(data);
            }
        } else if topic == "done" {
            if let Some(xruns) = data.get("xruns").and_then(|v| v.as_u64()) {
                if xruns > 0 {
                    println!("\n  !! {xruns} xrun(s)");
                }
            }
            break;
        } else if topic == "error" {
            let msg = data
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("error");
            eprintln!("\n  error: {msg}");
            break;
        }
    }

    if !results.is_empty() {
        io::print_summary(&results, "DUT", have_cal);
        let dir = io::output_dir(cfg);
        let ts = io::timestamp();
        let path = dir.join(format!("test_dut_{ts}.csv"));
        io::save_csv(&results, &path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn result_rows_are_lowercase_pass_and_capital_fail_with_detail_below() {
        let lines = result_lines(&[
            json!({"name": "Pure sine: THD < 0.05%", "pass": true, "detail": "THD = 0.0003%"}),
            json!({"name": "Synthetic 1% H2: THD \u{2248} 1.0%", "pass": false,
                   "detail": "THD = 0.8712% (expected 1.0% \u{00b1} 0.1%)"}),
            json!({"name": "no detail", "pass": true}),
        ]);
        assert_eq!(
            lines,
            vec![
                "  pass  Pure sine: THD < 0.05%",
                "        THD = 0.0003%",
                "  FAIL  Synthetic 1% H2: THD \u{2248} 1.0%",
                "        THD = 0.8712% (expected 1.0% \u{00b1} 0.1%)",
                "  pass  no detail",
            ]
        );
        assert!(lines.iter().all(|l| !l.contains('[')));
    }

    /// Headings carry no rule: the first result row follows the heading
    /// directly, and the two groups are split by exactly one blank line.
    #[test]
    fn software_report_has_no_rules_and_one_blank_line_between_groups() {
        let ack = json!({
            "all_pass": false,
            "results": [
                {"name": "Pure sine: THD < 0.05%", "pass": true, "detail": "THD = 0.0003%"},
                {"name": "Synthetic 1% H2", "pass": false, "detail": "THD = 0.8712%"},
            ],
        });
        let lines = software_lines(&ack);

        for l in &lines {
            assert!(!l.contains('\u{2500}'), "box-drawing rule in {l:?}");
            assert!(!l.contains("=="), "= rule in {l:?}");
            assert!(!l.contains("-----"), "- rule in {l:?}");
        }

        let heading = lines
            .iter()
            .position(|l| l == "  Software self-test")
            .expect("self-test heading");
        assert_eq!(lines[heading + 1], "  pass  Pure sine: THD < 0.05%");

        let second = lines
            .iter()
            .position(|l| l == "  Display-truth harness (T2/T3, #170)")
            .expect("display-truth heading");
        assert!(lines[second - 1].is_empty());
        assert!(!lines[second - 2].is_empty());
        assert_eq!(
            lines[heading..second]
                .iter()
                .filter(|l| l.is_empty())
                .count(),
            1
        );
        assert!(lines
            .windows(2)
            .all(|w| !(w[0].is_empty() && w[1].is_empty())));
    }
}
