use crate::client::AcClient;
use crate::io;
use crate::parse::CommandKind;
use super::{check_ack, get_cal, level_to_dbfs};

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

    if show_plot {
        launch_ui("sweep_frequency", cfg, None);
    }

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

    if show_plot {
        launch_ui("sweep_level", cfg, None);
    }

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

    let results = collect_sweep(client, "plot_level");
    if results.is_empty() {
        return;
    }
    io::print_summary(&results, "DUT", have_cal);
    save_results(&results, "plot_level", cfg);
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
            if data.get("type").and_then(|v| v.as_str()) == Some("sweep_point") {
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

pub(crate) fn launch_ui(mode: &str, cfg: &ac_core::config::Config, channels: Option<&[u32]>) {
    let bin = crate::spawn::find_binary("ac-ui");
    let bin = match bin {
        Some(p) => p,
        None => {
            eprintln!("  warning: ac-ui not found, skipping UI launch");
            return;
        }
    };
    let host = cfg.server_host.as_deref().unwrap_or("127.0.0.1");
    let mut args = vec![
        "--connect".to_string(),
        format!("tcp://{host}:5557"),
        "--ctrl".to_string(),
        format!("tcp://{host}:5556"),
    ];
    if mode != "spectrum" {
        args.push("--mode".to_string());
        args.push(mode.to_string());
    }
    if let Some(chs) = channels {
        let spec: Vec<String> = chs.iter().map(|c| c.to_string()).collect();
        args.push("--channels".to_string());
        args.push(spec.join(","));
    }
    if let Some(ref sess) = cfg.session {
        let d = io::session_dir(sess);
        args.push("--output-dir".to_string());
        args.push(d.to_string_lossy().into_owned());
    }
    std::process::Command::new(bin)
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .ok();
}
