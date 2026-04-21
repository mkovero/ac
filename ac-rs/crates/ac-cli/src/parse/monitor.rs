//\! `parse_monitor` — subcommand parser extracted from `parse/mod.rs`.

use super::*;

pub(super) fn parse_monitor(args: &[String], show_plot: bool) -> Result<ParsedCommand, String> {
    let mut args = args.to_vec();

    // Optional leading mode word: spectrum | cwt | cqt | reassigned.
    // Absent or a channel/numeric-looking token → default FFT spectrum
    // (preserves `ac monitor 0-3 20hz 20khz 0.1s`).
    let mode = match args.first() {
        Some(a) if a.eq_ignore_ascii_case("spectrum") => { args.remove(0); "spectrum" }
        Some(a) if a.eq_ignore_ascii_case("cwt")      => { args.remove(0); "cwt" }
        Some(a) if a.eq_ignore_ascii_case("cqt")      => { args.remove(0); "cqt" }
        Some(a) if a.eq_ignore_ascii_case("reassigned") => { args.remove(0); "reassigned" }
        _ => "spectrum",
    };

    if mode == "cqt" {
        return Ok(ParsedCommand {
            cmd: CommandKind::MonitorNotImplemented { technique: "cqt" },
            show_plot,
        });
    }
    if mode == "reassigned" {
        return Ok(ParsedCommand {
            cmd: CommandKind::MonitorNotImplemented { technique: "reassigned" },
            show_plot,
        });
    }

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
    let cmd = if mode == "cwt" {
        CommandKind::MonitorCwt { start_freq, end_freq, interval, channels }
    } else {
        CommandKind::Monitor { start_freq, end_freq, interval, channels }
    };
    Ok(ParsedCommand { cmd, show_plot })
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

    #[test]
    fn test_monitor_spectrum_explicit() {
        let p = parse(&args("monitor spectrum")).unwrap();
        assert!(matches!(p.cmd, CommandKind::Monitor { .. }));
    }

    #[test]
    fn test_monitor_cwt() {
        let p = parse(&args("monitor cwt")).unwrap();
        assert!(matches!(p.cmd, CommandKind::MonitorCwt { .. }));
    }

    #[test]
    fn test_monitor_cwt_with_channels() {
        let p = parse(&args("monitor cwt 0-3")).unwrap();
        match p.cmd {
            CommandKind::MonitorCwt { channels, .. } => {
                assert_eq!(channels, Some(vec![0, 1, 2, 3]));
            }
            other => panic!("expected MonitorCwt, got {other:?}"),
        }
    }

    #[test]
    fn test_monitor_cqt_not_implemented() {
        let p = parse(&args("monitor cqt")).unwrap();
        match p.cmd {
            CommandKind::MonitorNotImplemented { technique } => assert_eq!(technique, "cqt"),
            other => panic!("expected MonitorNotImplemented, got {other:?}"),
        }
    }

    #[test]
    fn test_monitor_reassigned_not_implemented() {
        let p = parse(&args("monitor reassigned")).unwrap();
        match p.cmd {
            CommandKind::MonitorNotImplemented { technique } => {
                assert_eq!(technique, "reassigned");
            }
            other => panic!("expected MonitorNotImplemented, got {other:?}"),
        }
    }
}
