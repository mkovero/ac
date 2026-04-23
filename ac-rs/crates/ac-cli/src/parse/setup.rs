//\! `parse_setup` — subcommand parser extracted from `parse/mod.rs`.

use super::*;

pub(super) fn parse_setup(args: &[String]) -> Result<ParsedCommand, String> {
    let mut output = None;
    let mut input = None;
    let mut reference = None;
    let mut device = None;
    let mut dbu_ref_vrms = None;
    let mut dmm_host = None;
    let mut gpio_port: Option<Option<String>> = None;
    let mut range_start = None;
    let mut range_stop = None;
    let mut server_idle_timeout_secs: Option<Option<u64>> = None;

    let mut remaining: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

    while !remaining.is_empty() {
        let key = expand(remaining.remove(0));
        if remaining.is_empty() {
            return Err(format!("setup: {key:?} needs a value"));
        }
        let val = remaining.remove(0);

        match key {
            "output" | "input" | "reference" | "device" => {
                let n: u32 = val.parse().map_err(|_| {
                    format!("setup: {key:?} value must be an integer, got {val:?}")
                })?;
                match key {
                    "output" => output = Some(n),
                    "input" => input = Some(n),
                    "reference" => reference = Some(n),
                    "device" => device = Some(n),
                    _ => unreachable!(),
                }
            }
            "dburef" | "dbu" => {
                let lvl = parse_level(val)
                    .map_err(|_| format!("setup dburef: expected voltage e.g. 775mv or 0.775v, got {val:?}"))?;
                match lvl {
                    LevelSpec::Vrms(v) => dbu_ref_vrms = Some(v),
                    _ => {
                        return Err(format!(
                            "setup dburef: expected voltage e.g. 775mv or 0.775v, got {val:?}"
                        ))
                    }
                }
            }
            "dmm" => dmm_host = Some(val.to_string()),
            "gpio" => {
                let lower = val.to_lowercase();
                if matches!(lower.as_str(), "none" | "off" | "disable" | "disabled") {
                    gpio_port = Some(None);
                } else {
                    gpio_port = Some(Some(val.to_string()));
                }
            }
            "range" => {
                range_start = Some(
                    parse_freq(val).map_err(|_| {
                        format!("setup range: expected frequency for start, got {val:?}")
                    })?,
                );
                if remaining.is_empty() {
                    return Err("setup range: needs two frequencies (start stop)".into());
                }
                let stop_val = remaining.remove(0);
                range_stop = Some(parse_freq(stop_val).map_err(|_| {
                    format!("setup range: expected frequency for stop, got {stop_val:?}")
                })?);
            }
            "server-timeout" | "server-idle-timeout" => {
                server_idle_timeout_secs = Some(parse_idle_duration(val).map_err(|e| {
                    format!("setup {key}: {e} (got {val:?})")
                })?);
            }
            _ => {
                return Err(format!(
                    "setup: unknown key {key:?}  \
                     (output | input | reference | device | dburef | dmm | gpio | range | \
                     server-timeout)"
                ))
            }
        }
    }

    Ok(ParsedCommand {
        cmd: CommandKind::Setup {
            output,
            input,
            reference,
            device,
            dbu_ref_vrms,
            dmm_host,
            gpio_port,
            range_start,
            range_stop,
            server_idle_timeout_secs,
        },
        show_plot: false,
    })
}

/// Parse a short duration spec — accepts bare integer seconds, an integer
/// with a `s`/`m`/`h` suffix, or one of `off | none | disable | 0` meaning
/// "no timeout". Returns `Ok(None)` for the disable forms, `Ok(Some(secs))`
/// for a real duration.
fn parse_idle_duration(s: &str) -> Result<Option<u64>, String> {
    let lower = s.to_lowercase();
    if matches!(lower.as_str(), "off" | "none" | "disable" | "disabled" | "0") {
        return Ok(None);
    }
    let (num_str, multiplier): (&str, u64) = if let Some(n) = lower.strip_suffix('h') {
        (n, 3600)
    } else if let Some(n) = lower.strip_suffix('m') {
        (n, 60)
    } else if let Some(n) = lower.strip_suffix('s') {
        (n, 1)
    } else {
        (lower.as_str(), 1)
    };
    let n: u64 = num_str
        .parse()
        .map_err(|_| "expected duration like 2h, 30m, 120s, or 'off'".to_string())?;
    if n == 0 {
        return Ok(None);
    }
    Ok(Some(n * multiplier))
}

#[cfg(test)]
mod tests {
    use super::super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn test_setup_output_input() {
        let p = parse(&args("setup output 11 input 0")).unwrap();
        match p.cmd {
            CommandKind::Setup { output, input, .. } => {
                assert_eq!(output, Some(11));
                assert_eq!(input, Some(0));
            }
            other => panic!("expected Setup, got {other:?}"),
        }
    }

    #[test]
    fn test_setup_abbreviated() {
        let p = parse(&args("se o 11 i 0")).unwrap();
        match p.cmd {
            CommandKind::Setup { output, input, .. } => {
                assert_eq!(output, Some(11));
                assert_eq!(input, Some(0));
            }
            other => panic!("expected Setup, got {other:?}"),
        }
    }

    #[test]
    fn test_setup_range() {
        let p = parse(&args("setup range 20hz 20khz")).unwrap();
        match p.cmd {
            CommandKind::Setup { range_start, range_stop, .. } => {
                assert!((range_start.unwrap() - 20.0).abs() < 1e-9);
                assert!((range_stop.unwrap() - 20000.0).abs() < 1e-9);
            }
            other => panic!("expected Setup with range, got {other:?}"),
        }
    }

    #[test]
    fn test_setup_dmm() {
        let p = parse(&args("setup dmm 192.168.1.100")).unwrap();
        match p.cmd {
            CommandKind::Setup { dmm_host, .. } => {
                assert_eq!(dmm_host, Some("192.168.1.100".to_string()));
            }
            other => panic!("expected Setup with dmm, got {other:?}"),
        }
    }

    fn timeout_of(s: &str) -> Option<Option<u64>> {
        let p = parse(&args(s)).unwrap();
        match p.cmd {
            CommandKind::Setup { server_idle_timeout_secs, .. } => server_idle_timeout_secs,
            other => panic!("expected Setup, got {other:?}"),
        }
    }

    #[test]
    fn test_setup_server_timeout_hours() {
        assert_eq!(timeout_of("setup server-timeout 2h"), Some(Some(7200)));
    }

    #[test]
    fn test_setup_server_timeout_minutes() {
        assert_eq!(timeout_of("setup server-timeout 30m"), Some(Some(1800)));
    }

    #[test]
    fn test_setup_server_timeout_seconds_explicit() {
        assert_eq!(timeout_of("setup server-timeout 120s"), Some(Some(120)));
    }

    #[test]
    fn test_setup_server_timeout_seconds_bare() {
        assert_eq!(timeout_of("setup server-timeout 90"), Some(Some(90)));
    }

    #[test]
    fn test_setup_server_timeout_off() {
        assert_eq!(timeout_of("setup server-timeout off"), Some(None));
        assert_eq!(timeout_of("setup server-timeout 0"), Some(None));
        assert_eq!(timeout_of("setup server-timeout disable"), Some(None));
    }

    #[test]
    fn test_setup_server_idle_timeout_alias() {
        assert_eq!(timeout_of("setup server-idle-timeout 1h"), Some(Some(3600)));
    }

    #[test]
    fn test_setup_server_timeout_rejects_garbage() {
        assert!(parse(&args("setup server-timeout banana")).is_err());
    }

    #[test]
    fn test_setup_server_timeout_does_not_touch_other_fields() {
        let p = parse(&args("setup server-timeout 5m")).unwrap();
        match p.cmd {
            CommandKind::Setup {
                output, input, reference, device,
                dbu_ref_vrms, dmm_host, gpio_port, range_start, range_stop,
                server_idle_timeout_secs,
            } => {
                assert!(output.is_none() && input.is_none() && reference.is_none());
                assert!(device.is_none() && dbu_ref_vrms.is_none() && dmm_host.is_none());
                assert!(gpio_port.is_none() && range_start.is_none() && range_stop.is_none());
                assert_eq!(server_idle_timeout_secs, Some(Some(300)));
            }
            other => panic!("expected Setup, got {other:?}"),
        }
    }
}
