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
}
