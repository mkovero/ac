//! Paint a [`VerificationLayout`] as a self-contained HTML page (#398).
//!
//! Painting only: every string and every chart geometry decision comes
//! from `report_layout::verification`, the same layout the terminal and
//! the PDF read. Order follows the UX spec — the header blocks (mic curve,
//! absolute, bands) and every excluded / unchecked line come before any
//! table, and the charts come after the numbers they illustrate.

use std::fmt::Write as _;

use super::{html_escape, plot, CSS};
use crate::measurement::report_layout::verification::{Field, Table, VerificationLayout};

/// Extra rules for the verification page: attention states and band marks.
const VERIFY_CSS: &str = r#"
.attention { color: #a66a00; font-weight: 600; }
td.pre, dd.pre { white-space: pre; }
svg .series { fill: none; stroke-width: 1.6; }
svg .band { stroke: #bbb; stroke-width: 1; fill: #888; }
svg .title { font-size: 12px; font-weight: 600; }
"#;

/// Cell text that marks a state the reader must not skim past.
fn is_attention(cell: &str) -> bool {
    matches!(
        cell,
        "fail"
            | "excluded"
            | "unchecked"
            | "not recorded"
            | "not evaluated"
            | "floor-limited"
            | "rises faster"
    )
}

fn write_fields(out: &mut String, fields: &[Field]) {
    if fields.is_empty() {
        return;
    }
    let _ = writeln!(out, "<dl class=\"meta\">");
    for f in fields {
        let class = if matches!(f.label, "excluded" | "unchecked") {
            " class=\"attention\""
        } else {
            ""
        };
        let _ = writeln!(
            out,
            "<dt{class}>{}</dt><dd class=\"pre\">{}</dd>",
            html_escape(f.label),
            f.lines
                .iter()
                .map(|l| html_escape(l))
                .collect::<Vec<_>>()
                .join("<br>")
        );
    }
    let _ = writeln!(out, "</dl>");
}

/// A table whose first column is a label; `not computed` sentences span
/// the remaining empty cells rather than squeezing into one.
fn write_table(out: &mut String, t: &Table) {
    let _ = write!(out, "<table><thead><tr>");
    for (i, c) in t.columns.iter().enumerate() {
        let class = if i == 0 { " class=\"label\"" } else { "" };
        let _ = write!(out, "<th{class}>{}</th>", html_escape(c));
    }
    let _ = writeln!(out, "</tr></thead><tbody>");
    for row in &t.rows {
        let _ = write!(out, "<tr>");
        let filled = row.iter().rposition(|c| !c.is_empty()).map_or(0, |i| i + 1);
        for (i, cell) in row.iter().enumerate().take(filled.max(1)) {
            let mut classes = Vec::new();
            if i == 0 {
                classes.push("label");
            }
            if is_attention(cell) {
                classes.push("attention");
            }
            let span = if i + 1 == filled && filled < row.len() {
                format!(" colspan=\"{}\"", row.len() - i)
            } else {
                String::new()
            };
            let class = if classes.is_empty() {
                String::new()
            } else {
                format!(" class=\"{}\"", classes.join(" "))
            };
            let _ = write!(out, "<td{class}{span}>{}</td>", html_escape(cell));
        }
        let _ = writeln!(out, "</tr>");
    }
    let _ = writeln!(out, "</tbody></table>");
}

/// Render a verification layout as a self-contained HTML document.
pub fn render_verification_html(l: &VerificationLayout) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "<!DOCTYPE html>");
    let _ = writeln!(out, "<html lang=\"en\"><head>");
    let _ = writeln!(out, "<meta charset=\"UTF-8\">");
    let _ = writeln!(out, "<title>{}</title>", html_escape(l.title));
    let _ = writeln!(out, "<style>{CSS}{VERIFY_CSS}</style>");
    let _ = writeln!(out, "</head><body>");
    let _ = writeln!(
        out,
        "<h1>{} <small>{}</small></h1>",
        html_escape(l.title),
        html_escape(&l.rendered_utc)
    );

    write_fields(&mut out, &l.header);
    write_fields(&mut out, &l.warnings);

    let _ = writeln!(out, "<h2>Runs</h2>");
    write_table(&mut out, &l.run_table());

    let _ = writeln!(out, "<h2>Verdicts</h2>");
    write_table(&mut out, &l.verdict_table());

    let _ = writeln!(out, "<h2>Harmonics</h2>");
    write_fields(&mut out, std::slice::from_ref(&l.harmonics));
    write_table(&mut out, &l.harmonic_table());
    let _ = writeln!(
        out,
        "<p class=\"note\">{}</p>",
        l.harmonic_footer
            .iter()
            .map(|s| html_escape(s))
            .collect::<Vec<_>>()
            .join("<br>")
    );

    let _ = writeln!(out, "<h2>Response</h2>");
    write_fields(&mut out, std::slice::from_ref(&l.response));
    write_table(&mut out, &l.flatness_table());

    let _ = writeln!(out, "<h2>Charts</h2>");
    for c in &l.charts {
        out.push_str(&plot::chart(c));
    }

    let _ = writeln!(out, "</body></html>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::measurement::report_layout::verification::testkit::sample_layout;

    #[test]
    fn every_verdict_string_of_the_layout_appears_in_the_html() {
        let l = sample_layout();
        let html = render_verification_html(&l);
        let strings = l.verdict_strings();
        assert!(strings.iter().any(|s| s == "pass" || s == "fail"));
        for s in strings {
            assert!(html.contains(&html_escape(&s)), "missing {s:?}");
        }
    }

    #[test]
    fn header_blocks_and_warnings_come_before_any_chart_and_are_escaped() {
        let html = render_verification_html(&sample_layout());
        let first_svg = html.find("<svg").expect("charts drawn");
        for needle in [
            "mic curve",
            "not compared: interface output volume not recorded",
            "lower edge 1.5 kHz, fixed",
            "run 2: tail decay 11.6 dB at 16 kHz",
        ] {
            let at = html
                .find(needle)
                .unwrap_or_else(|| panic!("missing {needle}"));
            assert!(at < first_svg, "{needle} after the first chart");
        }
        assert!(html.contains("run&lt;0&gt;.json"));
        assert!(!html.contains("run<0>.json"));
        assert_eq!(html.matches("<svg").count(), 4);
        assert!(html.contains("class=\"label attention\"") || html.contains("class=\"attention\""));
    }
}
