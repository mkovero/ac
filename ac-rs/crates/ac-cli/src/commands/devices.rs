use crate::client::AcClient;
use super::check_ack;

pub fn run(client: &mut AcClient) {
    let ack = check_ack(
        client.send_cmd(&serde_json::json!({"cmd": "devices"}), None),
        "devices",
    );

    let playback = ack
        .get("playback")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let capture = ack
        .get("capture")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let out_ch = ack
        .get("output_channel")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;
    let in_ch = ack
        .get("input_channel")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;
    let out_sticky = ack.get("output_port").and_then(|v| v.as_str());
    let in_sticky = ack.get("input_port").and_then(|v| v.as_str());
    let ref_ch = ack.get("reference_channel").and_then(|v| v.as_u64());
    let ref_sticky = ack.get("reference_port").and_then(|v| v.as_str());

    let port_str = |arr: &[serde_json::Value], i: usize| -> String {
        arr.get(i)
            .and_then(|v| v.as_str())
            .unwrap_or("??")
            .to_string()
    };

    let sticky_note = |sticky: Option<&str>, ports: &[serde_json::Value], ch: usize| -> String {
        match sticky {
            None => String::new(),
            Some(s) => {
                let found = ports
                    .iter()
                    .position(|v| v.as_str() == Some(s));
                match found {
                    Some(idx) if idx != ch => format!("  (reordered: now ch {idx})"),
                    None => "  (sticky port not found)".to_string(),
                    _ => String::new(),
                }
            }
        }
    };

    let out_name = port_str(&playback, out_ch);
    let in_name = port_str(&capture, in_ch);

    let out_suf = match out_sticky {
        Some(s) if s != out_name => format!("  ->  {s}"),
        _ => String::new(),
    };
    let in_suf = match in_sticky {
        Some(s) if s != in_name => format!("  ->  {s}"),
        _ => String::new(),
    };

    println!("\n  JACK ports:");
    println!(
        "  Configured:  output ch {out_ch}  ->  {out_name}{out_suf}{}",
        sticky_note(out_sticky, &playback, out_ch)
    );
    println!(
        "               input  ch {in_ch}  ->  {in_name}{in_suf}{}",
        sticky_note(in_sticky, &capture, in_ch)
    );
    if let Some(rch) = ref_ch {
        let ref_name = port_str(&capture, rch as usize);
        let ref_suf = match ref_sticky {
            Some(s) if s != ref_name.as_str() => format!("  ->  {s}"),
            _ => String::new(),
        };
        println!("               ref    ch {rch}  ->  {ref_name}{ref_suf}");
    }

    println!("\n  Playback:");
    for (i, p) in playback.iter().enumerate() {
        let name = p.as_str().unwrap_or("??");
        let mark = if i == out_ch { "  <--" } else { "" };
        println!("    {i:>3}  {name}{mark}");
    }

    println!("\n  Capture:");
    for (i, p) in capture.iter().enumerate() {
        let name = p.as_str().unwrap_or("??");
        let mark = if i == in_ch { "  <--" } else { "" };
        println!("    {i:>3}  {name}{mark}");
    }
    println!();
}
