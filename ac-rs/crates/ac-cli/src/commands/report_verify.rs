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

/// 256-colour codes (#398 UX): labels, values, and units or context
/// each have their own grey; attention, not alarm, for states the
/// operator must not skim past; a muted red for `error:`. `pass`, `used`
/// and `tracks drive` stay uncoloured. Plain text carries every state
/// without colour.
const LABEL: u8 = 247;
const VALUE: u8 = 254;
const CONTEXT: u8 = 241;
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

/// `text` left-aligned in `width` columns, painted with `code`.
/// Padding is computed on the plain text so colour never shifts a column.
fn cell(text: &str, width: usize, code: u8, on: bool) -> String {
    let pad = width.saturating_sub(text.chars().count());
    format!("{}{}", paint(text, code, on), " ".repeat(pad))
}

/// A pre-formatted reading such as `-40.0 dBFS` or `41.2 dB at 12.5 kHz`:
/// the leading number in the value grey, the unit and context after it in
/// the context grey. Text that does not start with a number (`not
/// measured`) is a value as a whole. Whitespace is kept byte for byte, so
/// the plain text is unchanged.
fn reading(text: &str, on: bool) -> String {
    let body = text.trim_start();
    let lead = &text[..text.len() - body.len()];
    let numeric = body
        .trim_start_matches(['+', '-', '\u{b1}'])
        .starts_with(|c: char| c.is_ascii_digit());
    match body.find(' ') {
        Some(i) if numeric => {
            let rest = &body[i..];
            let unit = rest.trim_start();
            format!(
                "{lead}{}{}{}",
                paint(&body[..i], VALUE, on),
                &rest[..rest.len() - unit.len()],
                paint(unit, CONTEXT, on)
            )
        }
        _ => format!("{lead}{}", paint(body, VALUE, on)),
    }
}

/// [`reading`] left-aligned in `width` columns.
fn reading_cell(text: &str, width: usize, on: bool) -> String {
    let pad = width.saturating_sub(text.chars().count());
    format!("{}{}", reading(text, on), " ".repeat(pad))
}

fn field_lines(f: &Field, on: bool) -> Vec<String> {
    let label = if matches!(f.label, "excluded" | "unchecked") {
        ATTENTION
    } else {
        LABEL
    };
    f.lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            let value = paint(line, VALUE, on);
            if i == 0 {
                format!("  {}{value}", cell(f.label, LABEL_W, label, on))
            } else {
                format!("{:w$}{value}", "", w = LABEL_W + 2)
            }
        })
        .collect()
}

/// A run row. Data rows print values; the column-name row prints labels
/// and the thresholds row context.
fn run_line(r: &RunRow, style: RowStyle, on: bool) -> String {
    let text = |t: &str, w: usize, attention: bool| match style {
        _ if attention => cell(t, w, ATTENTION, on),
        RowStyle::Labels => cell(t, w, LABEL, on),
        RowStyle::Context => cell(t, w, CONTEXT, on),
        RowStyle::Values => reading_cell(t, w, on),
    };
    let status = match style {
        RowStyle::Values if !r.status_attention => r.status.clone(),
        _ => text(&r.status, 0, r.status_attention),
    };
    format!(
        "  {:>3}  {}{}{}{}",
        r.run,
        text(&r.level, 13, false),
        text(&r.snr, 18, false),
        text(&r.tail, 25, r.tail_attention),
        status
    )
    .trim_end()
    .to_string()
}

#[derive(Clone, Copy)]
enum RowStyle {
    Labels,
    Context,
    Values,
}

/// A column-name row, every name in the label grey.
fn label_row(cells: &[(&str, usize)], on: bool) -> String {
    let mut s = String::from("  ");
    for (t, w) in cells {
        s.push_str(&cell(t, *w, LABEL, on));
    }
    s.trim_end().to_string()
}

/// The terminal summary: UX frames A–C, without the `wrote` line.
pub(crate) fn summary_lines(l: &VerificationLayout, on: bool) -> Vec<String> {
    let mut out = vec![
        format!(
            "  {}  {}",
            paint(l.title, LABEL, on),
            paint(&l.rendered_utc, VALUE, on)
        ),
        String::new(),
    ];
    for f in &l.header {
        out.extend(field_lines(f, on));
    }
    out.push(String::new());

    out.push(run_line(&l.run_header, RowStyle::Labels, on));
    out.push(run_line(&l.run_thresholds, RowStyle::Context, on));
    out.extend(l.runs.iter().map(|r| run_line(r, RowStyle::Values, on)));
    out.push(String::new());

    if !l.warnings.is_empty() {
        for f in &l.warnings {
            out.extend(field_lines(f, on));
        }
        out.push(String::new());
    }

    out.push(label_row(
        &[
            ("verdict", 18),
            ("band", 14),
            ("measured", 13),
            ("limit", 13),
            ("result", 0),
        ],
        on,
    ));
    for v in &l.verdicts {
        let name = cell(v.name, 18, LABEL, on);
        let band = cell(&v.band, 14, CONTEXT, on);
        out.push(match &v.body {
            Ok(c) => format!(
                "  {name}{band}{}     {}{}",
                reading(&format!("{:>8}", c.measured), on),
                cell(&c.limit, 13, CONTEXT, on),
                if c.attention {
                    paint(&c.result, ATTENTION, on)
                } else {
                    c.result.clone()
                }
            ),
            Err(text) => format!("  {name}{band}{}", paint(text, VALUE, on)),
        });
    }
    out.push(String::new());

    out.extend(field_lines(&l.harmonics, on));
    out.push(label_row(
        &[
            ("order", 8),
            (&l.harmonic_level_header, 16),
            ("per 10 dB drive", 18),
            ("expected", 11),
            ("reading", 0),
        ],
        on,
    ));
    for h in &l.harmonic_rows {
        // The `≤` is part of the value: dimming it would hide what the
        // number means.
        let level = format!(
            "{}{}",
            paint(h.prefix.trim_end(), VALUE, on),
            reading_cell(
                &format!("{}{:>8}", &h.prefix[h.prefix.trim_end().len()..], h.level),
                16 - h.prefix.trim_end().chars().count(),
                on
            )
        );
        let order = cell(&h.order, 8, LABEL, on);
        out.push(match &h.body {
            Ok(c) => format!(
                "  {order}{level}{}{}{}",
                reading_cell(&format!("{:>11}", c.slope), 18, on),
                cell(&format!("{:>7}", c.expected), 11, CONTEXT, on),
                if c.attention {
                    paint(&c.reading, ATTENTION, on)
                } else {
                    c.reading.clone()
                }
            ),
            Err(text) => format!("  {order}{level}{}", paint(text, VALUE, on)),
        });
    }
    out.extend(
        l.harmonic_footer
            .iter()
            .map(|s| format!("{:w$}{}", "", paint(s, CONTEXT, on), w = LABEL_W + 2)),
    );
    out.push(String::new());

    out.extend(field_lines(&l.response, on));
    out.push(label_row(&[("band", 14), ("flatness", 0)], on));
    out.extend(l.flatness.iter().map(|f| {
        format!(
            "  {}{}",
            cell(&f.band, 14, CONTEXT, on),
            reading(&f.value, on)
        )
    }));
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

    /// Remove SGR sequences, leaving the plain text a terminal would show.
    fn strip(s: &str) -> String {
        let mut out = String::new();
        let mut it = s.chars();
        while let Some(c) = it.next() {
            if c == '\x1b' {
                for d in it.by_ref() {
                    if d == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn colour_never_moves_a_column() {
        let plain = cell("excluded", 12, ATTENTION, false);
        let painted = cell("excluded", 12, ATTENTION, true);
        assert_eq!(plain, "excluded    ");
        assert!(painted.starts_with("\x1b[38;5;179m"));
        assert!(painted.ends_with("\x1b[0m    "));
        assert_eq!(paint("pass", ATTENTION, false), "pass");
        for t in [
            "-40.0 dBFS",
            "  -88.1 dB",
            "41.2 dB at 12.5 kHz",
            "not measured",
        ] {
            assert_eq!(
                strip(&reading_cell(t, 25, true)),
                reading_cell(t, 25, false)
            );
        }
    }

    /// UX colour spec: labels 247, values 254, units and context 241,
    /// attention 179; `used` and `pass` stay uncoloured.
    #[test]
    fn labels_values_and_units_take_their_own_grey() {
        assert_eq!(
            reading("41.2 dB at 12.5 kHz", true),
            "\x1b[38;5;254m41.2\x1b[0m \x1b[38;5;241mdB at 12.5 kHz\x1b[0m"
        );
        assert_eq!(
            reading("  \u{b1}0.21 dB", true),
            "  \x1b[38;5;254m\u{b1}0.21\x1b[0m \x1b[38;5;241mdB\x1b[0m"
        );
        assert_eq!(
            reading("not measured", true),
            "\x1b[38;5;254mnot measured\x1b[0m"
        );

        let field = Field {
            label: "order",
            lines: vec!["stimulus level, ascending".into()],
        };
        let line = &field_lines(&field, true)[0];
        assert!(line.starts_with("  \x1b[38;5;247morder\x1b[0m"));
        assert!(line.ends_with("\x1b[38;5;254mstimulus level, ascending\x1b[0m"));

        let row = RunRow {
            run: "1".into(),
            level: "-40.0 dBFS".into(),
            snr: "61.4 dB".into(),
            tail: "41.2 dB at 12.5 kHz".into(),
            status: "used".into(),
            tail_attention: false,
            status_attention: false,
        };
        let painted = run_line(&row, RowStyle::Values, true);
        assert!(painted.ends_with("\x1b[38;5;241mdB at 12.5 kHz\x1b[0m      used"));
        assert_eq!(
            strip(&painted),
            run_line(&row, RowStyle::Values, false),
            "colour must not change the plain text"
        );
        assert_eq!(
            run_line(&row, RowStyle::Values, false),
            "    1  -40.0 dBFS   61.4 dB           41.2 dB at 12.5 kHz      used"
        );
        let thresholds = RunRow {
            run: String::new(),
            level: String::new(),
            snr: "min 18.0 dB".into(),
            tail: "min 30.0 dB".into(),
            status: String::new(),
            tail_attention: false,
            status_attention: false,
        };
        assert!(run_line(&thresholds, RowStyle::Context, true)
            .contains("\x1b[38;5;241mmin 18.0 dB\x1b[0m"));
    }
}
