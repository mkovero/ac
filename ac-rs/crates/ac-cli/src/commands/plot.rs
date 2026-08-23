use super::{check_ack, get_cal, level_to_dbfs};
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
/// sentence (#346 AC4 / #353 UX revision 2, since option A′ switched the
/// threshold to a median floor and the old single-line form ran past 80
/// columns at large sample indices). Returns 2 lines: the statistic and
/// margin the threshold used, then whichever of three mutually exclusive
/// facts matters most — a causal bound was enforced, no causal bound
/// could be checked (the `distance` line below already states why: no τ
/// this run), or (#353) no sample cleared the threshold at all, so the
/// returned index is the peak rather than a detected onset. The last case
/// supersedes the bound clause: when nothing moved, whether a bound was
/// known never mattered. Falls back to the full string verbatim if its
/// shape ever changes underneath this, so a reader never sees nothing.
fn short_onset_rule(rule: &str) -> Vec<String> {
    let intro = format!(
        "median floor +{:.0} dB, backward walk",
        ac_core::measurement::sweep::ONSET_FLOOR_MARGIN_DB
    );
    if rule.contains("no sample cleared the threshold") {
        return vec![
            intro,
            "no sample above floor — arrival is peak-derived".to_string(),
        ];
    }
    if let Some(at) = rule.find("causal bound enforced at sample ") {
        let bound = &rule[at + "causal bound enforced at sample ".len()..];
        let bound = bound
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .unwrap_or("");
        return vec![intro, format!("causal bound enforced at sample {bound}")];
    }
    if rule.contains("no causal bound") {
        return vec![intro, "no causal bound (geometry not known)".to_string()];
    }
    vec![rule.to_string()]
}

/// The #283 read-out: arrival, arrival as distance, peak, pre-impulse
/// SNR, and the gate that produced them — decoded from the
/// `measurement/report` frame rather than recomputed off the raw IR
/// frame, so the printed numbers and the archived ones are the same
/// numbers by construction.
fn print_ir_report(report_frame: Option<&serde_json::Value>, cfg: &ac_core::config::Config) {
    use ac_core::measurement::report::{ArrivalDistance, MeasurementReport};

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

    println!(
        "  arrival       {:+} samples  ({:+.3} ms re gate centre @ {} Hz)",
        stats.delay_samples,
        stats.arrival_s * 1000.0,
        stats.sample_rate_hz,
    );
    // #346 AC4 / issue's UX comment: the rule that produced `arrival` must
    // reach the terminal, not stop at the JSON — a reader a year from now
    // must be able to tell a geometry-checked onset from a best-effort
    // one from the same line a human actually looks at. Printed as a
    // short derived tag rather than `onset_rule` verbatim (the UX
    // comment's own worst-case-width check found the full sentence runs
    // past 80 columns at this indent); the untruncated rule still rides
    // the persisted JSON via `IrStats::onset_rule`.
    let onset_lines = short_onset_rule(&stats.onset_rule);
    println!("                onset: {}", onset_lines[0]);
    let continuation_indent = " ".repeat("                onset: ".len());
    for line in &onset_lines[1..] {
        println!("{continuation_indent}{line}");
    }
    // The AC's load-bearing case: without a τ measured under this run's
    // exact conditions there is no distance, and the reason is printed
    // rather than the arrival being quietly reused as one.
    match report.ir_arrival_distance() {
        ArrivalDistance::Known {
            distance_m,
            speed_of_sound_m_s,
            provenance,
            ..
        } => {
            println!("  distance      {distance_m:.3} m  (c = {speed_of_sound_m_s:.1} m/s)");
            println!("                {provenance}");
        }
        ArrivalDistance::Unavailable { reason } => {
            println!("  distance      unavailable — {reason}");
        }
    }
    println!(
        "  peak          {:.4} FS  ({:+.2} dB re unity)  at sample {}  (diagnostic \u{2014} not arrival)",
        stats.peak_magnitude,
        20.0 * stats.peak_magnitude.max(1e-12).log10(),
        stats.peak_index,
    );
    // #353: `pre_impulse_snr_db` stays defined against the RMS floor
    // (unchanged, #346 architect review), but `onset` above now uses a
    // separate median floor — the two live three lines apart under the
    // same heading, so a reader who assumes they match is misled on any
    // contaminated capture (this SNR reads low there; it keeps the
    // inflated RMS floor by design).
    if stats.pre_impulse_snr_db.is_finite() {
        println!(
            "  pre-imp SNR   {:.1} dB  (re RMS floor; onset uses median)",
            stats.pre_impulse_snr_db
        );
    } else {
        println!("  pre-imp SNR   no measurable pre-impulse floor (silence)");
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
/// spawn is gone (see `attic/ac-ui`); `Monitor` now always renders via the
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

    /// QA (PR #377): `short_onset_rule`'s breakdown branch had no test
    /// naming its output — a typo in the `.contains("no sample cleared
    /// the threshold")` match here, or in the string it matches against
    /// in `ac_core::measurement::sweep::estimate_onset`, would fall
    /// through to the bound-clause branch instead, silently dropping the
    /// operator-facing warning at the terminal.
    #[test]
    fn short_onset_rule_surfaces_the_breakdown_line() {
        let rule = "backward threshold from floor (12 dB above pre-impulse median magnitude), \
                    no causal bound (geometry not known for this capture); no sample cleared \
                    the threshold — index is the peak, not an onset";
        let lines = short_onset_rule(rule);
        assert_eq!(
            lines,
            vec![
                "median floor +12 dB, backward walk".to_string(),
                "no sample above floor — arrival is peak-derived".to_string(),
            ]
        );
    }

    /// Companion case: the breakdown clause must supersede the bound
    /// clause even when a causal bound *was* enforced (per `sweep.rs`'s
    /// doc comment, the breakdown suffix only appears when the walk never
    /// moved at all, which makes the bound clause moot either way).
    #[test]
    fn short_onset_rule_breakdown_supersedes_bound_clause() {
        let rule = "backward threshold from floor (12 dB above pre-impulse median magnitude), \
                    causal bound enforced at sample 42; no sample cleared the threshold — \
                    index is the peak, not an onset";
        let lines = short_onset_rule(rule);
        assert_eq!(
            lines,
            vec![
                "median floor +12 dB, backward walk".to_string(),
                "no sample above floor — arrival is peak-derived".to_string(),
            ]
        );
    }

    #[test]
    fn short_onset_rule_reports_enforced_bound() {
        let rule = "backward threshold from floor (12 dB above pre-impulse median magnitude), \
                    causal bound enforced at sample 42";
        let lines = short_onset_rule(rule);
        assert_eq!(
            lines,
            vec![
                "median floor +12 dB, backward walk".to_string(),
                "causal bound enforced at sample 42".to_string(),
            ]
        );
    }
}
