use crate::client::AcClient;
use super::check_ack;

pub fn enable(client: &mut AcClient) {
    let ack = check_ack(
        client.send_cmd(&serde_json::json!({"cmd": "server_enable"}), None),
        "server_enable",
    );
    let mode = ack
        .get("listen_mode")
        .and_then(|v| v.as_str())
        .unwrap_or("public");
    println!("  Server listening: {mode}");
}

pub fn disable(client: &mut AcClient) {
    let ack = check_ack(
        client.send_cmd(&serde_json::json!({"cmd": "server_disable"}), None),
        "server_disable",
    );
    let mode = ack
        .get("listen_mode")
        .and_then(|v| v.as_str())
        .unwrap_or("local");
    println!("  Server listening: {mode}");
}

pub fn connections(client: &mut AcClient) {
    let ack = check_ack(
        client.send_cmd(&serde_json::json!({"cmd": "server_connections"}), None),
        "server_connections",
    );
    let mode = ack
        .get("listen_mode")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let ctrl_ep = ack
        .get("ctrl_endpoint")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let data_ep = ack
        .get("data_endpoint")
        .and_then(|v| v.as_str())
        .unwrap_or("?");

    println!("\n  Server connections:");
    println!("  Mode: {mode}");
    println!("  CTRL: {ctrl_ep}");
    println!("  DATA: {data_ep}");

    if let Some(workers) = ack.get("workers").and_then(|v| v.as_array()) {
        if !workers.is_empty() {
            println!(
                "  Workers: {}",
                workers
                    .iter()
                    .filter_map(|w| w.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    println!();
}

pub fn set_host(host: &str) {
    let mut cfg = ac_core::config::load(None).unwrap_or_default();
    cfg.server_host = if host == "localhost" {
        None
    } else {
        Some(host.to_string())
    };
    match ac_core::config::save(&cfg, None) {
        Ok(_) => {
            println!("  Server host set to: {host}");
            println!("  All ac commands will now route through tcp://{host}:5556");
        }
        Err(e) => eprintln!("  error saving config: {e}"),
    }
}
