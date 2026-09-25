//! `ac report verify` (#398): read a set of archived `plot ir` reports,
//! hand them to `measurement::verification`, print the set's verdicts in
//! the terminal and write one HTML or PDF document.
//!
//! This file owns column widths and colour only. Every string it prints
//! comes from `report_layout::verification`, the layout the HTML and PDF
//! documents paint, so the terminal cannot disagree with the document.
//! A refused set prints its block and writes nothing.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process;

use ac_core::measurement::report::{MeasurementReport, ReportReadError};
use ac_core::measurement::report_html::render_verification_html;
use ac_core::measurement::report_layout::verification::{
    layout, refusal, Field, RefusalBlock, RunRow, VerificationLayout,
};
use ac_core::measurement::report_pdf::render_verification_pdf;
use ac_core::measurement::verification::{verify, RunInput};
use ac_core::shared::calibration::parse_mic_curve;

use super::report::unsupported_schema_lines;
use crate::parse::ReportFormat;

/// 256-colour codes (#398 UX): attention, not alarm, for states the
/// operator must not skim past; a muted red for `error:`. `pass`, `used`
/// and `tracks drive` stay uncoloured. Plain text carries every state
/// without colour.
const ATTENTION: u8 = 179;
const ERROR: u8 = 167;

/// Width of the label column of a header field; continuation lines
/// indent past it.
const LABEL_W: usize = 14;

fn fail(lines: &[String]) -> ! {
    for l in lines {
        eprintln!("{l}");
    }
    process::exit(1);
}

pub fn run(paths: &[String], mic_curve: Option<&str>, format: ReportFormat) {
    let colour_err = colour_enabled(std::io::stderr().is_terminal());
    let mut inputs = Vec::with_capacity(paths.len());
    for path in paths {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => fail(&[format!("  error: cannot read {path}: {e}")]),
        };
        let report = match MeasurementReport::from_json(&text) {
            Ok(r) => r,
            Err(ReportReadError::UnsupportedSchema { found, supported }) => {
                fail(&unsupported_schema_lines(path, found, &supported))
            }
            Err(ReportReadError::Malformed(msg)) => fail(&[format!(
                "  error: invalid MeasurementReport JSON in {path}: {msg}"
            )]),
        };
        inputs.push(RunInput {
            file: path.clone(),
            report,
        });
    }
    let curve = mic_curve.map(|path| {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => fail(&[format!("  error: cannot read mic curve {path}: {e}")]),
        };
        match parse_mic_curve(&text, Some(path.to_string())) {
            Ok(c) => c,
            Err(e) => fail(&[format!("  error: invalid mic curve {path}: {e}")]),
        }
    });

    let set = match verify(inputs, curve.as_ref()) {
        Ok(s) => s,
        Err(r) => fail(&refusal_lines(&refusal(&r), colour_err)),
    };
    let l = layout(&set, &ac_core::shared::time::now_utc_iso8601());

    let (ext, bytes): (&str, Vec<u8>) = match format {
        ReportFormat::Html => ("html", render_verification_html(&l).into_bytes()),
        ReportFormat::Pdf => match render_verification_pdf(&l) {
            Ok(b) => ("pdf", b),
            Err(e) => fail(&[format!("  error: PDF render failed: {e:#}")]),
        },
    };
    let first = &set.runs[0];
    let out = output_path(&first.file, &first.timestamp_utc, ext);
    if let Err(e) = std::fs::write(&out, bytes) {
        fail(&[format!("  error: cannot write {}: {e}", out.display())]);
    }

    let colour = colour_enabled(std::io::stdout().is_terminal());
    for line in summary_lines(&l, colour) {
        println!("{line}");
    }
    println!(
        "{}",
        field_lines(
            &Field {
                label: "wrote",
                lines: vec![out.display().to_string()],
            },
            colour
        )
        .concat()
    );
}

/// `<dir of run 1>/verify-<run 1 capture, yyyymmddThhmmssZ>.<ext>`.
/// Named after a capture rather than the render time, so re-rendering a
/// set overwrites its document instead of accumulating copies.
fn output_path(first_file: &str, timestamp_utc: &str, ext: &str) -> PathBuf {
    let whole = timestamp_utc.split('.').next().unwrap_or(timestamp_utc);
    let mut stamp: String = whole.chars().filter(|c| *c != '-' && *c != ':').collect();
    if timestamp_utc.ends_with('Z') && !stamp.ends_with('Z') {
        stamp.push('Z');
    }
    let dir = Path::new(first_file).parent().unwrap_or(Path::new(""));
    dir.join(format!("verify-{stamp}.{ext}"))
}

fn colour_enabled(is_terminal: bool) -> bool {
    is_terminal && std::env::var_os("NO_COLOR").is_none()
}

fn paint(text: &str, code: u8, on: bool) -> String {
    if on && !text.is_empty() {
        format!("\x1b[38;5;{code}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

/// `text` left-aligned in `width` columns, painted when `attention`.
/// Padding is computed on the plain text so colour never shifts a column.
fn cell(text: &str, width: usize, attention: bool, on: bool) -> String {
    let pad = width.saturating_sub(text.chars().count());
    format!(
        "{}{}",
        paint(text, ATTENTION, on && attention),
        " ".repeat(pad)
    )
}

fn field_lines(f: &Field, on: bool) -> Vec<String> {
    let attention = matches!(f.label, "excluded" | "unchecked");
    f.lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            if i == 0 {
                format!("  {}{line}", cell(f.label, LABEL_W, attention, on))
            } else {
                format!("{:w$}{line}", "", w = LABEL_W + 2)
            }
        })
        .collect()
}

fn run_line(r: &RunRow, on: bool) -> String {
    format!(
        "  {:>3}  {:<13}{:<18}{}{}",
        r.run,
        r.level,
        r.snr,
        cell(&r.tail, 25, r.tail_attention, on),
        paint(&r.status, ATTENTION, on && r.status_attention)
    )
    .trim_end()
    .to_string()
}

/// The terminal summary: UX frames A–C, without the `wrote` line.
pub(crate) fn summary_lines(l: &VerificationLayout, on: bool) -> Vec<String> {
    let mut out = vec![format!("  {}  {}", l.title, l.rendered_utc), String::new()];
    for f in &l.header {
        out.extend(field_lines(f, on));
    }
    out.push(String::new());

    out.push(run_line(&l.run_header, false));
    out.push(run_line(&l.run_thresholds, false));
    out.extend(l.runs.iter().map(|r| run_line(r, on)));
    out.push(String::new());

    if !l.warnings.is_empty() {
        for f in &l.warnings {
            out.extend(field_lines(f, on));
        }
        out.push(String::new());
    }

    out.push(format!(
        "  {:<18}{:<14}{:<13}{:<13}{}",
        "verdict", "band", "measured", "limit", "result"
    ));
    for v in &l.verdicts {
        out.push(match &v.body {
            Ok(c) => format!(
                "  {:<18}{:<14}{:>8}     {:<13}{}",
                v.name,
                v.band,
                c.measured,
                c.limit,
                paint(&c.result, ATTENTION, on && c.attention)
            ),
            Err(text) => format!("  {:<18}{:<14}{text}", v.name, v.band),
        });
    }
    out.push(String::new());

    out.extend(field_lines(&l.harmonics, on));
    out.push(format!(
        "  {:<8}{:<16}{:<18}{:<11}{}",
        "order", l.harmonic_level_header, "per 10 dB drive", "expected", "reading"
    ));
    for h in &l.harmonic_rows {
        let level = format!("{}{:>8}", h.prefix, h.level);
        out.push(match &h.body {
            Ok(c) => format!(
                "  {:<8}{:<16}{:<18}{:<11}{}",
                h.order,
                level,
                format!("{:>11}", c.slope),
                format!("{:>7}", c.expected),
                paint(&c.reading, ATTENTION, on && c.attention)
            ),
            Err(text) => format!("  {:<8}{:<16}{text}", h.order, level),
        });
    }
    out.extend(
        l.harmonic_footer
            .iter()
            .map(|s| format!("{:w$}{s}", "", w = LABEL_W + 2)),
    );
    out.push(String::new());

    out.extend(field_lines(&l.response, on));
    out.push(format!("  {:<14}{}", "band", "flatness"));
    out.extend(
        l.flatness
            .iter()
            .map(|f| format!("  {:<14}{}", f.band, f.value)),
    );
    out.push(String::new());
    out
}

/// A refusal in the #429 block shape: `error:` line, aligned fields,
/// `output not written`. A label longer than the label column (a file
/// path) keeps three spaces before its value.
pub(crate) fn refusal_lines(b: &RefusalBlock, on: bool) -> Vec<String> {
    let mut out = vec![format!("  {} {}", paint("error:", ERROR, on), b.title)];
    for (label, value) in &b.rows {
        out.push(if label.chars().count() <= 10 {
            format!("         {label:<10} {value}")
        } else {
            format!("         {label}   {value}")
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ac_core::measurement::verification::VerificationRefusal;

    #[test]
    fn output_is_named_after_run_one_and_sits_beside_it() {
        assert_eq!(
            output_path("/home/rig/s30-a/m40.json", "2026-09-24T10:12:04Z", "html"),
            PathBuf::from("/home/rig/s30-a/verify-20260924T101204Z.html")
        );
        assert_eq!(
            output_path("m40.json", "2026-09-24T10:12:04.513Z", "pdf"),
            PathBuf::from("verify-20260924T101204Z.pdf")
        );
    }

    #[test]
    fn refusal_block_matches_frame_d() {
        let lines = refusal_lines(
            &refusal(&VerificationRefusal::TooFewReports { given: 1 }),
            false,
        );
        assert_eq!(
            lines,
            [
                "  error: report verify needs at least 2 reports",
                "         given      1",
                "         output     not written",
            ]
        );
        let twice = refusal_lines(
            &refusal(&VerificationRefusal::SameRunTwice {
                captured: "2026-09-24T10:12:04Z".into(),
                files: vec!["s30-a-m40.json".into(), "old/s30-a-m40.json".into()],
            }),
            false,
        );
        assert_eq!(twice[1], "         captured   2026-09-24T10:12:04Z");
        assert_eq!(twice[2], "         file       s30-a-m40.json");
    }

    #[test]
    fn colour_never_moves_a_column() {
        let plain = cell("excluded", 12, true, false);
        let painted = cell("excluded", 12, true, true);
        assert_eq!(plain, "excluded    ");
        assert!(painted.starts_with("\x1b[38;5;179m"));
        assert!(painted.ends_with("\x1b[0m    "));
        assert_eq!(paint("pass", ATTENTION, false), "pass");
    }
}
