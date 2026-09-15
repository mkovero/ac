use super::{check_ack, print_fixed_level};
use crate::client::AcClient;

pub fn run(client: &mut AcClient) {
    let ack = check_ack(
        client.send_cmd(&serde_json::json!({"cmd": "probe"}), None),
        "probe",
    );

    let n_playback = ack.get("n_playback").and_then(|v| v.as_u64()).unwrap_or(0);
    let n_capture = ack.get("n_capture").and_then(|v| v.as_u64()).unwrap_or(0);
    println!("\n  Port probe: {n_playback} playback, {n_capture} capture ports");
    print_fixed_level(
        ack.get("level_dbfs").and_then(|v| v.as_f64()),
        ack.get("max_dbfs").and_then(|v| v.as_f64()),
    );
    println!();
}
