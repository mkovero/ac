use crate::client::AcClient;
use crate::parse::CommandKind;
use super::{check_ack, get_cal, level_to_dbfs};
use crate::io;

pub fn run_software(client: &mut AcClient) {
    let ack = check_ack(
        client.send_cmd(&serde_json::json!({"cmd": "test_software"}), None),
        "test_software",
    );
    println!("\n  Software self-test");
    println!("  {}", "\u{2500}".repeat(40));

    if let Some(results) = ack.get("results").and_then(|v| v.as_array()) {
        for r in results {
            let name = r.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            let pass = r.get("pass").and_then(|v| v.as_bool()).unwrap_or(false);
            let mark = if pass { "PASS" } else { "FAIL" };
            let detail = r.get("detail").and_then(|v| v.as_str()).unwrap_or("");
            println!("  [{mark}]  {name}  {detail}");
        }
    }
    println!();
}

pub fn run_hardware(cmd: &CommandKind, client: &mut AcClient) {
    let dmm = match cmd {
        CommandKind::TestHardware { dmm } => *dmm,
        _ => unreachable!(),
    };

    let mut json = serde_json::json!({"cmd": "test_hardware"});
    if dmm {
        json["dmm"] = true.into();
    }

    check_ack(client.send_cmd(&json, None), "test_hardware");
    println!("\n  Hardware test started...\n");

    io::print_freq_header(false);

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
            if data.get("type").and_then(|v| v.as_str()) == Some("measurement/frequency_response/point") {
                io::print_freq_row(&data);
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

pub fn run_dut(
    cmd: &CommandKind,
    cfg: &ac_core::config::Config,
    client: &mut AcClient,
) {
    let (compare, level) = match cmd {
        CommandKind::TestDut { compare, level } => (*compare, level),
        _ => unreachable!(),
    };

    let cal = get_cal(client);
    let have_cal = cal.is_some();
    let level_db = level_to_dbfs(level, cal.as_ref());

    let mut json = serde_json::json!({"cmd": "test_dut", "level_dbfs": level_db});
    if compare {
        json["compare"] = true.into();
    }

    check_ack(client.send_cmd(&json, None), "test_dut");
    println!("\n  DUT test at {level_db:.1} dBFS\n");

    io::print_freq_header(have_cal);

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
            if data.get("type").and_then(|v| v.as_str()) == Some("measurement/frequency_response/point") {
                io::print_freq_row(&data);
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
