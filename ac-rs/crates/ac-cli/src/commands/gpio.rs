use crate::client::AcClient;
use super::check_ack;

pub fn run(client: &mut AcClient, log: bool) {
    if log {
        let ack = check_ack(
            client.send_cmd(&serde_json::json!({"cmd": "gpio_log"}), None),
            "gpio_log",
        );
        if let Some(events) = ack.get("events").and_then(|v| v.as_array()) {
            if events.is_empty() {
                println!("  No GPIO events.");
            } else {
                for e in events {
                    println!("  {e}");
                }
            }
        }
    } else {
        let ack = check_ack(
            client.send_cmd(&serde_json::json!({"cmd": "gpio"}), None),
            "gpio",
        );
        if let Some(status) = ack.get("status").and_then(|v| v.as_str()) {
            println!("  GPIO: {status}");
        } else {
            println!("  {ack}");
        }
    }
}
