use super::check_ack;
use crate::client::AcClient;
use crate::parse::CommandKind;

/// Print one reference leg's `channel  ->  sticky port` line.
///
/// `unset_note` is what to print when the channel is absent — `None` prints
/// nothing at all (the historical behaviour of the capture leg).
///
/// The sticky-without-a-channel case is called out rather than skipped: the
/// daemon refuses to resolve that config, and this display is where an
/// operator looks to understand the error it just returned. Printing the
/// channel line alone would show nothing wrong (#225 review, finding 2).
fn print_leg(
    cfg: &serde_json::Value,
    label: &str,
    channel_key: &str,
    port_key: &str,
    unset_note: Option<&str>,
) {
    let channel = cfg.get(channel_key).and_then(|v| v.as_u64());
    let port = cfg.get(port_key).and_then(|v| v.as_str()).unwrap_or("");
    match channel {
        Some(ch) => {
            print!("{label} {ch}");
            if !port.is_empty() {
                print!("  ->  {port}");
            }
            println!();
        }
        None if !port.is_empty() => {
            println!("{label} (none) — {port_key} is set to {port:?} but {channel_key} is not, so it is ignored");
        }
        None => {
            if let Some(note) = unset_note {
                println!("{label} {note}");
            }
        }
    }
}

pub fn run(cmd: &CommandKind, client: &mut AcClient) {
    let (
        output,
        input,
        reference,
        reference_output,
        device,
        dbu_ref_vrms,
        dmm_host,
        gpio_port,
        range_start,
        range_stop,
        server_idle_timeout_secs,
        temperature_c,
    ) = match cmd {
        CommandKind::Setup {
            output,
            input,
            reference,
            reference_output,
            device,
            dbu_ref_vrms,
            dmm_host,
            gpio_port,
            range_start,
            range_stop,
            server_idle_timeout_secs,
            temperature_c,
        } => (
            output,
            input,
            reference,
            reference_output,
            device,
            dbu_ref_vrms,
            dmm_host,
            gpio_port,
            range_start,
            range_stop,
            server_idle_timeout_secs,
            temperature_c,
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
    if let Some(v) = reference_output {
        match v {
            Some(ch) => update.insert("reference_output_channel".into(), (*ch).into()),
            None => update.insert("reference_output_channel".into(), serde_json::Value::Null),
        };
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
    if let Some(v) = server_idle_timeout_secs {
        match v {
            Some(secs) => update.insert("server_idle_timeout_secs".into(), (*secs).into()),
            None => update.insert("server_idle_timeout_secs".into(), serde_json::Value::Null),
        };
    }
    if let Some(v) = temperature_c {
        match v {
            Some(t) => update.insert("temperature_c".into(), (*t).into()),
            None => update.insert("temperature_c".into(), serde_json::Value::Null),
        };
    }

    let has_updates = !update.is_empty();

    let ack = check_ack(
        client.send_cmd(&serde_json::json!({"cmd": "setup", "update": update}), None),
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

    print_leg(
        &srv_cfg,
        "  Reference ch:  ",
        "reference_channel",
        "reference_port",
        None,
    );
    // The output leg prints even when unset: a reference output that silently
    // followed the main output is what #225 cost a rig session to find.
    print_leg(
        &srv_cfg,
        "  Ref output ch: ",
        "reference_output_channel",
        "reference_output_port",
        Some("(main output)"),
    );

    println!(
        "  dBu reference: {:.4} mVrms  ({:.8} V)",
        ref_vrms * 1000.0,
        ref_vrms
    );

    let dmm = srv_cfg.get("dmm_host").and_then(|v| v.as_str());
    println!("  DMM host:      {}", dmm.unwrap_or("(not configured)"));

    let gpio = srv_cfg.get("gpio_port").and_then(|v| v.as_str());
    println!("  GPIO port:     {}", gpio.unwrap_or("(not configured)"));

    let r_start = srv_cfg
        .get("range_start_hz")
        .and_then(|v| v.as_f64())
        .unwrap_or(20.0);
    let r_stop = srv_cfg
        .get("range_stop_hz")
        .and_then(|v| v.as_f64())
        .unwrap_or(20000.0);
    println!("  Range:         {r_start:.0} – {r_stop:.0} Hz");

    // Both the temperature and the speed it implies (#243). The derived
    // figure is printed because it, not the temperature, is what the delay
    // readout converts with — and because an unset temperature reads as a
    // different speed from any temperature that could be typed.
    let temp = srv_cfg.get("temperature_c").and_then(|v| v.as_f64());
    let c = ac_core::shared::conversions::speed_of_sound_from_config(temp);
    match temp {
        Some(t) => println!("  Room temp:     {t:.1} °C  (c = {c:.1} m/s)"),
        None => println!("  Room temp:     (not set — c = {c:.1} m/s assumed)"),
    }

    let timeout = srv_cfg
        .get("server_idle_timeout_secs")
        .and_then(|v| v.as_u64());
    match timeout {
        Some(secs) => println!("  Server idle:   {secs}s (auto-disable)"),
        None => println!("  Server idle:   (no timeout)"),
    }

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
            Some(ref a) if a.get("ok").and_then(|v| v.as_bool()) == Some(true) => match gp {
                Some(p) => println!("  GPIO: started on {p}"),
                None => println!("  GPIO: stopped"),
            },
            Some(ref a) => {
                let err = a.get("error").and_then(|e| e.as_str()).unwrap_or("error");
                println!("  GPIO: {err}");
            }
            None => println!("  GPIO: server not responding"),
        }
    }
    println!();
}
