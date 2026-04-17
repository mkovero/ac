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
    let (start, stop, level) = match cmd {
        CommandKind::Transfer {
            start,
            stop,
            level,
        } => (*start, *stop, level),
        _ => unreachable!(),
    };

    let cal = get_cal(client);
    let level_db = level_to_dbfs(level, cal.as_ref());

    let start_hz = start.unwrap_or(cfg.range_start_hz);
    let stop_hz = stop.unwrap_or(cfg.range_stop_hz);

    println!(
        "\n  Transfer function: {start_hz:.0} \u{2192} {stop_hz:.0} Hz  |  {level_db:.1} dBFS"
    );
    println!("  Stimulus: pink noise  |  H1 estimator with Welch averaging");

    if show_plot {
        super::plot::launch_ui("transfer", cfg);
    }

    let ack = check_ack(
        client.send_cmd(
            &serde_json::json!({
                "cmd": "transfer",
                "start_hz": start_hz,
                "stop_hz": stop_hz,
                "level_dbfs": level_db,
            }),
            None,
        ),
        "transfer",
    );
    if let (Some(out), Some(inp), Some(rp)) = (
        ack.get("out_port").and_then(|v| v.as_str()),
        ack.get("in_port").and_then(|v| v.as_str()),
        ack.get("ref_port").and_then(|v| v.as_str()),
    ) {
        println!("  Output: {out}  \u{2192}  Measurement: {inp}  |  Reference: {rp}");
    }
    println!("  Capturing... (this takes several seconds)");

    let mut result: Option<serde_json::Value> = None;

    loop {
        let frame = match client.recv_data(300_000) {
            Some(f) => f,
            None => {
                eprintln!("  error: timeout waiting for transfer data");
                break;
            }
        };
        let (topic, data) = frame;

        if topic == "data" {
            if data.get("type").and_then(|v| v.as_str()) == Some("transfer_result") {
                result = Some(data);
            }
        } else if topic == "done" {
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

    let result = match result {
        Some(r) => r,
        None => {
            println!("  No result received.");
            return;
        }
    };

    let freqs = result.get("freqs").and_then(|v| v.as_array());
    let coh = result.get("coherence").and_then(|v| v.as_array());
    let delay = result.get("delay_ms").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let delay_samp = result
        .get("delay_samples")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    if let (Some(fs), Some(cs)) = (freqs, coh) {
        let mean_coh: f64 = if cs.is_empty() {
            0.0
        } else {
            cs.iter()
                .filter_map(|v| v.as_f64())
                .sum::<f64>()
                / cs.len() as f64
        };
        let min_coh: f64 = cs
            .iter()
            .filter_map(|v| v.as_f64())
            .fold(f64::INFINITY, f64::min);
        let first = fs.first().and_then(|v| v.as_f64()).unwrap_or(0.0);
        let last = fs.last().and_then(|v| v.as_f64()).unwrap_or(0.0);

        println!("\n  Delay:     {delay_samp} samples  ({delay:.3} ms)");
        println!("  Coherence: mean {mean_coh:.3}  min {min_coh:.3}");
        println!("  Points:    {}  ({first:.1} \u{2013} {last:.0} Hz)", fs.len());
    }

    let dir = io::output_dir(cfg);
    let ts = io::timestamp();
    let path = dir.join(format!("transfer_{ts}.csv"));
    io::save_transfer_csv(&result, &path);
}
