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
            _ => {
                return Err(format!(
                    "setup: unknown key {key:?}  \
                     (output | input | reference | device | dburef | dmm | gpio | range)"
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
        },
        show_plot: false,
    })
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
}
