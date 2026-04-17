use crate::client::AcClient;
use crate::parse::CommandKind;
use super::check_ack;

pub fn run(
    cmd: &CommandKind,
    cfg: &ac_core::config::Config,
    client: &mut AcClient,
    show_plot: bool,
) {
    let (start_freq, end_freq, interval) = match cmd {
        CommandKind::Monitor {
            start_freq,
            end_freq,
            interval,
            ..
        } => (*start_freq, *end_freq, *interval),
        _ => unreachable!(),
    };

    let ack = check_ack(
        client.send_cmd(
            &serde_json::json!({
                "cmd": "monitor_spectrum",
                "freq_hz": start_freq,
                "interval": interval,
            }),
            None,
        ),
        "monitor_spectrum",
    );
    if let Some(p) = ack.get("in_port").and_then(|v| v.as_str()) {
        println!("  Input: {p}");
    }
    println!("  {start_freq:.0}\u{2013}{end_freq:.0} Hz  |  Ctrl+C or q to stop");

    if show_plot {
        super::plot::launch_ui("spectrum", cfg);
        println!("  Window open \u{2014} close the window or press Ctrl+C to stop.");
        super::generate::wait_for_stop(client, "monitor_spectrum");
        return;
    }

    run_tui(client, start_freq, end_freq);
}

fn run_tui(client: &mut AcClient, start_freq: f64, end_freq: f64) {
    crossterm::terminal::enable_raw_mode().ok();
    print!("\x1b[?25l\x1b[2J");

    let result = tui_loop(client, start_freq, end_freq);

    crossterm::terminal::disable_raw_mode().ok();
    print!("\x1b[?25h\x1b[2J\x1b[H");
    match result {
        Ok(()) => println!("  Stopped."),
        Err(msg) => println!("  Error: {msg}"),
    }
}

fn tui_loop(
    client: &mut AcClient,
    _start_freq: f64,
    _end_freq: f64,
) -> Result<(), String> {
    loop {
        if crossterm::event::poll(std::time::Duration::from_millis(50)).unwrap_or(false) {
            if let Ok(crossterm::event::Event::Key(key)) = crossterm::event::read() {
                use crossterm::event::KeyCode;
                match key.code {
                    KeyCode::Char('q') | KeyCode::Char('Q') => {
                        client.send_cmd(
                            &serde_json::json!({"cmd": "stop", "name": "monitor_spectrum"}),
                            None,
                        );
                        return Ok(());
                    }
                    KeyCode::Char('c')
                        if key
                            .modifiers
                            .contains(crossterm::event::KeyModifiers::CONTROL) =>
                    {
                        client.send_cmd(
                            &serde_json::json!({"cmd": "stop", "name": "monitor_spectrum"}),
                            None,
                        );
                        return Ok(());
                    }
                    _ => {}
                }
            }
        }

        if let Some((topic, data)) = client.recv_data(100) {
            if topic == "error" {
                let msg = data
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("error");
                return Err(msg.to_string());
            }
            if topic == "done" {
                return Ok(());
            }
            if topic == "data" {
                if data.get("type").and_then(|v| v.as_str()) == Some("spectrum") {
                    print_spectrum_line(&data);
                }
            }
        }
    }
}

fn print_spectrum_line(frame: &serde_json::Value) {
    let freq = frame.get("freq_hz").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let thd = frame.get("thd_pct").and_then(|v| v.as_f64());
    let thdn = frame.get("thdn_pct").and_then(|v| v.as_f64());
    let in_dbu = frame.get("in_dbu").and_then(|v| v.as_f64());

    let mut line = format!("  {freq:>7.1} Hz");
    if let Some(dbu) = in_dbu {
        line.push_str(&format!("  {dbu:>+6.1} dBu"));
    }
    if let Some(t) = thd {
        line.push_str(&format!("  THD {t:.4}%"));
    }
    if let Some(n) = thdn {
        line.push_str(&format!("  THD+N {n:.4}%"));
    }
    line.push_str("        ");
    print!("\x1b[H\x1b[2K{line}");
}
