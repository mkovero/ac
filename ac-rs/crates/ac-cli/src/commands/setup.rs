use crate::client::AcClient;
use crate::parse::CommandKind;
use super::check_ack;

pub fn run(cmd: &CommandKind, client: &mut AcClient) {
    let (output, input, reference, device, dbu_ref_vrms, dmm_host, gpio_port, range_start, range_stop) =
        match cmd {
            CommandKind::Setup {
                output,
                input,
                reference,
                device,
                dbu_ref_vrms,
                dmm_host,
                gpio_port,
                range_start,
                range_stop,
            } => (
                output, input, reference, device, dbu_ref_vrms, dmm_host, gpio_port, range_start,
                range_stop,
            ),
            _ => unreachable!(),
        };

    let mut update = serde_json::Map::new();
    if let Some(v) = output {
        update.insert("output_channel".into(), (*v).into());
    }
    if let Some(v) = input {
        update.insert("input_channel".into(), (*v).into());
    }
    if let Some(v) = reference {
        update.insert("reference_channel".into(), (*v).into());
    }
    if let Some(v) = device {
        update.insert("device".into(), (*v).into());
    }
    if let Some(v) = dbu_ref_vrms {
        update.insert("dbu_ref_vrms".into(), (*v).into());
    }
    if let Some(v) = dmm_host {
        update.insert("dmm_host".into(), v.clone().into());
    }
    if let Some(v) = gpio_port {
        match v {
            Some(port) => update.insert("gpio_port".into(), port.clone().into()),
            None => update.insert("gpio_port".into(), serde_json::Value::Null),
        };
    }
    if let Some(v) = range_start {
        update.insert("range_start_hz".into(), (*v).into());
    }
    if let Some(v) = range_stop {
        update.insert("range_stop_hz".into(), (*v).into());
    }

    let has_updates = !update.is_empty();

    let ack = check_ack(
        client.send_cmd(
            &serde_json::json!({"cmd": "setup", "update": update}),
            None,
        ),
        "setup",
    );

    let srv_cfg = ack.get("config").cloned().unwrap_or_default();
    let ref_vrms = srv_cfg
        .get("dbu_ref_vrms")
        .and_then(|v| v.as_f64())
        .unwrap_or(ac_core::shared::constants::DBU_REF_EXACT);

    println!("\n  -- Hardware config (server) --");
    println!(
        "  Device:         {}",
        srv_cfg.get("device").and_then(|v| v.as_u64()).unwrap_or(0)
    );
    println!(
        "  Output channel: {}",
        srv_cfg
            .get("output_channel")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
    );
    println!(
        "  Input channel:  {}",
        srv_cfg
            .get("input_channel")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
    );

    if let Some(rch) = srv_cfg.get("reference_channel").and_then(|v| v.as_u64()) {
        let rport = srv_cfg
            .get("reference_port")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        print!("  Reference ch:   {rch}");
        if !rport.is_empty() {
            print!("  ->  {rport}");
        }
        println!();
    }

    println!(
        "  dBu reference: {:.4} mVrms  ({:.8} V)",
        ref_vrms * 1000.0,
        ref_vrms
    );

    let dmm = srv_cfg.get("dmm_host").and_then(|v| v.as_str());
    println!(
        "  DMM host:      {}",
        dmm.unwrap_or("(not configured)")
    );

    let gpio = srv_cfg.get("gpio_port").and_then(|v| v.as_str());
    println!(
        "  GPIO port:     {}",
        gpio.unwrap_or("(not configured)")
    );

    let r_start = srv_cfg
        .get("range_start_hz")
        .and_then(|v| v.as_f64())
        .unwrap_or(20.0);
    let r_stop = srv_cfg
        .get("range_stop_hz")
        .and_then(|v| v.as_f64())
        .unwrap_or(20000.0);
    println!("  Range:         {r_start:.0} – {r_stop:.0} Hz");

    if has_updates {
        println!("  Saved.");
    }

    if let Some(gp) = gpio_port {
        let port_val: serde_json::Value = match gp {
            Some(p) => p.clone().into(),
            None => serde_json::Value::Null,
        };
        let gpio_ack = client.send_cmd(
            &serde_json::json!({"cmd": "gpio_setup", "port": port_val}),
            Some(5000),
        );
        match gpio_ack {
            Some(ref a) if a.get("ok").and_then(|v| v.as_bool()) == Some(true) => {
                match gp {
                    Some(p) => println!("  GPIO: started on {p}"),
                    None => println!("  GPIO: stopped"),
                }
            }
            Some(ref a) => {
                let err = a.get("error").and_then(|e| e.as_str()).unwrap_or("error");
                println!("  GPIO: {err}");
            }
            None => println!("  GPIO: server not responding"),
        }
    }
    println!();
}
