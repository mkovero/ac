//\! `parse_calibrate` — subcommand parser extracted from `parse/mod.rs`.

use super::*;

pub(super) fn parse_calibrate(args: &[String], show_plot: bool) -> Result<ParsedCommand, String> {
    let mut output_channel = None;
    let mut input_channel = None;
    let mut remaining: Vec<&String> = args.iter().collect();
    let mut clean = Vec::new();

    while !remaining.is_empty() {
        let key = expand(remaining[0]);
        if (key == "output" || key == "input") && remaining.len() > 1 {
            remaining.remove(0);
            let val_str = remaining.remove(0);
            let val: u32 = val_str
                .parse()
                .map_err(|_| format!("calibrate: {key:?} value must be an integer, got {val_str:?}"))?;
            if key == "output" {
                output_channel = Some(val);
            } else {
                input_channel = Some(val);
            }
        } else {
            clean.push(remaining.remove(0).clone());
        }
    }

    let mut tokens = classify_all(&clean)?;
    let level = pull(&mut tokens, TokenKind::Level)
        .map(|v| v.as_level())
        .unwrap_or(LevelSpec::Dbfs(-10.0));
    check_empty(&tokens)?;

    Ok(ParsedCommand {
        cmd: CommandKind::Calibrate {
            level,
            output_channel,
            input_channel,
        },
        show_plot,
    })
}

/// `ac calibrate mic-curve <path|clear> [input N] [output N]` — attach
/// or clear a mic frequency-response correction curve. The CLI parses
/// the file (so the user gets immediate feedback on bad files) and
/// uploads validated arrays to the daemon; `path == "clear"` drops
/// any stored curve on the channel.
pub(super) fn parse_calibrate_mic_curve(args: &[String]) -> Result<ParsedCommand, String> {
    if args.is_empty() {
        return Err("calibrate mic-curve: expected <path|clear> [input N] [output N]".into());
    }
    let mut output_channel = None;
    let mut input_channel = None;
    let mut remaining: Vec<&String> = args.iter().collect();
    let raw_path = remaining.remove(0).clone();
    let path = if raw_path.eq_ignore_ascii_case("clear") {
        None
    } else {
        Some(raw_path)
    };
    while !remaining.is_empty() {
        let key = expand(remaining[0]);
        if (key == "output" || key == "input") && remaining.len() > 1 {
            remaining.remove(0);
            let val_str = remaining.remove(0);
            let val: u32 = val_str.parse().map_err(|_| {
                format!("calibrate mic-curve: {key:?} value must be an integer, got {val_str:?}")
            })?;
            if key == "output" {
                output_channel = Some(val);
            } else {
                input_channel = Some(val);
            }
        } else {
            return Err(format!("calibrate mic-curve: unexpected token {:?}", remaining[0]));
        }
    }
    Ok(ParsedCommand {
        cmd: CommandKind::CalibrateMicCurve { path, output_channel, input_channel },
        show_plot: false,
    })
}

/// `ac calibrate spl [input N] [output N]` — pistonphone-reference SPL cal.
/// Voltage-cal arguments don't apply (no level / no playback), so this
/// parser only knows about channel selection.
pub(super) fn parse_calibrate_spl(args: &[String]) -> Result<ParsedCommand, String> {
    let mut output_channel = None;
    let mut input_channel = None;
    let mut remaining: Vec<&String> = args.iter().collect();

    while !remaining.is_empty() {
        let key = expand(remaining[0]);
        if (key == "output" || key == "input") && remaining.len() > 1 {
            remaining.remove(0);
            let val_str = remaining.remove(0);
            let val: u32 = val_str.parse().map_err(|_| {
                format!("calibrate spl: {key:?} value must be an integer, got {val_str:?}")
            })?;
            if key == "output" {
                output_channel = Some(val);
            } else {
                input_channel = Some(val);
            }
        } else {
            return Err(format!("calibrate spl: unexpected token {:?}", remaining[0]));
        }
    }

    Ok(ParsedCommand {
        cmd: CommandKind::CalibrateSpl {
            output_channel,
            input_channel,
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
    fn test_calibrate() {
        let p = parse(&args("calibrate")).unwrap();
        match p.cmd {
            CommandKind::Calibrate { level, output_channel, input_channel } => {
                assert!(matches!(level, LevelSpec::Dbfs(v) if (v - (-10.0)).abs() < 1e-9));
                assert!(output_channel.is_none());
                assert!(input_channel.is_none());
            }
            other => panic!("expected Calibrate, got {other:?}"),
        }
    }

    #[test]
    fn test_calibrate_show() {
        let p = parse(&args("calibrate show")).unwrap();
        assert!(matches!(p.cmd, CommandKind::CalibrateShow));

        let p = parse(&args("cal show")).unwrap();
        assert!(matches!(p.cmd, CommandKind::CalibrateShow));
    }

    #[test]
    fn test_calibrate_with_channels() {
        let p = parse(&args("calibrate output 3 input 1")).unwrap();
        match p.cmd {
            CommandKind::Calibrate { output_channel, input_channel, .. } => {
                assert_eq!(output_channel, Some(3));
                assert_eq!(input_channel, Some(1));
            }
            other => panic!("expected Calibrate, got {other:?}"),
        }
    }

    #[test]
    fn test_calibrate_spl_default_channels() {
        let p = parse(&args("calibrate spl")).unwrap();
        match p.cmd {
            CommandKind::CalibrateSpl { output_channel, input_channel } => {
                assert!(output_channel.is_none());
                assert!(input_channel.is_none());
            }
            other => panic!("expected CalibrateSpl, got {other:?}"),
        }
    }

    #[test]
    fn test_calibrate_spl_with_channels() {
        let p = parse(&args("calibrate spl input 2 output 1")).unwrap();
        match p.cmd {
            CommandKind::CalibrateSpl { output_channel, input_channel } => {
                assert_eq!(output_channel, Some(1));
                assert_eq!(input_channel, Some(2));
            }
            other => panic!("expected CalibrateSpl, got {other:?}"),
        }
    }

    #[test]
    fn test_cal_spl_alias() {
        let p = parse(&args("cal spl")).unwrap();
        assert!(matches!(p.cmd, CommandKind::CalibrateSpl { .. }));
    }

    #[test]
    fn test_calibrate_mic_curve_path() {
        let p = parse(&args("calibrate mic-curve /tmp/foo.frd input 1")).unwrap();
        match p.cmd {
            CommandKind::CalibrateMicCurve { path, output_channel, input_channel } => {
                assert_eq!(path, Some("/tmp/foo.frd".into()));
                assert!(output_channel.is_none());
                assert_eq!(input_channel, Some(1));
            }
            other => panic!("expected CalibrateMicCurve, got {other:?}"),
        }
    }

    #[test]
    fn test_calibrate_mic_curve_clear() {
        let p = parse(&args("cal mic-curve clear input 0")).unwrap();
        match p.cmd {
            CommandKind::CalibrateMicCurve { path, input_channel, .. } => {
                assert!(path.is_none());
                assert_eq!(input_channel, Some(0));
            }
            other => panic!("expected CalibrateMicCurve, got {other:?}"),
        }
    }
}
