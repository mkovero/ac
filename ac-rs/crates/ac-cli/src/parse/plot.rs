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

    // Support `bands <N>` as two separate tokens. The single-token
    // composite forms `<N>bands` / `<N>bpo` are handled by the
    // classifier; convert `bands N` to `Nbands` before classifying.
    let mut i = 0;
    while i + 1 < args.len() {
        if args[i].eq_ignore_ascii_case("bands") || args[i].eq_ignore_ascii_case("bpo") {
            if let Ok(n) = args[i + 1].parse::<u32>() {
                args[i] = format!("{n}bands");
                args.remove(i + 1);
            }
        }
        i += 1;
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
    let bpo = pull(&mut tokens, TokenKind::Bands).map(|v| v.as_u32());
    check_empty(&tokens)?;
    Ok(ParsedCommand {
        cmd: CommandKind::Plot {
            start,
            stop,
            level,
            ppd,
            bpo,
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
            CommandKind::Plot { start, stop, level, ppd, bpo } => {
                assert!((start.unwrap() - 20.0).abs() < 1e-9);
                assert!((stop.unwrap() - 20000.0).abs() < 1e-9);
                assert!(matches!(level, LevelSpec::Dbu(v) if v.abs() < 1e-9));
                assert_eq!(ppd, 20);
                assert_eq!(bpo, None);
            }
            other => panic!("expected Plot, got {other:?}"),
        }
    }

    #[test]
    fn test_plot_bands_two_tokens() {
        let p = parse(&args("plot 20hz 20khz 0dbu 10ppd bands 3")).unwrap();
        match p.cmd {
            CommandKind::Plot { bpo, .. } => assert_eq!(bpo, Some(3)),
            other => panic!("expected Plot, got {other:?}"),
        }
    }

    #[test]
    fn test_plot_bands_composite() {
        let p = parse(&args("plot 20hz 20khz 0dbu 12bands")).unwrap();
        match p.cmd {
            CommandKind::Plot { bpo, .. } => assert_eq!(bpo, Some(12)),
            other => panic!("expected Plot, got {other:?}"),
        }
    }

    #[test]
    fn test_plot_bpo_alias() {
        let p = parse(&args("plot 20hz 20khz 0dbu 6bpo")).unwrap();
        match p.cmd {
            CommandKind::Plot { bpo, .. } => assert_eq!(bpo, Some(6)),
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

    #[test]
    fn test_plot_bands_without_value_errors() {
        // A lone `bands` token with no following integer should fall
        // through to the classifier, which has no rule for it.
        assert!(parse(&args("plot 20hz 20khz 0dbu bands")).is_err());
    }
}
