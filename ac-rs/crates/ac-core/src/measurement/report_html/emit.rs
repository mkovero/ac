//! The two repeated HTML shapes: a `<dl class="meta">` key/value block
//! and a `<table>`.

use std::fmt::Write as _;

use super::html_escape;
use crate::measurement::report_layout::{Column, Row};

/// Key/value block. Emits nothing at all for an empty row list, so a
/// caller can hand over an optional block unconditionally.
pub(super) fn write_rows(out: &mut String, rows: &[Row]) {
    if rows.is_empty() {
        return;
    }
    let _ = writeln!(out, "<dl class=\"meta\">");
    for row in rows {
        // `label_html` is trusted markup from the layout module (a
        // subscript); `label` and every value are escaped.
        let label = match row.label_html {
            Some(markup) => markup.to_string(),
            None => html_escape(row.label),
        };
        let _ = writeln!(out, "<dt>{label}</dt><dd>{}</dd>", html_escape(&row.value));
    }
    let _ = writeln!(out, "</dl>");
}

pub(super) fn write_table(out: &mut String, columns: &[Column], rows: &[Vec<String>]) {
    let _ = write!(out, "<table><thead><tr>");
    for c in columns {
        let _ = write!(
            out,
            "<th{}>{}</th>",
            if c.label { " class=\"label\"" } else { "" },
            html_escape(c.html)
        );
    }
    let _ = writeln!(out, "</tr></thead><tbody>");
    for row in rows {
        let _ = write!(out, "<tr>");
        for (cell, c) in row.iter().zip(columns) {
            let _ = write!(
                out,
                "<td{}>{}</td>",
                if c.label { " class=\"label\"" } else { "" },
                html_escape(cell)
            );
        }
        let _ = writeln!(out, "</tr>");
    }
    let _ = writeln!(out, "</tbody></table>");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_row_list_emits_no_empty_dl() {
        let mut out = String::new();
        write_rows(&mut out, &[]);
        assert!(out.is_empty(), "{out}");
    }

    #[test]
    fn values_are_escaped_but_html_labels_are_not() {
        let mut out = String::new();
        write_rows(
            &mut out,
            &[
                Row::new("path", "<script>x</script>"),
                Row::html_label("V_RMS", "V<sub>RMS</sub>", "1.0 V"),
            ],
        );
        assert!(out.contains("&lt;script&gt;"), "{out}");
        assert!(!out.contains("<script>"), "{out}");
        assert!(out.contains("<dt>V<sub>RMS</sub></dt>"), "{out}");
    }

    #[test]
    fn table_cells_are_escaped() {
        let cols = crate::measurement::report_layout::spectrum_columns();
        let mut out = String::new();
        write_table(&mut out, cols, &[vec!["<b>".into(), "-30.00".into()]]);
        assert!(out.contains("&lt;b&gt;"), "{out}");
        assert!(!out.contains("<b>"), "{out}");
    }

    #[test]
    fn a_ragged_row_does_not_shift_later_cells_under_wrong_headers() {
        // Zipping truncates rather than misaligning: a short row loses
        // its trailing cells instead of silently borrowing a neighbour
        // column's header.
        let cols = crate::measurement::report_layout::spectrum_columns();
        let mut out = String::new();
        write_table(&mut out, cols, &[vec!["100.00".into()]]);
        assert_eq!(out.matches("<td").count(), 1, "{out}");
    }
}
