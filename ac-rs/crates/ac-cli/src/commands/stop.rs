use crate::client::AcClient;

pub fn run(client: &mut AcClient) {
    let ack = client.send_cmd(&serde_json::json!({"cmd": "stop"}), None);
    match ack {
        Some(ref v) if v.get("ok").and_then(|v| v.as_bool()) == Some(true) => {
            println!("  Stopped.");
        }
        Some(ref v) => {
            let err = v.get("error").and_then(|e| e.as_str()).unwrap_or("unknown error");
            println!("  {err}");
        }
        None => {
            eprintln!("  error: no response from server");
        }
    }
}
