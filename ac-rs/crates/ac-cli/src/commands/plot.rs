use super::{check_ack, get_cal, level_to_dbfs, print_level_clamp, print_level_clamp_range};
use crate::client::AcClient;
use crate::io;
use crate::parse::CommandKind;

pub fn run(
    cmd: &CommandKind,
    cfg: &ac_core::config::Config,
    client: &mut AcClient,
    show_plot: bool,
) {
    let (start, stop, level, ppd, bpo) = match cmd {
        CommandKind::Plot {
            start,
            stop,
            level,
            ppd,
            bpo,
        } => (*start, *stop, level, *ppd, *bpo),
        _ => unreachable!(),
    };

    let cal = get_cal(client);
    let have_cal = cal.is_some();
    if have_cal {
        println!("  Loaded calibration from server.");
    } else {
        println!("  No calibration found \u{2014} levels in dBFS only.");
    }
    let level_db = level_to_dbfs(level, cal.as_ref());

    let start_hz = start.unwrap_or(cfg.range_start_hz);
    let stop_hz = stop.unwrap_or(cfg.range_stop_hz);

    println!(
        "\n  Plot: {start_hz:.0} \u{2192} {stop_hz:.0} Hz  {} pts/decade  |  {level_db:.1} dBFS",
        ppd
    );
    io::print_freq_header(have_cal);

    let mut cmd_json = serde_json::json!({
        "cmd": "plot",
        "start_hz": start_hz,
        "stop_hz": stop_hz,
        "level_dbfs": level_db,
        "ppd": ppd,
    });
    if let Some(b) = bpo {
        cmd_json["bpo"] = serde_json::json!(b);
    }
    let ack = check_ack(client.send_cmd(&cmd_json, None), "plot");
    let applied_db = ack
        .get("level_dbfs")
        .and_then(|v| v.as_f64())
        .unwrap_or(level_db);
    print_level_clamp(level_db, applied_db);
    if let (Some(out), Some(inp)) = (
        ack.get("out_port").and_then(|v| v.as_str()),
        ack.get("in_port").and_then(|v| v.as_str()),
    ) {
        println!("  Output: {out}  \u{2192}  Input: {inp}");
    }

    // Spawn the UI only after the daemon ACKed the request — otherwise a
    // refused command (busy daemon, invalid args) flashes a window that
    // immediately disconnects.
    if show_plot {
        launch_ui(LaunchKind::SweepFreq, cfg, None);
    }

    let results = collect_sweep(client, "plot");
    if results.is_empty() {
        return;
    }
    io::print_summary(&results, "DUT", have_cal);
    save_results(&results, "plot", cfg);
}

pub fn run_level(
    cmd: &CommandKind,
    cfg: &ac_core::config::Config,
    client: &mut AcClient,
    show_plot: bool,
) {
    let (start, stop, freq, steps) = match cmd {
        CommandKind::PlotLevel {
            start,
            stop,
            freq,
            steps,
        } => (start, stop, *freq, *steps),
        _ => unreachable!(),
    };

    let cal = get_cal(client);
    let have_cal = cal.is_some();
    if have_cal {
        println!("  Loaded calibration from server.");
    } else {
        println!("  No calibration found \u{2014} levels in dBFS only.");
    }
    let start_db = level_to_dbfs(start, cal.as_ref());
    let stop_db = level_to_dbfs(stop, cal.as_ref());

    println!(
        "\n  Plot level: {start_db:.1} \u{2192} {stop_db:.1} dBFS  {freq:.0} Hz  |  {steps} steps"
    );
    io::print_freq_header(have_cal);

    let ack = check_ack(
        client.send_cmd(
            &serde_json::json!({
                "cmd": "plot_level",
                "freq_hz": freq,
                "start_dbfs": start_db,
                "stop_dbfs": stop_db,
                "steps": steps,
            }),
            None,
        ),
        "plot_level",
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
    if let (Some(out), Some(inp)) = (
        ack.get("out_port").and_then(|v| v.as_str()),
        ack.get("in_port").and_then(|v| v.as_str()),
    ) {
        println!("  Output: {out}  \u{2192}  Input: {inp}");
    }

    if show_plot {
        launch_ui(LaunchKind::SweepLevel, cfg, None);
    }

    let results = collect_sweep(client, "plot_level");
    if results.is_empty() {
        return;
    }
    io::print_summary(&results, "DUT", have_cal);
    save_results(&results, "plot_level", cfg);
}

/// `ac plot ir` — Farina log-sweep impulse response (#282; moved from
/// `ac sweep ir`). Unlike `sweep_level`/`sweep_frequency`, this command
/// captures and analyses — it now shares `plot`'s calibration-line
/// convention and, unlike the old `sweep ir` (which called
/// `generate::wait_for_stop` and printed nothing else), actually reads the
/// `measurement/impulse_response` and `measurement/report` frames the
/// daemon already publishes.
pub fn run_ir(cmd: &CommandKind, cfg: &ac_core::config::Config, client: &mut AcClient) {
    let (f1, f2, duration, level, n_harmonics, window_len, tail_s) = match cmd {
        CommandKind::PlotIr {
            f1,
            f2,
            duration,
            level,
            n_harmonics,
            window_len,
            tail_s,
        } => (
            *f1,
            *f2,
            *duration,
            level,
            *n_harmonics,
            *window_len,
            *tail_s,
        ),
        _ => unreachable!(),
    };

    let cal = get_cal(client);
    let have_cal = cal.is_some();
    if have_cal {
        println!("  Loaded calibration from server.");
    } else {
        println!("  No calibration found \u{2014} levels in dBFS only.");
    }
    let level_db = level_to_dbfs(level, cal.as_ref());

    let gate = format!(
        "{} harmonics, {} window, {} tail",
        n_harmonics
            .map(|v| v.to_string())
            .unwrap_or_else(|| "default".into()),
        window_len
            .map(|v| format!("{v}-sample"))
            .unwrap_or_else(|| "default".into()),
        tail_s
            .map(|v| format!("{v:.2}s"))
            .unwrap_or_else(|| "default".into()),
    );
    println!(
        "\n  IR: {f1:.0} \u{2192} {f2:.0} Hz  |  {level_db:.1} dBFS  |  {duration:.1}s  |  {gate}"
    );

    let mut cmd_json = serde_json::json!({
        "cmd": "plot_ir",
        "f1_hz": f1,
        "f2_hz": f2,
        "duration": duration,
        "level_dbfs": level_db,
    });
    if let Some(v) = n_harmonics {
        cmd_json["n_harmonics"] = serde_json::json!(v);
    }
    if let Some(v) = window_len {
        cmd_json["window_len"] = serde_json::json!(v);
    }
    if let Some(v) = tail_s {
        cmd_json["tail_s"] = serde_json::json!(v);
    }

    let ack = check_ack(client.send_cmd(&cmd_json, None), "plot_ir");
    let applied_db = ack
        .get("level_dbfs")
        .and_then(|v| v.as_f64())
        .unwrap_or(level_db);
    print_level_clamp(level_db, applied_db);
    if let Some(p) = ack.get("out_port").and_then(|v| v.as_str()) {
        println!("  Output: {p}");
    }
    println!("  Running IR measurement...\n");

    let (ir_frame, report_frame) = collect_ir(client, "plot_ir");
    print_ir_result(ir_frame.as_ref(), report_frame.as_ref(), duration, tail_s);
    print_ir_report(report_frame.as_ref(), cfg);
    print_ir_notes(report_frame.as_ref());
}

/// Derives the short terminal tag from `IrStats::onset_rule`'s full
/// sentence (#346 AC4, revised for #378's picker). Two facts a reader
/// needs a year later: which window the pick was made over, and whether
/// the pick landed inside it or on its edge — a pick sitting on the
/// window start is a stable, repeatable, possibly wrong number, and it
/// has to be visible on the line rather than inferable from the JSON.
///
/// Returns 2 lines normally and 3 when the pick is pinned to the window
/// start or when the picker declined. On a decline the second line is
/// the degenerate case named in `rule`, printed verbatim from between
/// its parentheses, so a case added in `ac-core` later reaches the
/// terminal without a change here. Falls back to the full string
/// verbatim if its shape ever changes underneath this, so a reader never
/// sees nothing.
fn short_onset_rule(rule: &str, onset_index: usize) -> Vec<String> {
    if rule.contains("picker declined") {
        let case = rule
            .split_once('(')
            .and_then(|(_, rest)| rest.split_once(')'))
            .map(|(inside, _)| inside.to_string());
        let mut lines =
            vec!["picker declined \u{2014} arrival is the peak, not an onset".to_string()];
        if let Some(case) = case {
            lines.push(case);
        }
        lines.push("check: gate length, peak position in gate".to_string());
        return lines;
    }
    let Some(start) = rule.find("window start at sample ").map(|at| {
        rule[at + "window start at sample ".len()..]
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .unwrap_or("")
            .to_string()
    }) else {
        return vec![rule.to_string()];
    };
    let limit = if rule.contains("search span is the tighter limit") {
        "search span"
    } else if rule.contains("causal bound enforced") {
        "causal bound"
    } else if rule.contains("no causal bound") {
        "search span, no geometry known"
    } else {
        return vec![rule.to_string()];
    };
    let intro = format!(
        "AIC change-point pick, {:.1} ms window",
        ac_core::measurement::sweep::ONSET_SEARCH_WINDOW_S * 1000.0
    );
    let pinned = rule.contains("pick landed on the window start");
    let clear = start
        .parse::<usize>()
        .map(|s| onset_index.saturating_sub(s))
        .unwrap_or(0);
    let mut lines = vec![
        intro,
        if pinned {
            format!("window start {start} ({limit}), pick ON start")
        } else {
            format!("window start {start} ({limit}), pick {clear} clear")
        },
    ];
    if pinned {
        lines.push("onset may lie earlier than the window allows".to_string());
    }
    lines
}

/// The read-out: arrival (samples and ms, re gate centre), peak,
/// pre-impulse SNR, and the gate that produced them — decoded from the
/// `measurement/report` frame rather than recomputed off the raw IR
/// frame, so the printed numbers and the archived ones are the same
/// numbers by construction. No distance figure — #391 removed the
/// ms → m conversion this used to also print.
fn print_ir_report(report_frame: Option<&serde_json::Value>, cfg: &ac_core::config::Config) {
    use ac_core::measurement::report::{IrVerdict, MeasurementReport, PRE_IMPULSE_SNR_MIN_DB};

    let Some(value) = report_frame.and_then(|f| f.get("report")) else {
        eprintln!("  !! no measurement/report frame — nothing to summarise");
        return;
    };
    let report: MeasurementReport = match serde_json::from_value(value.clone()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("  !! could not decode report: {e}");
            return;
        }
    };
    let Some(stats) = report.ir_stats() else {
        eprintln!("  !! report carries no impulse-response payload to summarise");
        return;
    };

    // A capture whose peak cannot be trusted (#376) is reported as
    // failed, not as a result with a number in it: no arrival line —
    // that is the exact plausible-looking wrong-number shape the issue
    // exists to close.
    if let IrVerdict::Failed { reason } = &stats.verdict {
        println!("  DECONVOLUTION FAILED \u{2014} {reason}");
        println!("                check: drive level, mic gain, distance, room noise");
        println!();
    } else {
        println!(
            "  arrival       {:+} samples  ({:+.3} ms re gate centre @ {} Hz)",
            stats.delay_samples,
            stats.arrival_s * 1000.0,
            stats.sample_rate_hz,
        );
        // #346 AC4 / #378's UX comment: the rule that produced `arrival`
        // must reach the terminal, not stop at the JSON — a reader a year
        // from now must be able to tell a pick found in the signal from
        // one pinned by the search bracket, from the same line a human
        // actually looks at. Printed as a short derived tag rather than
        // `onset_rule` verbatim (the full sentence runs past 80 columns at
        // this indent); the untruncated rule still rides the persisted
        // JSON via `IrStats::onset_rule`.
        let onset_lines = short_onset_rule(&stats.onset_rule, stats.onset_index);
        println!("                onset: {}", onset_lines[0]);
        let continuation_indent = " ".repeat("                onset: ".len());
        for line in &onset_lines[1..] {
            println!("{continuation_indent}{line}");
        }
    }
    println!(
        "  peak          {:.4} FS  ({:+.2} dB re unity)  at sample {}",
        stats.peak_magnitude,
        20.0 * stats.peak_magnitude.max(1e-12).log10(),
        stats.peak_index,
    );
    if matches!(stats.verdict, IrVerdict::Failed { .. }) {
        println!("                diagnostic only \u{2014} not a valid arrival");
    } else if stats.onset_index < stats.peak_index {
        // #378: the onset-to-peak distance is the quantity the issue is
        // fought over (110.4 samples at 1.000 m, 92.2 at 3.000 m). An
        // operator who moves the mic sees the estimator's distance bias,
        // or its absence, in one place instead of doing arithmetic across
        // two lines with different origins. Suppressed when the two
        // numbers agree — then the peak is the arrival and saying so once,
        // on the onset line, is enough.
        println!(
            "                diagnostic \u{2014} arrival is onset-derived, {} samples earlier",
            stats.peak_index - stats.onset_index,
        );
    }
    if stats.pre_impulse_snr_db.is_finite() {
        if matches!(stats.verdict, IrVerdict::Failed { .. }) {
            println!(
                "  pre-imp SNR   {:.1} dB  (required \u{2265} {:.1} dB, threshold set from rig data)",
                stats.pre_impulse_snr_db, PRE_IMPULSE_SNR_MIN_DB,
            );
        } else {
            println!("  pre-imp SNR   {:.1} dB", stats.pre_impulse_snr_db);
        }
    } else if let IrVerdict::Failed { reason } = &stats.verdict {
        // Non-finite here means `ir_stats` had nothing to measure a floor
        // from at all (see the reason already printed in the banner
        // above) — restate it rather than a generic "silence" that would
        // misdescribe a zero-peak or guard-band-exhausted capture alike.
        println!("  pre-imp SNR   {reason}");
    } else {
        // Non-finite but `Ok`: a zero floor against a nonzero peak is the
        // best possible capture, not an unmeasurable one.
        println!("  pre-imp SNR   \u{221e} dB  (zero measured floor)");
    }
    println!(
        "  gate          {} window, {} samples ({:.2} ms) → f_low {:.1} Hz",
        stats.gate_window_kind,
        stats.window_len,
        stats.gate_window_s * 1000.0,
        stats.gate_f_low_hz,
    );

    if let Some(dir) = cfg.report_dir.as_ref() {
        let stem = report.timestamp_utc.replace(':', "-");
        println!(
            "  report        {}",
            dir.join(format!("{stem}-plot_ir.json")).display()
        );
        println!(
            "  csv           {}",
            dir.join(format!("{stem}-plot_ir.csv")).display()
        );
    } else {
        eprintln!("  note: report_dir not configured — result not persisted (see `ac setup`)");
    }
}

/// Wait for `plot_ir`'s DATA frames: `measurement/impulse_response` and
/// `measurement/report` ride their own topics (not wrapped in a generic
/// `data` topic the way `plot`/`plot_level` per-point frames are), so this
/// mirrors `collect_sweep` but keys off the topic string directly.
fn collect_ir(
    client: &mut AcClient,
    cmd_name: &str,
) -> (Option<serde_json::Value>, Option<serde_json::Value>) {
    let mut ir_frame = None;
    let mut report_frame = None;
    loop {
        let frame = match client.recv_data(300_000) {
            Some(f) => f,
            None => {
                eprintln!("\n  error: timeout waiting for {cmd_name} data");
                break;
            }
        };
        let (topic, data) = frame;
        match topic.as_str() {
            "measurement/impulse_response" => ir_frame = Some(data),
            "measurement/report" => report_frame = Some(data),
            "done" => break,
            "error" => {
                let msg = data
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("error");
                eprintln!("\n  !! {msg}");
                break;
            }
            _ => {}
        }
    }
    (ir_frame, report_frame)
}

fn print_ir_result(
    ir_frame: Option<&serde_json::Value>,
    report_frame: Option<&serde_json::Value>,
    duration: f64,
    tail_s: Option<f64>,
) {
    let Some(data) = ir_frame.and_then(|f| f.get("data")) else {
        eprintln!("  !! no impulse response received");
        return;
    };
    // Peak, arrival and gate now come from the report frame via
    // `print_ir_report` (#283) — reading them off the raw IR frame here
    // as well would let the printed and archived numbers drift apart.
    if let Some(n) = data
        .get("harmonics")
        .and_then(|v| v.as_array())
        .map(Vec::len)
    {
        println!("  harmonics     {n} order(s) extracted");
    }
    // `tail_s` unset on the CLI side means "daemon default" (0.5s per
    // ZMQ.md's `plot_ir` request) — report the nominal figure either way,
    // the report `notes` line below carries the measured decay verdict.
    let tail = tail_s.unwrap_or(0.5);
    println!(
        "  captured      {:.2}s  ({duration:.2}s sweep + {tail:.2}s tail)",
        duration + tail
    );
    let _ = report_frame;
}

/// The report's `notes`: the ISO 18233 §6.3.2 measured tail-decay verdict
/// and the §B.5 linear-deconvolution artefact statement, one line each.
/// Printed last, and printed verbatim from the report so the operator
/// reads exactly what the archive records (#283).
fn print_ir_notes(report_frame: Option<&serde_json::Value>) {
    let Some(notes) = report_frame
        .and_then(|f| f.get("report"))
        .and_then(|r| r.get("notes"))
        .and_then(|v| v.as_str())
    else {
        return;
    };
    println!();
    for line in notes.lines() {
        println!("  {line}");
    }
}

fn collect_sweep(client: &mut AcClient, cmd_name: &str) -> Vec<serde_json::Value> {
    let mut results = Vec::new();

    loop {
        let frame = match client.recv_data(300_000) {
            Some(f) => f,
            None => {
                eprintln!("\n  error: timeout waiting for {cmd_name} data");
                break;
            }
        };
        let (topic, data) = frame;

        if topic == "data" {
            if data.get("type").and_then(|v| v.as_str())
                == Some("measurement/frequency_response/point")
            {
                io::print_freq_row(&data);
                results.push(data);
            }
        } else if topic == "done" {
            if let Some(xruns) = data.get("xruns").and_then(|v| v.as_u64()) {
                if xruns > 0 {
                    println!("\n  !! {xruns} xrun(s) during {cmd_name}");
                }
            }
            break;
        } else if topic == "error" {
            let msg = data
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("error");
            eprintln!("\n  !! {msg}");
            break;
        }
    }
    results
}

fn save_results(results: &[serde_json::Value], label: &str, cfg: &ac_core::config::Config) {
    let dir = io::output_dir(cfg);
    let ts = io::timestamp();
    let safe = label.replace(' ', "_");
    let path = dir.join(format!("{safe}_{ts}.csv"));
    io::save_csv(results, &path);
}

/// What `launch_ui` should do post-command. The GPU viewer this used to
/// spawn is gone; `Monitor` now always renders via the
/// terminal (`monitor_tui`), and the sweep variants just note that no
/// visual plot is shown — the CSV/stdout output already carries the data.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LaunchKind {
    /// Frequency sweep (paired with `ac plot ... show`).
    SweepFreq,
    /// Level sweep (paired with `ac plot level ... show`).
    SweepLevel,
    /// Live monitor view.
    Monitor,
}

pub(crate) fn launch_ui(kind: LaunchKind, cfg: &ac_core::config::Config, channels: Option<&[u32]>) {
    match kind {
        LaunchKind::Monitor => run_tui_fallback(cfg, channels),
        LaunchKind::SweepFreq | LaunchKind::SweepLevel => {
            eprintln!("  note: no visual plot display available — see CSV/stdout output above");
        }
    }
}

fn run_tui_fallback(cfg: &ac_core::config::Config, channels: Option<&[u32]>) {
    let chs: Vec<u32> = channels.map(|s| s.to_vec()).unwrap_or_default();
    if let Err(e) = super::monitor_tui::run(cfg, &chs) {
        eprintln!("  monitor: tui error: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::short_onset_rule;

    /// QA (PR #377), carried into #378: `short_onset_rule`'s decline
    /// branch had no test naming its output — a typo in the
    /// `.contains("picker declined")` match here, or in the string it
    /// matches against in `ac_core::measurement::sweep::estimate_onset`,
    /// would fall through to the window-clause branch instead, silently
    /// dropping the operator-facing warning at the terminal.
    #[test]
    fn short_onset_rule_surfaces_the_decline_line() {
        let rule = "onset picker declined (search window shorter than 2 samples) — index is \
                    the peak, not an onset";
        let lines = short_onset_rule(rule, 1479);
        assert_eq!(
            lines,
            vec![
                "picker declined — arrival is the peak, not an onset".to_string(),
                "search window shorter than 2 samples".to_string(),
                "check: gate length, peak position in gate".to_string(),
            ]
        );
    }

    /// A degenerate case added in `ac-core` later must reach the terminal
    /// without a change here: the parenthetical is printed verbatim, not
    /// matched against a list.
    #[test]
    fn short_onset_rule_prints_an_unknown_decline_case_verbatim() {
        let rule = "onset picker declined (a case invented by this test) — index is the peak, \
                    not an onset";
        let lines = short_onset_rule(rule, 1479);
        assert_eq!(lines[1], "a case invented by this test".to_string());
    }

    #[test]
    fn short_onset_rule_reports_the_window_start_and_how_clear_the_pick_is() {
        let rule = "AIC change-point pick over a 10.0 ms window; window start at sample 1305, \
                    causal bound enforced";
        let lines = short_onset_rule(rule, 1369);
        assert_eq!(
            lines,
            vec![
                "AIC change-point pick, 10.0 ms window".to_string(),
                "window start 1305 (causal bound), pick 64 clear".to_string(),
            ]
        );
    }

    #[test]
    fn short_onset_rule_names_the_search_span_when_no_geometry_is_known() {
        let rule = "AIC change-point pick over a 10.0 ms window; window start at sample 455, \
                    no causal bound (geometry not known for this capture)";
        let lines = short_onset_rule(rule, 519);
        assert_eq!(
            lines[1],
            "window start 455 (search span, no geometry known), pick 64 clear".to_string()
        );
    }

    /// A causal bound that did not set the window start must not read as
    /// though it did — the operator's question is which limit the pick is
    /// pinned against.
    #[test]
    fn short_onset_rule_names_the_search_span_when_the_bound_does_not_bind() {
        let rule = "AIC change-point pick over a 10.0 ms window; window start at sample 455, \
                    causal bound enforced at sample 10, search span is the tighter limit";
        let lines = short_onset_rule(rule, 519);
        assert_eq!(
            lines[1],
            "window start 455 (search span), pick 64 clear".to_string()
        );
    }

    /// The one case that costs a third line: a pick sitting on the window
    /// start is a confident number that may be an artefact of the search
    /// bounds, and it has to be distinguishable at a glance in a
    /// scrollback of many runs.
    #[test]
    fn short_onset_rule_flags_a_pick_pinned_to_the_window_start() {
        let rule = "AIC change-point pick over a 10.0 ms window; window start at sample 1305, \
                    causal bound enforced; pick landed on the window start — the true onset \
                    may lie earlier";
        let lines = short_onset_rule(rule, 1305);
        assert_eq!(
            lines,
            vec![
                "AIC change-point pick, 10.0 ms window".to_string(),
                "window start 1305 (causal bound), pick ON start".to_string(),
                "onset may lie earlier than the window allows".to_string(),
            ]
        );
    }

    /// Every line this function can emit must fit the 80-column budget at
    /// the 16- and 23-column indents `print_ir_report` uses, at a 6-digit
    /// sample index — the width check the #378 UX comment ran by hand.
    #[test]
    fn short_onset_rule_lines_fit_eighty_columns() {
        let mut rules = vec![
            "AIC change-point pick over a 10.0 ms window; window start at sample 262144, \
             causal bound enforced; pick landed on the window start — the true onset may \
             lie earlier"
                .to_string(),
            "AIC change-point pick over a 10.0 ms window; window start at sample 262144, \
             causal bound enforced at sample 9, search span is the tighter limit"
                .to_string(),
        ];
        // Every decline case `estimate_onset` can name, so a new one that
        // does not fit is caught here rather than at a rig terminal.
        for case in [
            "search window shorter than 2 samples",
            "zero variance in the search window",
            "peak at sample 0",
            "nothing in the search window above the pre-impulse floor",
            "no change point earlier than the peak in the window",
        ] {
            rules.push(format!(
                "onset picker declined ({case}) — index is the peak, not an onset"
            ));
        }
        for rule in &rules {
            for (i, line) in short_onset_rule(rule, 262_144).iter().enumerate() {
                let indent = if i == 0 { 16 + "onset: ".len() } else { 23 };
                assert!(
                    indent + line.chars().count() <= 80,
                    "line {:?} runs to {} columns",
                    line,
                    indent + line.chars().count()
                );
            }
        }
    }
}
