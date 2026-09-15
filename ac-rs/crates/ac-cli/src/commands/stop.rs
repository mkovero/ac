use crate::client::AcClient;

fn render_success(ack: &serde_json::Value) -> Vec<String> {
    let mut lines = ack
        .get("stopped")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
        .map(|name| format!("  stopped     {name}"))
        .collect::<Vec<_>>();
    if let Some(stimulus) = ack.get("stimulus").and_then(|v| v.as_str()) {
        lines.push(format!("  stimulus    {stimulus}"));
    }
    lines
}

pub fn run(client: &mut AcClient) {
    let ack = client.send_cmd(&serde_json::json!({"cmd": "stop"}), None);
    match ack {
        Some(ref v) if v.get("ok").and_then(|v| v.as_bool()) == Some(true) => {
            for line in render_success(v) {
                println!("{line}");
            }
        }
        Some(ref v) => {
            let err = v
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("unknown error");
            println!("  {err}");
        }
        None => {
            eprintln!("  error: no response from server");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn renders_stopped_command_and_confirmed_silence() {
        assert_eq!(
            render_success(&json!({
                "ok": true,
                "stopped": ["plot_ir"],
                "stimulus": "silent"
            })),
            vec!["  stopped     plot_ir", "  stimulus    silent"]
        );
    }

    #[test]
    fn empty_stop_does_not_invent_a_command() {
        assert_eq!(
            render_success(&json!({
                "ok": true,
                "stopped": [],
                "stimulus": "silent"
            })),
            vec!["  stimulus    silent"]
        );
    }
}
