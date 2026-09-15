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

#[cfg(test)]
mod tests {
    use super::{collect_sweep_frames, SweepOutcome};
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
