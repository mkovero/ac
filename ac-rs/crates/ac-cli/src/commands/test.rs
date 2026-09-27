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

    let suite = run_suite(client, "test_hardware");
    if !suite.ok() {
        std::process::exit(1);
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

    let suite = run_suite(client, "test_dut");
    if !suite.rows.is_empty() {
        let path = io::output_dir(cfg).join(format!("test_dut_{}.csv", io::timestamp()));
        match save_suite_csv(&suite.rows, &path) {
            Ok(()) => println!("  saved  {}\n", path.display()),
            Err(e) => eprintln!("  error: could not write {}: {e}", path.display()),
        }
    }
    if !suite.ok() {
        std::process::exit(1);
    }
}

/// How `test hardware` / `test dut` ended, as the CLI saw it.
///
/// Both workers publish one `test_result` frame per check and a `done`
/// with their own count (ZMQ.md). #619: the CLI used to keep only
/// frequency-response points, which neither worker sends, so both
/// commands printed a table header and exited 0 with nothing in it.
#[derive(Debug, Default)]
struct Suite {
    rows: Vec<Value>,
    /// `tests_run + dmm_run` from `done`; `None` until `done` arrives.
    reported: Option<u64>,
    /// An `error` frame or a timeout ended the run early.
    broken: bool,
}

impl Suite {
    /// Exit 0 only when every row passed, at least one ran, and the rows
    /// received match the daemon's own count. A silent or short run is a
    /// failure, not an empty success.
    fn ok(&self) -> bool {
        !self.broken
            && !self.rows.is_empty()
            && self.reported == Some(self.rows.len() as u64)
            && self.rows.iter().all(row_passed)
    }
}

/// What one DATA-socket frame means to a running suite.
#[derive(Debug, PartialEq)]
enum Step {
    /// Print these lines and keep reading.
    Print(Vec<String>),
    /// `test dut compare` wants the DUT bypassed; show this and wait.
    Prompt(String),
    /// Terminal frame: print these lines and stop.
    Done(Vec<String>),
    /// Unrelated frame (another command's, or a progress type).
    Skip,
}

fn run_suite(client: &mut AcClient, cmd_name: &str) -> Suite {
    let mut suite = Suite::default();
    loop {
        let Some(frame) = client.recv_data(300_000) else {
            eprintln!("\n  error: no result from the daemon in 300 s");
            suite.broken = true;
            return suite;
        };
        match consume(&mut suite, cmd_name, frame) {
            Step::Print(lines) => lines.iter().for_each(|l| println!("{l}")),
            Step::Done(lines) => {
                lines.iter().for_each(|l| println!("{l}"));
                return suite;
            }
            Step::Prompt(message) => {
                if !answer_bypass_prompt(client, &message) {
                    // Stopped before the bypass pass: the rows so far may
                    // all pass, but the comparison asked for never ran.
                    suite.broken = true;
                }
            }
            Step::Skip => {}
        }
    }
}

/// Fold one frame into `suite`. Pure, so what each frame prints is a unit
/// test rather than a rig run.
fn consume(suite: &mut Suite, cmd_name: &str, (topic, data): (String, Value)) -> Step {
    // Another command's frame. One without `cmd` is kept: an `error` that
    // lost its name must still end the run, not leave it to time out.
    if data
        .get("cmd")
        .and_then(Value::as_str)
        .is_some_and(|c| c != cmd_name)
    {
        return Step::Skip;
    }
    match topic.as_str() {
        "data" => match data.get("type").and_then(Value::as_str) {
            Some("test_result") => {
                let prev_tag = suite
                    .rows
                    .last()
                    .and_then(|r| r.get("tag"))
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let lines = suite_row_lines(&data, prev_tag.as_deref());
                suite.rows.push(data);
                Step::Print(lines)
            }
            Some("dut_compare_prompt") => Step::Prompt(
                data.get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("Bypass DUT and press Enter")
                    .to_owned(),
            ),
            _ => Step::Skip,
        },
        "done" => {
            let count = |k: &str| data.get(k).and_then(Value::as_u64).unwrap_or(0);
            suite.reported = Some(count("tests_run") + count("dmm_run"));
            let mut lines = vec![String::new()];
            lines.extend(summary_lines(suite));
            if let Some(line) = io::xrun_warning_line(count("xruns")) {
                lines.push(line);
            }
            lines.push(String::new());
            Step::Done(lines)
        }
        "error" => {
            suite.broken = true;
            let msg = data
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("error");
            Step::Done(vec![String::new(), format!("  error: {msg}")])
        }
        _ => Step::Skip,
    }
}

fn row_passed(row: &Value) -> bool {
    row.get("pass").and_then(Value::as_bool).unwrap_or(false)
}

/// One `test_result` as rows in the `test software` layout
/// ([`result_lines`]). A change of `tag` (`test dut compare`) starts a
/// group with its own heading. `tolerance` means different things per
/// suite: for `test_hardware` it is the pass criterion, for `test_dut` it
/// names what was measured (a DUT row fails only when it could not be
/// measured), so it is worded to match.
fn suite_row_lines(row: &Value, prev_tag: Option<&str>) -> Vec<String> {
    let mut lines = Vec::new();
    let tag = row.get("tag").and_then(Value::as_str);
    if tag.is_some() && tag != prev_tag {
        if prev_tag.is_some() {
            lines.push(String::new());
        }
        lines.push(match tag {
            Some("bypass") => "  Bypass".to_owned(),
            _ => "  With DUT".to_owned(),
        });
    }

    let name = row.get("name").and_then(Value::as_str).unwrap_or("?");
    let tolerance = row
        .get("tolerance")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let dmm = row.get("dmm").and_then(Value::as_bool) == Some(true);
    let name = match (tag.is_some(), dmm) {
        (true, _) if !tolerance.is_empty() => format!("{name} ({tolerance})"),
        (_, true) => format!("{name} (DMM)"),
        _ => name.to_owned(),
    };
    let rendered = serde_json::json!({
        "name": name,
        "pass": row_passed(row),
        "detail": row.get("detail").and_then(Value::as_str).unwrap_or_default(),
    });
    lines.extend(result_lines(&[rendered]));
    if tag.is_none() && !tolerance.is_empty() {
        lines.push(format!("        pass if {tolerance}"));
    }
    lines
}

/// Count line, plus a warning when the rows received disagree with the
/// daemon's own count — a dropped frame would otherwise read as a short,
/// clean suite.
fn summary_lines(suite: &Suite) -> Vec<String> {
    let received = suite.rows.len() as u64;
    let passed = suite.rows.iter().filter(|r| row_passed(r)).count();
    let mut lines = vec![if received == 0 {
        "  no tests ran".to_owned()
    } else {
        format!("  {passed} of {received} pass")
    }];
    if let Some(reported) = suite.reported.filter(|&n| n != received) {
        lines.push(format!(
            "  warning: the daemon reports {reported} tests run, {received} results arrived"
        ));
    }
    lines
}

/// `test dut compare`: the daemon waits up to 300 s for `dut_reply`, then
/// runs the bypass pass whether or not anything was bypassed (ZMQ.md). So
/// the reply is sent only on a real Enter; with no terminal to answer from
/// the run is stopped instead of producing unbypassed "bypass" rows.
/// Returns whether the bypass pass will run.
fn answer_bypass_prompt(client: &mut AcClient, message: &str) -> bool {
    println!("\n  {message}");
    let mut line = String::new();
    let answered = matches!(std::io::stdin().read_line(&mut line), Ok(n) if n > 0);
    let cmd = if answered {
        serde_json::json!({"cmd": "dut_reply"})
    } else {
        eprintln!("  error: no answer on stdin, stopping before the bypass pass");
        serde_json::json!({"cmd": "stop", "name": "test_dut"})
    };
    check_ack(client.send_cmd(&cmd, None), "test_dut");
    answered
}

/// One row per check. Columns follow the wire names so the file reads
/// against ZMQ.md without a mapping.
fn save_suite_csv(rows: &[Value], path: &std::path::Path) -> Result<(), csv::Error> {
    let mut w = csv::Writer::from_path(path)?;
    w.write_record(["tag", "name", "pass", "detail", "tolerance"])?;
    for r in rows {
        let s = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or_default();
        let pass = if row_passed(r) { "true" } else { "false" };
        w.write_record([s("tag"), s("name"), pass, s("detail"), s("tolerance")])?;
    }
    w.flush()?;
    Ok(())
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

    fn frame(topic: &str, data: Value) -> (String, Value) {
        (topic.to_owned(), data)
    }

    fn hw(name: &str, pass: bool, detail: &str, tolerance: &str) -> (String, Value) {
        frame(
            "data",
            json!({"type": "test_result", "cmd": "test_hardware", "name": name,
                   "pass": pass, "detail": detail, "tolerance": tolerance,
                   "mic_correction": "none", "spl_offset_db": null}),
        )
    }

    fn dut(tag: &str, name: &str, detail: &str, tolerance: &str) -> (String, Value) {
        frame(
            "data",
            json!({"type": "test_result", "cmd": "test_dut", "tag": tag, "name": name,
                   "pass": true, "detail": detail, "tolerance": tolerance}),
        )
    }

    /// Feed frames in order; collect every printed line and any prompt.
    fn feed(cmd: &str, frames: Vec<(String, Value)>) -> (Suite, Vec<String>, Vec<String>) {
        let mut suite = Suite::default();
        let (mut lines, mut prompts) = (Vec::new(), Vec::new());
        for f in frames {
            match consume(&mut suite, cmd, f) {
                Step::Print(l) | Step::Done(l) => lines.extend(l),
                Step::Prompt(m) => prompts.push(m),
                Step::Skip => {}
            }
        }
        (suite, lines, prompts)
    }

    /// #619 itself: the frames a `--fake-audio` `test_hardware` actually
    /// sent (captured 2026-09-27) print as rows, and the three failing
    /// checks make the run fail. The old CLI kept only
    /// `measurement/frequency_response/point` frames and printed nothing.
    #[test]
    fn hardware_results_print_and_a_failed_check_fails_the_run() {
        let frames = vec![
            hw(
                "Noise floor",
                false,
                "-23.0 dBFS / -23.0 dBFS",
                "< -80 dBFS",
            ),
            hw(
                "Level linearity",
                true,
                "[-42\u{2192}-36:6.00]",
                "monotonic, step error < 1 dB",
            ),
            hw(
                "THD floor (1 kHz)",
                false,
                "best 1.0000%",
                "best THD < 0.05%",
            ),
            hw(
                "Frequency response",
                true,
                "max deviation 0.00 dB",
                "< 1.0 dB vs 1 kHz ref",
            ),
            hw(
                "Channel match",
                false,
                "delta level: 0.000 dB",
                "level < 0.5 dB",
            ),
            hw(
                "Repeatability",
                true,
                "level sigma=0.0000 dB",
                "level sigma < 0.05 dB",
            ),
            frame(
                "done",
                json!({"cmd": "test_hardware", "tests_run": 6, "tests_pass": 3,
                                 "dmm_run": 0, "dmm_pass": 0, "xruns": 0}),
            ),
        ];
        assert!(frames
            .iter()
            .all(|(_, d)| d["type"] != "measurement/frequency_response/point"));

        let (suite, lines, _) = feed("test_hardware", frames);
        assert_eq!(
            lines[..3],
            [
                "  FAIL  Noise floor",
                "        -23.0 dBFS / -23.0 dBFS",
                "        pass if < -80 dBFS",
            ]
        );
        assert!(lines.contains(&"  pass  Repeatability".to_owned()));
        assert!(lines.contains(&"  3 of 6 pass".to_owned()));
        assert!(!suite.ok());
    }

    #[test]
    fn an_all_pass_run_with_matching_count_is_ok() {
        let (suite, _, _) = feed(
            "test_hardware",
            vec![
                hw("Noise floor", true, "-120 dBFS", "< -80 dBFS"),
                frame(
                    "done",
                    json!({"cmd": "test_hardware", "tests_run": 1, "dmm_run": 0}),
                ),
            ],
        );
        assert!(suite.ok());
    }

    /// Ask what makes it fail: a short run — a row lost on the way — must
    /// not read as a clean pass, and must say so.
    #[test]
    fn fewer_rows_than_the_daemon_ran_fails_and_warns() {
        let (suite, lines, _) = feed(
            "test_hardware",
            vec![
                hw("Noise floor", true, "-120 dBFS", "< -80 dBFS"),
                frame(
                    "done",
                    json!({"cmd": "test_hardware", "tests_run": 2, "dmm_run": 0}),
                ),
            ],
        );
        assert!(lines
            .contains(&"  warning: the daemon reports 2 tests run, 1 results arrived".to_owned()));
        assert!(!suite.ok());
    }

    #[test]
    fn nothing_ran_is_a_failure_not_an_empty_pass() {
        let (suite, lines, _) = feed(
            "test_hardware",
            vec![frame(
                "done",
                json!({"cmd": "test_hardware", "tests_run": 0}),
            )],
        );
        assert!(lines.contains(&"  no tests ran".to_owned()));
        assert!(!suite.ok());
    }

    #[test]
    fn dmm_rows_are_named_and_counted() {
        let mut row = hw("Absolute level", true, "0.3% error", "< 1% error");
        row.1["dmm"] = true.into();
        let (suite, lines, _) = feed(
            "test_hardware",
            vec![
                row,
                frame(
                    "done",
                    json!({"cmd": "test_hardware", "tests_run": 0, "dmm_run": 1}),
                ),
            ],
        );
        assert_eq!(lines[0], "  pass  Absolute level (DMM)");
        assert!(suite.ok());
    }

    /// `test dut compare`: each tag gets a heading, the prompt is surfaced
    /// once, and a DUT row names what it measured instead of "pass if".
    #[test]
    fn dut_compare_groups_by_tag_and_surfaces_the_prompt() {
        let (suite, lines, prompts) = feed(
            "test_dut",
            vec![
                dut("dut", "Gain", "+0.0 dB", "at 1 kHz"),
                frame(
                    "data",
                    json!({"type": "dut_compare_prompt", "cmd": "test_dut",
                                     "message": "Bypass DUT and press Enter"}),
                ),
                dut("bypass", "Gain", "+0.1 dB", "at 1 kHz"),
                frame(
                    "done",
                    json!({"cmd": "test_dut", "tests_run": 2, "compare": true,
                                     "xruns": 0}),
                ),
            ],
        );
        assert_eq!(prompts, ["Bypass DUT and press Enter"]);
        assert_eq!(
            lines[..7],
            [
                "  With DUT",
                "  pass  Gain (at 1 kHz)",
                "        +0.0 dB",
                "",
                "  Bypass",
                "  pass  Gain (at 1 kHz)",
                "        +0.1 dB",
            ]
        );
        assert!(!lines.iter().any(|l| l.contains("pass if")));
        assert!(lines.contains(&"  2 of 2 pass".to_owned()));
        assert!(suite.ok());
    }

    #[test]
    fn an_error_ends_the_run_as_a_failure_even_without_a_cmd() {
        let (suite, lines, _) = feed(
            "test_dut",
            vec![
                dut("dut", "Gain", "+0.0 dB", "at 1 kHz"),
                frame(
                    "error",
                    json!({"message": "backend does not support port routing"}),
                ),
            ],
        );
        assert_eq!(
            lines.last().unwrap(),
            "  error: backend does not support port routing"
        );
        assert!(!suite.ok());
    }

    #[test]
    fn another_commands_frames_are_ignored() {
        let (suite, lines, _) = feed(
            "test_dut",
            vec![
                hw("Noise floor", false, "x", "y"),
                frame("done", json!({"cmd": "test_hardware", "tests_run": 1})),
            ],
        );
        assert!(lines.is_empty());
        assert!(suite.rows.is_empty() && suite.reported.is_none());
    }

    #[test]
    fn csv_has_one_row_per_check_with_wire_column_names() {
        let dir = std::env::temp_dir().join(format!("ac-619-csv-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.csv");
        let rows = vec![
            dut("dut", "Gain", "+0.0 dB  (ref: a, b)", "at 1 kHz").1,
            dut("bypass", "Gain", "+0.1 dB", "at 1 kHz").1,
        ];
        save_suite_csv(&rows, &path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(
            text,
            "tag,name,pass,detail,tolerance\n\
             dut,Gain,true,\"+0.0 dB  (ref: a, b)\",at 1 kHz\n\
             bypass,Gain,true,+0.1 dB,at 1 kHz\n"
        );
    }
}
