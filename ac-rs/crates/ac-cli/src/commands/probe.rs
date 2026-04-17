use crate::client::AcClient;
use super::check_ack;

pub fn run(client: &mut AcClient) {
    let ack = check_ack(
        client.send_cmd(&serde_json::json!({"cmd": "probe"}), None),
        "probe",
    );

    println!("\n  Port probe");
    println!("  {}", "\u{2500}".repeat(50));

    if let Some(ports) = ack.get("ports").and_then(|v| v.as_array()) {
        for p in ports {
            let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            let dir = p.get("direction").and_then(|v| v.as_str()).unwrap_or("?");
            let active = p.get("active").and_then(|v| v.as_bool()).unwrap_or(false);
            let mark = if active { "*" } else { " " };
            println!("  {mark} {dir:<8} {name}");
        }
    }
    println!();
}
