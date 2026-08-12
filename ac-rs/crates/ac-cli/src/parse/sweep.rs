//! `parse_sweep` — deprecated alias surface (#282).
//!
//! Settled in #276/#282: `sweep` is a pure generator verb now. `ac sweep
//! level` / `ac sweep frequency` moved under `ac generate`; `ac sweep ir`
//! — a full Tier-1 capture+report command, not a generator — moved under
//! `ac plot ir`. This module keeps `ac sweep <noun>` working as a
//! documented alias rather than removing it outright, but it holds no
//! grammar of its own: each noun forwards straight into the parser that
//! now owns it, so there is exactly one parser per grammar, not two.

use super::*;

pub(super) fn parse_sweep(
    args: &mut Vec<String>,
    show_plot: bool,
) -> Result<ParsedCommand, String> {
    if args.is_empty() {
        return Err("sweep needs a noun: level | frequency | ir".into());
    }
    let noun = expand(&args[0]).to_string();
    let new_form = match noun.as_str() {
        "level" | "frequency" => "generate",
        "ir" => "plot",
        other => {
            return Err(format!(
                "unknown sweep noun: {other:?}  (level | frequency | ir)"
            ));
        }
    };
    eprintln!(
        "  warning: `ac sweep {noun}` is a deprecated alias for `ac {new_form} {noun}` \u{2014} \
         use the new form"
    );
    match new_form {
        "generate" => super::generate::parse_generate(args, show_plot),
        _ => super::plot::parse_plot(args, show_plot),
    }
}

#[cfg(test)]
mod tests {
    use super::super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn test_sweep_level_aliases_generate_level() {
        let p = parse(&args("sweep level -20dbu 6dbu 1khz")).unwrap();
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
    fn test_sweep_frequency_abbreviated_aliases_generate_frequency() {
        let p = parse(&args("s f 20hz 20khz 0dbu")).unwrap();
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
    fn test_sweep_defaults() {
        let p = parse(&args("sweep level")).unwrap();
        match p.cmd {
            CommandKind::SweepLevel {
                start,
                stop,
                freq,
                duration,
            } => {
                assert!(matches!(start, LevelSpec::Dbfs(v) if (v - (-40.0)).abs() < 1e-9));
                assert!(matches!(stop, LevelSpec::Dbfs(v) if v.abs() < 1e-9));
                assert!((freq - 1000.0).abs() < 1e-9);
                assert!((duration - 1.0).abs() < 1e-9);
            }
            other => panic!("expected SweepLevel with defaults, got {other:?}"),
        }
    }

    #[test]
    fn test_sweep_ir_aliases_plot_ir() {
        let p = parse(&args("sweep ir 20hz 20khz 1s -6dbu 5harm 4096win")).unwrap();
        match p.cmd {
            CommandKind::PlotIr {
                f1,
                f2,
                n_harmonics,
                window_len,
                ..
            } => {
                assert!((f1 - 20.0).abs() < 1e-9);
                assert!((f2 - 20000.0).abs() < 1e-9);
                assert_eq!(n_harmonics, Some(5));
                assert_eq!(window_len, Some(4096));
            }
            other => panic!("expected PlotIr, got {other:?}"),
        }
    }

    #[test]
    fn test_sweep_ir_abbreviated_aliases_plot_ir() {
        let p = parse(&args("s ir")).unwrap();
        assert!(matches!(p.cmd, CommandKind::PlotIr { .. }));
    }

    #[test]
    fn test_sweep_unknown_noun_errors() {
        assert!(parse(&args("sweep banana")).is_err());
    }
}
