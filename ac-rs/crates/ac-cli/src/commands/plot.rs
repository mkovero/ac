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
pub fn run_ir(cmd: &CommandKind, _cfg: &ac_core::config::Config, client: &mut AcClient) {
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
    let sr = data
        .get("sample_rate_hz")
        .and_then(|v| v.as_u64())
        .unwrap_or(48_000);
    if let Some(samples) = data.get("linear_ir").and_then(|v| v.as_array()) {
        let (peak_idx, peak_val) =
            samples
                .iter()
                .enumerate()
                .fold((0usize, 0.0f64), |acc, (i, v)| {
                    let mag = v.as_f64().unwrap_or(0.0).abs();
                    if mag > acc.1 {
                        (i, mag)
                    } else {
                        acc
                    }
                });
        println!(
            "  peak IR       {peak_val:.3} FS  at sample {peak_idx} ({:.4}s @ {sr} Hz)",
            peak_idx as f64 / sr as f64
        );
    }
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

    if let Some(note) = report_frame
        .and_then(|f| f.get("report"))
        .and_then(|r| r.get("notes"))
        .and_then(|v| v.as_str())
    {
        println!("  {note}");
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
