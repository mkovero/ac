use super::check_ack;
use crate::client::AcClient;
use crate::parse::{CommandKind, REPORT_DIR_TOKEN};
use std::path::{Path, PathBuf};

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

/// Resolve a `report-dir` value against this process, for a local daemon
/// (#472): a leading `~` / `~/` takes `home`, a relative path joins `cwd`.
/// `~user`, an empty value, or `~` with no `home` goes over unchanged — the
/// daemon refuses anything that is not absolute, so none of them can be
/// stored by accident. No `canonicalize`: a symlink or mount path stays the
/// path the operator named. Only `.` components are dropped, so `./none`
/// prints as `<cwd>/none`.
fn resolve_local_report_dir(raw: &str, home: Option<&str>, cwd: &Path) -> String {
    let expanded: PathBuf = match (raw, home) {
        ("~", Some(h)) => PathBuf::from(h),
        (r, Some(h)) if r.starts_with("~/") => Path::new(h).join(&r[2..]),
        (r, _) if r.is_empty() || r.starts_with('~') => return raw.to_string(),
        (r, _) => cwd.join(r),
    };
    expanded
        .components()
        .collect::<PathBuf>()
        .display()
        .to_string()
}

/// The two-line refusal `setup` prints when the daemon refused a
/// `report_dir` (#472 UX), or `None` when the reply is not that refusal and
/// the generic [`check_ack`] path should handle it.
fn refusal_lines(reply: &serde_json::Value) -> Option<[String; 2]> {
    if reply.get("ok").and_then(|v| v.as_bool()) == Some(true) {
        return None;
    }
    let refused = reply.get("refused")?;
    if refused.get("key").and_then(|v| v.as_str()) != Some("report_dir") {
        return None;
    }
    let path = refused.get("path").and_then(|v| v.as_str())?;
    let reason = refused.get("reason").and_then(|v| v.as_str())?;
    Some([
        format!("  error: {REPORT_DIR_TOKEN} {path}"),
        format!("         {reason} \u{2014} setting not changed"),
    ])
}

/// The `Report dir:` read-out (#472 UX): the path the daemon holds, a
/// `cannot write:` continuation when its probe failed, or the unset form
/// naming the consequence. A reply without `report_dir_status` (an older
/// daemon) prints the path alone.
fn report_dir_lines(
    srv_cfg: &serde_json::Value,
    status: Option<&serde_json::Value>,
) -> Vec<String> {
    let Some(dir) = srv_cfg.get("report_dir").and_then(|v| v.as_str()) else {
        return vec!["  Report dir:    (not set \u{2014} plot results are not saved)".to_string()];
    };
    let mut lines = vec![format!("  Report dir:    {dir}")];
    if let Some(status) = status {
        if status.get("writable").and_then(|v| v.as_bool()) == Some(false) {
            let err = status
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("reason not reported");
            lines.push(format!("                 cannot write: {err}"));
        }
    }
    lines
}

pub fn run(cmd: &CommandKind, cfg: &ac_core::config::Config, client: &mut AcClient) {
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
        report_dir,
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
            report_dir,
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
            report_dir,
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

    if let Some(v) = report_dir {
        match v {
            Some(dir) => {
                // A remote daemon's filesystem is not this one: its path
                // goes over exactly as typed, and the daemon refuses
                // anything that is not absolute there.
                let dir = if cfg.server_host.is_none() {
                    let home = std::env::var("HOME").ok();
                    let cwd = std::env::current_dir().unwrap_or_default();
                    resolve_local_report_dir(dir, home.as_deref(), &cwd)
                } else {
                    dir.clone()
                };
                update.insert("report_dir".into(), dir.into())
            }
            None => update.insert("report_dir".into(), serde_json::Value::Null),
        };
    }

    let has_updates = !update.is_empty();

    let reply = client.send_cmd(&serde_json::json!({"cmd": "setup", "update": update}), None);
    if let Some(lines) = reply.as_ref().and_then(refusal_lines) {
        for line in lines {
            eprintln!("{line}");
        }
        std::process::exit(1);
    }
    let ack = check_ack(reply, "setup");

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
    match ack.get("max_dbfs").and_then(|v| v.as_f64()) {
        Some(max) if max == 0.0 => {
            println!("  Emission max:  {max:.1} dBFS  (full scale, fixed)")
        }
        Some(max) => println!("  Emission max:  {max:.1} dBFS  (fixed)"),
        None => println!("  Emission max:  (not reported by this daemon)"),
    }
    if let Some(retired) = srv_cfg.get("drive_max_dbfs").and_then(|v| v.as_f64()) {
        println!(
            "  Retired key:   drive_max_dbfs {retired:.1}  \
             (every emission refused until removed)"
        );
    }

    // Both the temperature and the speed it implies. #391 removed the
    // delay readout's ms → m conversion this used to serve exclusively —
    // the derived figure stays because it's what the report renderers
    // display alongside an archived measurement's `PositionSnapshot`, and
    // because an unset temperature reads as a different speed from any
    // temperature that could be typed.
    let temp = srv_cfg.get("temperature_c").and_then(|v| v.as_f64());
    let c = ac_core::shared::conversions::speed_of_sound_from_config(temp);
    match temp {
        Some(t) => println!("  Room temp:     {t:.1} °C  (c = {c:.1} m/s)"),
        None => println!("  Room temp:     (not set — c = {c:.1} m/s assumed)"),
    }

    for line in report_dir_lines(&srv_cfg, ack.get("report_dir_status")) {
        println!("{line}");
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

#[cfg(test)]
mod tests {
    use super::{refusal_lines, report_dir_lines, resolve_local_report_dir};
    use serde_json::json;
    use std::path::Path;

    #[test]
    fn local_resolution_expands_home_and_joins_cwd() {
        let cwd = Path::new("/work/rig");
        let home = Some("/home/mui");
        assert_eq!(resolve_local_report_dir("~", home, cwd), "/home/mui");
        assert_eq!(
            resolve_local_report_dir("~/ac-reports", home, cwd),
            "/home/mui/ac-reports"
        );
        assert_eq!(
            resolve_local_report_dir("reports", home, cwd),
            "/work/rig/reports"
        );
        assert_eq!(
            resolve_local_report_dir("./none", home, cwd),
            "/work/rig/none"
        );
        assert_eq!(resolve_local_report_dir("/srv/r", home, cwd), "/srv/r");
        // Case is preserved, and `..` is not resolved (no canonicalize).
        assert_eq!(
            resolve_local_report_dir("../Reports", home, cwd),
            "/work/rig/../Reports"
        );
    }

    #[test]
    fn local_resolution_leaves_what_it_cannot_resolve_for_the_daemon_to_refuse() {
        let cwd = Path::new("/work/rig");
        assert_eq!(
            resolve_local_report_dir("~other/r", Some("/home/mui"), cwd),
            "~other/r"
        );
        assert_eq!(resolve_local_report_dir("~/r", None, cwd), "~/r");
        assert_eq!(resolve_local_report_dir("", Some("/home/mui"), cwd), "");
    }

    #[test]
    fn refusal_renders_two_lines_naming_the_token() {
        let reply = json!({
            "ok": false,
            "error": "report-dir /home/mui/ac-reprots: No such file or directory (os error 2) \u{2014} setting not changed",
            "refused": {
                "key": "report_dir",
                "path": "/home/mui/ac-reprots",
                "reason": "No such file or directory (os error 2)",
            },
        });
        let [a, b] = refusal_lines(&reply).expect("a report_dir refusal");
        assert_eq!(a, "  error: report-dir /home/mui/ac-reprots");
        assert_eq!(
            b,
            "         No such file or directory (os error 2) \u{2014} setting not changed"
        );
    }

    #[test]
    fn other_failures_fall_through_to_check_ack() {
        assert!(refusal_lines(&json!({"ok": false, "error": "config not saved"})).is_none());
        assert!(refusal_lines(&json!({"ok": true, "refused": {"key": "report_dir"}})).is_none());
    }

    #[test]
    fn report_dir_readout_forms() {
        assert_eq!(
            report_dir_lines(&json!({"report_dir": null}), None),
            vec!["  Report dir:    (not set \u{2014} plot results are not saved)"]
        );
        let cfg = json!({"report_dir": "/home/mui/ac-reports"});
        assert_eq!(
            report_dir_lines(&cfg, Some(&json!({"writable": true}))),
            vec!["  Report dir:    /home/mui/ac-reports"]
        );
        assert_eq!(
            report_dir_lines(&cfg, None),
            vec!["  Report dir:    /home/mui/ac-reports"],
            "an older daemon reports no status; the path prints alone"
        );
        assert_eq!(
            report_dir_lines(
                &cfg,
                Some(
                    &json!({"writable": false, "error": "No such file or directory (os error 2)"})
                )
            ),
            vec![
                "  Report dir:    /home/mui/ac-reports",
                "                 cannot write: No such file or directory (os error 2)",
            ]
        );
    }

    #[test]
    fn report_dir_value_starts_at_the_shared_column() {
        let line = &report_dir_lines(&json!({"report_dir": "/x"}), None)[0];
        assert_eq!(line.find("/x"), Some("  Room temp:     ".len()));
    }
}
