//\! `parse_monitor` — subcommand parser extracted from `parse/mod.rs`.

use super::*;

pub(super) fn parse_monitor(args: &[String], show_plot: bool) -> Result<ParsedCommand, String> {
    let mut args = args.to_vec();
    let channels = if args.first().map_or(false, |a| is_channel_spec(a)) {
        Some(parse_channels(&args.remove(0))?)
    } else {
        None
    };
    let mut tokens = classify_all(&args)?;
    let start_freq = pull(&mut tokens, TokenKind::Freq)
        .map(|v| v.as_f64())
        .unwrap_or(20.0);
    let end_freq = pull(&mut tokens, TokenKind::Freq)
        .map(|v| v.as_f64())
        .unwrap_or(20000.0);
    let interval = pull(&mut tokens, TokenKind::Time)
        .map(|v| v.as_f64())
        .unwrap_or(0.1);
    check_empty(&tokens)?;
    Ok(ParsedCommand {
        cmd: CommandKind::Monitor {
            start_freq,
            end_freq,
            interval,
            channels,
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
    fn test_monitor() {
        let p = parse(&args("monitor")).unwrap();
        match p.cmd {
            CommandKind::Monitor { start_freq, end_freq, interval, .. } => {
                assert!((start_freq - 20.0).abs() < 1e-9);
                assert!((end_freq - 20000.0).abs() < 1e-9);
                assert!((interval - 0.1).abs() < 1e-9);
            }
            other => panic!("expected Monitor, got {other:?}"),
        }
    }

    #[test]
    fn test_monitor_show() {
        let p = parse(&args("m sh")).unwrap();
        assert!(p.show_plot);
        assert!(matches!(p.cmd, CommandKind::Monitor { .. }));
    }

    #[test]
    fn test_monitor_channels() {
        let p = parse(&args("monitor 0-3,5")).unwrap();
        match p.cmd {
            CommandKind::Monitor { channels, .. } => {
                assert_eq!(channels, Some(vec![0, 1, 2, 3, 5]));
            }
            other => panic!("expected Monitor, got {other:?}"),
        }
    }

    #[test]
    fn test_monitor_no_channels() {
        let p = parse(&args("monitor")).unwrap();
        match p.cmd {
            CommandKind::Monitor { channels, .. } => {
                assert_eq!(channels, None);
            }
            other => panic!("expected Monitor, got {other:?}"),
        }
    }
}
