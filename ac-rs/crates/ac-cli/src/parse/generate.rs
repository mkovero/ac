//\! `parse_generate` — subcommand parser extracted from `parse/mod.rs`.

use super::*;

pub(super) fn parse_generate(
    args: &mut Vec<String>,
    show_plot: bool,
) -> Result<ParsedCommand, String> {
    if args.is_empty() {
        return Err("generate needs a noun: sine | pink | level | frequency".into());
    }
    let noun = expand(&args.remove(0)).to_string();

    match noun.as_str() {
        // Output-only ramps (#282) — reuse `CommandKind::SweepLevel` /
        // `SweepFrequency` unchanged; only the CLI entry noun moved from
        // `sweep` to `generate`. The daemon's `sweep_level`/`sweep_frequency`
        // wire names are untouched — they were always accurate for what
        // these commands do (ramp output, no capture).
        "level" => {
            let mut tokens = classify_all(args)?;
            let start_arg = pull(&mut tokens, TokenKind::Level);
            let level_defaulted = start_arg.is_none();
            let start = start_arg.map(|v| v.as_level()).unwrap_or(LevelSpec::Dbfs(
                ac_core::shared::emission_level::DEFAULT_RAMP_START_DBFS,
            ));
            let stop = pull(&mut tokens, TokenKind::Level)
                .map(|v| v.as_level())
                .unwrap_or(LevelSpec::Dbfs(
                    ac_core::shared::emission_level::DEFAULT_RAMP_STOP_DBFS,
                ));
            let freq = pull(&mut tokens, TokenKind::Freq)
                .map(|v| v.as_f64())
                .unwrap_or(1000.0);
            let duration = pull(&mut tokens, TokenKind::Time)
                .map(|v| v.as_f64())
                .unwrap_or(1.0);
            check_empty(&tokens)?;
            Ok(ParsedCommand {
                cmd: CommandKind::SweepLevel {
                    start,
                    stop,
                    level_defaulted,
                    freq,
                    duration,
                },
                show_plot,
            })
        }
        "frequency" => {
            let mut tokens = classify_all(args)?;
            let start = pull(&mut tokens, TokenKind::Freq).map(|v| v.as_f64());
            let stop = pull(&mut tokens, TokenKind::Freq).map(|v| v.as_f64());
            let level_arg = pull(&mut tokens, TokenKind::Level);
            let level_defaulted = level_arg.is_none();
            let level = level_arg.map(|v| v.as_level()).unwrap_or(LevelSpec::Dbfs(
                ac_core::shared::emission_level::DEFAULT_LEVEL_DBFS,
            ));
            let duration = pull(&mut tokens, TokenKind::Time)
                .map(|v| v.as_f64())
                .unwrap_or(1.0);
            check_empty(&tokens)?;
            Ok(ParsedCommand {
                cmd: CommandKind::SweepFrequency {
                    start,
                    stop,
                    level,
                    level_defaulted,
                    duration,
                },
                show_plot,
            })
        }
        "sine" => {
            let channels = if args.first().is_some_and(|a| is_channel_spec(a)) {
                Some(args.remove(0))
            } else {
                None
            };
            let mut tokens = classify_all(args)?;
            let level_arg = pull(&mut tokens, TokenKind::Level);
            let level_defaulted = level_arg.is_none();
            let level = level_arg.map(|v| v.as_level()).unwrap_or(LevelSpec::Dbfs(
                ac_core::shared::emission_level::DEFAULT_LEVEL_DBFS,
            ));
            let freq = pull(&mut tokens, TokenKind::Freq)
                .map(|v| v.as_f64())
                .unwrap_or(1000.0);
            check_empty(&tokens)?;
            Ok(ParsedCommand {
                cmd: CommandKind::GenerateSine {
                    level,
                    level_defaulted,
                    freq,
                    channels,
                },
                show_plot,
            })
        }
        "pink" => {
            let channels = if args.first().is_some_and(|a| is_channel_spec(a)) {
                Some(args.remove(0))
            } else {
                None
            };
            let mut tokens = classify_all(args)?;
            let level_arg = pull(&mut tokens, TokenKind::Level);
            let level_defaulted = level_arg.is_none();
            let level = level_arg.map(|v| v.as_level()).unwrap_or(LevelSpec::Dbfs(
                ac_core::shared::emission_level::DEFAULT_LEVEL_DBFS,
            ));
            check_empty(&tokens)?;
            Ok(ParsedCommand {
                cmd: CommandKind::GeneratePink {
                    level,
                    level_defaulted,
                    channels,
                },
                show_plot,
            })
        }
        other => Err(format!(
            "unknown generate noun: {other:?}  (sine | pink | level | frequency)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn test_generate_sine() {
        let p = parse(&args("g si 0dbu 1khz")).unwrap();
        match p.cmd {
            CommandKind::GenerateSine {
                level,
                freq,
                channels,
                ..
            } => {
                assert!(matches!(level, LevelSpec::Dbu(v) if v.abs() < 1e-9));
                assert!((freq - 1000.0).abs() < 1e-9);
                assert!(channels.is_none());
            }
            other => panic!("expected GenerateSine, got {other:?}"),
        }
    }

    #[test]
    fn test_generate_sine_with_channels() {
        let p = parse(&args("generate sine 0-11 0dbu 1khz")).unwrap();
        match p.cmd {
            CommandKind::GenerateSine { channels, .. } => {
                assert_eq!(channels, Some("0-11".into()));
            }
            other => panic!("expected GenerateSine, got {other:?}"),
        }
    }

    #[test]
    fn test_generate_pink() {
        let p = parse(&args("g pk -10dbfs")).unwrap();
        match p.cmd {
            CommandKind::GeneratePink {
                level, channels, ..
            } => {
                assert!(matches!(level, LevelSpec::Dbfs(v) if (v - (-10.0)).abs() < 1e-9));
                assert!(channels.is_none());
            }
            other => panic!("expected GeneratePink, got {other:?}"),
        }
    }

    // ─── `generate level` / `generate frequency` (#282) ────────────

    #[test]
    fn test_generate_level() {
        let p = parse(&args("generate level -20dbu 6dbu 1khz")).unwrap();
        match p.cmd {
            CommandKind::SweepLevel {
                start, stop, freq, ..
            } => {
                assert!(matches!(start, LevelSpec::Dbu(v) if (v - (-20.0)).abs() < 1e-9));
                assert!(matches!(stop, LevelSpec::Dbu(v) if (v - 6.0).abs() < 1e-9));
                assert!((freq - 1000.0).abs() < 1e-9);
            }
            other => panic!("expected SweepLevel, got {other:?}"),
        }
    }

    #[test]
    fn test_generate_level_abbreviated() {
        let p = parse(&args("g l")).unwrap();
        match p.cmd {
            CommandKind::SweepLevel {
                start, stop, freq, ..
            } => {
                assert_eq!(
                    start,
                    LevelSpec::Dbfs(ac_core::shared::emission_level::DEFAULT_RAMP_START_DBFS)
                );
                assert_eq!(
                    stop,
                    LevelSpec::Dbfs(ac_core::shared::emission_level::DEFAULT_RAMP_STOP_DBFS)
                );
                assert!((freq - 1000.0).abs() < 1e-9);
            }
            other => panic!("expected SweepLevel, got {other:?}"),
        }
    }

    #[test]
    fn test_generate_frequency_abbreviated() {
        let p = parse(&args("g f 20hz 20khz 0dbu")).unwrap();
        match p.cmd {
            CommandKind::SweepFrequency {
                start, stop, level, ..
            } => {
                assert!((start.unwrap() - 20.0).abs() < 1e-9);
                assert!((stop.unwrap() - 20000.0).abs() < 1e-9);
                assert!(matches!(level, LevelSpec::Dbu(v) if v.abs() < 1e-9));
            }
            other => panic!("expected SweepFrequency, got {other:?}"),
        }
    }

    #[test]
    fn every_generate_form_marks_and_uses_its_named_default() {
        let sine = parse(&args("generate sine")).unwrap();
        assert!(
            matches!(sine.cmd, CommandKind::GenerateSine { level: LevelSpec::Dbfs(v), level_defaulted: true, .. } if v == ac_core::shared::emission_level::DEFAULT_LEVEL_DBFS)
        );
        let pink = parse(&args("generate pink")).unwrap();
        assert!(
            matches!(pink.cmd, CommandKind::GeneratePink { level: LevelSpec::Dbfs(v), level_defaulted: true, .. } if v == ac_core::shared::emission_level::DEFAULT_LEVEL_DBFS)
        );
        let ramp = parse(&args("generate level")).unwrap();
        assert!(
            matches!(ramp.cmd, CommandKind::SweepLevel { start: LevelSpec::Dbfs(a), stop: LevelSpec::Dbfs(b), level_defaulted: true, .. } if a == ac_core::shared::emission_level::DEFAULT_RAMP_START_DBFS && b == ac_core::shared::emission_level::DEFAULT_RAMP_STOP_DBFS)
        );
        let frequency = parse(&args("generate frequency")).unwrap();
        assert!(
            matches!(frequency.cmd, CommandKind::SweepFrequency { level: LevelSpec::Dbfs(v), level_defaulted: true, .. } if v == ac_core::shared::emission_level::DEFAULT_LEVEL_DBFS)
        );
    }
}
