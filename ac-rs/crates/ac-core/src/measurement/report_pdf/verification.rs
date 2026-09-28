//! Paint a [`VerificationLayout`] as a PDF (#398).
//!
//! Painting only, from the same layout the terminal and the HTML read:
//! header blocks and every excluded / unchecked line first, then the
//! tables, then the charts. Tables are drawn here rather than through
//! `Cursor::table`, because a verification table's headers are not
//! static — the harmonic level column is headed with the drive it was read
//! at.

use anyhow::{Context, Result};
use printpdf::{BuiltinFont, Mm, PdfDocument};

use super::cursor::{Cursor, Fonts, BODY_MM, MARGIN_MM, PAGE_H_MM, PAGE_W_MM, ROW_MM, SIZE_BODY};
use super::metrics::Face;
use super::plot;
use crate::measurement::report_layout::verification::{Field, Table, VerificationLayout};

/// Column widths, millimetres, per table. A `not computed` sentence runs
/// on into the empty cells after it; every table leaves it the room.
const RUN_COLS_MM: [f32; 5] = [10.0, 26.0, 34.0, 48.0, 24.0];
const VERDICT_COLS_MM: [f32; 5] = [34.0, 28.0, 26.0, 30.0, 20.0];
const HARMONIC_COLS_MM: [f32; 5] = [14.0, 32.0, 34.0, 22.0, 30.0];
const FLATNESS_COLS_MM: [f32; 2] = [30.0, 40.0];

fn fields(cur: &mut Cursor, fields: &[Field]) {
    for f in fields {
        let mut label = f.label;
        for line in &f.lines {
            cur.kv(label, line);
            label = "";
        }
    }
}

fn table(cur: &mut Cursor, t: &Table, widths: &[f32]) {
    let header = |cur: &mut Cursor| {
        cur.ensure(ROW_MM * 2.0);
        let mut x = MARGIN_MM;
        let y = cur.y() - BODY_MM;
        for (c, w) in t.columns.iter().zip(widths) {
            cur.text_at(c, SIZE_BODY, x, y, Face::Bold);
            x += w;
        }
        cur.advance(BODY_MM + 0.8);
        cur.hline(cur.y(), MARGIN_MM, PAGE_W_MM - MARGIN_MM, 0.3);
        cur.advance(1.2);
    };
    header(cur);
    for row in &t.rows {
        if cur.would_overflow(ROW_MM) {
            cur.new_page();
            header(cur);
        }
        let mut x = MARGIN_MM;
        let y = cur.y() - BODY_MM;
        for (cell, w) in row.iter().zip(widths) {
            if !cell.is_empty() {
                cur.text_at(cell, SIZE_BODY, x, y, Face::Mono);
            }
            x += w;
        }
        cur.advance(ROW_MM);
    }
}

/// Render a verification layout as a PDF byte stream.
pub fn render_verification_pdf(l: &VerificationLayout) -> Result<Vec<u8>> {
    let (doc, page, layer) = PdfDocument::new(l.title, Mm(PAGE_W_MM), Mm(PAGE_H_MM), "Layer 1");
    let first = doc.get_page(page).get_layer(layer);
    let fonts = Fonts {
        regular: doc
            .add_builtin_font(BuiltinFont::Helvetica)
            .context("add Helvetica")?,
        bold: doc
            .add_builtin_font(BuiltinFont::HelveticaBold)
            .context("add Helvetica-Bold")?,
        mono: doc
            .add_builtin_font(BuiltinFont::Courier)
            .context("add Courier")?,
    };
    let mut cur = Cursor::new(&doc, &fonts, first);

    cur.title(&format!("{}  {}", l.title, l.rendered_utc));
    fields(&mut cur, &l.header);
    fields(&mut cur, &l.warnings);

    cur.heading("Runs");
    table(&mut cur, &l.run_table(), &RUN_COLS_MM);

    cur.heading("Verdicts");
    table(&mut cur, &l.verdict_table(), &VERDICT_COLS_MM);

    cur.heading("Harmonics");
    fields(&mut cur, std::slice::from_ref(&l.harmonics));
    table(&mut cur, &l.harmonic_table(), &HARMONIC_COLS_MM);
    for line in &l.harmonic_footer {
        cur.note(line);
    }

    cur.heading("Response");
    fields(&mut cur, std::slice::from_ref(&l.response));
    table(&mut cur, &l.flatness_table(), &FLATNESS_COLS_MM);

    cur.heading("Charts");
    for c in &l.charts {
        plot::draw_chart(&mut cur, c);
    }

    doc.save_to_bytes().context("serialize PDF")
}

#[cfg(test)]
mod tests {
    use super::super::cursor::winansi_char;
    use super::*;
    use crate::measurement::report_layout::verification::testkit::sample_layout;
    use printpdf::lopdf::content::Content;
    use printpdf::lopdf::Document;

    /// Every placed text run, decoded as WinAnsi — the encoding the core
    /// fonts draw in. `lopdf`'s own `extract_text` reads these bytes in
    /// another encoding and loses the en dash and the plus-minus sign.
    fn all_text(pdf: &[u8]) -> String {
        let doc = Document::load_mem(pdf).expect("parse pdf");
        let mut pages: Vec<(u32, _)> = doc.get_pages().into_iter().collect();
        pages.sort_by_key(|(n, _)| *n);
        let mut out = Vec::new();
        for (_, id) in pages {
            let data = doc.get_page_content(id).expect("page content");
            for op in Content::decode(&data).expect("decode").operations {
                if op.operator == "Tj" {
                    let bytes = op.operands[0].as_str().expect("string operand");
                    out.push(bytes.iter().map(|b| winansi_char(*b)).collect::<String>());
                }
            }
        }
        out.join("\n")
    }

    /// The PDF's core fonts spell `≤` as `<=`; everything else a verdict
    /// string carries is WinAnsi and survives as written.
    fn as_drawn(s: &str) -> String {
        s.replace('\u{2264}', "<=")
    }

    #[test]
    fn the_pdf_prints_the_same_verdict_strings_as_the_layout() {
        let l = sample_layout();
        let pdf = render_verification_pdf(&l).expect("render");
        assert!(pdf.starts_with(b"%PDF-"));
        let text = all_text(&pdf);
        let strings = l.verdict_strings();
        assert!(strings.iter().any(|s| s == "pass" || s == "fail"));
        for s in strings {
            assert!(text.contains(&as_drawn(&s)), "missing {s:?} in\n{text}");
        }
    }

    #[test]
    fn the_pdf_carries_the_header_blocks_and_the_exclusion() {
        let l = sample_layout();
        let text = all_text(&render_verification_pdf(&l).expect("render"));
        for needle in [
            "not compared: interface output volume not recorded",
            "lower edge 1.5 kHz, fixed; below it the response is the room",
            "run 2: tail decay 11.6 dB at 16 kHz, 30.0 dB required",
            "excluded",
            "min 18.0 dB",
        ] {
            assert!(text.contains(needle), "missing {needle:?} in\n{text}");
        }
    }
}
