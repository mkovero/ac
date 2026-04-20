//\! `parse_test` — subcommand parser extracted from `parse/mod.rs`.

use super::*;

pub(super) fn parse_test(args: &mut Vec<String>) -> Result<ParsedCommand, String> {
    if args.is_empty() {
        return Err("test needs a noun: software | hardware | dut".into());
    }
    let noun = expand(&args.remove(0)).to_string();

    match noun.as_str() {
        "software" => {
            if !args.is_empty() {
                return Err(format!("test software: unexpected argument(s): {args:?}"));
            }
            Ok(ParsedCommand {
                cmd: CommandKind::TestSoftware,
                show_plot: false,
            })
        }
        "hardware" => {
            let dmm = args.first().map_or(false, |a| expand(a) == "dmm");
            if dmm {
                args.remove(0);
            }
            if !args.is_empty() {
                return Err(format!("test hardware: unexpected argument(s): {args:?}"));
            }
            Ok(ParsedCommand {
                cmd: CommandKind::TestHardware { dmm },
                show_plot: false,
            })
        }
        "dut" => {
            let mut compare = false;
            let mut level = LevelSpec::Dbfs(-20.0);
            let mut leftover = Vec::new();
            for a in args.iter() {
                if expand(a) == "compare" {
                    compare = true;
                    continue;
                }
                if let Ok(l) = parse_level(a) {
                    level = l;
                    continue;
                }
                leftover.push(a.clone());
            }
            if !leftover.is_empty() {
                return Err(format!("test dut: unexpected argument(s): {leftover:?}"));
            }
            Ok(ParsedCommand {
                cmd: CommandKind::TestDut { compare, level },
                show_plot: false,
            })
        }
        other => Err(format!(
            "unknown test noun: {other:?}  (software | hardware | dut)"
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
    fn test_test_software() {
        let p = parse(&args("test software")).unwrap();
        assert!(matches!(p.cmd, CommandKind::TestSoftware));
        let p = parse(&args("te so")).unwrap();
        assert!(matches!(p.cmd, CommandKind::TestSoftware));
    }

    #[test]
    fn test_test_hardware_dmm() {
        let p = parse(&args("test hardware dmm")).unwrap();
        match p.cmd {
            CommandKind::TestHardware { dmm } => assert!(dmm),
            other => panic!("expected TestHardware, got {other:?}"),
        }
    }

    #[test]
    fn test_test_dut_compare() {
        let p = parse(&args("test dut compare -10dbfs")).unwrap();
        match p.cmd {
            CommandKind::TestDut { compare, level } => {
                assert!(compare);
                assert!(matches!(level, LevelSpec::Dbfs(v) if (v - (-10.0)).abs() < 1e-9));
            }
            other => panic!("expected TestDut, got {other:?}"),
        }
    }
}
