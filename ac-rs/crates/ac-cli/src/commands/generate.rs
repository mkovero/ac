use super::{
    await_session_check, check_ack, level_to_dbfs, level_unit, print_consumer_check, print_level,
    verdict_inline, voltage_scale, Scale,
};
use crate::client::AcClient;
use crate::parse::{CommandKind, LevelSpec};

pub fn run_sine(cmd: &CommandKind, client: &mut AcClient) {
    let (level, level_defaulted, freq, ch_spec) = match cmd {
        CommandKind::GenerateSine {
            level,
            level_defaulted,
            freq,
            channels,
        } => (level, *level_defaulted, *freq, channels),
        _ => unreachable!(),
    };

    let channels = resolve_channels(ch_spec, client, "sine");

    println!();
    let mut infos = channel_levels(client, &channels, level);
    let dbfs = infos
        .first()
        .map(|(_, d, _)| *d)
        .unwrap_or(ac_core::shared::emission_level::DEFAULT_LEVEL_DBFS);
    let consumes = infos
        .iter()
        .any(|(_, _, c)| voltage_scale(c.as_ref()).is_some())
        || level_unit(level) != "dbfs";
    let ack = check_ack(
        client.send_cmd(
            &serde_json::json!({
                "cmd": "generate",
                "freq_hz": freq,
                "level_dbfs": dbfs,
                "channels": channels,
                "level_unit": level_unit(level),
            }),
            None,
        ),
        "generate",
    );
    // #466: the channel lines wait for the session check.
    if check_then_print(
        client,
        "generate",
        &ack,
        &mut infos,
        level,
        consumes,
        Some(freq),
    )
    .is_err()
    {
        std::process::exit(1);
    }
    print_level(
        ack.get("level_dbfs").and_then(|v| v.as_f64()),
        level_defaulted,
        ack.get("max_dbfs").and_then(|v| v.as_f64()),
        None,
        false,
    );
    if let Some(ports) = ack.get("out_ports").and_then(|v| v.as_array()) {
        for p in ports {
            if let Some(s) = p.as_str() {
                println!("  -> {s}");
            }
        }
    }
    let n = channels.len();
    println!("\n  Playing {n} channel(s)... Ctrl+C or q to stop.\n");

    wait_for_stop(client, "generate");
}

pub fn run_pink(cmd: &CommandKind, client: &mut AcClient) {
    let (level, level_defaulted, ch_spec) = match cmd {
        CommandKind::GeneratePink {
            level,
            level_defaulted,
            channels,
        } => (level, *level_defaulted, channels),
        _ => unreachable!(),
    };

    let channels = resolve_channels(ch_spec, client, "pink");

    println!();
    let mut infos = channel_levels(client, &channels, level);
    let dbfs = infos
        .first()
        .map(|(_, d, _)| *d)
        .unwrap_or(ac_core::shared::emission_level::DEFAULT_LEVEL_DBFS);
    let consumes = infos
        .iter()
        .any(|(_, _, c)| voltage_scale(c.as_ref()).is_some())
        || level_unit(level) != "dbfs";
    let ack = check_ack(
        client.send_cmd(
            &serde_json::json!({
                "cmd": "generate_pink",
                "level_dbfs": dbfs,
                "channels": channels,
                "level_unit": level_unit(level),
            }),
            None,
        ),
        "generate_pink",
    );
    if check_then_print(
        client,
        "generate_pink",
        &ack,
        &mut infos,
        level,
        consumes,
        None,
    )
    .is_err()
    {
        std::process::exit(1);
    }
    print_level(
        ack.get("level_dbfs").and_then(|v| v.as_f64()),
        level_defaulted,
        ack.get("max_dbfs").and_then(|v| v.as_f64()),
        None,
        false,
    );
    if let Some(ports) = ack.get("out_ports").and_then(|v| v.as_array()) {
        for p in ports {
            if let Some(s) = p.as_str() {
                println!("  -> {s}");
            }
        }
    }
    let n = channels.len();
    println!("\n  Playing pink noise on {n} channel(s)... Ctrl+C or q to stop.\n");

    wait_for_stop(client, "generate_pink");
}

/// The channels to report and send. An explicit list was already validated
/// by the parser, so it is used as given; only an omitted list asks the
/// daemon for the configured output. When that fails, nothing is sent and
/// the process exits 1 (#517) — no channel is invented.
fn resolve_channels(ch_spec: &Option<Vec<u32>>, client: &mut AcClient, subcmd: &str) -> Vec<u32> {
    if let Some(channels) = ch_spec {
        return channels.clone();
    }
    let reply = client.send_cmd(&serde_json::json!({"cmd": "setup", "update": {}}), None);
    match configured_output(reply.as_ref()) {
        Ok(ch) => vec![ch],
        Err(e) => {
            for line in unresolved_channel_lines(&e, subcmd) {
                eprintln!("{line}");
            }
            std::process::exit(1);
        }
    }
}

/// Why the configured output could not be read from a `setup` reply.
#[derive(Debug, PartialEq)]
enum UnresolvedChannel {
    /// No reply at all (timeout, socket failure).
    NoReply,
    /// `ok` was not `true`; the daemon's `error` text, if any.
    Refused(Option<String>),
    /// `ok: true` but no `config.output_channel`.
    NoKey,
    /// `output_channel` present but not a `u32`.
    Unusable(serde_json::Value),
}

/// The configured output channel from a `setup` reply, or why there is none.
/// `ok` is checked first: a refused reply carries no `config`.
fn configured_output(reply: Option<&serde_json::Value>) -> Result<u32, UnresolvedChannel> {
    let reply = reply.ok_or(UnresolvedChannel::NoReply)?;
    if reply.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        return Err(UnresolvedChannel::Refused(
            reply
                .get("error")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        ));
    }
    let v = reply
        .get("config")
        .and_then(|c| c.get("output_channel"))
        .ok_or(UnresolvedChannel::NoKey)?;
    v.as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| UnresolvedChannel::Unusable(v.clone()))
}

/// The stderr refusal for an unresolved default channel (#517 UX). The last
/// line is the same for every cause.
fn unresolved_channel_lines(e: &UnresolvedChannel, subcmd: &str) -> Vec<String> {
    const INDENT: &str = "         ";
    let mut lines = Vec::new();
    match e {
        UnresolvedChannel::NoReply => {
            lines.push("  error: no channel given, and the server did not answer `setup`".into());
        }
        UnresolvedChannel::Refused(msg) => {
            lines.push("  error: no channel given, and the server refused `setup`:".into());
            match msg {
                Some(m) => {
                    let mut it = m.lines();
                    if let Some(first) = it.next() {
                        lines.push(format!("{INDENT}{first}"));
                    }
                    lines.extend(it.map(str::to_string));
                }
                None => lines.push(format!("{INDENT}server gave no reason")),
            }
        }
        UnresolvedChannel::NoKey => {
            lines.push(
                "  error: no channel given, and the `setup` reply has no output_channel".into(),
            );
        }
        UnresolvedChannel::Unusable(v) => {
            lines.push(
                "  error: no channel given, and `setup` returned an unusable output_channel".into(),
            );
            let text = serde_json::to_string(v).unwrap_or_default();
            let shown = if text.chars().count() > 54 {
                let mut t: String = text.chars().take(53).collect();
                t.push('\u{2026}');
                t
            } else {
                text
            };
            lines.push(format!("{INDENT}output_channel = {shown}"));
        }
    }
    lines.push(format!(
        "{INDENT}nothing was emitted \u{2014} pass the channel: ac generate {subcmd} <N>"
    ));
    lines
}

fn get_cal_for_channel(client: &mut AcClient, ch: u32) -> Option<serde_json::Value> {
    let reply = client.send_cmd(
        &serde_json::json!({"cmd": "get_calibration", "output_channel": ch}),
        None,
    )?;
    if reply.get("found").and_then(|v| v.as_bool()) == Some(true) {
        Some(reply)
    } else {
        None
    }
}

/// `(channel, dBFS, calibration reply)` per channel, before the command is
/// sent. A physical level whose scale is refused exits here.
fn channel_levels(
    client: &mut AcClient,
    channels: &[u32],
    level: &LevelSpec,
) -> Vec<(u32, f64, Option<serde_json::Value>)> {
    channels
        .iter()
        .map(|&ch| {
            let cal = get_cal_for_channel(client, ch);
            let dbfs = level_to_dbfs(level, cal.as_ref());
            (ch, dbfs, cal)
        })
        .collect()
}

/// Wait for the session check, print it, then the channel lines under the
/// verdicts it leaves (#466).
fn check_then_print(
    client: &mut AcClient,
    cmd_name: &str,
    ack: &serde_json::Value,
    infos: &mut [(u32, f64, Option<serde_json::Value>)],
    level: &LevelSpec,
    consumes: bool,
    freq: Option<f64>,
) -> Result<(), ()> {
    let wait = await_session_check(client, cmd_name, ack, 30_000);
    let frame = match &wait {
        super::SessionWait::Frame(f) => Some(f.clone()),
        _ => None,
    };
    let mut first = infos.first().and_then(|(_, _, c)| c.clone());
    print_consumer_check(wait, &mut first, Some(level), consumes)?;
    for (ch, dbfs, cal) in infos.iter_mut() {
        if let Some(f) = &frame {
            super::apply_fresh_verdict(cal, f);
        }
        print_channel_info(*ch, freq, *dbfs, cal);
    }
    Ok(())
}

fn print_channel_info(ch: u32, freq: Option<f64>, dbfs: f64, cal: &Option<serde_json::Value>) {
    for line in channel_info_lines(ch, freq, dbfs, cal.as_ref()) {
        println!("{line}");
    }
}

/// One channel's line, and the unverified-scale warning under it (#466 UX).
fn channel_info_lines(
    ch: u32,
    freq: Option<f64>,
    dbfs: f64,
    cal: Option<&serde_json::Value>,
) -> Vec<String> {
    let mut warning = Vec::new();
    let (vrms_s, cal_tag) = match voltage_scale(cal) {
        Some(Scale::Usable(ref_vrms, verdict)) => {
            let vrms = ref_vrms * 10.0_f64.powf(dbfs / 20.0);
            let dbu = ac_core::shared::conversions::vrms_to_dbu(vrms);
            if let ac_core::shared::calibration::LayerVerdict::Unverified { cause, reason } =
                &verdict
            {
                let when = cal
                    .and_then(|c| c.get("loop_gain_baseline"))
                    .and_then(|b| b.get("measured_at"))
                    .and_then(|v| v.as_str())
                    .map(ac_core::shared::calibration::session::whole_seconds);
                warning.push(match when {
                    Some(t) => format!("  warning: dBu scale from calibration of {t},"),
                    None => "  warning: dBu scale from an earlier calibration,".to_string(),
                });
                warning.push(format!(
                    "           unverified this session \u{2014} {}",
                    verdict_inline(*cause, reason)
                ));
            }
            (
                ac_core::shared::conversions::fmt_vrms(vrms),
                format!("{dbu:+.2} dBu"),
            )
        }
        Some(Scale::Withheld(_)) => ("-".to_string(), format!("{dbfs:.1} dBFS (voltage refused)")),
        None => ("  -".to_string(), format!("{dbfs:.1} dBFS (uncal)")),
    };

    let mut lines = vec![match freq {
        Some(f) => format!("  ch {ch:>3}  {f:.0} Hz  {vrms_s:>14}  {cal_tag}"),
        None => format!("  ch {ch:>3}  pink noise  {vrms_s:>14}  {cal_tag}"),
    }];
    lines.extend(warning);
    lines
}

pub(crate) fn wait_for_stop(client: &mut AcClient, cmd_name: &str) {
    crossterm::terminal::enable_raw_mode().ok();
    let result = wait_loop(client, cmd_name);
    crossterm::terminal::disable_raw_mode().ok();
    if let Err(reason) = result {
        println!("\n  {reason}");
    }
}

fn wait_loop(client: &mut AcClient, cmd_name: &str) -> Result<(), String> {
    loop {
        if crossterm::event::poll(std::time::Duration::from_millis(50)).unwrap_or(false) {
            if let Ok(crossterm::event::Event::Key(key)) = crossterm::event::read() {
                use crossterm::event::KeyCode;
                match key.code {
                    KeyCode::Char('q') | KeyCode::Char('Q') => {
                        crossterm::terminal::disable_raw_mode().ok();
                        client
                            .send_cmd(&serde_json::json!({"cmd": "stop", "name": cmd_name}), None);
                        return Err("Stopped.".into());
                    }
                    KeyCode::Char('c')
                        if key
                            .modifiers
                            .contains(crossterm::event::KeyModifiers::CONTROL) =>
                    {
                        crossterm::terminal::disable_raw_mode().ok();
                        client
                            .send_cmd(&serde_json::json!({"cmd": "stop", "name": cmd_name}), None);
                        return Err("Stopped.".into());
                    }
                    _ => {}
                }
            }
        }

        if let Some((topic, frame)) = client.recv_data(100) {
            let frame_cmd = frame.get("cmd").and_then(|v| v.as_str()).unwrap_or("");
            if topic == "error" && (frame_cmd.is_empty() || frame_cmd == cmd_name) {
                let msg = frame
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("error");
                return Err(format!("error: {msg}"));
            }
            if topic == "done" && (frame_cmd.is_empty() || frame_cmd == cmd_name) {
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cal(verdict: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "key": "out1_in1",
            "vrms_at_0dbfs_out": 3.4641,
            "loop_gain_baseline": {"measured_at": "2026-09-15T23:43:04.123Z"},
            "session_check": {"voltage": verdict},
        })
    }

    #[test]
    fn a_refused_scale_prints_no_voltage_and_says_why() {
        let c = cal(serde_json::json!({
            "state": "refused", "measured": 2.42, "stored": -0.6, "delta": 3.02,
            "tolerance": 0.1, "unit": "dB", "stored_at": "t", "checked_at": "t",
            "source": "probe",
        }));
        let lines = channel_info_lines(1, Some(1000.0), -40.0, Some(&c));
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].ends_with("-40.0 dBFS (voltage refused)"),
            "{lines:?}"
        );
        assert!(!lines[0].contains("dBu") && !lines[0].contains("Vrms"));
    }

    #[test]
    fn an_unverified_scale_is_applied_with_its_calibration_date() {
        let c = cal(serde_json::json!({
            "state": "unverified", "cause": "no_loopback",
            "reason": "no reference loopback configured; check: x",
        }));
        let lines = channel_info_lines(1, Some(1000.0), -40.0, Some(&c));
        assert!(lines[0].ends_with("-26.99 dBu"), "{lines:?}");
        assert_eq!(
            lines[1],
            "  warning: dBu scale from calibration of 2026-09-15T23:43:04Z,"
        );
        assert_eq!(
            lines[2],
            "           unverified this session \u{2014} no reference loopback configured"
        );
    }

    #[test]
    fn a_verified_scale_prints_no_warning() {
        let c = cal(serde_json::json!({
            "state": "verified", "measured": -0.61, "stored": -0.6, "delta": -0.01,
            "tolerance": 0.1, "unit": "dB", "stored_at": "t", "checked_at": "t",
            "source": "probe",
        }));
        assert_eq!(channel_info_lines(1, None, -40.0, Some(&c)).len(), 1);
    }

    fn classify(reply: serde_json::Value) -> Result<u32, UnresolvedChannel> {
        configured_output(Some(&reply))
    }

    #[test]
    fn a_missing_reply_is_refused_not_defaulted() {
        assert_eq!(configured_output(None), Err(UnresolvedChannel::NoReply));
    }

    #[test]
    fn a_failed_reply_is_refused_with_its_error_text() {
        assert_eq!(
            classify(serde_json::json!({"ok": false, "error": "unknown command: 'setup'"})),
            Err(UnresolvedChannel::Refused(Some(
                "unknown command: 'setup'".into()
            )))
        );
        assert_eq!(
            classify(serde_json::json!({"ok": false})),
            Err(UnresolvedChannel::Refused(None))
        );
        // `ok` is checked before the key: a refused reply that still carries
        // a channel is not used.
        assert_eq!(
            classify(serde_json::json!({"ok": false, "config": {"output_channel": 3}})),
            Err(UnresolvedChannel::Refused(None))
        );
    }

    #[test]
    fn a_reply_without_the_key_is_refused() {
        assert_eq!(
            classify(serde_json::json!({"ok": true})),
            Err(UnresolvedChannel::NoKey)
        );
        assert_eq!(
            classify(serde_json::json!({"ok": true, "config": {}})),
            Err(UnresolvedChannel::NoKey)
        );
    }

    #[test]
    fn a_reply_with_the_key_resolves_to_that_channel() {
        assert_eq!(
            classify(serde_json::json!({"ok": true, "config": {"output_channel": 3}})),
            Ok(3)
        );
        assert_eq!(
            classify(serde_json::json!({"ok": true, "config": {"output_channel": 0}})),
            Ok(0)
        );
        assert_eq!(
            classify(serde_json::json!({"ok": true, "config": {"output_channel": 4294967295u64}})),
            Ok(u32::MAX)
        );
    }

    #[test]
    fn a_value_that_is_not_a_u32_is_unusable_not_narrowed() {
        for v in [
            serde_json::json!(4294967296u64),
            serde_json::json!("3"),
            serde_json::json!(-1),
            serde_json::json!(3.0),
            serde_json::Value::Null,
        ] {
            assert_eq!(
                classify(serde_json::json!({"ok": true, "config": {"output_channel": v.clone()}})),
                Err(UnresolvedChannel::Unusable(v))
            );
        }
    }

    #[test]
    fn refusal_lines_match_the_ux_spec() {
        assert_eq!(
            unresolved_channel_lines(&UnresolvedChannel::NoReply, "sine"),
            [
                "  error: no channel given, and the server did not answer `setup`",
                "         nothing was emitted \u{2014} pass the channel: ac generate sine <N>",
            ]
        );
        assert_eq!(
            unresolved_channel_lines(
                &UnresolvedChannel::Refused(Some("unknown command: 'setup'".into())),
                "pink"
            ),
            [
                "  error: no channel given, and the server refused `setup`:",
                "         unknown command: 'setup'",
                "         nothing was emitted \u{2014} pass the channel: ac generate pink <N>",
            ]
        );
        assert_eq!(
            unresolved_channel_lines(&UnresolvedChannel::Refused(None), "sine")[1],
            "         server gave no reason"
        );
        assert_eq!(
            unresolved_channel_lines(&UnresolvedChannel::NoKey, "sine"),
            [
                "  error: no channel given, and the `setup` reply has no output_channel",
                "         nothing was emitted \u{2014} pass the channel: ac generate sine <N>",
            ]
        );
        assert_eq!(
            unresolved_channel_lines(
                &UnresolvedChannel::Unusable(serde_json::json!(4294967296u64)),
                "sine"
            ),
            [
                "  error: no channel given, and `setup` returned an unusable output_channel",
                "         output_channel = 4294967296",
                "         nothing was emitted \u{2014} pass the channel: ac generate sine <N>",
            ]
        );
        assert_eq!(
            unresolved_channel_lines(&UnresolvedChannel::Unusable(serde_json::json!("3")), "sine")
                [1],
            "         output_channel = \"3\""
        );
    }

    #[test]
    fn a_multi_line_daemon_error_indents_only_its_first_line() {
        let msg = "setup not saved: x\n                 continued here".to_string();
        let lines = unresolved_channel_lines(&UnresolvedChannel::Refused(Some(msg)), "sine");
        assert_eq!(lines[1], "         setup not saved: x");
        assert_eq!(lines[2], "                 continued here");
    }

    #[test]
    fn a_long_unusable_value_is_truncated_to_80_columns() {
        let long = "x".repeat(100);
        let lines = unresolved_channel_lines(
            &UnresolvedChannel::Unusable(serde_json::Value::String(long)),
            "sine",
        );
        let expected = format!("         output_channel = \"{}\u{2026}", "x".repeat(52));
        assert_eq!(lines[1], expected);
        assert_eq!(lines[1].chars().count(), 80);
        // Exactly 54 characters of JSON text is shown whole.
        let fits = serde_json::Value::String("y".repeat(52));
        let lines = unresolved_channel_lines(&UnresolvedChannel::Unusable(fits), "sine");
        assert_eq!(
            lines[1],
            format!("         output_channel = \"{}\"", "y".repeat(52))
        );
    }

    #[test]
    fn the_last_refusal_line_is_the_same_for_every_cause() {
        let causes = [
            UnresolvedChannel::NoReply,
            UnresolvedChannel::Refused(None),
            UnresolvedChannel::Refused(Some("a\nb".into())),
            UnresolvedChannel::NoKey,
            UnresolvedChannel::Unusable(serde_json::Value::Null),
        ];
        for sub in ["sine", "pink"] {
            let expected = format!(
                "         nothing was emitted \u{2014} pass the channel: ac generate {sub} <N>"
            );
            for c in &causes {
                let lines = unresolved_channel_lines(c, sub);
                assert_eq!(lines.last().unwrap(), &expected, "{c:?}");
                assert!(lines.iter().all(|l| l.chars().count() <= 80), "{lines:?}");
            }
        }
    }
}
