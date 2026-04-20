//\! `parse_plot` — subcommand parser extracted from `parse/mod.rs`.

use super::*;

pub(super) fn parse_plot(args: &mut Vec<String>, show_plot: bool) -> Result<ParsedCommand, String> {
    if args.first().map(|a| expand(a)) == Some("level") {
        args.remove(0);
        let mut tokens = classify_all(args)?;
        let start = pull(&mut tokens, TokenKind::Level)
            .map(|v| v.as_level())
            .unwrap_or(LevelSpec::Dbfs(-40.0));
        let stop = pull(&mut tokens, TokenKind::Level)
            .map(|v| v.as_level())
            .unwrap_or(LevelSpec::Dbfs(0.0));
        let freq = pull(&mut tokens, TokenKind::Freq)
            .map(|v| v.as_f64())
            .unwrap_or(1000.0);
        let steps = pull(&mut tokens, TokenKind::Steps)
            .map(|v| v.as_u32())
            .unwrap_or(26);
        check_empty(&tokens)?;
        return Ok(ParsedCommand {
            cmd: CommandKind::PlotLevel {
                start,
                stop,
                freq,
                steps,
            },
            show_plot,
        });
    }

    let mut tokens = classify_all(args)?;
    let start = pull(&mut tokens, TokenKind::Freq).map(|v| v.as_f64());
    let stop = pull(&mut tokens, TokenKind::Freq).map(|v| v.as_f64());
    let level = pull(&mut tokens, TokenKind::Level)
        .map(|v| v.as_level())
        .unwrap_or(LevelSpec::Dbfs(-20.0));
    let ppd = pull(&mut tokens, TokenKind::Ppd)
        .map(|v| v.as_u32())
        .unwrap_or(10);
    check_empty(&tokens)?;
    Ok(ParsedCommand {
        cmd: CommandKind::Plot {
            start,
            stop,
            level,
            ppd,
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
    fn test_plot() {
        let p = parse(&args("plot 20hz 20khz 0dbu 20ppd show")).unwrap();
        assert!(p.show_plot);
        match p.cmd {
            CommandKind::Plot { start, stop, level, ppd } => {
                assert!((start.unwrap() - 20.0).abs() < 1e-9);
                assert!((stop.unwrap() - 20000.0).abs() < 1e-9);
                assert!(matches!(level, LevelSpec::Dbu(v) if v.abs() < 1e-9));
                assert_eq!(ppd, 20);
            }
            other => panic!("expected Plot, got {other:?}"),
        }
    }

    #[test]
    fn test_plot_level() {
        let p = parse(&args("plot level -20dbu 6dbu 1khz 26steps show")).unwrap();
        assert!(p.show_plot);
        match p.cmd {
            CommandKind::PlotLevel { start, stop, freq, steps } => {
                assert!(matches!(start, LevelSpec::Dbu(v) if (v - (-20.0)).abs() < 1e-9));
                assert!(matches!(stop, LevelSpec::Dbu(v) if (v - 6.0).abs() < 1e-9));
                assert!((freq - 1000.0).abs() < 1e-9);
                assert_eq!(steps, 26);
            }
            other => panic!("expected PlotLevel, got {other:?}"),
        }
    }

    #[test]
    fn test_plot_abbreviated() {
        let p = parse(&args("p 20hz 20khz 0dbu 10ppd")).unwrap();
        assert!(matches!(p.cmd, CommandKind::Plot { .. }));
    }
}
