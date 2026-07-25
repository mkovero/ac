//\! `parse_monitor` — subcommand parser extracted from `parse/mod.rs`.

use super::*;

pub(super) fn parse_monitor(args: &[String], show_plot: bool) -> Result<ParsedCommand, String> {
    let mut args = args.to_vec();

    // `--tui` keeps the ratatui terminal monitor (M4d-CLI); strip it
    // before positional parsing so it can sit anywhere.
    let tui = args.iter().any(|a| a == "--tui");
    args.retain(|a| a != "--tui");

    // Optional leading mode word: spectrum | cwt | cqt | reassigned.
    // Absent or a channel/numeric-looking token → default FFT spectrum
    // (preserves `ac monitor 0-3 20hz 20khz 0.1s`).
    let mode = match args.first() {
        Some(a) if a.eq_ignore_ascii_case("spectrum") => {
            args.remove(0);
            "spectrum"
        }
        Some(a) if a.eq_ignore_ascii_case("cwt") => {
            args.remove(0);
            "cwt"
        }
        Some(a) if a.eq_ignore_ascii_case("cqt") => {
            args.remove(0);
            "cqt"
        }
        Some(a) if a.eq_ignore_ascii_case("reassigned") => {
            args.remove(0);
            "reassigned"
        }
        _ => "spectrum",
    };

    let channels = if args.first().is_some_and(|a| is_channel_spec(a)) {
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
    let cmd = match mode {
        "cwt" => CommandKind::MonitorCwt {
            start_freq,
            end_freq,
            interval,
            channels,
        },
        "cqt" => CommandKind::MonitorCqt {
            start_freq,
            end_freq,
            interval,
            channels,
        },
        "reassigned" => CommandKind::MonitorReassigned {
            start_freq,
            end_freq,
            interval,
            channels,
        },
        _ => CommandKind::Monitor {
            start_freq,
            end_freq,
            interval,
            channels,
            tui,
        },
    };
    Ok(ParsedCommand { cmd, show_plot })
}

/// `ac transfer [channels]` — launch the transfer view. Channels are
/// optional (the app resolves meas/ref from config); a channel spec
/// overrides the meas leg the same way `ac monitor` does. No `--drive`,
/// no drive option of any kind — a CLI launch is always drive-off.
pub(super) fn parse_transfer(args: &[String]) -> Result<ParsedCommand, String> {
    let mut args = args.to_vec();
    let channels = if args.first().is_some_and(|a| is_channel_spec(a)) {
        Some(parse_channels(&args.remove(0))?)
    } else {
        None
    };
    check_empty(&classify_all(&args)?)?;
    Ok(ParsedCommand {
        cmd: CommandKind::Transfer { channels },
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
    fn test_transfer_bare() {
        let p = parse(&args("transfer")).unwrap();
        assert!(matches!(p.cmd, CommandKind::Transfer { channels: None }));
    }

    #[test]
    fn test_transfer_abbreviation_and_channel() {
        // `tr` expands to `transfer`; an explicit channel spec is carried.
        let p = parse(&args("tr 2")).unwrap();
        match p.cmd {
            CommandKind::Transfer { channels } => assert_eq!(channels, Some(vec![2])),
            other => panic!("expected Transfer, got {other:?}"),
        }
    }

    // The load-bearing AC (#185): `ac transfer` carries no drive option of
    // any kind — the CommandKind has no drive field to set, so a CLI
    // launch is structurally drive-off. Guard against a future field.
    #[test]
    fn transfer_command_has_no_drive_knob() {
        let p = parse(&args("transfer")).unwrap();
        // Debug-formatting the whole command must not mention "drive" —
        // the moment someone adds a drive param here, this trips.
        let dbg = format!("{:?}", p.cmd);
        assert!(
            !dbg.to_lowercase().contains("drive"),
            "ac transfer must have no drive option: {dbg}"
        );
    }

    #[test]
    fn test_monitor_tui_flag() {
        let p = parse(&args("monitor --tui")).unwrap();
        assert!(matches!(p.cmd, CommandKind::Monitor { tui: true, .. }));
        // Default (no flag) is not tui — spawns ac-view.
        let p = parse(&args("monitor")).unwrap();
        assert!(matches!(p.cmd, CommandKind::Monitor { tui: false, .. }));
    }

    #[test]
    fn test_monitor_tui_flag_with_channels_anywhere() {
        // `--tui` can sit anywhere; channels still parse.
        let p = parse(&args("monitor 0-3 --tui")).unwrap();
        match p.cmd {
            CommandKind::Monitor { channels, tui, .. } => {
                assert_eq!(channels, Some(vec![0, 1, 2, 3]));
                assert!(tui);
            }
            other => panic!("expected Monitor, got {other:?}"),
        }
    }

    #[test]
    fn test_monitor() {
        let p = parse(&args("monitor")).unwrap();
        match p.cmd {
            CommandKind::Monitor {
                start_freq,
                end_freq,
                interval,
                ..
            } => {
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
    fn test_monitor_cqt() {
        let p = parse(&args("monitor cqt")).unwrap();
        assert!(matches!(p.cmd, CommandKind::MonitorCqt { .. }));
    }

    #[test]
    fn test_monitor_cqt_with_channels() {
        let p = parse(&args("monitor cqt 0-3")).unwrap();
        match p.cmd {
            CommandKind::MonitorCqt { channels, .. } => {
                assert_eq!(channels, Some(vec![0, 1, 2, 3]));
            }
            other => panic!("expected MonitorCqt, got {other:?}"),
        }
    }

    #[test]
    fn test_monitor_reassigned() {
        let p = parse(&args("monitor reassigned")).unwrap();
        assert!(matches!(p.cmd, CommandKind::MonitorReassigned { .. }));
    }

    #[test]
    fn test_monitor_reassigned_with_channels() {
        let p = parse(&args("monitor reassigned 0,2")).unwrap();
        match p.cmd {
            CommandKind::MonitorReassigned { channels, .. } => {
                assert_eq!(channels, Some(vec![0, 2]));
            }
            other => panic!("expected MonitorReassigned, got {other:?}"),
        }
    }
}
