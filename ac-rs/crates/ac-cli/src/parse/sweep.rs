//\! `parse_sweep` — subcommand parser extracted from `parse/mod.rs`.

use super::*;

pub(super) fn parse_sweep(args: &mut Vec<String>, show_plot: bool) -> Result<ParsedCommand, String> {
    if args.is_empty() {
        return Err("sweep needs a noun: level | frequency".into());
    }
    let noun = expand(&args.remove(0)).to_string();
    let mut tokens = classify_all(args)?;

    match noun.as_str() {
        "level" => {
            let start = pull(&mut tokens, TokenKind::Level)
                .map(|v| v.as_level())
                .unwrap_or(LevelSpec::Dbfs(-40.0));
            let stop = pull(&mut tokens, TokenKind::Level)
                .map(|v| v.as_level())
                .unwrap_or(LevelSpec::Dbfs(0.0));
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
                    freq,
                    duration,
                },
                show_plot,
            })
        }
        "frequency" => {
            let start = pull(&mut tokens, TokenKind::Freq).map(|v| v.as_f64());
            let stop = pull(&mut tokens, TokenKind::Freq).map(|v| v.as_f64());
            let level = pull(&mut tokens, TokenKind::Level)
                .map(|v| v.as_level())
                .unwrap_or(LevelSpec::Dbfs(-20.0));
            let duration = pull(&mut tokens, TokenKind::Time)
                .map(|v| v.as_f64())
                .unwrap_or(1.0);
            check_empty(&tokens)?;
            Ok(ParsedCommand {
                cmd: CommandKind::SweepFrequency {
                    start,
                    stop,
                    level,
                    duration,
                },
                show_plot,
            })
        }
        "ir" => {
            let f1 = pull(&mut tokens, TokenKind::Freq).map(|v| v.as_f64()).unwrap_or(20.0);
            let f2 = pull(&mut tokens, TokenKind::Freq).map(|v| v.as_f64()).unwrap_or(20_000.0);
            let duration = pull(&mut tokens, TokenKind::Time).map(|v| v.as_f64()).unwrap_or(1.0);
            let level = pull(&mut tokens, TokenKind::Level)
                .map(|v| v.as_level())
                .unwrap_or(LevelSpec::Dbfs(-6.0));
            check_empty(&tokens)?;
            Ok(ParsedCommand {
                cmd: CommandKind::SweepIr { f1, f2, duration, level },
                show_plot,
            })
        }
        other => Err(format!(
            "unknown sweep noun: {other:?}  (level | frequency | ir)"
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
    fn test_sweep_level() {
        let p = parse(&args("sweep level -20dbu 6dbu 1khz")).unwrap();
        match p.cmd {
            CommandKind::SweepLevel { start, stop, freq, .. } => {
                assert!(matches!(start, LevelSpec::Dbu(v) if (v - (-20.0)).abs() < 1e-9));
                assert!(matches!(stop, LevelSpec::Dbu(v) if (v - 6.0).abs() < 1e-9));
                assert!((freq - 1000.0).abs() < 1e-9);
            }
            other => panic!("expected SweepLevel, got {other:?}"),
        }
    }

    #[test]
    fn test_sweep_frequency_abbreviated() {
        let p = parse(&args("s f 20hz 20khz 0dbu")).unwrap();
        match p.cmd {
            CommandKind::SweepFrequency { start, stop, level, .. } => {
                assert!((start.unwrap() - 20.0).abs() < 1e-9);
                assert!((stop.unwrap() - 20000.0).abs() < 1e-9);
                assert!(matches!(level, LevelSpec::Dbu(v) if v.abs() < 1e-9));
            }
            other => panic!("expected SweepFrequency, got {other:?}"),
        }
    }

    #[test]
    fn test_sweep_defaults() {
        let p = parse(&args("sweep level")).unwrap();
        match p.cmd {
            CommandKind::SweepLevel { start, stop, freq, duration } => {
                assert!(matches!(start, LevelSpec::Dbfs(v) if (v - (-40.0)).abs() < 1e-9));
                assert!(matches!(stop, LevelSpec::Dbfs(v) if v.abs() < 1e-9));
                assert!((freq - 1000.0).abs() < 1e-9);
                assert!((duration - 1.0).abs() < 1e-9);
            }
            other => panic!("expected SweepLevel with defaults, got {other:?}"),
        }
    }
}
