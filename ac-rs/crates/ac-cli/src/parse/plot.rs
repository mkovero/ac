//\! `parse_plot` — subcommand parser extracted from `parse/mod.rs`.

use super::*;

pub(super) fn parse_plot(args: &mut Vec<String>, show_plot: bool) -> Result<ParsedCommand, String> {
    // `--verbose` / `-v` adds the per-point `fund` and `noise` columns and,
    // once the run completes, the harmonic table to `plot` / `plot level`
    // (#116, #132); stripped here the way `parse_monitor` strips `--tui`, so
    // `classify_all` never sees `-v`. `plot ir` has none of these, so there
    // the flags are put back as typed and refused below.
    let is_verbose = |a: &String| a == "--verbose" || a == "-v";
    let verbose_flags: Vec<String> = args.iter().filter(|a| is_verbose(a)).cloned().collect();
    let verbose = !verbose_flags.is_empty();
    args.retain(|a| !is_verbose(a));
    if args.first().map(|a| expand(a)) == Some("level") {
        args.remove(0);
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
        let steps = pull(&mut tokens, TokenKind::Steps)
            .map(|v| v.as_u32())
            .unwrap_or(26);
        check_empty(&tokens)?;
        return Ok(ParsedCommand {
            cmd: CommandKind::PlotLevel {
                start,
                stop,
                level_defaulted,
                freq,
                steps,
                verbose,
            },
            show_plot,
        });
    }

    if args.first().map(|a| expand(a)) == Some("ir") {
        args.remove(0);
        // `sti` (#724): a bare word, not a classified token.
        let before = args.len();
        args.retain(|a| !a.eq_ignore_ascii_case("sti"));
        let sti = args.len() != before;
        // `speech <dB(A)>` (#726): the operational speech level for the
        // level-corrected STI. Two words, taken out before classifying.
        let mut speech_dba = None;
        if let Some(i) = args.iter().position(|a| a.eq_ignore_ascii_case("speech")) {
            let v = args
                .get(i + 1)
                .and_then(|v| {
                    v.trim_end_matches(|c: char| c.is_ascii_alphabetic() || c == '(' || c == ')')
                        .parse::<f64>()
                        .ok()
                })
                .ok_or_else(|| "speech needs a level in dB(A), e.g. speech 65".to_string())?;
            args.drain(i..=i + 1);
            speech_dba = Some(v);
        }
        args.extend(verbose_flags);
        let mut tokens = classify_all(args)?;
        // Unset stays unset: the daemon applies `ac-core`'s defaults and
        // echoes them in the ack (#501).
        let f1 = pull(&mut tokens, TokenKind::Freq).map(|v| v.as_f64());
        let f2 = pull(&mut tokens, TokenKind::Freq).map(|v| v.as_f64());
        let duration = pull(&mut tokens, TokenKind::Time).map(|v| v.as_f64());
        let level_arg = pull(&mut tokens, TokenKind::Level);
        let level_defaulted = level_arg.is_none();
        let level = level_arg.map(|v| v.as_level()).unwrap_or(LevelSpec::Dbfs(
            ac_core::shared::emission_level::DEFAULT_LEVEL_DBFS,
        ));
        let n_harmonics = pull(&mut tokens, TokenKind::Harmonics).map(|v| v.as_u32());
        let window_len = pull(&mut tokens, TokenKind::Window).map(|v| v.as_u32());
        // Second `Time` token, pulled after `duration` — same positional
        // pattern `plot level`'s start/stop `Level` pair uses.
        let tail_s = pull(&mut tokens, TokenKind::Time).map(|v| v.as_f64());
        let distance_m = pull(&mut tokens, TokenKind::Distance).map(|v| v.as_f64());
        check_empty(&tokens)?;
        return Ok(ParsedCommand {
            cmd: CommandKind::PlotIr {
                f1,
                f2,
                duration,
                level,
                level_defaulted,
                n_harmonics,
                window_len,
                tail_s,
                distance_m,
                sti,
                speech_dba,
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
    let level_arg = pull(&mut tokens, TokenKind::Level);
    let level_defaulted = level_arg.is_none();
    let level = level_arg.map(|v| v.as_level()).unwrap_or(LevelSpec::Dbfs(
        ac_core::shared::emission_level::DEFAULT_LEVEL_DBFS,
    ));
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
            level_defaulted,
            ppd,
            bpo,
            verbose,
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
            CommandKind::Plot {
                start,
                stop,
                level,
                ppd,
                bpo,
                ..
            } => {
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
            CommandKind::PlotLevel {
                start,
                stop,
                freq,
                steps,
                ..
            } => {
                assert!(matches!(start, LevelSpec::Dbu(v) if (v - (-20.0)).abs() < 1e-9));
                assert!(matches!(stop, LevelSpec::Dbu(v) if (v - 6.0).abs() < 1e-9));
                assert!((freq - 1000.0).abs() < 1e-9);
                assert_eq!(steps, 26);
            }
            other => panic!("expected PlotLevel, got {other:?}"),
        }
    }

    /// #116, #132: `--verbose` / `-v` sets the verbose flag on `plot` and
    /// `plot level`, wherever it sits, and is off by default.
    #[test]
    fn verbose_flag_on_plot_and_plot_level() {
        let p = parse(&args("plot 20hz 20khz -v -10dbfs")).unwrap();
        match p.cmd {
            CommandKind::Plot { verbose, level, .. } => {
                assert!(verbose);
                assert!(matches!(level, LevelSpec::Dbfs(v) if (v + 10.0).abs() < 1e-9));
            }
            other => panic!("expected Plot, got {other:?}"),
        }
        let p = parse(&args("plot level -v -20dbu 6dbu")).unwrap();
        assert!(matches!(
            p.cmd,
            CommandKind::PlotLevel { verbose: true, .. }
        ));
        let p = parse(&args("plot -v level -30dbfs 0dbfs 1khz 5steps")).unwrap();
        assert!(matches!(
            p.cmd,
            CommandKind::PlotLevel {
                verbose: true,
                steps: 5,
                ..
            }
        ));
        let p = parse(&args("plot 20hz 20khz --verbose -10dbfs")).unwrap();
        assert!(matches!(p.cmd, CommandKind::Plot { verbose: true, .. }));
        let p = parse(&args("plot 20hz 20khz -10dbfs")).unwrap();
        assert!(matches!(p.cmd, CommandKind::Plot { verbose: false, .. }));
        let p = parse(&args("plot --verbose level -20dbu 6dbu")).unwrap();
        assert!(matches!(
            p.cmd,
            CommandKind::PlotLevel { verbose: true, .. }
        ));
        let p = parse(&args("plot level -20dbu 6dbu")).unwrap();
        assert!(matches!(
            p.cmd,
            CommandKind::PlotLevel { verbose: false, .. }
        ));
    }

    /// `--verbose` / `-v` is never silently ignored: `plot ir` and other
    /// verbs have no verbose output, so the token is refused as leftover.
    #[test]
    fn verbose_flag_is_refused_where_it_does_nothing() {
        assert!(parse(&args("plot ir 20hz 20khz --verbose")).is_err());
        assert!(parse(&args("plot ir 20hz 20khz -v")).is_err());
        assert!(parse(&args("generate sine --verbose")).is_err());
        assert!(parse(&args("generate sine -v")).is_err());
        assert!(parse(&args("test dut --verbose")).is_err());
        assert!(parse(&args("test dut -v")).is_err());
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

    #[test]
    fn test_plot_ir() {
        let p = parse(&args("plot ir 20hz 20khz 1s -6dbu 5harm 4096win 0.8s")).unwrap();
        match p.cmd {
            CommandKind::PlotIr {
                f1,
                f2,
                duration,
                level,
                n_harmonics,
                window_len,
                tail_s,
                distance_m,
                ..
            } => {
                assert_eq!(distance_m, None);
                assert_eq!(f1, Some(20.0));
                assert_eq!(f2, Some(20000.0));
                assert_eq!(duration, Some(1.0));
                assert!(matches!(level, LevelSpec::Dbu(v) if (v - (-6.0)).abs() < 1e-9));
                assert_eq!(n_harmonics, Some(5));
                assert_eq!(window_len, Some(4096));
                assert!((tail_s.unwrap() - 0.8).abs() < 1e-9);
            }
            other => panic!("expected PlotIr, got {other:?}"),
        }
    }

    /// #724: `sti` anywhere among `plot ir`'s arguments asks for the STI,
    /// in any case, without disturbing the other tokens.
    #[test]
    fn test_plot_ir_sti_flag() {
        for line in [
            "plot ir sti",
            "plot ir 2s STI 1.5m",
            "plot ir -50dbfs sti 2s 1.6s",
        ] {
            let p = parse(&args(line)).unwrap();
            match p.cmd {
                CommandKind::PlotIr { sti, .. } => assert!(sti, "{line}"),
                other => panic!("{line}: expected PlotIr, got {other:?}"),
            }
        }
        match parse(&args("plot ir 2s STI 1.5m")).unwrap().cmd {
            CommandKind::PlotIr {
                duration,
                distance_m,
                ..
            } => {
                assert_eq!(duration, Some(2.0));
                assert_eq!(distance_m, Some(1.5));
            }
            other => panic!("expected PlotIr, got {other:?}"),
        }
    }

    /// #726: `speech <dB(A)>` gives the operational speech level; a bare
    /// `speech` with no number is refused.
    #[test]
    fn test_plot_ir_speech_level() {
        for line in ["plot ir sti speech 65", "plot ir speech 65dBA sti 2s"] {
            match parse(&args(line)).unwrap().cmd {
                CommandKind::PlotIr {
                    sti, speech_dba, ..
                } => {
                    assert!(sti, "{line}");
                    assert_eq!(speech_dba, Some(65.0), "{line}");
                }
                other => panic!("{line}: expected PlotIr, got {other:?}"),
            }
        }
        assert!(parse(&args("plot ir sti speech")).is_err());
    }

    #[test]
    fn test_plot_ir_defaults() {
        let p = parse(&args("plot ir")).unwrap();
        match p.cmd {
            CommandKind::PlotIr {
                f1,
                f2,
                duration,
                level,
                level_defaulted,
                n_harmonics,
                window_len,
                tail_s,
                distance_m,
                sti,
                speech_dba,
            } => {
                assert_eq!(speech_dba, None);
                assert_eq!(distance_m, None);
                assert!(!sti, "sti is off unless asked for");
                // Unset — the daemon applies `ac-core`'s defaults (#501),
                // so the CLI holds no copy that could drift from them.
                assert_eq!(f1, None);
                assert_eq!(f2, None);
                assert_eq!(duration, None);
                assert_eq!(
                    level,
                    LevelSpec::Dbfs(ac_core::shared::emission_level::DEFAULT_LEVEL_DBFS)
                );
                assert!(level_defaulted);
                // Unset — the daemon applies its own defaults, not the CLI.
                assert_eq!(n_harmonics, None);
                assert_eq!(window_len, None);
                assert_eq!(tail_s, None);
            }
            other => panic!("expected PlotIr, got {other:?}"),
        }
    }

    #[test]
    fn plot_and_plot_level_defaults_are_named_and_marked() {
        let p = parse(&args("plot")).unwrap();
        assert!(
            matches!(p.cmd, CommandKind::Plot { level: LevelSpec::Dbfs(v), level_defaulted: true, .. } if v == ac_core::shared::emission_level::DEFAULT_LEVEL_DBFS)
        );
        let p = parse(&args("plot level")).unwrap();
        assert!(
            matches!(p.cmd, CommandKind::PlotLevel { start: LevelSpec::Dbfs(a), stop: LevelSpec::Dbfs(b), level_defaulted: true, .. } if a == ac_core::shared::emission_level::DEFAULT_RAMP_START_DBFS && b == ac_core::shared::emission_level::DEFAULT_RAMP_STOP_DBFS)
        );
    }

    #[test]
    fn test_plot_ir_abbreviated() {
        let p = parse(&args("p ir 20hz 20khz")).unwrap();
        assert!(matches!(p.cmd, CommandKind::PlotIr { .. }));
    }

    /// Test against the rejected form: a bare integer meant as
    /// `n_harmonics` with the `harm` marker forgotten must NOT silently
    /// land as harmonics — it collides with the existing bare-number-is-
    /// dBFS rule and becomes a second `Level` token, which `check_empty`
    /// then rejects as leftover.
    #[test]
    fn test_plot_ir_bare_integer_is_not_harmonics() {
        assert!(parse(&args("plot ir 20hz 20khz 1s -6dbu 5")).is_err());
    }

    /// #460: `<N>m` is `plot ir`'s source-to-mic distance.
    #[test]
    fn test_plot_ir_distance_token() {
        let p = parse(&args("plot ir 20hz 20khz 1s -6dbu 5harm 4096win 0.8s 1.5m")).unwrap();
        match p.cmd {
            CommandKind::PlotIr {
                distance_m, tail_s, ..
            } => {
                assert_eq!(distance_m, Some(1.5));
                assert_eq!(tail_s, Some(0.8));
            }
            other => panic!("expected PlotIr, got {other:?}"),
        }
    }

    /// A distance that parses but is unusable is refused at parse time,
    /// naming the token, before anything reaches the daemon.
    #[test]
    fn test_plot_ir_distance_must_be_finite_and_positive() {
        for bad in ["0m", "-1m", "infm", "nanm"] {
            let err = parse(&args(&format!("plot ir 20hz 20khz {bad}")))
                .err()
                .unwrap_or_else(|| panic!("{bad} must not parse"));
            assert!(
                err.contains("distance must be finite and > 0 m"),
                "{bad}: {err}"
            );
            assert!(err.contains(bad), "{bad}: {err}");
        }
    }

    /// Test against the rejected reading: `mm`, `cm` and `1ms` must not be
    /// taken as a distance in metres. They fail to parse instead.
    #[test]
    fn test_plot_ir_other_m_suffixes_are_not_a_distance() {
        for bad in ["5mm", "5cm", "1ms"] {
            assert!(
                parse(&args(&format!("plot ir 20hz 20khz {bad}"))).is_err(),
                "{bad} must not parse"
            );
        }
    }
}
