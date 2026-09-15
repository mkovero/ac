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

    let (results, outcome) = collect_sweep(client, "plot");
    if outcome != SweepOutcome::Done || results.is_empty() {
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

    let (results, outcome) = collect_sweep(client, "plot_level");
    if outcome != SweepOutcome::Done || results.is_empty() {
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
    let (f1, f2, duration, level, n_harmonics, window_len, tail_s, distance_m) = match cmd {
        CommandKind::PlotIr {
            f1,
            f2,
            duration,
            level,
            n_harmonics,
            window_len,
            tail_s,
            distance_m,
        } => (
            *f1,
            *f2,
            *duration,
            level,
            *n_harmonics,
            *window_len,
            *tail_s,
            *distance_m,
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
    // #460 UX: echo the typed distance before emission, so a token typo
    // (`0.8m` meant as `0.8s`) is visible before the result, and state its
    // absence rather than hide it.
    match distance_m {
        Some(d) => println!("  distance      {d} m"),
        None => println!("  distance      not given"),
    }

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
    if let Some(v) = distance_m {
        cmd_json["distance_m"] = serde_json::json!(v);
    }

    let ack = check_ack(client.send_cmd(&cmd_json, None), "plot_ir");
    let applied_db = ack
        .get("level_dbfs")
        .and_then(|v| v.as_f64())
        .unwrap_or(level_db);
    print_level_clamp(level_db, applied_db);
    let out_port = ack.get("out_port").and_then(|v| v.as_str());
    if let Some(p) = out_port {
        println!("  Output:       {p}");
    }
    // #460 UX: every port the sweep leaves through or is referenced against,
    // printed before the result. A reference output equal to the main output
    // drives nothing extra, so it is not named twice.
    if let Some(p) = ack.get("ref_out_port").and_then(|v| v.as_str()) {
        if Some(p) != out_port {
            println!("  Also driven:  {p}");
        }
    }
    if let Some(p) = ack.get("ref_in_port").and_then(|v| v.as_str()) {
        println!("  Ref input:    {p}");
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
/// Returns 3 lines normally (intro, window start, and the causal-bound row
/// built from `bound`'s own fields, #460) and 4 when the pick is pinned to
/// the window start. On a decline it returns the decline line, the case, and
/// a `check:` line — plus the bound row when the bound itself caused it. On a decline the second line is
/// the degenerate case named in `rule`, printed verbatim from between
/// its parentheses, so a case added in `ac-core` later reaches the
/// terminal without a change here. Falls back to the full string
/// verbatim if its shape ever changes underneath this, so a reader never
/// sees nothing.
fn short_onset_rule(
    rule: &str,
    onset_index: usize,
    bound: &ac_core::measurement::sweep::CausalBound,
) -> Vec<String> {
    if rule.contains("picker declined") {
        let case = rule
            .split_once('(')
            .and_then(|(_, rest)| rest.split_once(')'))
            .map(|(inside, _)| inside.to_string());
        let mut lines = vec!["picker declined \u{2014} no onset estimate".to_string()];
        let bound_caused_it = case.as_deref() == Some("causal bound at or after the peak");
        if let Some(case) = case {
            lines.push(case);
        }
        if bound_caused_it {
            // #460: the bound's own inputs put it there, so those are what
            // to check, not the gate.
            lines.push(causal_bound_row(bound));
            lines.push("check: typed distance, reference loopback routing".to_string());
        } else {
            lines.push("check: gate length, peak position in gate".to_string());
        }
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
        "search span"
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
    // #460 AC3 / UX row 3: what the bound was built from, or which input
    // it lacked — always on the same row, so the eye finds the answer there.
    lines.push(causal_bound_row(bound));
    if pinned {
        lines.push("onset may lie earlier than the window allows".to_string());
    }
    lines
}

/// Onset block row 3 (#460 UX), printed from the bound's own fields rather
/// than parsed from `onset_rule`. An assumed speed of sound is said so: an
/// unset temperature silently uses the default `c`.
fn causal_bound_row(bound: &ac_core::measurement::sweep::CausalBound) -> String {
    use ac_core::measurement::sweep::{CausalBound, MissingBoundInput};
    match bound {
        CausalBound::Enforced { inputs, .. } => {
            let c = match inputs.temperature_c {
                Some(t) => format!("c {:.1} m/s at {t:.1} \u{b0}C", inputs.speed_of_sound_m_s),
                None => format!("c {:.1} m/s assumed", inputs.speed_of_sound_m_s),
            };
            format!("bound from ref latency + {} m, {c}", inputs.distance_m)
        }
        CausalBound::Unavailable(MissingBoundInput::Distance) => {
            "no causal bound \u{2014} distance not given (token: 1m)".to_string()
        }
        CausalBound::Unavailable(MissingBoundInput::ReferenceLatency { .. }) => {
            "no causal bound \u{2014} ref latency unavailable (below)".to_string()
        }
        CausalBound::Unavailable(MissingBoundInput::Both { .. }) => {
            "no causal bound \u{2014} no distance, ref latency unavailable".to_string()
        }
    }
}

/// The `ref latency` read-out (#460 UX), always printed: the same-capture
/// reference τ in `calibrate`'s `Delay:` format, or its unavailable reason
/// with any `; check: ` part on its own `check:` line.
fn reference_latency_lines(
    reference: Option<&ac_core::measurement::report::ReferenceLatency>,
    sample_rate_hz: u32,
) -> Vec<String> {
    use ac_core::measurement::report::ReferenceLatency;
    let unavailable = |reason: &str| match reason.split_once("; check: ") {
        Some((observation, places)) => vec![
            format!("  ref latency   unavailable \u{2014} {observation}"),
            format!("                check: {places}"),
        ],
        None => vec![format!("  ref latency   unavailable \u{2014} {reason}")],
    };
    match reference {
        Some(ReferenceLatency::Measured(m)) => {
            let samples = m.tau_s * sample_rate_hz as f64;
            let samples_txt = if (samples - samples.round()).abs() < 0.01 {
                format!("{}", samples.round() as i64)
            } else {
                format!("{samples:.1}")
            };
            let snr = m
                .pre_impulse_snr_db
                .map(|v| format!("{v:.1} dB"))
                .unwrap_or_else(|| "\u{221e} dB".to_string());
            vec![format!(
                "  ref latency   {:.4} ms  ({samples_txt} samples, SNR {snr}, same capture)",
                m.tau_s * 1000.0
            )]
        }
        Some(ReferenceLatency::Unavailable { reason }) => unavailable(reason),
        None => unavailable("not recorded (report predates schema v7)"),
    }
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
    }
    println!(
        "  peak          {:.4} FS  ({:+.2} dB re unity)  at sample {}",
        stats.peak_magnitude,
        20.0 * stats.peak_magnitude.max(1e-12).log10(),
        stats.peak_index,
    );
    if matches!(stats.verdict, IrVerdict::Failed { .. }) {
        println!("                diagnostic only \u{2014} not a valid arrival");
    } else {
        // `arrival` is the peak's offset (#378 contingency, AC6 rig run
        // 2026-09-15); the onset is printed under the peak it is measured
        // against, as a diagnostic. #346 AC4's requirement still holds for
        // it: the rule that produced the onset reaches the terminal, not
        // only the JSON. Printed as a short derived tag rather than
        // `onset_rule` verbatim (the full sentence runs past 80 columns at
        // this indent); the untruncated rule still rides the persisted
        // JSON via `IrStats::onset_rule`.
        let onset_lines =
            short_onset_rule(&stats.onset_rule, stats.onset_index, &stats.causal_bound);
        println!("                onset: {}", onset_lines[0]);
        let continuation_indent = " ".repeat("                onset: ".len());
        for line in &onset_lines[1..] {
            println!("{continuation_indent}{line}");
        }
        // #378: the onset-to-peak distance is the quantity AC6 found moving
        // with position (492.5 samples at 1.000 m, 627.6 at 2.000 m on the
        // rig). Printed in one place so an operator who moves the mic sees
        // it move, and labelled so it cannot be read as the arrival.
        if stats.onset_index < stats.peak_index {
            println!(
                "                diagnostic \u{2014} onset {} samples before peak, not the arrival",
                stats.peak_index - stats.onset_index,
            );
        }
    }
    for line in reference_latency_lines(report.reference_latency.as_ref(), stats.sample_rate_hz) {
        println!("{line}");
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

/// Whether a sweep reached its terminal `done` frame. Anything else — a
/// terminal `error` (analyzer failure, #428) or a timeout — leaves
/// `results` holding only a prefix that must never be treated as a
/// complete artifact: no summary printed, no CSV written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SweepOutcome {
    Done,
    Failed,
}

fn collect_sweep(client: &mut AcClient, cmd_name: &str) -> (Vec<serde_json::Value>, SweepOutcome) {
    collect_sweep_frames(|| client.recv_data(300_000), cmd_name)
}

/// Core of `collect_sweep`, generic over the frame source so the
/// atomic-failure gating (a terminal `error` must never leave `outcome ==
/// Done`, however many `measurement/frequency_response/point` frames
/// preceded it) can be unit-tested without a real `AcClient`/socket —
/// see `tests::error_after_points_is_failed_not_done` below (#428 QA).
fn collect_sweep_frames(
    mut next_frame: impl FnMut() -> Option<(String, serde_json::Value)>,
    cmd_name: &str,
) -> (Vec<serde_json::Value>, SweepOutcome) {
    let mut results = Vec::new();
    let mut outcome = SweepOutcome::Failed;

    loop {
        let frame = match next_frame() {
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
            outcome = SweepOutcome::Done;
            break;
        } else if topic == "error" {
            let msg = data
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("error");
            let partial = match (
                data.get("requested_points").and_then(|v| v.as_u64()),
                data.get("completed_points").and_then(|v| v.as_u64()),
            ) {
                (Some(requested), Some(completed)) => {
                    format!(" ({completed} of {requested} points completed; no report written)")
                }
                _ => String::new(),
            };
            eprintln!("\n  !! {msg}{partial}");
            break;
        }
    }
    (results, outcome)
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
    use super::{collect_sweep_frames, reference_latency_lines, short_onset_rule, SweepOutcome};
    use ac_core::measurement::report::{MeasuredReferenceLatency, ReferenceLatency};
    use ac_core::measurement::sweep::{BoundInputs, CausalBound, MissingBoundInput};
    use std::collections::VecDeque;

    fn point(freq_hz: f64) -> serde_json::Value {
        serde_json::json!({
            "type": "measurement/frequency_response/point",
            "freq_hz": freq_hz,
        })
    }

    /// PR #451 QA finding (#428): a terminal `error` after some points had
    /// already streamed must report `SweepOutcome::Failed` and only the
    /// completed prefix — `run`/`run_level` gate `print_summary`/
    /// `save_results` on `outcome == Done`, so this is what makes the
    /// atomic-failure guarantee reach the CLI's own summary/CSV output,
    /// not just the daemon's wire frames.
    #[test]
    fn error_after_points_is_failed_not_done() {
        let mut frames: VecDeque<(String, serde_json::Value)> = VecDeque::from([
            ("data".to_string(), point(100.0)),
            ("data".to_string(), point(200.0)),
            (
                "error".to_string(),
                serde_json::json!({
                    "cmd": "plot",
                    "message": "capture at 1000 Hz has 48 samples; minimum is 256",
                    "requested_points": 5,
                    "completed_points": 2,
                }),
            ),
            // Must never be reached: a real daemon does not publish a
            // point or `done` after a terminal `error`, and the loop must
            // not either.
            ("done".to_string(), serde_json::json!({"xruns": 0})),
        ]);

        let (results, outcome) = collect_sweep_frames(|| frames.pop_front(), "plot");

        assert_eq!(outcome, SweepOutcome::Failed);
        assert_eq!(
            results.len(),
            2,
            "only the pre-failure points should be retained: {results:?}"
        );
    }

    #[test]
    fn done_after_points_is_done() {
        let mut frames: VecDeque<(String, serde_json::Value)> = VecDeque::from([
            ("data".to_string(), point(100.0)),
            ("data".to_string(), point(200.0)),
            ("done".to_string(), serde_json::json!({"xruns": 0})),
        ]);

        let (results, outcome) = collect_sweep_frames(|| frames.pop_front(), "plot");

        assert_eq!(outcome, SweepOutcome::Done);
        assert_eq!(results.len(), 2);
    }

    fn unbounded() -> CausalBound {
        CausalBound::Unavailable(MissingBoundInput::Both {
            reference_reason: "no reference configured (ac setup reference)".into(),
        })
    }

    fn enforced(distance_m: f64, temperature_c: Option<f64>) -> CausalBound {
        CausalBound::Enforced {
            index: 1305,
            inputs: BoundInputs {
                reference_tau_s: 0.017_822_9,
                distance_m,
                speed_of_sound_m_s: ac_core::shared::conversions::speed_of_sound_from_config(
                    temperature_c,
                ),
                temperature_c,
            },
        }
    }

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
        let lines = short_onset_rule(rule, 1479, &unbounded());
        assert_eq!(
            lines,
            vec![
                "picker declined — no onset estimate".to_string(),
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
        let lines = short_onset_rule(rule, 1479, &unbounded());
        assert_eq!(lines[1], "a case invented by this test".to_string());
    }

    /// #460 UX frame 5: a bound at or after the peak names the bound's inputs
    /// and says to check them, not the gate.
    #[test]
    fn short_onset_rule_names_the_bound_inputs_when_the_bound_caused_the_decline() {
        let rule = "onset picker declined (causal bound at or after the peak) — index is the \
                    peak, not an onset";
        let lines = short_onset_rule(rule, 1479, &enforced(3.0, None));
        assert_eq!(
            lines,
            vec![
                "picker declined — no onset estimate".to_string(),
                "causal bound at or after the peak".to_string(),
                "bound from ref latency + 3 m, c 343.0 m/s assumed".to_string(),
                "check: typed distance, reference loopback routing".to_string(),
            ]
        );
    }

    #[test]
    fn short_onset_rule_reports_the_window_start_how_clear_the_pick_is_and_the_bound() {
        let rule = "AIC change-point pick over a 10.0 ms window; window start at sample 1305, \
                    causal bound enforced";
        let lines = short_onset_rule(rule, 1369, &enforced(1.0, Some(21.5)));
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert_eq!(lines[0], "AIC change-point pick, 10.0 ms window");
        assert_eq!(lines[1], "window start 1305 (causal bound), pick 64 clear");
        assert!(
            lines[2].starts_with("bound from ref latency + 1 m, c ")
                && lines[2].ends_with(" m/s at 21.5 °C"),
            "{:?}",
            lines[2]
        );
    }

    /// #460 AC3 / UX: the old `(search span, no geometry known)` named
    /// neither input. The window line now says only which limit set it, and
    /// row 3 names what was missing.
    #[test]
    fn short_onset_rule_names_the_missing_input_when_no_bound_applies() {
        let rule = "AIC change-point pick over a 10.0 ms window; window start at sample 455, \
                    no causal bound (distance not given)";
        let cases = [
            (
                MissingBoundInput::Distance,
                "no causal bound — distance not given (token: 1m)",
            ),
            (
                MissingBoundInput::ReferenceLatency {
                    reason: "no reference configured (ac setup reference)".into(),
                },
                "no causal bound — ref latency unavailable (below)",
            ),
            (
                MissingBoundInput::Both {
                    reference_reason: "no reference configured (ac setup reference)".into(),
                },
                "no causal bound — no distance, ref latency unavailable",
            ),
        ];
        for (missing, row) in cases {
            let lines = short_onset_rule(rule, 519, &CausalBound::Unavailable(missing));
            assert_eq!(lines[1], "window start 455 (search span), pick 64 clear");
            assert_eq!(lines[2], row);
            assert!(
                !lines.iter().any(|l| l.contains("no geometry known")),
                "pre-#460 clause survives: {lines:?}"
            );
        }
    }

    /// A causal bound that did not set the window start must not read as
    /// though it did — the operator's question is which limit the pick is
    /// pinned against.
    #[test]
    fn short_onset_rule_names_the_search_span_when_the_bound_does_not_bind() {
        let rule = "AIC change-point pick over a 10.0 ms window; window start at sample 455, \
                    causal bound enforced at sample 10, search span is the tighter limit";
        let lines = short_onset_rule(rule, 519, &enforced(1.0, None));
        assert_eq!(
            lines[1],
            "window start 455 (search span), pick 64 clear".to_string()
        );
    }

    /// The abnormal case keeps the extra line (#460 UX frame 6): rows 1–3
    /// as normal, the pinned warning on row 4.
    #[test]
    fn short_onset_rule_flags_a_pick_pinned_to_the_window_start() {
        let rule = "AIC change-point pick over a 10.0 ms window; window start at sample 1305, \
                    causal bound enforced; pick landed on the window start — the true onset \
                    may lie earlier";
        let lines = short_onset_rule(rule, 1305, &enforced(1.0, None));
        assert_eq!(
            lines,
            vec![
                "AIC change-point pick, 10.0 ms window".to_string(),
                "window start 1305 (causal bound), pick ON start".to_string(),
                "bound from ref latency + 1 m, c 343.0 m/s assumed".to_string(),
                "onset may lie earlier than the window allows".to_string(),
            ]
        );
    }

    /// #460 UX: the `ref latency` line in `calibrate`'s `Delay:` format, and
    /// unavailable reasons with their `check:` part on its own line.
    #[test]
    fn reference_latency_lines_print_the_measured_tau_or_the_reason() {
        let measured = ReferenceLatency::Measured(MeasuredReferenceLatency {
            tau_s: 1711.0 / 96_000.0,
            pre_impulse_snr_db: Some(61.8),
            method: "farina_same_capture_reference_v1".into(),
            output_port: "system:playback_2".into(),
            input_port: "system:capture_2".into(),
        });
        assert_eq!(
            reference_latency_lines(Some(&measured), 96_000),
            vec!["  ref latency   17.8229 ms  (1711 samples, SNR 61.8 dB, same capture)"]
        );

        let silent_floor = ReferenceLatency::Measured(MeasuredReferenceLatency {
            tau_s: 20.0 / 48_000.0,
            pre_impulse_snr_db: None,
            method: "farina_same_capture_reference_v1".into(),
            output_port: "fake:playback_1".into(),
            input_port: "fake:capture_1".into(),
        });
        assert_eq!(
            reference_latency_lines(Some(&silent_floor), 48_000),
            vec!["  ref latency   0.4167 ms  (20 samples, SNR ∞ dB, same capture)"]
        );

        let refused = ReferenceLatency::Unavailable {
            reason:
                "peak SNR 9.3 dB, need 24.0 dB; check: reference loopback cable, ref input gain"
                    .into(),
        };
        assert_eq!(
            reference_latency_lines(Some(&refused), 96_000),
            vec![
                "  ref latency   unavailable — peak SNR 9.3 dB, need 24.0 dB",
                "                check: reference loopback cable, ref input gain",
            ]
        );

        let none_configured = ReferenceLatency::Unavailable {
            reason: "no reference configured (ac setup reference)".into(),
        };
        assert_eq!(
            reference_latency_lines(Some(&none_configured), 96_000),
            vec!["  ref latency   unavailable — no reference configured (ac setup reference)"]
        );
        assert_eq!(reference_latency_lines(None, 96_000).len(), 1);
    }

    /// Every line the onset block and the `ref latency` read-out can emit must
    /// fit 80 columns at the indents `print_ir_report` uses — at a 6-digit
    /// sample index, the widest distance and temperature the #460 UX pass
    /// measured, every decline case, and every reference reason.
    #[test]
    fn onset_and_reference_lines_fit_eighty_columns() {
        let mut rules = vec![
            "AIC change-point pick over a 10.0 ms window; window start at sample 262144, \
             causal bound enforced; pick landed on the window start — the true onset may \
             lie earlier"
                .to_string(),
            "AIC change-point pick over a 10.0 ms window; window start at sample 262144, \
             causal bound enforced at sample 9, search span is the tighter limit"
                .to_string(),
            "AIC change-point pick over a 10.0 ms window; window start at sample 262144, \
             no causal bound (no distance, reference latency unavailable)"
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
            "causal bound at or after the peak",
        ] {
            rules.push(format!(
                "onset picker declined ({case}) — index is the peak, not an onset"
            ));
        }
        let bounds = [
            enforced(12.25, Some(-10.5)),
            enforced(12.25, None),
            CausalBound::Unavailable(MissingBoundInput::Distance),
            CausalBound::Unavailable(MissingBoundInput::ReferenceLatency {
                reason: String::new(),
            }),
            unbounded(),
        ];
        for rule in &rules {
            for bound in &bounds {
                for (i, line) in short_onset_rule(rule, 262_144, bound).iter().enumerate() {
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

        let long_tau = ReferenceLatency::Measured(MeasuredReferenceLatency {
            tau_s: 0.123_456_7,
            pre_impulse_snr_db: Some(104.3),
            method: String::new(),
            output_port: String::new(),
            input_port: String::new(),
        });
        let mut references = vec![long_tau];
        for reason in [
            "no reference configured (ac setup reference)",
            "backend cpal cannot capture a reference",
            "peak SNR 9.3 dB, need 24.0 dB; check: reference loopback cable, ref input gain",
            "peak at reference window edge; check: reference loopback routing, capture tail",
            "xrun during capture; check: JACK period size, system load",
            "tail 0.08 s, reference window needs 0.10 s; check: lengthen the tail token (e.g. 0.8s)",
        ] {
            references.push(ReferenceLatency::Unavailable {
                reason: reason.to_string(),
            });
        }
        for reference in &references {
            for line in reference_latency_lines(Some(reference), 192_000) {
                assert!(
                    line.chars().count() <= 80,
                    "line {:?} runs to {} columns",
                    line,
                    line.chars().count()
                );
            }
        }
    }
}
