//! `parse_report` — `ac report` and `ac report verify`, extracted from
//! `parse/mod.rs`.

use super::*;

pub(super) fn parse_report(args: &[String]) -> Result<ParsedCommand, String> {
    if args
        .first()
        .is_some_and(|a| a.eq_ignore_ascii_case("verify"))
    {
        return parse_verify(&args[1..]);
    }
    if args.is_empty() || args.len() > 2 {
        return Err("report: requires <path.json> [html|pdf]".into());
    }
    let format = match args.get(1).map(String::as_str) {
        None | Some("html") => ReportFormat::Html,
        Some("pdf") => ReportFormat::Pdf,
        Some(other) => {
            return Err(format!(
                "report: unknown format {other:?} (expected html or pdf)"
            ));
        }
    };
    Ok(ParsedCommand {
        cmd: CommandKind::Report {
            path: args[0].clone(),
            format,
        },
        show_plot: false,
    })
}

/// `verify <a.json> <b.json> [<c.json> …] [mic-curve <path>] [html|pdf]`.
/// `mic-curve` takes the next token; a trailing `html` / `pdf` is the
/// format; every other token is a report path. Too few paths is not a
/// parse error: the command refuses the set with its own block, which
/// names how many were given.
fn parse_verify(args: &[String]) -> Result<ParsedCommand, String> {
    let mut paths = Vec::new();
    let mut mic_curve = None;
    let mut format = ReportFormat::Html;
    let mut i = 0;
    while i < args.len() {
        let token = args[i].as_str();
        if token.eq_ignore_ascii_case("mic-curve") {
            let Some(path) = args.get(i + 1) else {
                return Err("report verify: mic-curve needs a <path>".into());
            };
            if mic_curve.is_some() {
                return Err("report verify: mic-curve given twice".into());
            }
            mic_curve = Some(path.clone());
            i += 2;
            continue;
        }
        if i + 1 == args.len() && (token == "html" || token == "pdf") {
            format = if token == "pdf" {
                ReportFormat::Pdf
            } else {
                ReportFormat::Html
            };
        } else {
            paths.push(token.to_string());
        }
        i += 1;
    }
    Ok(ParsedCommand {
        cmd: CommandKind::ReportVerify {
            paths,
            mic_curve,
            format,
        },
        show_plot: false,
    })
}

#[cfg(test)]
mod tests {
    use super::super::{parse, CommandKind, ReportFormat};

    fn verify(argv: &[&str]) -> (Vec<String>, Option<String>, ReportFormat) {
        let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        match parse(&argv).expect("parses").cmd {
            CommandKind::ReportVerify {
                paths,
                mic_curve,
                format,
            } => (paths, mic_curve, format),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn verify_takes_paths_a_mic_curve_and_a_trailing_format() {
        let (paths, curve, format) = verify(&[
            "report",
            "verify",
            "a.json",
            "b.json",
            "mic-curve",
            "M30.frd",
            "c.json",
            "pdf",
        ]);
        assert_eq!(paths, ["a.json", "b.json", "c.json"]);
        assert_eq!(curve.as_deref(), Some("M30.frd"));
        assert_eq!(format, ReportFormat::Pdf);

        let (paths, curve, format) = verify(&["report", "verify", "a.json", "b.json"]);
        assert_eq!(paths.len(), 2);
        assert_eq!(curve, None);
        assert_eq!(format, ReportFormat::Html);
    }

    #[test]
    fn a_format_word_is_only_a_format_at_the_end() {
        let (paths, _, format) = verify(&["report", "verify", "html", "b.json"]);
        assert_eq!(paths, ["html", "b.json"]);
        assert_eq!(format, ReportFormat::Html);
    }

    #[test]
    fn too_few_paths_parse_so_the_command_can_refuse_with_its_block() {
        let (paths, _, _) = verify(&["report", "verify", "a.json"]);
        assert_eq!(paths, ["a.json"]);
    }

    #[test]
    fn mic_curve_needs_a_path_and_is_given_once() {
        let argv = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(parse(&argv(&["report", "verify", "a.json", "mic-curve"])).is_err());
        assert!(parse(&argv(&[
            "report",
            "verify",
            "a",
            "mic-curve",
            "x",
            "mic-curve",
            "y"
        ]))
        .is_err());
    }

    #[test]
    fn single_report_is_unchanged() {
        let argv: Vec<String> = ["report", "r.json", "pdf"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(matches!(
            parse(&argv).unwrap().cmd,
            CommandKind::Report {
                format: ReportFormat::Pdf,
                ..
            }
        ));
    }
}
