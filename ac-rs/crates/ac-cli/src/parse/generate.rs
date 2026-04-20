//\! `parse_generate` — subcommand parser extracted from `parse/mod.rs`.

use super::*;

pub(super) fn parse_generate(args: &mut Vec<String>, show_plot: bool) -> Result<ParsedCommand, String> {
    if args.is_empty() {
        return Err("generate needs a noun: sine | pink".into());
    }
    let noun = expand(&args.remove(0)).to_string();

    match noun.as_str() {
        "sine" => {
            let channels = if args.first().map_or(false, |a| is_channel_spec(a)) {
                Some(args.remove(0))
            } else {
                None
            };
            let mut tokens = classify_all(args)?;
            let level = pull(&mut tokens, TokenKind::Level).map(|v| v.as_level());
            let freq = pull(&mut tokens, TokenKind::Freq)
                .map(|v| v.as_f64())
                .unwrap_or(1000.0);
            check_empty(&tokens)?;
            Ok(ParsedCommand {
                cmd: CommandKind::GenerateSine {
                    level,
                    freq,
                    channels,
                },
                show_plot,
            })
        }
        "pink" => {
            let channels = if args.first().map_or(false, |a| is_channel_spec(a)) {
                Some(args.remove(0))
            } else {
                None
            };
            let mut tokens = classify_all(args)?;
            let level = pull(&mut tokens, TokenKind::Level).map(|v| v.as_level());
            check_empty(&tokens)?;
            Ok(ParsedCommand {
                cmd: CommandKind::GeneratePink {
                    level,
                    channels,
                },
                show_plot,
            })
        }
        other => Err(format!(
            "unknown generate noun: {other:?}  (sine | pink)"
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
            CommandKind::GenerateSine { level, freq, channels } => {
                assert!(matches!(level, Some(LevelSpec::Dbu(v)) if v.abs() < 1e-9));
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
            CommandKind::GeneratePink { level, channels } => {
                assert!(matches!(level, Some(LevelSpec::Dbfs(v)) if (v - (-10.0)).abs() < 1e-9));
                assert!(channels.is_none());
            }
            other => panic!("expected GeneratePink, got {other:?}"),
        }
    }
}
